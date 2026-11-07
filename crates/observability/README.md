# observability

Shared request diagnostics for the CLI, HTTP/MCP adapters, service, SQLite workers and storage libraries. The binary installs the subscriber; this crate never changes global logging state.

`Request` owns one start/completion summary. Nested requests reuse correlation and counters; `ApiError.request_id` uses that correlation. `Request::scope` enters the captured dispatcher, span and counters only during each future poll and destruction. Use `capture().scope(future)` for spawned async work and `capture().run(|| work())` inside worker threads or blocking tasks, capturing before dispatch. This preserves cancellation semantics and prevents concurrent requests from sharing context.

`phase` reports transitions and previous-phase duration. `PageIo`, `cache` and `count` populate debug summaries independently of trace filtering. Counts represent operations; pager accesses and remote-page fetches are separate. Coalesced cache misses remain misses; disk bypass counts include read/write attempts. Confirmed S3 bytes exclude incomplete reads and uncertain writes. Dropped work is `aborted`, without claiming a commit outcome.

Targets are `ponydb` and `ponydb::*`. Only application-controlled labels and numeric measurements belong in events. Database identifiers use a private process salt; object keys are explicitly allowed, escaped by the text/JSON formatter. Never record arguments, DTOs, SQL, parameters, results, headers, paths, credentials, tokens, checksums or raw errors. The binary disables dependency targets at every level.
