**English** | [中文](http.zh-CN.md)

# HTTP guide

Deploy the optional service when applications or remote AI Agents need HTTP access. HTTP and CLI use the same object storage and database operations.

## Install

Install `ponydb` on the service host using [CLI installation](cli.md#install), or deploy with [Docker](#docker) below. Remote HTTP clients do not need ponydb installed.

## Before you start

Prepare the bucket, configuration file, or environment variables using [object storage and configuration](configuration.md), then configure authentication and start the server.

### Configure authentication

The server requires at least one authentication principal. Add this to `ponydb.toml`, replacing the example token with a long random secret of at least 16 bytes:

```toml
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

For example, use OpenSSL to generate a 32-byte random secret in hexadecimal, then replace the example token with the output:

```sh
openssl rand -hex 32
```

Alternatively, set the complete authentication object through the environment:

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
```

Bearer tokens authenticate HTTP/MCP clients; S3 credentials grant the ponydb process access to storage. The native CLI uses S3 credentials directly. Principal IDs and tokens must be unique; authentication changes require restarting the server.

To restrict a principal to queries on `notes`, use `databases = ["notes"]` and `scopes = ["db:read", "data:read"]`; add `db:list` if it should list accessible databases. The example admin's `*` grants access to all databases and operations.

| Scope | Purpose |
| --- | --- |
| `*` | Grant all operations within the principal's database grants. |
| `db:list` | List databases within the principal's database grants for which it has `db:read`. |
| `db:read` | Read database details. |
| `db:create` | Create databases. |
| `db:manage` | Update database settings and metadata, or logically delete databases; changing snapshot retention also requires `db:backup`. |
| `data:read` | Query data through SQL. |
| `data:write` | Insert, update, or delete data through SQL. |
| `schema:read` | Read table, index, view, and trigger definitions through the schema endpoint. |
| `schema:write` | Create, alter, or drop tables, indexes, views, and triggers through SQL. |
| `db:backup` | List, inspect, and export retained snapshots, or query snapshots; queries also require the corresponding data read permission. |
| `db:maintenance` | Run online compaction and GC. |

### Validate configuration and start

Input example:

```sh
export PONYDB_CONFIG="/absolute/path/ponydb.toml"
ponydb config validate --target server
ponydb server --listen 127.0.0.1:8080
```

`config validate` checks authentication configuration and performs live read, write, conditional write, list, and delete probes under the configured storage prefix. The server runs in the foreground; Ctrl-C or `SIGTERM` stops accepting connections and waits for requests already running. Use a TLS reverse proxy when exposing the API to remote clients.

### Docker

After preparing storage and authentication configuration, run a Release image from the directory containing `ponydb.toml`:

```sh
docker run --rm -p 127.0.0.1:8080:8080 \
  --mount "type=bind,source=$PWD/ponydb.toml,target=/config/ponydb.toml,readonly" \
  -e PONYDB_SERVER__LISTEN=0.0.0.0:8080 \
  ghcr.io/hackerneocom/ponydb:0.1.1 server --config /config/ponydb.toml
```

The example uses `0.1.1`. To use another version, choose its full tag from [GitHub Releases](https://github.com/hackerneocom/ponydb/releases), or use its digest to pin an exact image. Release images target Linux amd64/arm64; Docker Hub mirror addresses are listed in release notes.

The configuration must be readable by the container's unprivileged user, and its S3 endpoint must be reachable inside the container. `localhost` in that file refers to the container itself. To forward host credentials or authentication, add `-e PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`, `-e PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`, optional `-e PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`, and `-e PONYDB_SERVER__AUTH` as needed.

Binding to `0.0.0.0` inside the container allows port forwarding.

## API reference

All examples below use `http://127.0.0.1:8080` as the server address.

In the client terminal, set the token to the same value configured for the principal. Subsequent examples use this variable:

```sh
export API_TOKEN='replace-with-a-long-random-token'
```

| Topic | Purpose | Method and path |
| --- | --- | --- |
| Configuration and diagnostics | Check process liveness | [`GET /healthz`](#get-healthz) |
| Database management | Create a database | [`POST /v1/databases`](#post-v1databases) |
| | List databases | [`GET /v1/databases`](#get-v1databases) |
| | Read database details | [`GET /v1/databases/{database}`](#get-v1databasesdatabase) |
| | Update database settings and metadata | [`PATCH /v1/databases/{database}`](#patch-v1databasesdatabase) |
| | Logically delete a database | [`DELETE /v1/databases/{database}`](#delete-v1databasesdatabase) |
| SQL and schema | Execute SQL | [`POST /v1/databases/{database}/execute`](#post-v1databasesdatabaseexecute) |
| | Read database schema | [`GET /v1/databases/{database}/schema`](#get-v1databasesdatabaseschema) |
| Snapshots | List retained snapshots | [`GET /v1/databases/{database}/snapshots`](#get-v1databasesdatabasesnapshots) |
| | Read snapshot details | [`GET /v1/databases/{database}/snapshots/{snapshot_id}`](#get-v1databasesdatabasesnapshotssnapshot_id) |
| | Query a specific snapshot | [Snapshot options for `execute`](#query-a-specific-snapshot) |
| Backup and restore | Download a specific snapshot | [`GET /v1/databases/{database}/snapshots/{snapshot_id}/export`](#get-v1databasesdatabasesnapshotssnapshot_idexport) |
| Online maintenance | Merge fragmented storage segments to improve reads | [`POST /v1/databases/{database}/compact`](#post-v1databasesdatabasecompact) |
| | Remove expired history and unreachable objects to reduce storage usage | [`POST /v1/databases/{database}/gc`](#post-v1databasesdatabasegc) |
| Service and AI Agents | Connect to MCP over HTTP | [`POST /mcp`](#post-mcp) |

Replace `{database}` with the database name and `{snapshot_id}` with a retained snapshot ID.

### Common conventions

#### Headers and request bodies

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `Authorization: Bearer TOKEN` | For `/v1` routes and `/mcp` | | Bearer token authentication; `/healthz` needs no authentication. |
| `Content-Type: application/json` | | | Use when sending a JSON body; the body limit is 8 MiB. |
| `Accept: application/x-ndjson` | | JSON | Supported only for read-only SQL queries. See [output and scripting](#output-and-scripting) for the format. |
| `If-Match: ETAG` | | | Condition for PATCH and DELETE; use the latest database details' `ETag` header, preserving quotes. |

Database names are case-sensitive and immutable. Path, query, and header values must agree with duplicate fields in the body; `database` and `snapshot_id` supplied in the path can be omitted from the body. Unknown fields, duplicate JSON keys, invalid types, and conflicting values are rejected.

URL-encode query cursors, for example with `curl --get --data-urlencode "cursor=$CURSOR"`. The server's `requests.default_timeout_ms` sets the request deadline, with a built-in default of 90000 milliseconds.

### Configuration and diagnostics

#### GET /healthz

Check process liveness. No parameters or authentication are required.

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/healthz
```

Output example (HTTP `200`):

```json
{"status":"ok"}
```

This endpoint does not verify object storage availability. To check authenticated storage access, request `GET /v1/databases`.

### Database management

#### POST /v1/databases

Create a database. A new database starts at data version `1`; its default snapshot retention comes from `snapshots.retention_days` (7 days).

JSON body parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `name` | Yes | | Unique, immutable database name; 1–64 Unicode BMP characters, excluding NUL, `/`, `\`, `.`, and trailing ASCII spaces. |
| `description` | | Empty string | Description; at most 16 KiB of UTF-8 bytes. |
| `metadata` | | `null` | Custom JSON metadata; at most 64 KiB of UTF-8 bytes when serialized as compact JSON. |

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"api_notes","description":"Notes over HTTP","metadata":{"project":"notes"}}'
```

Output example (HTTP `201`):

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "api_notes",
  "data_version": "1"
}
```

#### GET /v1/databases

List databases accessible to the current principal, with pagination.

Query parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `limit` | | 50 | Maximum databases per page, range 1–1000. |
| `cursor` | | Start at the first page | The previous response's `next_cursor`. |
| `include_deleted` | | `false` | Include logically deleted databases. |

Input example:

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases?limit=20&include_deleted=true" \
  -H "Authorization: Bearer $API_TOKEN"
```

Output example (excerpt):

```json
{
  "outcome": "read_only",
  "databases": [
    {"name":"api_notes","description":"Notes over HTTP","status":"active","data_version":"1"}
  ],
  "next_cursor": null
}
```

A `null` `next_cursor` means there is no next page; otherwise pass its value unchanged as `cursor`. Each database entry has the same fields as the details response.

#### GET /v1/databases/{database}

Read database details, including data version, management ETag, and commit token. No parameters beyond the database path parameter are required.

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" -i
```

Output example (JSON excerpt; `-i` also shows response headers):

```json
{
  "name": "api_notes",
  "description": "Notes over HTTP",
  "status": "active",
  "metadata_version": "1",
  "data_version": "1",
  "etag": "\"ETAG_FROM_DETAILS\"",
  "metadata": {"project":"notes"},
  "snapshot_retention_days": 7,
  "commit_token": "COMMIT_TOKEN"
}
```

The `ETag` response header matches `etag` in the JSON body. Fetch a fresh value before updating or deleting.

#### PATCH /v1/databases/{database}

Update the description, custom metadata, or snapshot retention without changing the database name or SQL data version.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `description` | | Keep current value | JSON field; new description, at most 16 KiB of UTF-8 bytes. |
| `metadata` | | | JSON field; replace custom metadata, at most 64 KiB of UTF-8 bytes as compact JSON. |
| `snapshot_retention_days` | | Keep current value | JSON field; retention in days, range 0–36500. Changing it also requires `db:backup`. |
| `If-Match` | | | Header; update only if the management ETag still matches. |

Input example:

```sh
# Replace the placeholder with the latest details' ETag, preserving quotes
curl --fail-with-body -X PATCH http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'If-Match: "ETAG_FROM_DETAILS"' \
  -d '{"description":"Personal notes","snapshot_retention_days":14}'
```

Output example:

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "api_notes",
  "metadata_version": "2"
}
```

Retention changes do not immediately delete objects; explicit GC removes history. An ETag mismatch returns HTTP `412` with `METADATA_PRECONDITION_FAILED`.

#### DELETE /v1/databases/{database}

Logically delete a database, retaining its tombstone and history within retention. The optional `If-Match` header requires a matching management ETag.

Input example:

```sh
# After completing other examples, fetch the latest ETag, then delete
curl --fail-with-body -X DELETE http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'If-Match: "ETAG_FROM_DETAILS"'
```

Output example:

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "api_notes",
  "status": "deleted"
}
```

### SQL and schema

#### POST /v1/databases/{database}/execute

Execute one SQL statement or an atomic batch. See [SQLite compatibility](sqlite.md) for supported features, unsupported syntax, and limits.

JSON body parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `sql` | Either this or `statements` | | One SQL statement. |
| `params` | | No bound parameters | Typed parameter array for a single statement, bound in order. |
| `statements` | Either this or `sql` | | Nonempty array of statements, each with `sql` and optional `params`; cannot be combined with top-level `sql` or `params`. |
| `consistency` | | `strong` | `strong`, `snapshot`, or `at_least`; the latter two are query-only. |
| `snapshot_id` | When `consistency=snapshot` | | Snapshot ID; obtain from the snapshot list. |
| `commit_token` | When `consistency=at_least` | | Commit token; the read version must be at least the token's version. |
| `max_rows` | | `query.max_rows` (1000) | Query-only; positive and no greater than `query.max_rows`. Exceeding the limit returns an error rather than silently truncating. |

**Create a table and write**: atomically create a table and insert a note in the new `api_notes` database.

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"statements":[{"sql":"CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"},{"sql":"INSERT INTO notes(title) VALUES (?)","params":[{"type":"text","value":"First note"}]}]}'
```

Output example:

```json
{
  "outcome": "committed",
  "database": "api_notes",
  "committed": true,
  "data_version": "2",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns":[],"rows":[],"rows_affected":"0"},
    {"columns":[],"rows":[],"rows_affected":"1"}
  ]
}
```

Statements execute in order and roll back together on failure; no `BEGIN` or `COMMIT` is needed. Statement arrays use write transaction semantics and require write permissions even when they contain only queries. They cannot use read options such as `max_rows`, snapshots, or commit tokens.

**Query data**:

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT id, title FROM notes ORDER BY id LIMIT 10","max_rows":10}'
```

Output example:

```json
{
  "outcome": "read_only",
  "database": "api_notes",
  "data_version": "2",
  "results": [
    {
      "columns": ["id", "title"],
      "rows": [[{"type":"integer","value":"1"},{"type":"text","value":"First note"}]],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

`results` follows statement order and includes `columns`, typed `rows`, and string-valued `rows_affected`. Parameters and result cells share the `null`, `integer`, `real`, `text`, and `blob` types; integers use decimal strings and BLOBs use Base64 strings.

To read at least a previous write's version, add `"consistency":"at_least","commit_token":"COMMIT_TOKEN"` to the query request, replacing the token with the value returned by that write.

#### GET /v1/databases/{database}/schema

Read definitions of user tables, indexes, views, and triggers. Omitting filters returns all non-internal objects.

Query parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `table` | | | Exact, case-sensitive match for an existing non-internal table; includes its indexes and triggers without resolving view dependencies. |
| `type` | | | `table`, `index`, `view`, or `trigger`; intersects with the `table` filter. |

Input example:

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases/api_notes/schema?table=notes&type=table" \
  -H "Authorization: Bearer $API_TOKEN"
```

Output example (excerpt):

```json
{
  "outcome": "read_only",
  "database": "api_notes",
  "data_version": "2",
  "results": [
    {
      "columns": ["type", "name", "tbl_name", "sql"],
      "rows": [[
        {"type":"text","value":"table"},
        {"type":"text","value":"notes"},
        {"type":"text","value":"notes"},
        {"type":"text","value":"CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"}
      ]]
    }
  ]
}
```

`columns` and row cells correspond, in order, to the object type, object name, owning table or view, and creation SQL. A missing table returns `TABLE_NOT_FOUND`; an existing table with no matching type returns an empty result.

### Snapshots

#### GET /v1/databases/{database}/snapshots

List retained snapshots for historical queries and exports, with pagination. Requires `db:backup`.

Query parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `limit` | | 50 | Snapshots per page, range 1–1000. |
| `cursor` | | Start at the first page | The previous response's `next_cursor`. |

Input example:

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases/api_notes/snapshots?limit=20" \
  -H "Authorization: Bearer $API_TOKEN"
```

Output example (excerpt):

```json
{
  "outcome": "read_only",
  "snapshots": [{"snapshot_id":"SNAPSHOT_ID","data_version":"2"}],
  "next_cursor": "CURSOR",
  "retention_days": 14,
  "time_authority": "system_clock",
  "history_gaps": []
}
```

To continue listing, pass `next_cursor` unchanged as `cursor`. `retention_days` is the database's current snapshot retention.

Subsequent examples use an actual ID from the snapshot list. Set it first:

```sh
export SNAPSHOT_ID='SNAPSHOT_ID_FROM_LIST'
```

#### GET /v1/databases/{database}/snapshots/{snapshot_id}

Read retained snapshot details. Requires `db:backup`; no parameters beyond the path parameters are required.

Input example:

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID" \
  -H "Authorization: Bearer $API_TOKEN"
```

Output example (excerpt):

```json
{
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "2",
  "retired_at_upper_ms": null,
  "observed_at_ms": 1794182400000
}
```

#### Query a specific snapshot

Use `execute` with `consistency` and `snapshot_id` to query historical data. This also requires `db:backup`.

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"sql\":\"SELECT id, title FROM notes ORDER BY id\",\"consistency\":\"snapshot\",\"snapshot_id\":\"$SNAPSHOT_ID\"}"
```

The output has the same structure as a read-only SQL response, with `data_version` matching the selected snapshot.

### Backup and restore

#### GET /v1/databases/{database}/snapshots/{snapshot_id}/export

Download a retained snapshot as a standalone SQLite file. Requires `db:backup`; no parameters beyond the path parameters are required.

Input example:

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID/export" \
  -H "Authorization: Bearer $API_TOKEN" \
  -o ./api-notes-backup.sqlite
```

Success returns HTTP `200` with `Content-Type: application/vnd.sqlite3`, saving the response body to `./api-notes-backup.sqlite`. Wait for the download to complete before using the file.

Current-database export and file restore are available only through the CLI; see [CLI backup and restore](cli.md#backup-and-restore).

### Online maintenance

Maintenance work budgets and request deadlines are separate.

#### POST /v1/databases/{database}/compact

Merge storage segments online without changing the logical data version. Requires `db:maintenance`.

JSON body parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `budget_ms` | | 10000 | Work budget, range 1–60000 milliseconds. |
| `max_segments` | | 32 | Maximum number of segments to process, range 1–32. |

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/compact \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{}'
```

Output example:

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "segments_processed": 1,
  "data_version": "2"
}
```

The actual number processed depends on database state.

#### POST /v1/databases/{database}/gc

Remove expired history and unreachable objects online. Requires `db:maintenance`.

JSON body parameters:

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `budget_ms` | | `maintenance.gc_default_budget_ms` (60000) | Work budget in milliseconds; positive and no greater than `requests.max_timeout_ms`. |
| `max_objects` | | `maintenance.gc_max_objects_per_call` (1000) | Maximum attempted object deletions; the same setting also determines the deployment cap. Must be positive. |

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/gc \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"budget_ms":60000,"max_objects":1000}'
```

Output example:

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "phase": "complete",
  "deleted_objects": 0
}
```

Possible output when the budget or object limit is exhausted:

```json
{
  "outcome": "partial",
  "complete": false,
  "database_available": true,
  "phase": "sweeping",
  "deleted_objects": 1000
}
```

When compaction or GC returns `outcome: "partial"`, call the same operation again. The HTTP status is still `200`; clients must also inspect `outcome` and `complete`. GC has no pause, recovery, or resume endpoints.

Explicit GC removes historical snapshots after retention ends. Keep the system clock accurate and export history that needs long-term preservation. See [configuration](configuration.md) for query and export limits.

### Service and AI Agents

#### POST /mcp

Serve HTTP MCP on the same port, sharing identities and storage with the REST API. See [AI Agent setup](agent-clients.md#http-mcp) for client configuration, request headers, a protocol probe, and tool calls.

## Output and scripting

Successful JSON operations return business results directly. Database creation returns HTTP `201`; other successful JSON operations return `200`. SQLite downloads return a binary body.

Read-only SQL accepts `Accept: application/x-ndjson` for newline-delimited JSON records:

Input example:

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/x-ndjson' \
  -d '{"sql":"SELECT 1"}'
```

Output example (one record per line):

```jsonl
{"type":"result_start","data_version":"2"}
{"type":"rows","columns":["1"],"rows":[[{"type":"integer","value":"1"}]]}
{"type":"done","outcome":"read_only"}
```

Scripts should check both HTTP status and business results. The examples use `curl --fail-with-body` to return a nonzero exit code on HTTP errors while retaining the error body. Example output for an ETag mismatch (HTTP `412`):

```json
{
  "ok": false,
  "error": {
    "request_id": "REQUEST_ID",
    "code": "METADATA_PRECONDITION_FAILED",
    "message": "Management ETag does not match",
    "retryable": false,
    "outcome": "not_committed"
  }
}
```

| HTTP status | Common codes and handling |
| --- | --- |
| `400` | `INVALID_REQUEST` and others; check parameters, types, and SQL. |
| `401` / `403` | `UNAUTHENTICATED` / `FORBIDDEN`; check the token, database access, and operation scopes. |
| `404` | `DATABASE_NOT_FOUND` / `TABLE_NOT_FOUND`; check names. |
| `409` | `TX_CONFLICT` / `DATABASE_EXISTS`; check concurrent writes or whether the database already exists. |
| `410` | `DATABASE_DELETED` / `SNAPSHOT_EXPIRED`; check database state or snapshot retention. |
| `412` | `METADATA_PRECONDITION_FAILED`; read fresh details before deciding to update or delete. |
| `413` | `RESOURCE_LIMIT`; check body size or other resource limits. |
| `429` | `RATE_LIMITED`; reduce request frequency. |
| `503` | `STORAGE_UNAVAILABLE`, `MAINTENANCE`, `MAINTENANCE_REQUIRED`, or `OUTCOME_UNKNOWN`; use the code and `outcome` to decide the next action. |

Repeating a write executes it again. If the response is lost or returns `OUTCOME_UNKNOWN`, inspect the database before deciding to retry. Business keys or unique constraints can help detect duplicate operations.
