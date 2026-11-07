# ponydb

通过原生 CLI 或 HTTP API，使用存储在 S3 上的 SQLite 数据库。

创建数据库，执行参数化 SQL 和原子事务，查看历史快照，导出标准 SQLite 备份。CLI 支持 Linux、macOS 和 Windows，直接连接 S3；应用需要 API 接入时，可启动 HTTP server。MCP 客户端也可通过 HTTP 或 stdio 连接。

[English](README.md) · [快速开始](#快速开始) · [配置方式](#配置方式) · [CLI 使用](#cli-使用) · [HTTP-server](#http-server)

## 安装

安装 Rust 1.95 后，在仓库根目录执行：

```sh
cargo install --locked --path apps/cli
ponydb version
```

`ponydb` 会安装到 Cargo 的二进制目录，请确保该目录已加入 `PATH`。SQLite 已内置。使用容器运行 HTTP server 的方式见 [Docker](#docker)。

## 快速开始

准备一个已创建的 S3 兼容存储桶，以及对该桶和所用前缀具有 List/Get/Put/Delete 权限的凭证。存储服务必须支持条件写入，可通过 `config validate` 检查兼容性。本地 S3 服务可参考 [RustFS 开发环境](.devcontainer/README.md#rustfs)。

以下命令使用 POSIX shell 语法；Windows 用户请按所用 shell 调整环境变量设置方式。

复制配置模板，将其中四项未注释的 S3 配置替换为实际值：endpoint、bucket、access key ID 和 secret access key。

```sh
cp examples/ponydb.toml ./ponydb.toml
# 编辑 ponydb.toml 后再继续。
export PONYDB_CONFIG="$PWD/ponydb.toml"
ponydb config validate --target cli
```

后续示例沿用该环境变量，也可以在每条命令中指定 `--config ./ponydb.toml`。凭证还可通过[环境变量](#环境变量)传入。

创建数据库，写入一条笔记，然后查询：

```sh
ponydb databases create notes --description "My notes"
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"
ponydb db notes execute "INSERT INTO notes(title) VALUES (?)" \
  --param '{"type":"text","value":"First note"}'
ponydb db notes execute "SELECT id, title FROM notes ORDER BY id" --output table
```

查询结果中应包含 ID 为 `1`、标题为 `First note` 的笔记。执行这些命令无需启动 HTTP server。

## 配置方式

CLI、HTTP server 和 stdio MCP 使用同一套 TOML 配置与环境变量。[完整配置模板](examples/ponydb.toml)列出了可调整的设置和默认值。修改可选配置时，需要同时取消节标题和对应设置的注释。

### 配置文件

CLI 访问所需的最小文件示例：

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

将 endpoint 和凭证替换为存储服务的实际值，并提前创建桶。使用相同 endpoint、bucket 和 prefix 的客户端访问同一组数据库。`storage.force_path_style` 默认为 `true`，适用于 MinIO 等服务；OSS 等要求虚拟主机寻址的服务应设为 `false`。endpoint 和 bucket 始终分别配置。

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

查询结果超限会报错，不会静默截断。使用 SQL `LIMIT` 或分页控制结果大小。耗时较长的导出还可能需要提高请求超时和读取时限，见[导出说明](apps/cli/README.zh-CN.md#数据库与快照导出)。

## CLI 使用

`databases` 用于创建和列出数据库；`db DATABASE_NAME [ACTION]` 用于操作指定数据库，省略动作时返回数据库详情。数据库名必须显式提供，区分大小写且不可修改，长度为 1–64 个 Unicode BMP 字符，不允许 NUL、`/`、`\`、`.` 或尾随 ASCII 空格。名称含空格时请在 shell 中加引号。

### 管理数据库

```sh
ponydb databases list --limit 20
ponydb databases list --include-deleted
ponydb db notes
ponydb db notes update --description "Personal notes"
ponydb db notes schema
ponydb db notes schema --table notes --type table
```

列表还有下一页时，响应会返回 `next_cursor`，通过 `--cursor CURSOR` 继续获取。Schema 类型筛选支持 `table`、`index`、`view`、`trigger`；`--table` 精确匹配表名，且该表必须存在。

更新和删除可使用 `ponydb db notes` 返回的 `etag` 进行条件操作。保留值中嵌入的双引号，例如：

```sh
ponydb db notes update --if-match '"ETAG_FROM_DETAILS"' --description "Updated notes"
```

`ponydb db notes delete` 逻辑删除数据库。省略 `--if-match` 时，更新和删除不要求先前获取的 ETag 匹配。描述上限为 16 KiB UTF-8 字节；通过 `--metadata-file metadata.json` 提供的自定义元数据，上限为紧凑 JSON 序列化后的 64 KiB UTF-8 字节。

### SQL、参数与事务

单条 SQL 可直接传入，也可从 UTF-8 文件或 stdin 读取：

```sh
ponydb db notes execute "SELECT * FROM notes LIMIT 10" --output table
ponydb db notes execute --sql-file ./query.sql
printf 'SELECT count(*) FROM notes' | ponydb db notes execute --sql-file -
```

使用重复的 `--param` 绑定多个参数，或通过 `--params-file params.json` 传入 JSON 数组，两种形式互斥。参数和 JSON 结果单元格均使用以下类型：

| 类型 | JSON 示例 |
| --- | --- |
| Null | `{"type":"null"}` |
| Integer | `{"type":"integer","value":"42"}` |
| Real | `{"type":"real","value":3.14}` |
| Text | `{"type":"text","value":"A note"}` |
| Blob | `{"type":"blob","value":"SGVsbG8="}`（base64） |

整数以字符串表示，避免丢失 64 位精度。JSON 结果包含 `results` 数组，每项有 `columns` 和类型化的 `rows`；写入还返回 `committed` 和 `commit_token`。

需要原子执行多条语句时，将以下内容保存为 `statements.json`：

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

语句按顺序执行，任一失败则整个请求回滚。无需添加 `BEGIN` 或 `COMMIT`，也不要在 `--sql-file` 中放多条语句。语句数组即使只包含查询也需要写权限，且不能使用 `--max-rows`、快照一致性等只读选项。

接收 JSON 请求的数据库命令可通过 `--input FILE|-` 读取完整请求，但数据库名仍须在命令中显式提供，并与请求一致。未知字段、重复键、文件与命令参数冲突均会报错。一次调用最多只能有一个输入读取 stdin。

### 快照、备份与恢复

```sh
ponydb db notes snapshot list --limit 20
ponydb db notes snapshot get SNAPSHOT_ID
ponydb db notes execute "SELECT * FROM notes" \
  --consistency snapshot --snapshot-id SNAPSHOT_ID
ponydb db notes export ./notes-backup.sqlite
ponydb db notes snapshot export SNAPSHOT_ID ./notes-history.sqlite
ponydb db restored_notes restore file ./notes-backup.sqlite
```

将 `SNAPSHOT_ID` 替换为 `snapshot list` 返回的保留快照 ID。普通查询默认使用 `strong` 一致性；`at_least` 查询可配合先前写入返回的 `--commit-token TOKEN`，要求读取的版本不早于该次写入。

导出生成独立 SQLite 文件，不覆盖已有文件，目标目录必须存在。文件恢复仅支持 CLI，输入须为稳定、完整且使用 DELETE journal 模式的 SQLite 备份。目标数据库必须不存在或已删除，因此不要先创建 `restored_notes`。恢复需要本地临时磁盘空间，进度输出到 stderr，无总执行截止时间，也不支持断点续传；请等待最终成功结果后再视为完成。

### 维护与脚本集成

```sh
ponydb db notes compact
ponydb db notes gc
ponydb db notes gc --budget-ms 300000 --deadline-ms 330000
ponydb db notes execute "SELECT 1" --output json > result.json
ponydb --help
ponydb db notes execute --help
```

压缩和 GC 在线执行。返回 `outcome: "partial"`、退出码 `6` 时，再次调用相同操作继续。压缩默认工作预算为 10 秒、最多处理 32 个段；GC 默认为 60 秒、最多尝试删除 1000 个对象。历史快照在保留期结束后通过显式 GC 清理；请保持系统时钟准确，并导出需要长期保存的历史。

成功结果写入 stdout，诊断日志、错误和恢复进度写入 stderr；请通过退出码判断是否成功。`--output json|table` 选择 CLI 结果格式。`--deadline-ms` 可覆盖恢复之外的数据库操作超时，也适用于 `mcp`；`config`、`version`、`server` 不接受该参数。Server 和 MCP 命令不接受 `--output`。

| 退出码 | 含义 |
| --- | --- |
| `0` | 成功 |
| `1` | 其他错误 |
| `2` | 配置或请求无效 |
| `3` | 认证失败或权限不足 |
| `4` | 事务冲突或 ETag 不匹配 |
| `5` | 写入结果未知，先检查数据再决定是否重试 |
| `6` | 维护在本次工作预算内未完成，再次调用继续 |

重复调用写入会再次执行。响应丢失或返回 `OUTCOME_UNKNOWN` 时，先检查数据库，再决定是否重试；可通过业务键或唯一约束识别重复操作。

## HTTP server

### 配置认证并启动

服务使用已有的 S3 配置，并要求至少配置一个认证身份。在 `ponydb.toml` 中加入以下内容，将示例 token 替换为至少 16 字节的长随机密钥：

```toml
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

也可以通过环境变量传入完整认证对象：

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
```

Bearer token 用于 HTTP/MCP 客户端认证，S3 凭证用于 ponydb 进程访问存储。原生 CLI 直接使用 S3 凭证，不使用服务 token。身份 ID 和 token 均须唯一；修改认证配置后需重启服务。

如果只允许某个身份查询 `notes`，可设置 `databases = ["notes"]` 和 `scopes = ["db:read", "data:read"]`；如需列出可访问数据库，再加入 `db:list`。其他权限包括 `db:create`、`db:manage`、`data:write`、`schema:write`、`db:backup`、`db:maintenance`。上面的管理员示例用 `*` 授予全部数据库和操作权限。

```sh
ponydb config validate --target server
ponydb server --listen 127.0.0.1:8080
```

服务在前台运行。Ctrl-C 或 `SIGTERM` 会停止接收新连接，并等待进行中的请求。另开终端检查进程存活状态：

```sh
curl --fail-with-body http://127.0.0.1:8080/healthz
```

该接口无需认证，返回 `{"status":"ok"}`，但不验证 S3 是否可用。检查认证后的存储访问可请求 `GET /v1/databases`。对外提供 API 时，通过 TLS 反向代理接入。

### 创建数据库并执行 SQL

在另一个终端将 token 设为认证身份中配置的相同值。以下示例创建独立的 `api_notes` 数据库：

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

响应使用与 CLI 相同的 SQL 值类型。只读查询返回 `outcome: "read_only"`，写入成功返回 `committed: true`。请求体上限为 8 MiB；路径、查询参数、请求头中的值须与请求体中的同名字段一致。只读查询可通过 `Accept: application/x-ndjson` 获取 `result_start`、`rows`、`done` 记录。

### API 速查

全部 `/v1` 接口均需 `Authorization: Bearer TOKEN`。将 `{database}` 替换为数据库名，`{snapshot_id}` 替换为保留快照 ID。

| 方法 | 路径 | 请求 / 用途 |
| --- | --- | --- |
| `POST` | `/v1/databases` | `{"name":"notes"}`；可选描述和元数据 |
| `GET` | `/v1/databases` | 列表；查询参数 `limit`、`cursor`、`include_deleted` |
| `GET` | `/v1/databases/{database}` | 数据库详情和 ETag |
| `PATCH` | `/v1/databases/{database}` | 更新描述、元数据或 `snapshot_retention_days` |
| `DELETE` | `/v1/databases/{database}` | 逻辑删除数据库 |
| `POST` | `/v1/databases/{database}/execute` | 单条 `sql`/`params` 或原子 `statements` 数组 |
| `GET` | `/v1/databases/{database}/schema` | 可选查询参数 `table`、`type` |
| `POST` | `/v1/databases/{database}/compact` | `{"budget_ms":10000,"max_segments":32}` |
| `POST` | `/v1/databases/{database}/gc` | 可选 `budget_ms`、`max_objects` |
| `GET` | `/v1/databases/{database}/snapshots` | 历史列表；查询参数 `limit`、`cursor` |
| `GET` | `/v1/databases/{database}/snapshots/{snapshot_id}` | 快照详情 |
| `GET` | `/v1/databases/{database}/snapshots/{snapshot_id}/export` | SQLite 下载，需要 `db:backup` |

先获取数据库详情中的 `ETag` 响应头，再在 PATCH 或 DELETE 中传入 `If-Match`，即可要求版本匹配。ETag 不匹配返回 HTTP `412`，事务冲突返回 `409`。数据库创建成功返回 `201`，其他 JSON 操作成功返回 `200`。错误结构为 `{"ok":false,"error":{...}}`，包含 `code`、`message`、`retryable`、`outcome`。收到 `503` 且错误码为 `OUTCOME_UNKNOWN` 时，先检查数据，再决定是否重复写入。

HTTP 支持下载历史快照；导出当前数据库和从文件恢复使用 CLI。例如，将 `SNAPSHOT_ID` 设为快照列表接口返回的 ID 后，下载备份：

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID/export" \
  -H "Authorization: Bearer $API_TOKEN" \
  -o ./api-notes-backup.sqlite
```

### Docker

在仓库根目录构建镜像，镜像默认启动 `ponydb server`：

```sh
docker build -t ponydb .
docker run --rm -p 127.0.0.1:8080:8080 \
  --mount "type=bind,source=$PWD/ponydb.toml,target=/config/ponydb.toml,readonly" \
  -e PONYDB_SERVER__LISTEN=0.0.0.0:8080 \
  ponydb server --config /config/ponydb.toml
```

挂载的配置须包含 S3 凭证和服务认证身份，容器内的非特权用户须能读取该文件，S3 endpoint 也须能从容器内访问。配置中的 `localhost` 指容器自身。需要通过宿主机环境变量传入凭证或认证时，按需添加 `-e PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`、`-e PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`、可选的 `-e PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN` 和 `-e PONYDB_SERVER__AUTH`。容器内监听 `0.0.0.0` 以支持端口转发，示例仅将端口发布到宿主机回环地址。

## MCP 客户端

让客户端启动本地 MCP 进程时使用：

```sh
ponydb --config /absolute/path/ponydb.toml mcp
```

该进程直接使用 S3 凭证。HTTP server 默认还提供 `/mcp`，使用相同 Bearer 认证；`mcp.enabled = false` 可禁用 HTTP MCP 接口。客户端配置和协议请求头要求见 [stdio 接入说明](apps/cli/README.zh-CN.md#通过-stdio-mcp-接入-agent)及 [HTTP MCP 说明](apps/cli/SERVER.zh-CN.md)。

## 更多文档

- [CLI 参考](apps/cli/README.zh-CN.md) / [English](apps/cli/README.md)
- [HTTP 与 MCP 参考](apps/cli/SERVER.zh-CN.md) / [English](apps/cli/SERVER.md)
- [完整配置模板](examples/ponydb.toml)
- [开发环境](.devcontainer/README.md#rustfs)与[测试说明](tests/README.md)

## 许可证

[GNU Affero General Public License，仅限第 3 版（AGPL-3.0-only）](LICENSE)。
