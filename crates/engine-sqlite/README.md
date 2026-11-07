**English** | [中文](README.zh-CN.md)

# engine-sqlite

Runs parameterized SQL on one immutable snapshot and returns an unpublished candidate: frozen dirty pages plus statement results. Scheduling, deadlines, and the remote commit belong to the caller.

## Purpose

The workspace pins `rusqlite` to 0.41.1 and bundles SQLite 3.54.1; it does not use the system SQLite library.

The repository's `.cargo/config.toml` sets `LIBSQLITE3_FLAGS=SQLITE_ENABLE_MATH_FUNCTIONS` for bundled builds, including CI and Docker. Math functions such as `sqrt`, `pow`, `log`, `ln`, and `sin` are available in SQL without loading an extension or creating a virtual table. If you override `LIBSQLITE3_FLAGS`, retain this macro. Diagnostics report `ENABLE_MATH_FUNCTIONS`, and server startup requires it. Domain errors such as `sqrt(-1)` return NULL; non-finite results such as `exp(1000)` are rejected by the service's existing result validation.

SQLite must execute with its normal transactional rules, but a ponydb write is not durable until `versioned-store` publishes a new manifest and `head.json`. This crate is the local half of that split. It also exposes build diagnostics for the bundled SQLite that the server and CLI print at startup.

## How it works

### One transaction

`prepare_with_limits` builds a `CowFile` over the caller's `Snapshot`, opens a `sqlite-vfs` workspace, and checks that SQLite accepted this configuration:

- `page_size` is the snapshot's page size
- `journal_mode=DELETE`
- `synchronous=FULL`
- `foreign_keys=ON`
- `mmap_size=0`, `temp_store=FILE`, `threads=0`

It then runs `BEGIN IMMEDIATE` and installs an authorizer. `ATTACH`, `DETACH`, `PRAGMA`, nested `BEGIN` / `SAVEPOINT`, virtual tables, and `load_extension` are denied. Reads, writes, and schema changes are allowed only when `SqlPermissions` says so. `query_only` is turned on when the caller already knows the statement is read-only.

Each statement is prepared with bound parameters. A progress handler calls `RequestContext::check` every 1000 virtual-machine steps and interrupts SQLite when the request is cancelled or past its deadline. Rows and result bytes are counted against `max_result_rows` and `max_result_bytes`. Exceeding either fails the transaction; results are not truncated. After the statements, the engine clears the authorizer, runs `COMMIT`, checks that the connection returned to autocommit, and freezes the workspace. The returned `PreparedTransaction` carries `read_only`, the `FrozenPages`, and one `StatementResult` per statement.

`inspect` compiles each statement far enough to read SQLite's read-only flag, then `ROLLBACK`s without stepping user SQL. `database-service` uses that to decide whether an `sql_execute` call is a query or a write.

Statement count and total SQL bytes are rejected before the workspace is opened when they exceed the limits.

### Worker pool

`worker::WorkerPool` owns a fixed set of threads named `sqlite-N` and a bounded job queue. `prepare` admits at most one active job per database id and a configured number per subject. A second job for the same database, or a full queue, returns `Busy`.

The pool bridges the async snapshot onto the worker with `remote-pager::bridge`. The Tokio task runs `BridgeServer::serve`. The worker runs the synchronous prepare. `tokio::select` waits for both: the bridge finishing means the worker dropped its snapshot sender, and the task still joins the worker result. If the prepare future is dropped, the request context is cancelled so the worker and the bridge stop. A worker panic becomes `Error::Worker`.

`Error::into_cause` groups those failures for the service. SQL errors keep the rusqlite value, including statements that failed inside the VFS. Snapshot I/O and workspace failures are reported without their inner text.

`diagnostics` opens an in-memory connection and reports `sqlite3_libversion`, `sqlite_source_id`, and `PRAGMA compile_options`.

JSON and JSONB tests exercise bound text input, nested extraction, `->` / `->>`, updates, Unicode, NULLs, and 64-bit integers through the VFS and authorizer. They also cover `json_each` / `json_tree` and their `jsonb_` variants, which can be queried without creating a virtual table. Service and CLI tests verify persisted updates, JSONB Blob parameter roundtrips, read-only access, and rollback on malformed JSON.

## Runtime diagnostics

Worker admission, queue wait and execution are debug events. Jobs carry the caller's diagnostic context into the SQLite thread; statement text, parameters and results are excluded. Libraries do not install a global subscriber.
