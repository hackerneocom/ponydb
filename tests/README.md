# CLI S3 end-to-end tests

Run release tooling regression checks with `python3 -m unittest discover -s tests/release -v`; S3 is not required.
The release workflow also runs the full CLI test suite on two native Linux architectures, along with container, static linking,
Alpine/Debian, and TLS/DNS checks. See the [release guide](../docs/releases.md) for execution and recovery instructions.

`cli_e2e.py` is the common test runner. Tests are organized by command in `cli_e2e/test_*.py` and discovered automatically using Python's standard-library `unittest` module. The tests use real CLI subprocesses and S3-compatible storage to cover all CLI commands, including the HTTP server, stdio MCP, 11 business operations, current-version and snapshot exports, and local file restore. They require only the Python 3.9+ standard library and a compiled CLI.

Run all tests or select suites corresponding to individual test files. Repeat `--suite` to select multiple suites:

```sh
python3 tests/cli_e2e.py --list
python3 tests/cli_e2e.py --offline
ACCESS_KEY=... SECRET_KEY=... python3 tests/cli_e2e.py --suite execute --suite schema
ACCESS_KEY=... SECRET_KEY=... python3 tests/cli_e2e.py --suite server --suite mcp
```

`--list` requires no CLI, credentials, or S3. `--offline` explicitly skips scenarios that require S3 and runs help text, argument validation, version, and configuration display tests; an offline pass does not establish that real S3 tests pass. By default, the runner executes all tests and requires credentials. It continues running other independent scenarios and summarizes failures. Use `--failfast` to stop after the first failure while still cleaning up the current scenario. The runner returns 0 when the selected tests pass, 1 on failure, and 2 for invalid runner arguments.

[GitHub Actions CI](../.github/workflows/ci.yml) runs a separate `e2e` job (displayed as `CLI, server and MCP end-to-end`) on pushes and pull requests. The job uses Ubuntu 24.04, Rust 1.95.0, and Python 3.11. It starts the same RustFS 1.0.4 image as the development container, pinned by digest, waits for `/health` to become ready, and uses the runner's preinstalled AWS CLI to create the `ponydb-test` bucket. The port binds only to the runner's `127.0.0.1:9000`. Test credentials belong to the temporary container, so no repository Secrets or manual bucket creation are required. After building the CLI, the job runs `python3 tests/cli_e2e.py` without `--offline` or `--suite`, covering every command, server, HTTP MCP, and stdio MCP scenario. Test failures fail the job and print RustFS logs. The job always attempts to remove the container and its temporary volume when it finishes. The overall job timeout is 30 minutes; the test step timeout is 10 minutes.

Commands map to test files as follows. The `db` row means `db DATABASE_NAME`; all other `db` rows also require an explicit database name:

| Command | Test file | Main checks |
| --- | --- | --- |
| `version` | `test_version.py` | Operands, JSON / table output |
| `config show` | `test_config_show.py` | Credential redaction, defaults, environment overrides |
| `config validate` | `test_config_validate.py` | Storage contract, probe object cleanup, authentication errors |
| `databases create` | `test_databases_create.py` | Positional arguments, DTOs, metadata file/stdin input, duplicate creation, keyword/Chinese names |
| `databases list` | `test_databases_list.py` | Pagination, DTOs, soft-delete filtering, boolean flag defaults, storage errors |
| `db` | `test_database_get.py` | Explicit name, DTO consistency, missing databases and manifest corruption |
| `db update` | `test_database_update.py` | Optional ETag, conflicts, metadata size/UTF-8, retention |
| `db delete` | `test_database_delete.py` | Optional/explicit/DTO ETags, conflicts, read behavior after deletion |
| `db execute` | `test_execute.py` | Types, parameter/file/stdin input, SQL edge cases, transactions, math, JSON/JSONB, historical consistency |
| `db schema` | `test_schema.py` | Table/type filters, DTOs, missing-table errors |
| `db export` | `test_database_export.py` | Current data export, DTO/file input, digests, overwrite prevention, restore from exports |
| `db snapshot list` | `test_snapshot_list.py` | History, pagination/DTOs, legacy HEAD format diagnostics |
| `db snapshot get` | `test_snapshot_get.py` | Snapshot descriptions, DTO alternatives to positional arguments and conflicts |
| `db snapshot export` | `test_snapshot_export.py` | Historical data export, DTOs, digests, overwrite prevention, restore from exports |
| `db compact` | `test_compact.py` | Defaults, budgets/segment counts, DTO conflicts, data/history integrity |
| `db gc` | `test_gc.py` | Continuation across batches, exit code 6, defaults, conflicts, data/history integrity |
| `db restore file` | `test_restore.py` | File restore, progress, no overall timeout, invalid backups, new generations and storage layout |
| `server` | `test_server.py` | Start/stop/restart, health checks, authentication/permissions, ETags, transactions/NDJSON, snapshot downloads, HTTP MCP |
| `mcp` | `test_mcp.py` | All tools, transactions/history, protocol error recovery, mixed modern/legacy protocols, cancellation/deadlines/EOF, stdout isolation |
| Cross-command | `test_arguments.py`, `test_logging.py` | Actual help text/command coverage, argument constraints/strict DTOs, log levels and privacy |

