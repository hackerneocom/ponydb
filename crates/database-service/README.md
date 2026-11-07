**English** | [中文](README.zh-CN.md)

# database-service

The 11 business operations behind HTTP, MCP, and the native CLI. Transport code authenticates the caller and builds an `protocol::Operation`. This crate checks authorization, talks to `versioned-store`, and runs SQL through `engine-sqlite`.

## Purpose

All entries share authorization, transaction atomicity, publication, and error semantics. Each SQL call executes independently; there is no request deduplication, ledger, or response replay.

`Service::call` is the single dispatch. It checks the request deadline, runs the operation, and cancels the context when it returns so leftover snapshot reads stop.

## How it works

`Service::new` requires a `BlobStore` whose capabilities validate, then starts a `WorkerPool`. `ServiceOptions` carries worker and queue sizes, SQL execution limits, segment size, snapshot retention and the overlay hard limit. `runtime-config` fills those options from the shared TOML file.

Authorization uses `Principal::allows`. Scopes are specific (`db:list`, `db:read`, `data:read`, `data:write`, `schema:write`, `schema:read`, `db:maintenance`, `db:restore`, `db:backup`). The CLI principal is the direct holder of the S3 credential and is granted `*`.

HEAD contention returns `TX_CONFLICT`. An unconfirmed commit returns `OUTCOME_UNKNOWN`, with `retryable: false` and `outcome: unknown`; the caller must inspect business data before deciding to submit again.

The ETag is the object storage ETag returned by the same read of `head.json`, passed through unchanged without additional storage requests. Metadata updates and deletes may send that value as optional `if_match` and fail with `METADATA_PRECONDITION_FAILED` when it does not match the head that was read. Omitting `if_match` operates on the current head; publishing still uses an atomic conditional write to reject concurrent head changes. The commit token is an opaque encoding of namespace, database, incarnation, and `data_version`, used by `at_least` reads.

A service instance owns one shared `ObjectReader`. Calls and SQL inspection/execution reuse verified immutable manifests, index nodes, and data objects, retaining at most 16 MiB of bodies and 8192 entries. The cache is bound to the backend and full physical object identity; HEAD is never cached. A new CLI process starts with an empty cache.

### SQL

`sql_execute` loads the head, then asks the worker pool to inspect the statement. Inspection compiles it and reads SQLite's read-only flag without stepping the statement.

A read-only statement needs `data:read`. Consistency selects the manifest:

- `strong` — the current head, which must be `active`
- `snapshot` — a history record addressed by `snapshot_id`, which needs `db:backup`
- `at_least` — the current head, after checking that `commit_token` names this namespace, database, and incarnation, and a `data_version` no newer than the head

Reads return typed results with `outcome: read_only` and publish no version. `max_rows` can only tighten the limit; oversized results fail rather than truncate.

Writes require `data:write` or `schema:write` and reject query-only options. The worker executes SQL and freezes pages; the service writes data dependencies and snapshot history before `publish_head` atomically publishes them. Repeated DDL is compiled normally and may fail because the table already exists. `SQL_ERROR` includes the SQLite diagnostic, such as a missing table, a constraint failure, `not authorized`, or multiple statements. Cancellation inside a statement is `DEADLINE_EXCEEDED`. Workspace and snapshot failures keep a stable message and do not include object keys or local paths.

A `sql_execute` request with `statements` uses the same publish path, always as a write, and rejects read options. `schema_get` is a read-only `sql_execute` of `sqlite_schema`.

### Database lifecycle

Creation requires an absent or deleted target; GC does not block lifecycle changes, which still compete on the same HEAD CAS. Initial creation uses create-if-absent and tombstone reuse uses CAS. Each creation gets a fresh internal storage identity and snapshot history. Deletion leaves a tombstone; old snapshots are unavailable after recreation. Clients address the current logical database by its immutable name, so a late request may operate on its replacement.

### Maintenance

`database_compact` merges delta pages into base segments within its budget. `database_gc` runs online: every call claims a new epoch, marks current and retained snapshot roots, lists old-generation objects, then deletes unreachable candidates after the reader grace period. Reachability, frontier and candidates live in a temporary local SQLite file; no GC objects are uploaded. Calls clean up only their own HEAD.gc record, preserving concurrent commits. A stale record is automatically replaced by the next GC; it never blocks database access or lifecycle changes. `complete: false` / `outcome: "partial"` means another independent call may reclaim more objects. GC records retirement and expires non-current history according to HEAD retention. See [online GC](../../docs/online-gc.md).

Compaction source reads preserve typed errors: deadline expiry and cancellation return `DEADLINE_EXCEEDED`; storage access and transport failures retain `STORAGE_UNAVAILABLE` diagnostics and retryability; missing referenced objects and failed integrity checks return `STORAGE_CORRUPT`. A source read failure does not publish a new HEAD.

### Restore

