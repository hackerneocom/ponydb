**English** | [中文](README.zh-CN.md)

# ponydb server

HTTP `/v1` and MCP on one port. The process loads `runtime-config`, opens the S3 store, and serves `database-service`. It does not implement a second set of database rules.

## Purpose

All entries call the same business layer with 11 operations. Each write executes independently; responses are not stored or replayed. Inspect business data before deciding to retry an uncertain write.

## How it works

`ponydb server` with no subcommand starts the service. `config validate --target server` validates authentication before running the [live S3 contract probe](README.md), which creates and deletes temporary objects under the configured prefix. `config show` remains offline. Startup checks that the bundled SQLite has an authorizer, loads the authentication configuration, and builds `Service` from `config.s3()` and `config.service_options()`. `--listen` overrides `server.listen`. Ctrl-C or `SIGTERM` stops accepting connections and waits for requests already running. `/healthz` reports process liveness only. A database in maintenance does not fail that route.

The authentication configuration is JSON `{ "principals": [ { "id", "token", "databases", "scopes" } ] }`. Tokens are at least 16 bytes, ids and tokens are unique, and at least one principal is required. Comparison of the bearer token walks both byte strings and folds XOR differences, so the check does not return on the first mismatch.

`router` mounts:

| Route | Transport |
| --- | --- |
| `GET /healthz` | liveness JSON |
| `/v1/databases` and nested paths | HTTP API |
| `POST /mcp` | MCP, when `mcp.enabled` |

The body limit is 8 MiB. HTTP JSON is parsed with `protocol::strict_json`. Path parameters, query parameters, and the `If-Match` header (as `if_match`) are written into the same object. A value that disagrees with the body is `INVALID_REQUEST` and is rejected before S3. The resulting object is deserialized as an `Operation` and passed to `Service::call` with a `RequestContext` of `requests.default_timeout_ms`. `ApiError.code` selects the HTTP status (`TX_CONFLICT` is 409, `OUTCOME_UNKNOWN` is 503, and so on). `database_create` returns 201. When the JSON result contains `etag`, that value is copied to the `ETag` header. A read-only `sql_execute` whose `Accept` header includes `application/x-ndjson` is streamed as `result_start`, `rows`, and `done` lines.

`POST /mcp` requires `MCP-Protocol-Version: 2026-07-28` and an `MCP-Method` header equal to the JSON-RPC method. `tools/call` also requires `MCP-Name` equal to the tool name. A request that carries an `Origin` header is rejected with 403, including localhost, because this build has no browser origin allow-list. The body is then handed to `mcp-server::dispatch`.

### Authentication through environment variables

Set `PONYDB_SERVER__AUTH` to the complete authentication JSON directly:

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
ponydb config validate --target server
```

The corresponding key, `server.auth`, can be written directly in the main TOML configuration; the environment variable takes precedence. Authentication is loaded at startup; restart after changes. `config show` and Debug output redact authentication secrets. For deployment entirely through environment variables, also set `PONYDB_STORAGE__ENDPOINT`, `PONYDB_STORAGE__BUCKET`, and `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID` / `PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY` (plus `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN` for temporary credentials).

```toml
# Or set PONYDB_SERVER__AUTH to the complete JSON object.
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

Restore is available only through CLI `db DATABASE_NAME restore file SQLITE_FILE`; HTTP and MCP do not expose it.
