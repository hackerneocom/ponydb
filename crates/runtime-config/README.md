**English** | [中文](README.zh-CN.md)

# runtime-config

One strict configuration parser for the `ponydb` CLI and `ponydb server`. Both entry points load the same TOML, apply the same environment overrides, and reject unknown keys.

## Purpose

The server and the CLI must construct the same `database-service` limits and the same S3 client from one file. Unsupported combinations (another backend, WAL, a missing atomic-write requirement) fail at load time. `AppConfig::validate` checks local settings. The CLI `config validate` additionally constructs `config.s3()` and runs the live storage probe described in [the CLI README](../../apps/cli/README.md); only a successful probe and cleanup report `storage_capabilities_verified: true`. `config show` remains offline.

## How it works

`AppConfig` is a tree of sections with defaults and `deny_unknown_fields`: `storage`, `engine`, `cache`, `runtime`, `transactions`, `query`, `requests`, `maintenance`, `snapshots`, `exports`, `logging`, `server`, `mcp`, and `cli`. There is no database-context section. `config_version` must be 1. Storage layout must be 1. The engine must be bundled SQLite on the `s3_versioned` VFS with `DELETE` journals, `synchronous=FULL`, foreign keys, and `mmap_size=0`. Worker threads, queue depth, dirty-byte limits, query limits, timeouts, maintenance budgets each have an allowed range checked in `validate`.

`load` resolves the file in this order:

1. The path argument (`--config`).
2. `PONYDB_CONFIG`.
3. The platform default: `$XDG_CONFIG_HOME/ponydb/config.toml` on Linux, `~/Library/Application Support/ponydb/config.toml` on macOS, `%APPDATA%\ponydb\config.toml` on Windows.

A missing default file means the built-in defaults. A missing explicit file is an error. The file is parsed as TOML. Relative paths for the cache directory are resolved against the config file's directory.

[`examples/ponydb.toml`](../../examples/ponydb.toml) lists the settings users can adjust that affect runtime behavior. Only `storage.endpoint`, `storage.bucket`, and the two credential keys are active; optional settings and section headers are commented with their built-in defaults. Uncomment the section header and assignment together to override a default. The optional session token and cache directory default to unset; their sample assignments are examples. `ponydb server` needs at least one authentication principal; the example is commented and the default array is empty. `config show` and `version` do not require S3 access.

Fixed protocol/engine values and fields without runtime effect are omitted from the example.

### Environment overrides

All configuration fields support environment overrides; there is no per-field allowlist. Both `PONYDB_<SECTION>__<KEY>` and `PONYDB_<SECTION>.<KEY>` address the same path (names are lowercased). Prefer `__` in shell scripts because shell variable assignments do not accept dots. Strings and optional string/path fields take literal text; booleans and unsigned integers use their corresponding textual values. Objects accept JSON and merge recursively with existing fields; arrays accept JSON and replace the entire array. Numeric path components address existing array elements, for example `PONYDB_SERVER__AUTH__PRINCIPALS__0__TOKEN`. Set the complete array first if it does not yet contain the element.

Environment overrides are applied in path order, with parents before children, so specific fields take precedence over enclosing objects. Multiple environment names resolving to the same path are rejected. `PONYDB_CONFIG` only selects the file. Unknown keys and invalid types are errors. A final `BTreeMap` of overrides, used by the server for `--listen`, is applied with the same value rules. The merged document is deserialized and `validate`d again.

For example, `PONYDB_STORAGE='{"bucket":"my-bucket"}'` preserves other storage settings; `PONYDB_SERVER__AUTH__PRINCIPALS='[...]'` replaces the principals array. Authentication uses the same JSON override mechanism as every other object.

`storage.credentials` contains ordinary settings: `access_key_id`, `secret_access_key` (empty strings by default), and optional `session_token` (unset by default). Their environment overrides are `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`, `PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`, and `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`, applied per field over TOML. Creating the S3 client requires nonempty access and secret keys; it does not read `AWS_*` variables directly or discover another identity. `s3` builds an `object-store-s3::S3Store` with an 8 MiB object cap and a 10 second operation timeout. `config show`, Debug output, and parse errors do not expose credential values.

`service_options` maps the runtime fields from engine, cache, runtime, transaction, query, request, maintenance, snapshot and export sections onto `database_service::ServiceOptions`. `namespace` is `s3:{endpoint}:{bucket}:{prefix}` and becomes part of the commit token namespace. `effective(entry)` returns the config plus the sections that entry does not use: the CLI ignores `server` and `mcp`, and the server ignores `cli`.

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

`storage.force_path_style` defaults to `true`, preserving path-style addressing for MinIO. Set it to `false` for virtual-hosted-style addressing required by OSS; keep the regional endpoint (for example `https://oss-cn-beijing.aliyuncs.com`) and bucket as separate settings. The SDK constructs the bucket hostname. Environment override: `PONYDB_STORAGE__FORCE_PATH_STYLE=false`. The value must be a boolean.

## Export budget

`[exports].max_file_bytes` is a positive per-file byte limit for CLI/HTTP snapshot exports (default 10737418240, 10 GiB), mapped to `ServiceOptions::export_max_bytes`. Export uses temporary disk space up to the file size for each concurrent request. Materialization obeys both the request deadline and the database HEAD's read lease; HTTP download obeys the remaining request deadline. Raising the file-size limit does not extend that lease.

The application enables a shared disk cache for verified immutable objects by default. Reads use parsed-node memory, object-body memory, disk, then S3; HEAD and CAS verification always use S3. Separate CLI processes can reuse `objects-v1.sqlite3`. Linux uses `$XDG_CACHE_HOME/ponydb` (absolute paths only), falling back to `$HOME/.cache/ponydb`; macOS uses `$HOME/Library/Caches/ponydb`, Windows `%LOCALAPPDATA%/ponydb`. `cache.directory` overrides this location; `cache.disk_budget_mib = 0` disables disk caching. `config show` reports the resolved directory without creating files.

`cache.disk_budget_mib` limits cached body bytes (default 2048 MiB), shared across all namespaces in a directory, with at most 65,536 entries. SQLite metadata, journals and free pages are additional; incremental reclamation may lag. Processes sharing a directory should use the same budget. `runtime.work_disk_budget_mib` limits local SQLite execution workspace bytes (default 2048 MiB). Restore temporary files remain outside these quotas.

Disk entries are isolated by backend endpoint/bucket/prefix, full object key, length and SHA-256. Every disk read verifies the body, and invalid entries are refetched. Approximate LRU touches are coalesced and throttled to 60 seconds. Disk lookup and fill share a 50 ms wait budget excluding S3 time; busy/failed caches fall back with bounded stderr diagnostics. One blocking task per reader may finish after timeout or cancellation, retaining its execution permit; maintenance is throttled and best effort. Cache contents are disposable and should be stored in a directory trusted by the same users as the database credentials. Library readers remain memory-only unless explicitly configured with `DiskCacheOptions`.

## Runtime diagnostics

The binary activates `logging.level` / `logging.format` after successful configuration loading. Defaults are warn/json; PONYDB_LOGGING__LEVEL and PONYDB_LOGGING__FORMAT override them. No RUST_LOG override is installed. Libraries do not install a global subscriber.