`db DATABASE_NAME restore file SQLITE_FILE` is available only in the CLI. It copies a stable, standalone SQLite backup in DELETE journal mode to a local temporary file, validates SQLite integrity, foreign keys and supported schema, computes SHA-256, and uploads formal database segments directly to S3. It then builds the index, manifest and first snapshot, and conditionally publishes HEAD. There is no staging upload, file download, restore session, or resumable progress file. The target must be absent or deleted; active databases are never overwritten.

Restore has no total execution deadline and rejects `--deadline-ms`; request timeout defaults do not apply. Individual S3 operations retain their timeouts. No restore file-size or disk-space quota is configured: insufficient temporary disk space fails the operation and removes the temporary file. Ctrl+C cancels local work and further uploads; publication already in flight must still have its result confirmed. Failed unpublished objects can be reclaimed by later GC after a database HEAD exists. A definitive failure requires running the whole command again; `OUTCOME_UNKNOWN` requires inspecting the database before deciding whether to retry.

Stages and progress always go to stderr; stdout contains only the final JSON/table result (`committed`, `outcome`, `name`, `data_version`, `total_bytes`, `sha256`, `page_size`). Interactive terminals refresh in place at most every 200 ms; redirected stderr contains stage changes and progress lines at most every five seconds within a stage. Copying, hashing and confirmed segment uploads report bytes and percentage; validation, index construction and publication report stage and elapsed time. Upload reaching 100% is not completion: HEAD publication must succeed. HTTP and MCP do not expose restore.

### Operations

| Operation | Effect on HEAD |
| --- | --- |
| `database_create` | create |
| `database_list`, `database_get`, `schema_get` | read |
| `database_update`, `database_delete` | metadata CAS |
| `sql_execute` | read, or publish when the statement writes |
| `database_compact`, `database_gc` | publish, possibly partial |
| `snapshot_list`, `snapshot_get` | read history |

Object storage errors use the shared JSON error fields and business error codes. When available, `message` includes the S3 operation, HTTP status, provider error code and provider message, for example `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` Configured access keys, secret keys and session tokens are replaced with `[REDACTED]`; control characters become spaces, and provider codes/messages are limited to 128/1024 characters plus an ellipsis. This minimal redaction does not hide all resource information echoed by the provider. Request/response headers, raw response bodies and SDK error chains are not included. Remote HTTP errors are retryable for 408, 429 and 5xx; other statuses are not retryable. Not-found, conditional-write and uncertain-commit handling takes precedence. CLI, HTTP and MCP share these diagnostics.

### Binary snapshot export

`Service::export_database` accepts a database name, principal, request context and async writer. It reads HEAD once and pins the active database's current Manifest and version; deleted databases return `DATABASE_DELETED`. The CLI-only `db DATABASE_NAME export SQLITE_FILE` entrypoint shares backup permission (`db:backup`), size/read limits, verification and SHA-256 logic with historical export. It adds no JSON operation or remote endpoint.

`Service::export_snapshot` accepts a strict `protocol::SnapshotExport` request, a principal, request context and async writer. It requires `db:backup` and materializes a retained Manifest in verified ranges of at most 1 MiB, returning snapshot ID, data version, byte length and SHA-256. Callers own temporary files and publish only on success. Storage errors retain their typed mapping; cancellation stops pending reads/writes. Reads are capped by the request and HEAD read lease, preserving GC drain safety. Deleted history is allowed until recreation; online GC is allowed. This binary API is separate from the 11 JSON operations and is exposed by CLI and HTTP only.

Explicit GC records `retired_at_upper_ms` for superseded snapshots and expires them on later calls after the HEAD retention period. The first observation starts the N-day wait; zero days removes all non-current history. Current and shared data remain protected, and physical deletion still waits for reader drain. Time uses timezone-independent Unix milliseconds; deployments must keep system clocks accurate.

`schema_get` uses a dedicated DTO: required `database`, optional nonempty `table`, and optional `type` (`table`, `index`, `view`, `trigger`). `table` matches `sqlite_schema.tbl_name` exactly and case-sensitively, including associated indexes and triggers, without resolving view dependencies. Filters combine with AND. A specified table must exist as a non-internal table; otherwise `TABLE_NOT_FOUND` is returned (HTTP 404, CLI exit 1). An existing table with no matching object type, or a type-only filter with no matches, returns empty rows. Existence and definitions are read in one query from the same snapshot. HTTP uses the same query parameter names, CLI uses `--table` and `--type`, and MCP advertises these fields. Queries bind filter values and retain internal-object exclusion, ordering, and result budgets.

`storage-namespace` owns path and boundary rules: references, cache lookups and GC bind to the complete database root prefix. Restoring a deleted database switches incarnation and rebuilds snapshot history. Old snapshots are no longer queryable after publication, even while their objects remain pending GC. Existing readers retain their original leases, and GC preserves the cross-lifecycle drain window.

## Runtime diagnostics

Business calls, exports and CLI restores share request IDs and debug phase/outcome summaries. SQL input and result contents are never logged. Libraries do not install a global subscriber.
