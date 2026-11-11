**English** | [中文](README.zh-CN.md)

# ponydb

[User guide](../../skills/ponydb/references/cli.md) · [Configuration](../../skills/ponydb/references/configuration.md) · [AI Agent clients](../../skills/ponydb/references/agent-clients.md)

Native CLI for Windows, Linux, and macOS. It loads the same configuration as `ponydb server`, opens S3 from this process, and calls `database-service` directly. No server process is required.

The [shared example](../../examples/ponydb.toml) lists the settings users can adjust that affect runtime behavior, with only the four required S3 fields active. Uncomment an optional section header together with its settings to override defaults. Token, authentication and directory examples are marked separately from built-in defaults.

## Purpose

All entries call the same business layer with 11 operations. Each write executes independently; responses are not stored or replayed. Inspect business data before deciding to retry an uncertain write.

The principal is `Principal::direct`, the holder of the S3 credentials in this process. Network authorization is the bucket policy, not `server.auth`.

## How it works

Database names are explicit positional arguments. Common options are `--config`, `--output`, and `--deadline-ms`. Validate them against the final command: server/MCP reject output format; config/version/server/restore reject deadlines. Deadlines use the requests configuration defaults.

Use `databases` for create/list, and `db DATABASE_NAME` for details or database actions (execute, schema, snapshot, restore and maintenance), plus `config` and `version`. JSON business commands accept flags for common fields and `--input` for a full DTO; file restore requires its positional file. `--sql-file`, `--params-file`, and `--metadata-file` read UTF-8, optionally with a BOM. At most one argument may be `-` for stdin. Files are parsed with `protocol::strict_json`. Duplicate keys, unknown fields, and a flag that disagrees with the file fail before any S3 call.

Create/update accept at most **64 KiB (65,536 bytes) of compact UTF-8 JSON** in `metadata`, including keys, punctuation and escaping; formatting whitespace is excluded. `description` accepts at most **16 KiB of UTF-8 bytes**. Both `--metadata-file METADATA_FILE|-` and `--input REQUEST_FILE|-` use these limits. Oversized fields fail locally with `INVALID_REQUEST` (exit 2), before S3 access. The limits reserve space for internal HEAD fields within its 1 MiB cap. Reading persisted HEADs uses the storage-format limits; updates can reduce metadata that exceeds the request limit. HTTP and MCP use the same validation.

`config validate` first checks local settings (and `server.auth` for `--target server|all`), then probes the configured S3 bucket and prefix using the configured credentials. It verifies conditional creation (`If-None-Match: *`), matching/stale CAS (`If-Match`), four rounds of concurrent create/CAS, immediate integrity-checked GET, Range GET, a PUT/GET of up to 2 MiB, and paginated `ListObjectsV2` with and without `/` delimiter. Probes use a random `_ponydb-validation/<uuid>/` beneath `storage.prefix`, require List/Get/Put/Delete permission, and never modify database keys. Only keys attempted by this run are deleted; GET and LIST verify cleanup. Success reports `storage_capabilities_verified: true`. Failures return `INVALID_CONFIG` (exit 2) with the failed check and sanitized provider diagnostics; cleanup failures or uncertain writes also identify the relative probe prefix for inspection. The probe budget is `requests.default_timeout_ms`, followed by up to 30 seconds for cleanup. A killed process or a late PUT after timeout can leave probe objects. These finite checks do not verify the provider's maximum object size or prove behavior under every failure or future load. `config show` remains offline. `version` prints the program version, the bundled SQLite version and source id, the MCP protocol version, layout version 1, and the operation count.


Successful JSON is printed to stdout with two-space indentation and line breaks. `--output table` renders rows as a text table. A compact or GC result with `outcome: "partial"` still prints the body and exits 6. Compaction reports `complete` without `resume_required`; call compact again if incomplete. Errors use the same pretty-printed JSON format on stderr.

| Exit code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Other error |
| 2 | Invalid configuration or request |
| 3 | `UNAUTHENTICATED` or `FORBIDDEN` |
| 4 | `TX_CONFLICT` or `METADATA_PRECONDITION_FAILED` |
| 5 | `OUTCOME_UNKNOWN` — inspect business data before retrying |
| 6 | Stopped inside this call's budget; run the operation again to continue |

## Argument conventions