`support.py` centralizes CLI invocation, timeouts, cache diagnostics, SQLite historical data fixtures, signed S3 requests, and cleanup. `catalog.py` discovers test modules automatically, so no manual test list is needed. When adding a command, add a corresponding `test_*.py`, declare `COMMANDS`, and write `CliTestCase` tests. `test_arguments.py` recursively reads actual `--help` output and compares the command tree with each module's `COMMANDS`; missing commands or references to removed commands cause failures. Each test method must create its own required data and must not depend on the execution order of other methods or modules.

`transports.py` provides real HTTP/JSON-RPC clients and server/MCP process fixtures with bounded lifetimes. The `server` suite covers REST and HTTP MCP: invalid/missing tokens, read-only identities and database scopes, ETag consistency with the CLI and 412 responses for stale ETags, transaction rollback on failure, NDJSON line streaming, historical snapshots and SQLite downloads, compaction/GC, strict input validation and 404/410 errors, and HTTP MCP protocol version/method/tool-name headers, Origin, Accept, and enablement configuration. It also stops and restarts the server to verify committed data and CLI writes made while the server was stopped.

The `mcp` suite invokes every business tool in `tools/list` through stdin/stdout and checks that business content matches between text and structured results. It covers SQL transaction rollback, persistence across processes, historical reads, data integrity after maintenance, business errors, continued service after invalid JSON, per-request metadata, unknown versions/methods/tools, and legacy handshakes mixed with modern requests. Long queries separately verify explicit cancellation, request timeouts, and cancellation on EOF. Ping and subsequent requests remain usable, and the process exits within 10 seconds of EOF without extra stdout output. Disabling HTTP MCP does not affect stdio MCP. All scenarios use real S3 and are explicitly skipped with `--offline`.

The devcontainer runs RustFS by default. After the first startup, sign in to the console at `http://localhost:9001` with `rustfsadmin` / `rustfsadmin` and create the `ponydb-test` bucket without enabling versioning or object locking. The console address is in the development container's network namespace; a browser on the host needs editor port forwarding or an SSH tunnel. Then run inside the development container:

```sh
cargo build --locked -p ponydb
export ACCESS_KEY=rustfsadmin
export SECRET_KEY=rustfsadmin
python3 tests/cli_e2e.py
```

The default endpoint is `http://localhost:9000`, bucket `ponydb-test`, and region `us-east-1`. Override these settings and the CLI path with `E2E_ENDPOINT`, `E2E_BUCKET`, `E2E_REGION`, and `E2E_CLI`. The script does not automatically load `.env`; the caller must supply credential environment variables.

