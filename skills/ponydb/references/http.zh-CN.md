[English](http.md) | **中文**

# HTTP 使用指南

需要供应用或远程 AI Agent 通过 HTTP 访问时，部署此可选服务。HTTP 与 CLI 使用相同的对象存储和数据库操作。

## 安装

在服务主机上按 [CLI 安装](cli.zh-CN.md#安装)准备 `ponydb`，或使用下方的 [Docker](#docker) 部署。远程 HTTP 客户端无需安装 ponydb。

## 使用准备

先按[对象存储和配置](configuration.zh-CN.md)准备存储桶、配置文件或环境变量，再配置服务认证并启动。

### 配置认证

服务要求至少配置一个认证身份。在 `ponydb.toml` 中加入以下内容，将示例 token 替换为至少 16 字节的长随机密钥：

```toml
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

例如，使用 OpenSSL 生成 32 字节的随机密钥，以十六进制输出，再用输出值替换示例 token：

```sh
openssl rand -hex 32
```

也可以通过环境变量传入完整认证对象：

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
```

Bearer token 用于 HTTP/MCP 客户端认证，S3 凭证用于 ponydb 进程访问存储。原生 CLI 直接使用 S3 凭证。身份 ID 和 token 均须唯一；修改认证配置后需重启服务。

如果只允许某个身份查询 `notes`，可设置 `databases = ["notes"]` 和 `scopes = ["db:read", "data:read"]`；如需列出可访问数据库，再加入 `db:list`。上面的管理员示例用 `*` 授予全部数据库和操作权限。

| 权限 | 用途 |
| --- | --- |
| `*` | 授予授权数据库范围内的全部操作权限。 |
| `db:list` | 列出授权范围内具有 `db:read` 权限的数据库。 |
| `db:read` | 查看数据库详情。 |
| `db:create` | 创建数据库。 |
| `db:manage` | 更新数据库配置与元数据，或逻辑删除数据库；修改快照保留期还需 `db:backup`。 |
| `data:read` | 通过 SQL 查询数据。 |
| `data:write` | 通过 SQL 插入、更新或删除数据。 |
| `schema:read` | 通过 Schema 接口查看表、索引、视图和触发器定义。 |
| `schema:write` | 通过 SQL 创建、修改或删除表、索引、视图和触发器。 |
| `db:backup` | 列出、查看和导出保留快照，或使用快照查询；查询还需相应的数据读取权限。 |
| `db:maintenance` | 执行在线压缩和 GC。 |

### 验证配置并启动

输入示例：

```sh
export PONYDB_CONFIG="/absolute/path/ponydb.toml"
ponydb config validate --target server
ponydb server --listen 127.0.0.1:8080
```

`config validate` 检查认证配置，并在配置的存储前缀下执行真实的读写、条件写入、列表和删除探测。服务在前台持续运行；Ctrl-C 或 `SIGTERM` 会停止接收新连接，并等待进行中的请求。对外提供 API 时，通过 TLS 反向代理接入。

### Docker

完成存储与认证配置后，在 `ponydb.toml` 所在目录使用 Release 镜像启动：

```sh
docker run --rm -p 127.0.0.1:8080:8080 \
  --mount "type=bind,source=$PWD/ponydb.toml,target=/config/ponydb.toml,readonly" \
  -e PONYDB_SERVER__LISTEN=0.0.0.0:8080 \
  ghcr.io/hackerneocom/ponydb:0.1.1 server --config /config/ponydb.toml
```

示例使用 `0.1.1`。如需其他版本，可选择 [GitHub Releases](https://github.com/hackerneocom/ponydb/releases) 中对应版本的完整标签，或使用对应 digest 固定镜像。发布镜像面向 Linux amd64/arm64，Docker Hub 镜像地址见 Release 说明。

配置文件须可被容器内非特权用户读取，S3 端点也须能从容器内访问。配置中的 `localhost` 指容器自身。需要从宿主机环境传入凭证或认证时，按需添加 `-e PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`、`-e PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`、可选的 `-e PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN` 和 `-e PONYDB_SERVER__AUTH`。

容器内监听 `0.0.0.0` 以支持端口转发。

## 接口参考

以下示例均使用 `http://127.0.0.1:8080` 作为服务地址。

在客户端终端将 token 设为认证身份中配置的相同值，供后续示例使用：

```sh
export API_TOKEN='replace-with-a-long-random-token'
```

| 主题 | 功能 | 方法与路径 |
| --- | --- | --- |
| 配置与诊断 | 检查进程存活状态 | [`GET /healthz`](#get-healthz) |
| 数据库管理 | 创建数据库 | [`POST /v1/databases`](#post-v1databases) |
| | 列出数据库 | [`GET /v1/databases`](#get-v1databases) |
| | 查看数据库详情 | [`GET /v1/databases/{database}`](#get-v1databasesdatabase) |
| | 更新数据库配置与元数据 | [`PATCH /v1/databases/{database}`](#patch-v1databasesdatabase) |
| | 逻辑删除数据库 | [`DELETE /v1/databases/{database}`](#delete-v1databasesdatabase) |
| SQL 与 Schema | 执行 SQL | [`POST /v1/databases/{database}/execute`](#post-v1databasesdatabaseexecute) |
| | 查看数据库 Schema | [`GET /v1/databases/{database}/schema`](#get-v1databasesdatabaseschema) |
| 快照 | 列出保留快照 | [`GET /v1/databases/{database}/snapshots`](#get-v1databasesdatabasesnapshots) |
| | 查看快照详情 | [`GET /v1/databases/{database}/snapshots/{snapshot_id}`](#get-v1databasesdatabasesnapshotssnapshot_id) |
| | 使用指定快照查询数据 | [`execute` 的快照参数](#使用指定快照查询数据) |
| 备份与恢复 | 下载指定快照 | [`GET /v1/databases/{database}/snapshots/{snapshot_id}/export`](#get-v1databasesdatabasesnapshotssnapshot_idexport) |
| 在线维护 | 合并碎片的存储段，提升数据读取效率 | [`POST /v1/databases/{database}/compact`](#post-v1databasesdatabasecompact) |
| | 清理过期历史与不可达对象，降低对象存储空间占用 | [`POST /v1/databases/{database}/gc`](#post-v1databasesdatabasegc) |
| 服务与 AI Agent | 通过 HTTP 接入 MCP | [`POST /mcp`](#post-mcp) |

将 `{database}` 替换为数据库名，`{snapshot_id}` 替换为保留快照 ID。

### 通用约定

#### 请求头与请求体

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `Authorization: Bearer TOKEN` | `/v1` 接口及 `/mcp` 必填 | | Bearer token 认证；`/healthz` 无需认证。 |
| `Content-Type: application/json` | | | 发送 JSON 请求体时使用；请求体上限为 8 MiB。 |
| `Accept: application/x-ndjson` | | JSON | 仅 SQL 只读查询支持。格式见[输出与脚本集成](#输出与脚本集成)。 |
| `If-Match: ETAG` | | | PATCH、DELETE 的条件；取自最新数据库详情的 `ETag` 响应头，保留双引号。 |

数据库名区分大小写，创建后不可修改。路径、查询参数、请求头中的值须与请求体中的同名字段一致；路径已提供的 `database`、`snapshot_id` 可从请求体省略。未知字段、重复 JSON 键、无效类型及冲突值均会报错。

查询参数中的游标应进行 URL 编码，可使用 `curl --get --data-urlencode "cursor=$CURSOR"` 传入。请求截止时间由服务端 `requests.default_timeout_ms` 配置，内置默认值为 90000 毫秒。

### 配置与诊断

#### GET /healthz

检查进程存活状态，无参数、无需认证。

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/healthz
```

输出示例（HTTP `200`）：

```json
{"status":"ok"}
```

该接口不验证对象存储是否可用。检查认证后的存储访问可请求 `GET /v1/databases`。

### 数据库管理

#### POST /v1/databases

创建数据库。新库的数据版本为 `1`，默认快照保留期来自 `snapshots.retention_days`（7 天）。

JSON 请求体参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `name` | 是 | | 唯一且不可修改的数据库名；1–64 个 Unicode BMP 字符，不允许 NUL、`/`、`\`、`.` 或尾随 ASCII 空格。 |
| `description` | | 空字符串 | 描述；最多 16 KiB UTF-8 字节。 |
| `metadata` | | `null` | 自定义元数据 JSON；紧凑序列化后最多 64 KiB UTF-8 字节。 |

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"api_notes","description":"Notes over HTTP","metadata":{"project":"notes"}}'
```

输出示例（HTTP `201`）：

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "api_notes",
  "data_version": "1"
}
```

#### GET /v1/databases

分页列出当前身份可访问的数据库。

查询参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `limit` | | 50 | 每页最多返回的数据库数，范围 1–1000。 |
| `cursor` | | 从第一页开始 | 上次响应的 `next_cursor`。 |
| `include_deleted` | | `false` | 是否包含已逻辑删除的数据库。 |

输入示例：

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases?limit=20&include_deleted=true" \
  -H "Authorization: Bearer $API_TOKEN"
```

输出示例（节选）：

```json
{
  "outcome": "read_only",
  "databases": [
    {"name":"api_notes","description":"Notes over HTTP","status":"active","data_version":"1"}
  ],
  "next_cursor": null
}
```

`next_cursor` 为 `null` 表示没有下一页；非空时，将其原样传入 `cursor`。每个数据库条目的字段与详情查询相同。

#### GET /v1/databases/{database}

读取数据库详情，包括数据版本、管理 ETag 和提交令牌。除数据库路径参数外，无其他参数。

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" -i
```

输出示例（JSON 节选；`-i` 还会显示响应头）：

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

`ETag` 响应头与 JSON 中的 `etag` 相同。更新或删除前，请重新读取最新值。

#### PATCH /v1/databases/{database}

更新描述、自定义元数据或快照保留期，不改变数据库名和 SQL 数据版本。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `description` | | 保留原值 | JSON 字段；新描述，最多 16 KiB UTF-8 字节。 |
| `metadata` | | | JSON 字段；替换自定义元数据，紧凑序列化后最多 64 KiB UTF-8 字节。 |
| `snapshot_retention_days` | | 保留原值 | JSON 字段；快照保留天数，范围 0–36500。调整时还需 `db:backup` 权限。 |
| `If-Match` | | | 请求头；仅在管理 ETag 仍匹配时更新。 |

输入示例：

```sh
# 将占位值替换为最新详情返回的 ETag，保留双引号
curl --fail-with-body -X PATCH http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'If-Match: "ETAG_FROM_DETAILS"' \
  -d '{"description":"Personal notes","snapshot_retention_days":14}'
```

输出示例：

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "api_notes",
  "metadata_version": "2"
}
```

保留期调整不会立即清理对象，历史清理由显式 GC 执行。ETag 不匹配时返回 HTTP `412`，错误码为 `METADATA_PRECONDITION_FAILED`。

#### DELETE /v1/databases/{database}

逻辑删除数据库，保留墓碑和保留期内的历史。可选请求头 `If-Match` 要求管理 ETag 匹配。

输入示例：

```sh
# 完成其他示例后，重新读取最新 ETag，再逻辑删除数据库
curl --fail-with-body -X DELETE http://127.0.0.1:8080/v1/databases/api_notes \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'If-Match: "ETAG_FROM_DETAILS"'
```

输出示例：

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "api_notes",
  "status": "deleted"
}
```

### SQL 与 Schema

#### POST /v1/databases/{database}/execute

执行单条 SQL，或原子执行多条语句。支持的 SQL 功能、暂不支持的语法及使用限制见 [SQLite 兼容性](sqlite.zh-CN.md)。

JSON 请求体参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `sql` | 与 `statements` 二选一 | | 单条 SQL。 |
| `params` | | 不绑定参数 | 单条 SQL 的类型化参数数组，按顺序绑定。 |
| `statements` | 与 `sql` 二选一 | | 非空语句数组，每项含 `sql` 和可选 `params`；不能同时提供顶层 `sql`、`params`。 |
| `consistency` | | `strong` | `strong`、`snapshot` 或 `at_least`；后两者仅用于查询。 |
| `snapshot_id` | `consistency=snapshot` 时必填 | | 快照 ID，取自快照列表。 |
| `commit_token` | `consistency=at_least` 时必填 | | 提交令牌，读取版本不得早于令牌中的版本。 |
| `max_rows` | | `query.max_rows`（1000） | 仅用于查询，须为正数且不超过 `query.max_rows`。超限报错，不会静默截断。 |

**建表与写入**：在新建的 `api_notes` 中原子创建表并插入一条笔记。

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"statements":[{"sql":"CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"},{"sql":"INSERT INTO notes(title) VALUES (?)","params":[{"type":"text","value":"First note"}]}]}'
```

输出示例：

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

语句按顺序执行，任一失败则整体回滚，无需添加 `BEGIN` 或 `COMMIT`。语句数组沿用写事务语义，即使只包含查询也需要写权限；不能使用 `max_rows`、快照或提交令牌等只读选项。

**查询数据**：

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT id, title FROM notes ORDER BY id LIMIT 10","max_rows":10}'
```

输出示例：

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

`results` 按语句顺序返回，包含 `columns`、类型化的 `rows` 和字符串形式的 `rows_affected`。参数和结果单元格共用 `null`、`integer`、`real`、`text`、`blob` 类型；整数用十进制字符串表示，BLOB 用 Base64 字符串表示。

需要保证读取不早于先前写入时，在查询请求中加入 `"consistency":"at_least","commit_token":"COMMIT_TOKEN"`，并将令牌替换为先前写入返回的值。

#### GET /v1/databases/{database}/schema

查看用户表、索引、视图和触发器的定义。省略筛选项时返回全部非内部对象。

查询参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `table` | | | 精确匹配存在的非内部表名，区分大小写；包含其索引和触发器，不解析视图依赖。 |
| `type` | | | `table`、`index`、`view` 或 `trigger`；与 `table` 取交集。 |

输入示例：

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases/api_notes/schema?table=notes&type=table" \
  -H "Authorization: Bearer $API_TOKEN"
```

输出示例（节选）：

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

`columns` 与每行单元格按顺序对应对象类型、对象名称、所属表或视图名称以及创建 SQL。指定的表不存在时返回 `TABLE_NOT_FOUND`；表存在但没有匹配类型时返回空结果。

### 快照

#### GET /v1/databases/{database}/snapshots

分页列出保留快照，用于历史查询和导出。需要 `db:backup` 权限。

查询参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `limit` | | 50 | 每页快照数，范围 1–1000。 |
| `cursor` | | 从第一页开始 | 上次响应的 `next_cursor`。 |

输入示例：

```sh
curl --fail-with-body "http://127.0.0.1:8080/v1/databases/api_notes/snapshots?limit=20" \
  -H "Authorization: Bearer $API_TOKEN"
```

输出示例（节选）：

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

需要继续列举时，将 `next_cursor` 原样传给 `cursor`。`retention_days` 是数据库当前的快照保留期。

后续示例使用快照列表返回的实际 ID，请先设置：

```sh
export SNAPSHOT_ID='SNAPSHOT_ID_FROM_LIST'
```

#### GET /v1/databases/{database}/snapshots/{snapshot_id}

读取保留快照的描述信息。需要 `db:backup` 权限；除路径参数外，无其他参数。

输入示例：

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID" \
  -H "Authorization: Bearer $API_TOKEN"
```

输出示例（节选）：

```json
{
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "2",
  "retired_at_upper_ms": null,
  "observed_at_ms": 1794182400000
}
```

#### 使用指定快照查询数据

通过 `execute` 的 `consistency` 和 `snapshot_id` 查询历史数据，还需 `db:backup` 权限。

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"sql\":\"SELECT id, title FROM notes ORDER BY id\",\"consistency\":\"snapshot\",\"snapshot_id\":\"$SNAPSHOT_ID\"}"
```

输出结构与 SQL 只读查询相同，`data_version` 对应所选快照。

### 备份与恢复

#### GET /v1/databases/{database}/snapshots/{snapshot_id}/export

将指定保留快照下载为独立 SQLite 文件。需要 `db:backup` 权限；除路径参数外，无其他参数。

输入示例：

```sh
curl --fail-with-body \
  "http://127.0.0.1:8080/v1/databases/api_notes/snapshots/$SNAPSHOT_ID/export" \
  -H "Authorization: Bearer $API_TOKEN" \
  -o ./api-notes-backup.sqlite
```

成功返回 HTTP `200`，`Content-Type` 为 `application/vnd.sqlite3`，响应体写入 `./api-notes-backup.sqlite`。下载完成后再使用该文件。

导出当前数据库和从文件恢复仅支持 CLI，见 [CLI 备份与恢复](cli.zh-CN.md#备份与恢复)。

### 在线维护

维护工作预算与请求截止时间相互独立。

#### POST /v1/databases/{database}/compact

在线合并存储段，保持逻辑数据版本不变。需要 `db:maintenance` 权限。

JSON 请求体参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `budget_ms` | | 10000 | 本次工作预算，范围 1–60000 毫秒。 |
| `max_segments` | | 32 | 最多处理的存储段数，范围 1–32。 |

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/compact \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{}'
```

输出示例：

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "segments_processed": 1,
  "data_version": "2"
}
```

实际处理数量取决于数据库状态。

#### POST /v1/databases/{database}/gc

在线清理过期历史和不可达对象。需要 `db:maintenance` 权限。

JSON 请求体参数：

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `budget_ms` | | `maintenance.gc_default_budget_ms`（60000） | 本次工作预算，单位为毫秒；须为正数且不超过 `requests.max_timeout_ms`。 |
| `max_objects` | | `maintenance.gc_max_objects_per_call`（1000） | 最多尝试删除的对象数；部署上限也由该配置决定，须为正数。 |

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/gc \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"budget_ms":60000,"max_objects":1000}'
```

输出示例：

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "phase": "complete",
  "deleted_objects": 0
}
```

预算或对象数量上限耗尽时的可能输出：

```json
{
  "outcome": "partial",
  "complete": false,
  "database_available": true,
  "phase": "sweeping",
  "deleted_objects": 1000
}
```

压缩或 GC 返回 `outcome: "partial"` 时，再次调用同一操作继续。HTTP 状态仍为 `200`，客户端还需检查 `outcome` 和 `complete`。GC 没有暂停、恢复或断点续传接口。

历史快照在保留期结束后通过显式 GC 清理。请保持系统时钟准确，并导出需要长期保存的历史。查询与导出限制见[配置指南](configuration.zh-CN.md)。

### 服务与 AI Agent

#### POST /mcp

通过同一服务端口提供 HTTP MCP，与 REST API 共用身份和存储。客户端设置、请求头、协议探测和工具调用方式见 [AI Agent 接入](agent-clients.zh-CN.md#http-mcp)。

## 输出与脚本集成

成功的 JSON 操作直接返回业务结果。创建数据库返回 HTTP `201`，其他 JSON 操作成功返回 `200`；SQLite 下载返回二进制响应体。

只读 SQL 可通过 `Accept: application/x-ndjson` 获取逐行 JSON 记录：

输入示例：

```sh
curl --fail-with-body http://127.0.0.1:8080/v1/databases/api_notes/execute \
  -H "Authorization: Bearer $API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/x-ndjson' \
  -d '{"sql":"SELECT 1"}'
```

输出示例（每条记录占一行）：

```jsonl
{"type":"result_start","data_version":"2"}
{"type":"rows","columns":["1"],"rows":[[{"type":"integer","value":"1"}]]}
{"type":"done","outcome":"read_only"}
```

脚本应同时检查 HTTP 状态和业务结果。示例中的 `curl --fail-with-body` 会在 HTTP 错误时返回非零退出码，并保留错误响应体。ETag 不匹配时的输出示例（HTTP `412`）：

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

| HTTP 状态 | 常见错误码与处理方式 |
| --- | --- |
| `400` | `INVALID_REQUEST` 等；检查参数、类型和 SQL。 |
| `401` / `403` | `UNAUTHENTICATED` / `FORBIDDEN`；检查 token、数据库授权和操作权限。 |
| `404` | `DATABASE_NOT_FOUND` / `TABLE_NOT_FOUND`；检查名称。 |
| `409` | `TX_CONFLICT` / `DATABASE_EXISTS`；检查并发写入或数据库是否已存在。 |
| `410` | `DATABASE_DELETED` / `SNAPSHOT_EXPIRED`；检查数据库状态或快照保留期。 |
| `412` | `METADATA_PRECONDITION_FAILED`；重新读取详情后再决定是否更新或删除。 |
| `413` | `RESOURCE_LIMIT`；检查请求体大小或其他资源限制。 |
| `429` | `RATE_LIMITED`；降低请求频率。 |
| `503` | `STORAGE_UNAVAILABLE`、`MAINTENANCE`、`MAINTENANCE_REQUIRED` 或 `OUTCOME_UNKNOWN`；按错误码和 `outcome` 判断后续操作。 |

重复调用写入会再次执行。响应丢失或返回 `OUTCOME_UNKNOWN` 时，先检查数据库，再决定是否重试；可通过业务键或唯一约束识别重复操作。