Bundled SQLite includes math functions without additional configuration. `sqrt(-1)` returns NULL; a non-finite result such as `exp(1000)` returns `SQL_ERROR` (exit 1).

JSON / JSONB operations are covered by the CLI/S3 end-to-end tests, including persistence, updates, traversal, and rollback on malformed JSON. See the [test instructions](../../tests/README.md) for the local RustFS setup. Raw JSONB results use the protocol's `blob` type (base64); use `json(...)` to return JSON text.

E2E tests live in separate command modules under `tests/cli_e2e/` and cover every
CLI command, including HTTP server and stdio MCP startup, requests, and shutdown.
From the repository root, run `python3 tests/cli_e2e.py --list` to list groups,
`python3 tests/cli_e2e.py --suite execute --suite schema` to select groups (with
`ACCESS_KEY` and `SECRET_KEY` set), or `python3 tests/cli_e2e.py --offline` for
checks without S3. Test methods use independent fixtures and clean their S3 prefixes
even after assertion failures; actual help output is checked for command coverage.
Use `--suite server --suite mcp` to check HTTP auth/ETags, SQL rollback and NDJSON,
snapshot downloads, server restart, HTTP MCP headers, every stdio MCP tool,
legacy negotiation, protocol error recovery, cancellation, deadlines, and EOF shutdown.
The [CI E2E job](../../.github/workflows/ci.yml) builds the CLI and runs every suite
against an automatically initialized RustFS container on push and pull requests.

Operation subjects are positional; behavior controls remain options. Database names use `DATABASE_NAME`, including creation and all single-database operations. File placeholders identify their purpose: `CONFIG_FILE`, `REQUEST_FILE`, `SQL_FILE`, `PARAMS_FILE`, `METADATA_FILE`, `SQLITE_FILE`. Snapshot identifiers use `SNAPSHOT_ID`. Each command accepts only its own request arguments:

```sh
ponydb databases create notes --description "My notes"
ponydb db notes
ponydb db notes update --if-match ETAG --description "Updated notes"
ponydb db notes update --description "Updated notes"
ponydb db notes delete --if-match ETAG
ponydb db notes compact
ponydb db notes compact --budget-ms 5000 --max-segments 16
ponydb db notes gc --budget-ms 60000 --max-objects 1000
ponydb db notes schema
ponydb db notes schema --table notes --type index
ponydb db notes execute "SELECT 1" --max-rows 10
ponydb db notes execute "SELECT sqrt(9), pow(2, 3), log(100)"
ponydb db notes execute "SELECT json_extract(jsonb_object('count', 2), '$.count')"
ponydb db notes snapshot get SNAPSHOT_ID
ponydb db notes restore file backup.sqlite
```

`schema` accepts `--table TABLE_NAME` and `--type table|index|view|trigger`, also available as `table` and `type` in `--input`. The table filter matches `sqlite_schema.tbl_name` exactly and case-sensitively, including associated indexes and triggers; it has no wildcards or view dependency resolution. Filters combine with AND. A specified table must exist as a non-internal table; otherwise `TABLE_NOT_FOUND` is returned (HTTP 404, CLI exit 1). An existing table with no matching object type, or a type-only filter with no matches, returns empty rows. Existence and definitions are read in one query from the same snapshot. Omitting filters returns all non-internal objects, subject to the existing query budgets.

Compact accepts optional `--budget-ms` (default 10000, range 1–60000 milliseconds) and `--max-segments` (default 32, range 1–32). Each omitted field uses its default after merging the `--input` DTO with explicit options; conflicts are rejected. GC accepts optional `--budget-ms` (default 60000, configured by `maintenance.gc_default_budget_ms`) and `--max-objects` (default and deployment cap 1000, configured by `maintenance.gc_max_objects_per_call`). Explicit budgets can exceed the default up to `requests.max_timeout_ms` (1800000 maximum). DTO fields may supply these maintenance values.

CLI update and delete optionally accept `--if-match ETAG` or DTO `if_match`, using the complete object etag returned by `db DATABASE_NAME` (including embedded quotes). Omitting it operates on the current HEAD; publishing still uses conditional writes to reject concurrent changes. DTOs can supply other required fields, but database names must be explicit; file restore requires SQLITE_FILE. SQL and `--sql-file FILE|-` are mutually exclusive; use `--input` for statement arrays.

