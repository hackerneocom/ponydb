# ponydb

Use SQLite databases stored in S3 through a native CLI or an HTTP API.

Create databases, run parameterized SQL and atomic transactions, inspect snapshots, and export standard SQLite backups. The CLI works on Linux, macOS, and Windows and connects directly to S3; start the optional HTTP server when applications need API access. MCP clients can connect over HTTP or stdio.

[中文](README.zh-CN.md) · [Quick start](#quick-start) · [Configuration](#configuration) · [CLI usage](#cli-usage) · [HTTP server](#http-server)

## Install

From the repository root, with Rust 1.95 installed:

```sh
cargo install --locked --path apps/cli
ponydb version
```

This installs `ponydb` in Cargo's binary directory; make sure that directory is on your `PATH`. SQLite is bundled. To run the HTTP server in a container, see [Docker](#docker).

## Quick start

Prepare an existing S3-compatible bucket and credentials with List/Get/Put/Delete access to the bucket and the prefix you will use. The backend must support conditional writes; `config validate` checks compatibility. For a local S3 service, see the [RustFS development setup](.devcontainer/README.md#rustfs).

The shell examples below use a POSIX shell. On Windows, use the equivalent environment-variable syntax in your shell.

Copy the configuration template and replace its four active S3 values: endpoint, bucket, access key ID, and secret access key.

```sh
cp examples/ponydb.toml ./ponydb.toml
# Edit ponydb.toml before continuing.
export PONYDB_CONFIG="$PWD/ponydb.toml"
ponydb config validate --target cli
```

Keep this environment variable set for the following examples, or pass `--config ./ponydb.toml` to each command. Credentials can also come from [environment variables](#environment-variables).

Create a database, add a note, and query it:

```sh
ponydb databases create notes --description "My notes"
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"
ponydb db notes execute "INSERT INTO notes(title) VALUES (?)" \
  --param '{"type":"text","value":"First note"}'
ponydb db notes execute "SELECT id, title FROM notes ORDER BY id" --output table
```

The query returns the note with ID `1` and title `First note`. No HTTP server is needed for these commands.

## Configuration

The CLI, HTTP server, and stdio MCP use the same TOML configuration and environment variables. The [annotated configuration template](examples/ponydb.toml) lists available user settings and defaults. Uncomment an optional section header together with its settings to change a default.

### Configuration file

A minimal file for CLI access is:

```toml
[storage]
endpoint = "https://s3.example.com"
bucket = "my-bucket"
region = "us-east-1"
prefix = "ponydb/"

[storage.credentials]
access_key_id = "<access-key-id>"
secret_access_key = "<secret-access-key>"
# session_token = "<temporary-credential-token>"
```

Replace the endpoint and credentials with your provider's values. Create the bucket beforehand. Clients using the same endpoint, bucket, and prefix access the same databases. `storage.force_path_style` defaults to `true` for services such as MinIO; set it to `false` for providers requiring virtual-hosted addressing, such as OSS. Keep the endpoint and bucket as separate values.

Configuration files are selected in this order; files are not merged:

| Priority | Source |
| --- | --- |
| 1 | `ponydb --config /path/to/ponydb.toml ...` |
| 2 | `PONYDB_CONFIG=/path/to/ponydb.toml` |
| 3 | Platform default below |

| Platform | Default configuration file |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/ponydb/config.toml`, or `~/.config/ponydb/config.toml` when unset |
| macOS | `~/Library/Application Support/ponydb/config.toml` |
| Windows | `%APPDATA%\ponydb\config.toml` |

If the platform default file is missing, built-in defaults and environment variables are used. A missing file explicitly selected by `--config` or `PONYDB_CONFIG` is an error. Relative file paths in the TOML file are resolved against its directory.

### Environment variables

Environment variables override values from the file. Use `PONYDB_<SECTION>__<KEY>`; nested keys use another `__`. For a deployment without a configuration file, leave `PONYDB_CONFIG` unset and supply the four required S3 values:

```sh
export PONYDB_STORAGE__ENDPOINT=https://s3.example.com
export PONYDB_STORAGE__BUCKET=my-bucket
export PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID='<access-key-id>'
export PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY='<secret-access-key>'
ponydb databases list
```

For temporary credentials, also set `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`. The application does not read `AWS_*` credential variables or AWS credential files. Unknown configuration fields and unknown `PONYDB_` variables are rejected. A command-specific override such as `server --listen` takes precedence over configuration and environment values.

### Check and adjust settings

```sh
ponydb config show --target cli
ponydb config show --target server
ponydb config validate --target cli
ponydb config validate --target server
```

`config show` works offline and always redacts credentials and authentication tokens. `config validate` checks settings and performs live S3 checks using temporary objects under the configured prefix, including a 2 MiB object. It requires List/Get/Put/Delete permissions and cleans up its probe objects; an interrupted run can leave probe objects behind. `--target server` or `all` also checks server authentication.

Common settings:

| Setting | Default | Use |
| --- | --- | --- |
| `server.listen` | `127.0.0.1:8080` | HTTP bind address |
| `cli.output` | `json` | CLI output: `json` or `table` |
| `requests.default_timeout_ms` | `90000` | Default request timeout; excludes file restore |
| `requests.max_timeout_ms` | `1800000` | Maximum allowed request timeout |
| `query.max_rows` | `1000` | Maximum rows returned by a query |
| `query.max_result_bytes` | `1048576` | Maximum serialized query result size (1 MiB) |
| `exports.max_file_bytes` | `10737418240` | Maximum exported SQLite file size (10 GiB) |
| `snapshots.retention_days` | `7` | Default history retention period |
| `cache.directory` | User cache directory | Local disposable cache location |
| `cache.disk_budget_mib` | `2048` | Cached data budget; `0` disables disk caching |
| `logging.level` / `logging.format` | `warn` / `json` | Diagnostics on stderr; format also accepts `text` |

Query limits cause an error rather than silently truncating results. Use SQL `LIMIT` or pagination to keep results within the configured limits. Longer exports may also need a longer request timeout and read duration; see the [export reference](apps/cli/README.md#database-and-snapshot-export).

## CLI usage

Use `databases` to create or list databases, and `db DATABASE_NAME [ACTION]` to work with one database. Omitting the action shows its details. Database names are required, case-sensitive, and immutable; names contain 1–64 Unicode BMP characters, excluding NUL, `/`, `\`, `.`, and trailing ASCII spaces. Quote names containing spaces in your shell.

### Manage databases

```sh
ponydb databases list --limit 20
ponydb databases list --include-deleted
ponydb db notes
ponydb db notes update --description "Personal notes"
ponydb db notes schema
ponydb db notes schema --table notes --type table
```

List responses return `next_cursor` when another page is available; pass it with `--cursor CURSOR`. Schema filters accept `table`, `index`, `view`, or `trigger`; `--table` must name an existing table and is matched exactly.

Update and delete can require a matching `etag` from `ponydb db notes`. Keep the double quotes embedded in that value, for example:

```sh
ponydb db notes update --if-match '"ETAG_FROM_DETAILS"' --description "Updated notes"
```

`ponydb db notes delete` logically deletes the database. Omit `--if-match` to update or delete without requiring a previously fetched ETag. Descriptions are limited to 16 KiB of UTF-8; custom metadata, supplied with `--metadata-file metadata.json`, is limited to 64 KiB of compact UTF-8 JSON.

### SQL, parameters, and transactions

Execute one SQL statement inline, from a UTF-8 file, or from stdin:

```sh
ponydb db notes execute "SELECT * FROM notes LIMIT 10" --output table
ponydb db notes execute --sql-file ./query.sql
printf 'SELECT count(*) FROM notes' | ponydb db notes execute --sql-file -
```

Bind values with repeated `--param` options or a JSON array in `--params-file params.json`. The two forms are mutually exclusive. Parameters and JSON result cells use these types:

| Type | JSON example |
| --- | --- |
| Null | `{"type":"null"}` |
| Integer | `{"type":"integer","value":"42"}` |
| Real | `{"type":"real","value":3.14}` |
| Text | `{"type":"text","value":"A note"}` |
| Blob | `{"type":"blob","value":"SGVsbG8="}` (base64) |

Integers use strings to preserve 64-bit precision. JSON results contain a `results` array with `columns` and typed `rows`; writes also return `committed` and a `commit_token`.

For an atomic transaction, save this as `statements.json`:

```json
{
  "database": "notes",
  "statements": [
    {
      "sql": "INSERT INTO notes(title) VALUES (?)",
      "params": [{"type": "text", "value": "Second note"}]
    },
    {"sql": "SELECT id, title FROM notes ORDER BY id"}
  ]
}
```

```sh
ponydb db notes execute --input ./statements.json
```

Statements run in order; any failure rolls back the entire request. Do not add `BEGIN` or `COMMIT`, or place multiple statements in `--sql-file`. Statement arrays require write permission even if they contain only queries, and cannot use read-only options such as `--max-rows` or snapshot consistency.

JSON-based database commands accept a full request via `--input FILE|-`; the database name must still be explicit and agree with the request. Unknown fields, duplicate keys, and conflicting file/command values are errors. At most one input can read stdin per invocation.

### Snapshots, backup, and restore

```sh
ponydb db notes snapshot list --limit 20
ponydb db notes snapshot get SNAPSHOT_ID
ponydb db notes execute "SELECT * FROM notes" \
  --consistency snapshot --snapshot-id SNAPSHOT_ID
ponydb db notes export ./notes-backup.sqlite
ponydb db notes snapshot export SNAPSHOT_ID ./notes-history.sqlite
ponydb db restored_notes restore file ./notes-backup.sqlite
```

Replace `SNAPSHOT_ID` with a retained ID from `snapshot list`. Ordinary queries use `strong` consistency; `at_least` reads accept `--commit-token TOKEN` from a prior write to require at least that version.

Exports produce standalone SQLite files and never overwrite existing files; the destination directory must exist. File restore is CLI-only and requires a stable, complete SQLite backup in DELETE journal mode. Its target must be absent or deleted, so do not create `restored_notes` first. Restore needs local temporary disk space, reports progress on stderr, and has no total execution deadline or resumable state. Wait for the final successful result before treating it as complete.

### Maintenance and scripting

```sh
ponydb db notes compact
ponydb db notes gc
ponydb db notes gc --budget-ms 300000 --deadline-ms 330000
ponydb db notes execute "SELECT 1" --output json > result.json
ponydb --help
ponydb db notes execute --help
```

Compaction and GC run online. If they return `outcome: "partial"` and exit `6`, run the same operation again to continue. Compaction defaults to a 10-second work budget and at most 32 segments; GC defaults to 60 seconds and at most 1000 deletion attempts. Snapshot expiry is applied during explicit GC after the retention period; keep system clocks accurate and export history you need to keep.

Successful results go to stdout; diagnostics, errors, and restore progress go to stderr. Use the exit code to determine success. `--output json|table` selects the CLI result format. `--deadline-ms` overrides request timeouts for database operations except restore; it is also accepted by `mcp`, but not by `config`, `version`, or `server`. Server and MCP commands do not accept `--output`.

| Exit code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Other error |
| `2` | Invalid configuration or request |
| `3` | Authentication or permission denied |
| `4` | Transaction conflict or ETag mismatch |
| `5` | Write outcome unknown; inspect data before retrying |
| `6` | Maintenance stopped within its work budget; call again |

Writes execute again when repeated. If a response is lost or the result is `OUTCOME_UNKNOWN`, inspect the database before retrying; use business keys or unique constraints to detect duplicate work.

## HTTP server

### Configure authentication and start

The server uses your S3 configuration and requires at least one authentication principal. Add this to `ponydb.toml`, replacing the example token with a long random secret of at least 16 bytes:

```toml
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

Alternatively, set the complete authentication object through the environment:

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
```

Bearer tokens authenticate HTTP/MCP clients; S3 credentials grant the ponydb process access to storage. The native CLI uses S3 credentials directly and does not use server tokens. Principal IDs and tokens must be unique; authentication changes require restarting the server.

To restrict a principal to queries on `notes`, use `databases = ["notes"]` and `scopes = ["db:read", "data:read"]`; add `db:list` if it should list accessible databases. Other scopes include `db:create`, `db:manage`, `data:write`, `schema:write`, `db:backup`, and `db:maintenance`. The example admin's `*` grants access to all databases and operations.

```sh
ponydb config validate --target server
ponydb server --listen 127.0.0.1:8080
```

The server runs in the foreground. Ctrl-C or `SIGTERM` stops accepting connections and waits for requests already running. Check liveness from another terminal:

```sh
curl --fail-with-body http://127.0.0.1:8080/healthz
```

This unauthenticated endpoint returns `{"status":"ok"}`; it does not verify S3 availability. To check authenticated storage access, request `GET /v1/databases`. Use a TLS reverse proxy when exposing the API to remote clients.

### Create a database and run SQL

In another terminal, set the token to the same value configured for the principal. This example creates a separate `api_notes` database:

```sh
export API_TOKEN='replace-with-a-long-random-token'

curl --fail-with-body http://127.0.0.1:8080/v1/databases \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"api_notes","description":"Notes over HTTP"}'

curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"statements":[{"sql":"CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"},{"sql":"INSERT INTO notes(title) VALUES (?)","params":[{"type":"text","value":"First note"}]}]}'

curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT id, title FROM notes ORDER BY id","max_rows":10}'
```

Responses use the same typed SQL values as the CLI. A read-only query returns `outcome: "read_only"`; a successful write returns `committed: true`. Request bodies are limited to 8 MiB. Path/query/header values must agree with duplicate fields in the body. For read-only queries, `Accept: application/x-ndjson` selects `result_start`, `rows`, and `done` records.

### API reference

All `/v1` routes require `Authorization: Bearer TOKEN`. Replace `{database}` with the database name and `{snapshot_id}` with a retained snapshot ID.

| Method | Path | Request / purpose |
| --- | --- | --- |
| `POST` | `/v1/databases` | `{"name":"notes"}`; optional description and metadata |
| `GET` | `/v1/databases` | List; query `limit`, `cursor`, `include_deleted` |
| `GET` | `/v1/databases/{database}` | Database details and ETag |
| `PATCH` | `/v1/databases/{database}` | Update description, metadata, or `snapshot_retention_days` |
| `DELETE` | `/v1/databases/{database}` | Logically delete a database |
| `POST` | `/v1/databases/{database}/execute` | Single `sql`/`params` or atomic `statements` array |
| `GET` | `/v1/databases/{database}/schema` | Optional `table` and `type` query filters |
| `POST` | `/v1/databases/{database}/compact` | `{"budget_ms":10000,"max_segments":32}` |
| `POST` | `/v1/databases/{database}/gc` | Optional `budget_ms`, `max_objects` |
| `GET` | `/v1/databases/{database}/snapshots` | History; query `limit`, `cursor` |
| `GET` | `/v1/databases/{database}/snapshots/{snapshot_id}` | Snapshot details |
| `GET` | `/v1/databases/{database}/snapshots/{snapshot_id}/export` | SQLite download; requires `db:backup` |

Fetch the database details to obtain its `ETag` header, then pass `If-Match` on PATCH or DELETE to require that version. An ETag mismatch returns HTTP `412`; a transaction conflict returns `409`. Successful database creation returns `201`, other successful JSON operations return `200`. Errors use `{"ok":false,"error":{...}}` with `code`, `message`, `retryable`, and `outcome`. A `503` with `OUTCOME_UNKNOWN` requires checking data before repeating a write.

HTTP supports snapshot downloads; current-database export and file restore use the CLI. For example, after setting `SNAPSHOT_ID` to an ID from the snapshots route, download a backup with:

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID/export" \
  -H "Authorization: Bearer $API_TOKEN" \
  -o ./api-notes-backup.sqlite
```

### Docker

Build from the repository root. The image starts `ponydb server` by default:

```sh
docker build -t ponydb .
docker run --rm -p 127.0.0.1:8080:8080 \
  --mount "type=bind,source=$PWD/ponydb.toml,target=/config/ponydb.toml,readonly" \
  -e PONYDB_SERVER__LISTEN=0.0.0.0:8080 \
  ponydb server --config /config/ponydb.toml
```

The mounted configuration must contain S3 credentials and a server principal, be readable by the container's unprivileged user, and use an S3 endpoint reachable from inside the container. `localhost` in that file refers to the container itself. To supply credentials or authentication from the host environment, add `-e PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`, `-e PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`, optional `-e PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`, and `-e PONYDB_SERVER__AUTH` as needed. Binding to `0.0.0.0` inside the container allows port forwarding; the example publishes the port on the host's loopback address.

## MCP clients

For a client that launches a local MCP process, use:

```sh
ponydb --config /absolute/path/ponydb.toml mcp
```

This uses S3 credentials directly. The HTTP server also exposes `/mcp` by default with the same Bearer authentication; `mcp.enabled = false` disables that HTTP endpoint. See the [stdio client setup](apps/cli/README.md#agent-clients-over-stdio-mcp) and [HTTP MCP requirements](apps/cli/SERVER.md) for client configuration and protocol headers.

## More documentation

- [CLI reference](apps/cli/README.md) / [中文](apps/cli/README.zh-CN.md)
- [HTTP and MCP reference](apps/cli/SERVER.md) / [中文](apps/cli/SERVER.zh-CN.md)
- [Configuration template](examples/ponydb.toml)
- [Development environment](.devcontainer/README.md#rustfs) and [test instructions](tests/README.md)

## License

[GNU Affero General Public License, version 3 only (AGPL-3.0-only)](LICENSE).