For other S3-compatible storage, replace the credentials, endpoint, and bucket above with your configuration. See the [devcontainer documentation](../.devcontainer/README.md#rustfs) for the RustFS image version, data volume, and rebuild instructions.

The underlying storage contract tests create separate test buckets and cover 16 rounds of concurrent creation/CAS, range reads, checksums, pagination, deletion, and outcome classification after lost responses. Run them against the same RustFS instance:

```sh
P0_S3_ENDPOINT=http://localhost:9000 \
AWS_ACCESS_KEY_ID=rustfsadmin \
AWS_SECRET_ACCESS_KEY=rustfsadmin \
cargo test --locked -p object-store-s3 --test live -- --ignored
```

Each test method uses its own random `cli-e2e/<uuid>/` storage prefix, temporary configuration, cache, and process working directory. Cleanup is registered with `unittest.addCleanup`, so objects under the current prefix are deleted even after assertion failures, followed by another listing to confirm that the prefix is empty. The S3 fixture interface restricts access to the current prefix and does not delete the bucket. Cleanup failures are reported as test errors. Server/MCP subprocesses stop before storage and temporary directories are cleaned up. Forcefully terminating the test process may skip cleanup; each test prints its scenario name and prefix at the start. The test account needs List/Get/Put/Delete permissions for that prefix. Tests use a bucket without versioning enabled, and cleanup does not enumerate historical object versions.

Successful commands may include predefined disk cache degradation diagnostics on stderr. These diagnostics use `WARN` / `ponydb::cache` / `disk_cache_degraded` events in the default JSON format. The script matches the event and allowed reason values exactly and preserves them in test stderr as `WARN <check name>:`. Exit codes, JSON/table results, restore progress, and other stderr output are still validated strictly. A cache timeout alone does not fail data validation after GC; row counts and historical queries must still pass.

Checks include:

- Database creation, retrieval, listing, metadata updates, optional ETags (from CLI arguments or DTOs), stale ETag conflicts on update/delete, and soft deletion; database and schema commands use the positional argument `DATABASE_NAME`.
- Explicit names, DTO merging, and conflict validation for `db DATABASE_NAME`; update and delete may omit the ETag, but validate it strictly when supplied.
- SQL persistence, parameter types (including the maximum i64, Chinese text, Blob, and NULL), files with a BOM and stdin, and table output.
- Results and types for the bundled math functions `sqrt`, `pow`, and `log`, NULL for domain errors, and `SQL_ERROR` for non-finite results.
- Parameterized JSON / JSONB writes, reads across processes, field updates, and array traversal; JSONB returned as a Blob and rebound as a parameter; invalid JSON rolling back the entire transaction without changing persisted data.
- Repeated writes actually executing, constraint conflicts, transaction commits, and rollback on failure.
- Snapshot lists and details, historical reads, commit token consistency, compaction, GC continuation across batches, and data/history integrity after maintenance.
- Current-version and historical snapshot exports, request files and stdin, file contents and digests, overwrite prevention, rejection of current-version exports for deleted databases, and restore from exported files.
- CLI local file restore, stderr progress, ignoring the default overall request timeout, and rejection of invalid backups; old snapshots remaining readable after deletion; after restore, a new generation containing only its first snapshot, a continuing increase in HEAD revision, and invalidation of old ETags/tokens/snapshots.
- Actual objects retaining layout v1, with the deployment prefix appended only once; names remaining immutable logical identifiers, with no separate database ID or catalog.
- The operation, HTTP status, server-side reason, credential redaction, and non-retryable semantics for object storage authentication failures.
- Missing-database messages including the requested name; deleting a test database's manifest returning `STORAGE_CORRUPT`, distinct from a missing database.
- Adding legacy restore fields to an isolated test database's HEAD causing snapshot listing errors that include the database name, HEAD validation stage, and unknown field, with a hint about possible version incompatibility.
- `config validate` checking the storage contract online with probe objects up to 2 MiB and emptying its isolated probe prefix; validation failures with invalid credentials including the specific stage and HTTP status without leaking credentials.
- Configuration credential redaction, duplicate JSON keys, unknown fields, argument conflicts, multiple stdin sources, invalid deadlines, and exit codes 2/4/6.

To shorten the wait for readers to drain during GC, the test configuration sets `maintenance.max_read_duration_ms` to 3000 and uses defaults for other business settings. Lifecycle steps within a test method run sequentially; different methods are independent. The tests do not use fault injection to simulate `OUTCOME_UNKNOWN`, process interruption, or network partitions.

Logging tests cover debug summaries, trace details for pages/S3/cache, actual keys, request correlation across threads, exclusion of sensitive content, publication conflicts and unknown outcomes, HTTP/MCP error correlation, and stdio stdout isolation. `cli_e2e.py` also verifies debug/trace output levels against real storage.