For conditional updates/deletes, pass the complete `etag` string returned by `db DATABASE_NAME`, including its embedded double quotes. For example, `--if-match '"v1-HASH"'`.

Database names are explicit positional arguments. Common options are `--config`, `--output`, and `--deadline-ms`. Validate them against the final command: server/MCP reject output format; config/version/server/restore reject deadlines. Deadlines use the requests configuration defaults.


Execute multiple statements atomically with `db DATABASE_NAME execute --input statements.json`. Supply either `sql`/`params` or a nonempty `statements` array, with typed parameters per statement. No `BEGIN`/`COMMIT` is needed: any failure rolls back the entire request, and results follow statement order. Arrays retain write-transaction semantics: they require write permission and publish one version even for queries, and reject read options such as `max_rows`, snapshots and commit tokens. `--sql-file` still accepts one statement; SQL is never split on semicolons.

```json
{
  "database": "personal_notes",
  "statements": [
    {"sql": "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"},
    {"sql": "INSERT INTO notes VALUES(?, ?)", "params": [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}]},
    {"sql": "SELECT * FROM notes"}
  ]
}
```

## AI Agent clients over stdio MCP

Run `ponydb --config /absolute/path/ponydb.toml mcp`. The client launches this long-lived process; no HTTP server or bearer token is needed. It uses the same S3 credentials and direct principal as ordinary CLI commands, with access to all 11 tools. `mcp.enabled` controls HTTP only.

For clients accepting an `mcpServers` configuration:

```json
{
  "mcpServers": {
    "ponydb": {
      "command": "/absolute/path/ponydb",
      "args": ["--config", "/absolute/path/ponydb.toml", "mcp"]
    }
  }
}
```

Client configuration formats vary; the command and arguments above are the process invocation. Storage credentials may also be supplied through the existing `PONYDB_STORAGE__CREDENTIALS__*` environment variables.

The stdio transport supports the current `2026-07-28` protocol plus legacy versions `2025-11-25`, `2025-06-18`, `2025-03-26`, and `2024-11-05`.

Modern requests need no initialization handshake. Every request includes `params._meta` with `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`; `io.modelcontextprotocol/clientInfo` is optional. For example:

```json
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
```

`server/discover` returns all supported versions and tool capabilities. Clients may also call `tools/list` or `tools/call` directly with the same metadata. Missing required metadata returns `-32602`; unsupported versions return `-32022` with `data.supported` and `data.requested`. Modern results retain `resultType: complete`. Cancelled modern requests emit no further responses.

Legacy clients use `initialize` and `notifications/initialized`. Unknown versions offered through this legacy handshake receive `2025-11-25` for the client to accept or disconnect. Older versions omit unsupported structured results/schema fields. Modern requests and legacy calls can coexist in one process; response formatting is scoped to each request. `ponydb version` reports the latest stdio version and the full supported list.

The implementation follows the official [versioning](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) and [stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio) specifications for the tools capability it exposes.

Messages are newline-delimited JSON-RPC. Stdout contains protocol messages only; startup errors go to stderr. EOF cancels outstanding work and exits. Each request uses the configured deadline (overridable with `--deadline-ms`); up to 32 requests can be in flight, and input messages are limited to 8 MiB. Cancellation is cooperative and does not undo committed writes; inspect business data before retrying uncertain writes.

Database names are explicit positional arguments. Common options are `--config`, `--output`, and `--deadline-ms`. Validate them against the final command: server/MCP reject output format; config/version/server/restore reject deadlines. Deadlines use the requests configuration defaults.


Database identity is the immutable `name`: create with `databases create notes` or
`{"name":"notes"}`. Create results and database metadata (get/list) expose only
`name`, supplied by the caller at creation.
All single-database CLI operations use `db DATABASE_NAME [ACTION]`; omitting ACTION returns details. Use `databases create DATABASE_NAME` and `databases list` for collection operations. Names are mandatory; matching DTO fields must agree with the positional name.

