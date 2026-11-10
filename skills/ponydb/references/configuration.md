**English** | [中文](configuration.zh-CN.md)

# Object storage and configuration

CLI, stdio MCP, the HTTP service and Docker use one configuration format. Configure storage here once, then follow the [CLI](cli.md), [HTTP](http.md) or [AI Agent](agent-clients.md) guide.

## Object storage

Create the bucket before connecting. The service must provide:

- S3-compatible GetObject, PutObject, DeleteObject and paginated ListObjectsV2, including prefix and delimiter listing.
- Atomic conditional PutObject with both `If-None-Match: *` and ETag-based `If-Match`, including under concurrent requests. Conditional reads alone are insufficient.
- Strong read-after-write consistency, Range GET, preserved SHA-256 object metadata and object sizes of at least 8 MiB.
- Credentials with List/Get/Put/Delete permissions for the selected bucket and prefix. With AWS IAM, these correspond to `s3:ListBucket`, `s3:GetObject`, `s3:PutObject` and `s3:DeleteObject`.

Before first use and after switching providers, you can run `ponydb config validate --target cli` to verify that the object storage service meets these requirements.

### Explicitly supported services

| Service | Deployment | Endpoint / settings |
| --- | --- | --- |
| [RustFS](https://rustfs.com/) (recommended for self-hosting) | Open source, self-hosted | Your S3 API address, such as `http://127.0.0.1:9000`; path style (`true`) |
| [Volcengine TOS](https://www.volcengine.com/product/tos) (recommended for China regions) | Managed cloud storage | Regional **S3** endpoint, such as `https://tos-s3-cn-beijing.volces.com`; matching signing region |
| [Cloudflare R2](https://www.cloudflare.com/products/r2/) (recommended for regions outside China) | Managed cloud storage | `https://<account-id>.r2.cloudflarestorage.com`; signing region `auto` |
| [MinIO](https://www.min.io/) | Open source, self-hosted | Your S3 API address; path style (`true`) |
| [AWS S3](https://aws.amazon.com/s3/) | Managed cloud storage | Regional endpoint, such as `https://s3.us-east-1.amazonaws.com`; matching signing region |

> [!IMPORTANT]
> Do not apply lifecycle deletion rules to ponydb objects; use ponydb GC to reclaim unreferenced data.

## Configuration

Clients using the same endpoint, bucket and prefix access the same databases.

The [annotated configuration template](https://github.com/hackerneocom/ponydb/blob/main/examples/ponydb.toml) lists available settings and defaults. Uncomment an optional section header together with its settings to change a default.

### Configuration file

Here is a minimal working configuration example:

```toml
[storage]
# Required: S3 API endpoint without the bucket name. Defaults to an empty string.
# Use the provider's API endpoint rather than a CDN or website endpoint.
endpoint = "https://s3.example.com"
# Required: name of an existing bucket. Defaults to an empty string.
bucket = "my-bucket"
# Signing region. Defaults to "us-east-1"; follow your provider's requirements ("auto" for R2).
region = "us-east-1"
# Defaults to true for path-style addressing; use false if the endpoint requires virtual-hosted addressing.
force_path_style = true
# Object-key prefix within the bucket. Defaults to "" (no prefix); this example uses "ponydb/".
prefix = "ponydb/"

[storage.credentials]
# Required: access key ID and secret access key. Both default to empty strings.
access_key_id = "<access-key-id>"
secret_access_key = "<secret-access-key>"
# Optional session token, unset by default. Uncomment and supply it for temporary credentials.
# session_token = "<temporary-credential-token>"
```

Replace the endpoint, bucket and credentials with your own values, and choose the region and addressing style from the provider settings above.

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

Query limits cause an error rather than silently truncating results. Use SQL `LIMIT` or pagination to keep results within the configured limits. Longer exports may also need a longer request timeout and read duration; see the [export reference](https://github.com/hackerneocom/ponydb/blob/main/apps/cli/README.md#database-and-snapshot-export).
