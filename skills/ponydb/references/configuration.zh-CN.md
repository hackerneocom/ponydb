[English](configuration.md) | **中文**

# 对象存储和配置

CLI、stdio MCP、HTTP 服务和 Docker 使用同一套配置格式。在这里完成存储配置后，继续阅读 [CLI](cli.zh-CN.md)、[HTTP](http.zh-CN.md) 或 [AI Agent](agent-clients.zh-CN.md) 指南。

## 对象存储

连接前先创建存储桶。服务必须提供：

- S3 兼容的 GetObject、PutObject、DeleteObject 和分页 ListObjectsV2，包括前缀与分隔符列表。
- 原子条件 PutObject，同时支持 `If-None-Match: *` 和基于 ETag 的 `If-Match`，并在并发请求下保持正确。仅支持条件读取不够。
- 强写后读一致性、Range GET、保留 SHA-256 对象元数据，以及至少 8 MiB 的单对象大小。
- 对所选桶和前缀具有 List/Get/Put/Delete 权限的凭证。AWS IAM 对应 `s3:ListBucket`、`s3:GetObject`、`s3:PutObject`、`s3:DeleteObject`。

首次使用及切换服务商后，可执行 `ponydb config validate --target cli` 验证对象存储服务是否符合要求。

### 明确已支持的服务

| 服务 | 部署形式 | 端点与设置 |
| --- | --- | --- |
| [RustFS](https://rustfs.com/)（自托管推荐） | 开源，自托管 | S3 API 地址，例如 `http://127.0.0.1:9000`；路径式寻址（`true`） |
| [火山引擎 TOS](https://www.volcengine.com/product/tos)（中国区域推荐） | 托管云存储 | 地域 **S3** 端点，例如 `https://tos-s3-cn-beijing.volces.com`；签名地域须匹配 |
| [Cloudflare R2](https://www.cloudflare.com/products/r2/)（非中国区域推荐） | 托管云存储 | `https://<account-id>.r2.cloudflarestorage.com`；签名地域为 `auto` |
| [MinIO](https://www.min.io/) | 开源，自托管 | 自有 S3 API 地址；路径式寻址（`true`） |
| [AWS S3](https://aws.amazon.com/s3/) | 托管云存储 | 地域端点，例如 `https://s3.us-east-1.amazonaws.com`；签名地域须匹配 |

> [!IMPORTANT]
> 不要通过生命周期规则删除 ponydb 对象，应使用 ponydb GC 回收不再引用的数据。

## 配置

连接相同端点、桶和前缀的客户端访问同一组数据库。

[完整配置模板](https://github.com/hackerneocom/ponydb/blob/main/examples/ponydb.toml)列出了可调整的设置和默认值。修改可选配置时，需要同时取消节标题和对应设置的注释。

### 配置文件

以下为最小可用配置实例：

```toml
[storage]
# 必填：S3 API 端点，不包含桶名；默认空字符串。
# 使用服务商的 API 端点，避免使用 CDN 或静态网站地址。
endpoint = "https://s3.example.com"
# 必填：已创建的存储桶名称；默认空字符串。
bucket = "my-bucket"
# 请求签名地域，默认 "us-east-1"；按服务商要求填写，R2 使用 "auto"。
region = "us-east-1"
# 默认 true，使用路径式寻址；端点要求虚拟主机式寻址时设为 false。
force_path_style = true
# 桶内对象键前缀，默认 ""（无前缀）；此处示例使用 "ponydb/"。
prefix = "ponydb/"

[storage.credentials]
# 必填：访问密钥 ID 和密钥；两者默认均为空字符串。
access_key_id = "<access-key-id>"
secret_access_key = "<secret-access-key>"
# 可选：临时凭证的会话令牌，默认未设置；使用临时凭证时取消注释并填写。
# session_token = "<temporary-credential-token>"
```

将端点、桶和凭证替换为实际值，并按上表选择签名地域与寻址方式。

按以下顺序选择配置文件，各文件之间不合并：

| 优先级 | 来源 |
| --- | --- |
| 1 | `ponydb --config /path/to/ponydb.toml ...` |
| 2 | `PONYDB_CONFIG=/path/to/ponydb.toml` |
| 3 | 下表中的平台默认路径 |

| 平台 | 默认配置文件 |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/ponydb/config.toml`，未设置时为 `~/.config/ponydb/config.toml` |
| macOS | `~/Library/Application Support/ponydb/config.toml` |
| Windows | `%APPDATA%\ponydb\config.toml` |

平台默认文件不存在时，使用内置默认值和环境变量；通过 `--config` 或 `PONYDB_CONFIG` 显式指定的文件不存在则报错。TOML 文件中的相对文件路径以配置文件所在目录为基准。

### 环境变量

环境变量覆盖文件中的值。命名格式为 `PONYDB_<SECTION>__<KEY>`，更深层字段继续用 `__` 分隔。无需配置文件时，保持 `PONYDB_CONFIG` 未设置，并提供四项必需的 S3 配置：

```sh
export PONYDB_STORAGE__ENDPOINT=https://s3.example.com
export PONYDB_STORAGE__BUCKET=my-bucket
export PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID='<access-key-id>'
export PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY='<secret-access-key>'
ponydb databases list
```

临时凭证还需设置 `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`。应用不读取 `AWS_*` 凭证变量或 AWS 凭证文件。未知配置字段及未知 `PONYDB_` 变量会被拒绝。`server --listen` 等命令专用参数优先于配置文件和环境变量。

### 检查配置与常用设置

```sh
ponydb config show --target cli
ponydb config show --target server
ponydb config validate --target cli
ponydb config validate --target server
```

`config show` 可离线运行，始终隐藏凭证和认证 token。`config validate` 检查配置，并在所配置的前缀下创建临时对象执行真实 S3 检查，包含一个 2 MiB 对象。它需要 List/Get/Put/Delete 权限，并会清理探测对象；执行被中断时可能留下探测对象。`--target server` 或 `all` 还检查服务认证配置。

常用设置：

| 配置项 | 默认值 | 用途 |
| --- | --- | --- |
| `server.listen` | `127.0.0.1:8080` | HTTP 监听地址 |
| `cli.output` | `json` | CLI 输出格式：`json` 或 `table` |
| `requests.default_timeout_ms` | `90000` | 默认请求超时，文件恢复除外 |
| `requests.max_timeout_ms` | `1800000` | 请求超时上限 |
| `query.max_rows` | `1000` | 查询返回行数上限 |
| `query.max_result_bytes` | `1048576` | 查询结果序列化后的大小上限，1 MiB |
| `exports.max_file_bytes` | `10737418240` | 单个 SQLite 导出文件上限，10 GiB |
| `snapshots.retention_days` | `7` | 默认历史保留天数 |
| `cache.directory` | 用户缓存目录 | 本地可丢弃缓存的位置 |
| `cache.disk_budget_mib` | `2048` | 缓存数据预算，`0` 禁用磁盘缓存 |
| `logging.level` / `logging.format` | `warn` / `json` | stderr 诊断日志；格式也支持 `text` |

查询结果超限会报错，不会静默截断。使用 SQL `LIMIT` 或分页控制结果大小。耗时较长的导出还可能需要提高请求超时和读取时限，见[导出说明](https://github.com/hackerneocom/ponydb/blob/main/apps/cli/README.zh-CN.md#数据库与快照导出)。