Names follow [MySQL database identifier rules](https://dev.mysql.com/doc/refman/8.0/en/identifiers.html)
and the [64-character limit](https://dev.mysql.com/doc/refman/8.0/en/identifier-length.html):
1–64 Unicode BMP characters, excluding NUL, `/`, `\`, `.`, and trailing ASCII space.
Pass the literal name without SQL backticks; numbers, reserved words, Chinese,
spaces within names and hyphens are accepted as for quoted MySQL identifiers.
Names are case-sensitive, preserved verbatim, and never trimmed or case-folded.
The persistent HEAD identifies the database by the same `name`.


Layout v1 uses these paths: `{prefix}databases/{name}/head.json` is the sole authoritative publication point; immutable objects use `incarnations/{incarnation_id}/generations/{epoch:016x}/{category}/{uuid}`. A name is an immutable, reusable logical identifier. Restoring a deleted database creates a new incarnation, resets snapshot history and invalidates old tokens. There is no separate database ID, global catalog or rename operation.

Object storage errors use the shared JSON error fields and business error codes. When available, `message` includes the S3 operation, HTTP status, provider error code and provider message, for example `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` Configured access keys, secret keys and session tokens are replaced with `[REDACTED]`; control characters become spaces, and provider codes/messages are limited to 128/1024 characters plus an ellipsis. This minimal redaction does not hide all resource information echoed by the provider. Request/response headers, raw response bodies and SDK error chains are not included. Remote HTTP errors are retryable for 408, 429 and 5xx; other statuses are not retryable. Not-found, conditional-write and uncertain-commit handling takes precedence. CLI, HTTP and MCP share these diagnostics.

A missing database HEAD returns `DATABASE_NOT_FOUND` with `Database 'test3' not found` (using the requested name). Check the database name and the configured storage endpoint, bucket and prefix. If an object referenced by an existing database is missing, the response is `STORAGE_CORRUPT` with `Database 'test3' storage is incomplete: a referenced object is missing`; it does not expose the object key. Both errors have `retryable: false` and `outcome: not_committed`.

HEAD validation errors also return `STORAGE_CORRUPT`, with the database name, `HEAD validation failed`, and a bounded reason. For example: `Database 'test' HEAD validation failed: unknown field 'restore'; stored HEAD may be incompatible with this version`. Reasons distinguish invalid JSON, oversized metadata, unknown or missing fields, invalid field types/values, and violated HEAD constraints. Field names are shown only for ASCII letters, digits and underscores up to 64 characters; other names are redacted. Stored field values, object keys and raw Serde errors are excluded. HEAD validation is strict. These errors have `retryable: false`, `outcome: not_committed`, and CLI exit code 1.

`storage.force_path_style` defaults to `true`, preserving path-style addressing for MinIO. Set it to `false` for virtual-hosted-style addressing required by OSS; keep the regional endpoint (for example `https://oss-cn-beijing.aliyuncs.com`) and bucket as separate settings. The SDK constructs the bucket hostname. Environment override: `PONYDB_STORAGE__FORCE_PATH_STYLE=false`. The value must be a boolean.

## Database and snapshot export

Export the current database version without looking up a snapshot ID:

```sh
ponydb db notes export backup.sqlite
ponydb db notes export --input current-export.json
```

For this command, the request file contains `{"database":"notes","file":"backup.sqlite"}`. It reads HEAD once and pins that active database's Manifest and data version; concurrent writes or compaction do not change the exported image. Deleted databases return `DATABASE_DELETED`; use `snapshot export` for retained history. This current-version entrypoint is CLI-only and shares the limits, file publication and result format described below.

Export a retained version to a standalone SQLite file, suitable for opening with SQLite or importing through `db DATABASE_NAME restore file`:

```sh
ponydb db notes snapshot export SNAPSHOT_ID backup.sqlite
ponydb db notes snapshot export --input export.json
```

The CLI request file contains `{"database":"notes","snapshot_id":"SNAPSHOT_ID","file":"backup.sqlite"}`. The database name is always explicit in the command; matching explicit fields may accompany the DTO, and conflicts are rejected. `--input -` reads JSON from stdin. The destination is a file, never stdout (`-` is rejected); its parent directory must exist. Existing destinations are never overwritten. A temporary file in the destination directory is published atomically after completion. stdout contains the usual JSON/table summary with file path, snapshot ID, data version, byte length and SHA-256.

Authenticated HTTP clients can download `GET /v1/databases/{database}/snapshots/{snapshot_id}/export` with `db:backup` permission. The server first materializes a temporary file, then sends `application/vnd.sqlite3` with `Content-Length` and an attachment filename. Failures before download use the normal JSON error envelope. Temporary files are removed on failure or when the response body is released.

`[exports].max_file_bytes` limits each file (default 10737418240 bytes / 10 GiB); simultaneous exports require separate temporary disk space. Materialization follows the existing request deadline and the database's maximum read duration, so a large or slow export may time out even below the size limit. HTTP transmission uses the remaining request deadline. Each export pins one Manifest and does not publish a version. Retained history of a deleted database remains exportable until recreation; restoring a deleted target invalidates old snapshots only when the new HEAD is published. MCP exports and resumable downloads are not supported.

GC runs online and accepts defaults: `ponydb db notes gc`. Each call starts a fresh mark and returns the database available even when its work budget is exhausted. `max_objects` limits DELETE attempts; `deleted_objects` counts confirmed deletions only. Partial success exits with code 6 and `outcome: "partial"`; `complete` concerns this call's discovered candidates, not future garbage. No pass ID, pause flag, recovery command or resume flag is required. Request timeout is independent (default 90000 ms); for a five-minute budget use `ponydb db notes gc --budget-ms 300000 --deadline-ms 330000`. Layout 1 is required; other layout versions are rejected.

Explicit GC records `retired_at_upper_ms` for superseded snapshots and expires them on later calls after the HEAD retention period. The first observation starts the N-day wait; zero days removes all non-current history. Current and shared data remain protected, and physical deletion still waits for reader drain. Time uses timezone-independent Unix milliseconds; deployments must keep system clocks accurate.

## SQLite file restore

`db DATABASE_NAME restore file SQLITE_FILE` is available only in the CLI. It copies a stable, standalone SQLite backup in DELETE journal mode to a local temporary file, validates SQLite integrity, foreign keys and supported schema, computes SHA-256, and uploads formal database segments directly to S3. It then builds the index, manifest and first snapshot, and conditionally publishes HEAD. There is no staging upload, file download, restore session, or resumable progress file. The target must be absent or deleted; active databases are never overwritten.

Restore has no total execution deadline and rejects `--deadline-ms`; request timeout defaults do not apply. Individual S3 operations retain their timeouts. No restore file-size or disk-space quota is configured: insufficient temporary disk space fails the operation and removes the temporary file. Ctrl+C cancels local work and further uploads; publication already in flight must still have its result confirmed. Failed unpublished objects can be reclaimed by later GC after a database HEAD exists. A definitive failure requires running the whole command again; `OUTCOME_UNKNOWN` requires inspecting the database before deciding whether to retry.

Stages and progress always go to stderr; stdout contains only the final JSON/table result (`committed`, `outcome`, `name`, `data_version`, `total_bytes`, `sha256`, `page_size`). Interactive terminals refresh in place at most every 200 ms; redirected stderr contains stage changes and progress lines at most every five seconds within a stage. Copying, hashing and confirmed segment uploads report bytes and percentage; validation, index construction and publication report stage and elapsed time. Upload reaching 100% is not completion: HEAD publication must succeed. HTTP and MCP do not expose restore.

The application enables a shared disk cache for verified immutable objects by default. Reads use parsed-node memory, object-body memory, disk, then S3; HEAD and CAS verification always use S3. Separate CLI processes can reuse `objects-v1.sqlite3`. Linux uses `$XDG_CACHE_HOME/ponydb` (absolute paths only), falling back to `$HOME/.cache/ponydb`; macOS uses `$HOME/Library/Caches/ponydb`, Windows `%LOCALAPPDATA%/ponydb`. `cache.directory` overrides this location; `cache.disk_budget_mib = 0` disables disk caching. `config show` reports the resolved directory without creating files.

`cache.disk_budget_mib` limits cached body bytes (default 2048 MiB), shared across all namespaces in a directory, with at most 65,536 entries. SQLite metadata, journals and free pages are additional; incremental reclamation may lag. Processes sharing a directory should use the same budget. `runtime.work_disk_budget_mib` limits local SQLite execution workspace bytes (default 2048 MiB). Restore temporary files remain outside these quotas.

Disk entries are isolated by backend endpoint/bucket/prefix, full object key, length and SHA-256. Every disk read verifies the body, and invalid entries are refetched. Approximate LRU touches are coalesced and throttled to 60 seconds. Disk lookup and fill share a 50 ms wait budget excluding S3 time; busy/failed caches fall back with bounded stderr diagnostics. Successful commands may therefore have nonempty stderr; use the exit code and result to determine business success. One blocking task per reader may finish after timeout or cancellation, retaining its execution permit; maintenance is throttled and best effort. Cache contents are disposable and should be stored in a directory trusted by the same users as the database credentials. Library readers remain memory-only unless explicitly configured with `DiskCacheOptions`.

Confirmed immutable metadata PUTs also fill the body cache and, when enabled, the disk cache. Admission covers `manifests`, `segment-index`, `delta-index` and `snapshots`, up to 1 MiB per object. It uses spare capacity without evicting existing entries; data segments, delta packs and custom categories remain demand-loaded. An unknown PUT outcome must first be confirmed by a direct backend read with identical bytes. Cache failure or cancellation skips the optional fill without changing the confirmed upload result. Index nodes are parsed and validated on first use, then shared through the existing node cache.

## Runtime logs

`logging.level` defaults to `warn`; `logging.format` defaults to `json` and also accepts `text`. CLI, HTTP and MCP diagnostics go to stderr. Business results go to stdout in the selected output format; MCP stdout contains protocol messages only.

```toml
[logging]
level = "debug"
format = "json"
```

```sh
PONYDB_LOGGING__LEVEL=debug ponydb db notes execute "SELECT 1"
PONYDB_LOGGING__LEVEL=trace PONYDB_LOGGING__FORMAT=text ponydb db notes execute "SELECT 1"
```

Debug records request IDs, anonymous database identifiers, operation/phase transitions, queue and execution times, publication outcomes, data versions and request-level I/O/cache totals. Trace additionally records each pager/remote-page operation, S3 call and cache lookup/fill/eviction/bypass. S3 events include the actual object key (or LIST prefix), including the configured storage prefix; cache events use backend-relative keys. Keys escape control characters. Trace is intentionally detailed; debug omits successful per-operation events. Health checks, ping and progress refreshes do not produce request logs.

No level records SQL, parameters, result contents, metadata, descriptions, credentials, tokens, request headers, complete request URLs, local paths or content checksums. Object keys may contain database names and are explicitly permitted. Standalone database identifiers are anonymized consistently within one process, using a private random salt. Dependency logs are disabled even at trace. API errors and restore progress are written separately to stderr; JSON logs are one object per line.

Summary counts describe operations rather than unique pages/objects. Pager access and remote-page reads are separate totals; S3 byte counts include successfully verified reads and confirmed writes. Cache coalescing counts as an initial miss; disk bypasses include unavailable read/write attempts. A dropped request records `aborted`, without claiming whether a write committed. Unknown publication outcomes remain `unknown` until confirmed.

S3 SDK failures additionally emit `s3_request_failed` at debug or trace. This diagnostic includes the endpoint, actual hostname/port, environment proxy selection, SDK variant/dispatch category, elapsed time, SDK attempt count, effective timeouts, HTTP status/server request ID when available, and a sanitized complete error chain. `error_chain` is a JSON-encoded array; I/O entries include `io_kind` and available `raw_os_error`. When `timeout_config_observed=true`, missing timeout fields mean disabled. The current SDK defaults to a 3100ms connect timeout; CLI operation and operation-attempt timeouts are 10000ms. These logs preserve the existing timeout/retry behavior. See [S3 diagnostics](../../crates/object-store-s3/README.md#runtime-diagnostics) for redaction and field details.

Capture a Windows failure in PowerShell:

```powershell
$env:PONYDB_LOGGING__LEVEL = "debug"
$env:PONYDB_LOGGING__FORMAT = "json"
.\ponydb.exe db agent_context schema --output json 2> ponydb-s3-diagnostic.log
```

The manual **Windows S3 diagnostic CLI** Actions workflow builds and tests the selected ref with Rust 1.95 and packages a Windows x64 MSVC CLI with a static CRT. Download its diagnostic artifact after the run succeeds. It does not publish a release. The same package can be built on a Windows development machine using `python scripts/release.py build --target x86_64-pc-windows-msvc` followed by `python scripts/release.py pack --target x86_64-pc-windows-msvc`.
