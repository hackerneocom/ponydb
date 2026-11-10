[English](cli.md) | **中文**

# CLI 使用指南

CLI 可直接连接对象存储操作数据库，无需启动 HTTP 服务。

## 安装

从 [GitHub Releases](https://github.com/hackerneocom/ponydb/releases) 下载预编译 CLI，无需安装 Rust：

1. 选择所需版本，下载适合系统的 Linux x86_64/ARM64、macOS Intel/Apple Silicon 或 Windows x86_64 压缩包及 `SHA256SUMS`。
2. 将 `ponydb`（Windows 为 `ponydb.exe`）放入 `PATH` 中的目录。
3. 运行 `ponydb version` 确认安装成功，输入与输出见下方命令参考。

SQLite 已内置。下载、校验和安装命令见[安装指南](installation.zh-CN.md)；容器部署见 [Docker](http.zh-CN.md#docker)。

## 使用准备

首次使用前，请按[对象存储和配置](configuration.zh-CN.md)准备存储桶、配置文件或环境变量，并完成配置检查。

本文使用 POSIX shell，Windows 请使用相应终端语法。各主题可独立查阅。

## 命令参考

| 主题 | 功能 | 命令 |
| --- | --- | --- |
| 配置与诊断 | 查看版本 | [`version`](#version) |
| | 查看生效配置 | [`config show`](#config-show) |
| | 验证配置与对象存储能力 | [`config validate`](#config-validate) |
| | 查看命令帮助 | [`help` / `--help`](#help----help) |
| 数据库管理 | 创建数据库 | [`databases create DATABASE_NAME`](#databases-create-database_name) |
| | 列出数据库 | [`databases list`](#databases-list) |
| | 查看数据库详情 | [`db DATABASE_NAME`](#db-database_name) |
| | 更新数据库配置与元数据 | [`db DATABASE_NAME update`](#db-database_name-update) |
| | 逻辑删除数据库 | [`db DATABASE_NAME delete`](#db-database_name-delete) |
| SQL 与 Schema | 执行 SQL | [`db DATABASE_NAME execute`](#db-database_name-execute) |
| | 查看数据库 Schema | [`db DATABASE_NAME schema`](#db-database_name-schema) |
| 快照 | 列出保留快照 | [`db DATABASE_NAME snapshot list`](#db-database_name-snapshot-list) |
| | 查看快照详情 | [`db DATABASE_NAME snapshot get SNAPSHOT_ID`](#db-database_name-snapshot-get-snapshot_id) |
| | 使用指定快照查询数据 | [`db DATABASE_NAME execute --consistency snapshot --snapshot-id SNAPSHOT_ID`](#db-database_name-execute---consistency-snapshot---snapshot-id-snapshot_id) |
| 备份与恢复 | 导出当前版本 | [`db DATABASE_NAME export SQLITE_FILE`](#db-database_name-export-sqlite_file) |
| | 导出指定快照 | [`db DATABASE_NAME snapshot export SNAPSHOT_ID SQLITE_FILE`](#db-database_name-snapshot-export-snapshot_id-sqlite_file) |
| | 从 SQLite 文件恢复 | [`db DATABASE_NAME restore file SQLITE_FILE`](#db-database_name-restore-file-sqlite_file) |
| 在线维护 | 合并碎片的存储段，提升数据读取效率 | [`db DATABASE_NAME compact`](#db-database_name-compact) |
| | 清理过期历史与不可达对象，降低对象存储空间占用 | [`db DATABASE_NAME gc`](#db-database_name-gc) |
| 服务与 MCP | 启动 HTTP 服务（含 HTTP MCP） | [`server`](#server) |
| | 启动 stdio MCP | [`mcp`](#mcp) |

表中命令均省略 `ponydb` 前缀。`DATABASE_NAME`、`SNAPSHOT_ID`、`SQLITE_FILE` 需替换为实际值。

### 通用约定

#### 全局参数

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--config CONFIG_FILE` |  | `PONYDB_CONFIG` 或平台默认路径 | 指定 TOML 配置文件。可用于所有命令；`version` 不读取配置。 |
| `--output FORMAT` |  | `cli.output`（内置值 `json`） | `json` 或 `table`。`server`、`mcp` 不接受。 |
| `--deadline-ms MILLISECONDS` |  | `requests.default_timeout_ms`（90000） | 单次请求超时；须为正数且不超过配置上限（内置 1800000）。适用于业务命令及 `mcp`，不适用于 `config`、`version`、`server`、`restore file`。 |
| `-h, --help` |  |  | 显示当前命令的帮助文本并退出。 |

全局参数可放在子命令之前或之后；适用范围始终按最终子命令校验。

#### 数据库名与文件输入

数据库名必须通过位置参数显式提供，区分大小写，创建后不可修改。名称含空格时，请加引号。

参数表中的 `--input REQUEST_FILE|-` 表示读取完整 JSON 请求。请求可补充其他参数，但数据库名仍须显式传入，并与请求中的 `database`（创建时为 `name`）一致。

在含动作的命令中，`--input` 应放在动作之后，例如 `db notes execute --input request.json`；`db notes --input details.json` 仅用于详情查询。

未知字段、重复键、无效类型和请求与显式参数中的冲突值均会报错。一次调用最多只能有一个输入读取 stdin。

`--sql-file`、`--params-file`、`--metadata-file` 也支持 `-`。SQLite 导出目标和恢复源文件不支持 `-`；恢复也不接受 `--input`。

### 配置与诊断

#### version

查看程序、SQLite 和协议版本。不需要对象存储配置，无专用参数。

输入示例：

```sh
# 检查安装版本
ponydb version
```

输出示例（节选）：

```json
{
  "program": "0.1.1",
  "sqlite_version": "SQLITE_VERSION",
  "mcp_protocol_version": "2026-07-28"
}
```

`SQLITE_VERSION` 是版本字符串占位值。

#### config show

查看生效配置，始终隐藏凭证和认证 token，可离线执行。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--target SCOPE` |  | `cli` | `cli`、`server` 或 `all`。标记当前入口不使用的配置节。 |

输入示例：

```sh
# 查看 CLI 生效配置，凭证始终脱敏
ponydb config show --target cli
```

输出示例（节选）：

```json
{
  "config": {
    "cli": {
      "output": "json"
    },
    "storage": {
      "credentials": {
        "access_key_id": "[REDACTED]",
        "secret_access_key": "[REDACTED]"
      }
    }
  },
  "entry": "cli",
  "inactive": [
    "server",
    "server.auth",
    "mcp"
  ]
}
```

#### config validate

检查配置，并在配置的对象存储前缀下执行真实的读写、条件写入、列表和删除探测。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--target SCOPE` |  | `cli` | `cli`、`server` 或 `all`。`server`、`all` 还检查服务认证配置。 |

输入示例：

```sh
# 验证 CLI 配置和对象存储能力
ponydb config validate --target cli
```

输出示例：

```json
{
  "valid": true,
  "target": "cli",
  "storage_capabilities_verified": true
}
```

探测需要 List/Get/Put/Delete 权限，包含一个 2 MiB 临时对象，完成后会清理探测对象；中断可能留下临时对象。

#### help / --help

查看命令或子命令帮助。帮助使用原生文本格式，不受 JSON 输出设置影响。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `help [COMMAND...]` |  |  | 查看指定命令路径的帮助，例如 `help db execute`。 |
| `-h, --help` |  |  | 在命令后添加该选项；`--help` 显示完整帮助。 |

输入示例：

```sh
# 查看完整命令列表
ponydb --help

# 查看 SQL 子命令参数；也可使用 ponydb help db execute
ponydb db notes execute --help
```

顶层帮助输出示例（文本节选）：

```text
Usage: ponydb [OPTIONS] <COMMAND>

Commands:
  server     Serve HTTP and MCP.
  mcp        Serve MCP over stdin/stdout for agent clients.
  databases  Create and list databases.
  db         Inspect or operate on an explicitly named database.
  config     Show and validate the effective configuration.
  version    Show program, SQLite and protocol versions.
  help       Print this message or the help of the given subcommand(s)
```

### 数据库管理

#### databases create DATABASE_NAME

创建数据库。新库的数据版本为 `1`，默认快照保留期来自 `snapshots.retention_days`（7 天）。

数据库名长度为 1–64 个 Unicode BMP 字符，不允许 NUL、`/`、`\`、`.` 或尾随 ASCII 空格。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `DATABASE_NAME` | 是 |  | 唯一且不可修改的数据库名。 |
| `--description TEXT` |  | 空字符串 | 描述；最多 16 KiB UTF-8 字节。 |
| `--metadata-file METADATA_FILE\|-` |  | `null` | 自定义元数据 JSON；紧凑序列化后最多 64 KiB UTF-8 字节。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

先将以下内容保存为 `metadata.json`：

```json
{"project": "notes"}
```

输入示例：

```sh
# 创建数据库并附加描述和元数据
ponydb databases create notes --description "My notes" --metadata-file ./metadata.json
```

输出示例：

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "notes",
  "data_version": "1"
}
```

#### databases list

分页列出当前存储命名空间中的数据库。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--limit NUMBER` |  | 50 | 每页最多返回的数据库数；范围 1–1000。 |
| `--cursor CURSOR` |  | 从第一页开始 | 上次响应的 `next_cursor`。 |
| `--include-deleted` |  | `false`（不包含） | 包含已逻辑删除的数据库；无需传值。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 每页最多列出 20 个数据库，并包含已删除项
ponydb databases list --limit 20 --include-deleted
```

输出示例（节选）：

```json
{
  "outcome": "read_only",
  "databases": [
    {
      "name": "notes",
      "description": "My notes",
      "status": "active",
      "data_version": "1"
    }
  ],
  "next_cursor": null
}
```

`next_cursor` 为 `null` 表示没有下一页；非空时，将其原样传入 `--cursor`。每个数据库条目的字段与详情查询相同。

#### db DATABASE_NAME

省略动作时，读取数据库详情，包括数据版本、管理 ETag 和提交令牌。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `DATABASE_NAME` | 是 |  | 数据库名；不支持隐式的当前数据库。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 读取 notes 详情与最新 ETag
ponydb db notes
```

输出示例（节选）：

```json
{
  "name": "notes",
  "description": "My notes",
  "status": "active",
  "metadata_version": "1",
  "data_version": "1",
  "etag": "\"ETAG_FROM_DETAILS\"",
  "metadata": {
    "project": "notes"
  },
  "snapshot_retention_days": 7,
  "commit_token": "COMMIT_TOKEN"
}
```

#### db DATABASE_NAME update

更新描述、自定义元数据或快照保留期，不改变数据库名和 SQL 数据版本。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--description TEXT` |  | 保留原值 | 新描述，最多 16 KiB UTF-8 字节。 |
| `--metadata-file METADATA_FILE\|-` |  |  | 替换自定义元数据，紧凑序列化后最多 64 KiB UTF-8 字节。 |
| `--snapshot-retention-days DAYS` |  | 保留原值 | 快照保留天数，范围 0–36500。 |
| `--if-match ETAG` |  | 不要求匹配先前读取的 ETag | 仅在管理 ETag 仍匹配时更新。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 将 ETag 占位值替换为最新详情返回的值，并保留嵌入的双引号
ponydb db notes update --if-match '"ETAG_FROM_DETAILS"' \
  --description "Personal notes" --snapshot-retention-days 14
```

输出示例：

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "notes",
  "metadata_version": "2"
}
```

保留期调整不会立即清理对象，历史清理由显式 GC 执行。ETag 不匹配时返回 `METADATA_PRECONDITION_FAILED`，退出码为 `4`。

#### db DATABASE_NAME delete

逻辑删除数据库，保留墓碑和保留期内的历史。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--if-match ETAG` |  |  | 仅在管理 ETag 仍匹配时删除，值取自最新详情。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 重新读取最新 ETag，再逻辑删除数据库
ponydb db notes delete --if-match '"ETAG_FROM_DETAILS"'
```

输出示例：

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "notes",
  "status": "deleted"
}
```

### SQL 与 Schema

#### db DATABASE_NAME execute

执行单条 SQL，或通过完整 JSON 请求原子执行多条语句。

支持的 SQL 功能、暂不支持的语法及使用限制见 [SQLite 兼容性](sqlite.zh-CN.md)。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `SQL` |  |  | 单条 SQL 位置参数；与 `--sql-file` 互斥。以 `-` 开头的 SQL 可放在 `--` 后。 |
| `--sql-file SQL_FILE\|-` |  |  | 读取一条 UTF-8 SQL，不按分号拆分多条语句。 |
| `--param PARAM_JSON` |  |  | 类型化参数，可重复；与 `--params-file` 互斥。 |
| `--params-file PARAMS_FILE\|-` |  |  | 读取类型化参数的 JSON 数组，按顺序绑定。 |
| `--consistency MODE` |  | `strong` | `strong`、`snapshot` 或 `at_least`；后两者仅用于查询。 |
| `--snapshot-id SNAPSHOT_ID` | 使用 `snapshot` 一致性时 |  | 快照 ID，取自快照列表。 |
| `--commit-token TOKEN` | 使用 `at_least` 时 |  | 提交令牌，读取版本不得早于令牌中的版本。 |
| `--max-rows NUMBER` |  | `query.max_rows`（内置值 1000） | 仅用于查询，须为正数且不超过 `query.max_rows`。超限报错，不会静默截断。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

**建表与写入**：在新建的 `notes` 中依次执行以下命令。

输入示例：

```sh
# 创建笔记表，插入时可省略整数主键 id
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT, content TEXT)"
```

输出示例：

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "2",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {
      "columns": [],
      "rows": [],
      "rows_affected": "0"
    }
  ]
}
```

输入示例：

```sh
# 用类型化参数绑定标题和正文
ponydb db notes execute "INSERT INTO notes(title, content) VALUES (?, ?)" \
  --param '{"type":"text","value":"First note"}' \
  --param '{"type":"text","value":"Hello, ponydb!"}'
```

也可以直接在 SQL 中写入完整数据，与上面的参数化写法任选其一：

```sh
ponydb db notes execute "INSERT INTO notes(title, content) VALUES ('First note', 'Hello, ponydb!')"
```

输出示例：

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "3",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {
      "columns": [],
      "rows": [],
      "rows_affected": "1"
    }
  ]
}
```

参数和结果单元格使用以下类型；整数以字符串表示，避免丢失 64 位精度。

| 类型 | JSON 示例 |
| --- | --- |
| Null | `{"type":"null"}` |
| Integer | `{"type":"integer","value":"42"}` |
| Real | `{"type":"real","value":3.14}` |
| Text | `{"type":"text","value":"A note"}` |
| Blob | `{"type":"blob","value":"SGVsbG8="}`（base64） |

输入示例：

```sh
# 用 SQL LIMIT 控制结果大小，并设置返回行数上限
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id LIMIT 10" --max-rows 10
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

`results` 按语句顺序返回，包含 `columns`、类型化的 `rows` 和字符串形式的 `rows_affected`。写入还返回 `committed` 和 `commit_token`。

**文件与 stdin**：将 `SELECT count(*) FROM notes` 保存为 `query.sql`，任选以下一种输入方式。

输入示例：

```sh
# 从文件读取单条 SQL
ponydb db notes execute --sql-file ./query.sql

# 从 stdin 读取同一条 SQL
printf 'SELECT count(*) FROM notes' | ponydb db notes execute --sql-file -
```

两种方式的输出相同：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["count(*)"],
      "rows": [
        [{"type": "integer", "value": "1"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**从文件绑定参数**：将以下内容保存为 `params.json`。

```json
[{"type": "integer", "value": "1"}]
```

输入示例：

```sh
# 从数组文件绑定查询参数
ponydb db notes execute "SELECT id, title, content FROM notes WHERE id = ?" --params-file ./params.json
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**事务输入**：将以下内容保存为 `statements.json`。

```json
{
  "database": "notes",
  "statements": [
    {
      "sql": "INSERT INTO notes(title, content) VALUES (?, ?)",
      "params": [
        {"type": "text", "value": "Second note"},
        {"type": "text", "value": "Another note."}
      ]
    },
    {"sql": "SELECT id, title, content FROM notes ORDER BY id"}
  ]
}
```

输入示例：

```sh
# 在一次请求中插入并查询，任一失败则整体回滚
ponydb db notes execute --input ./statements.json
```

输出示例：

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "4",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"},
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}],
        [{"type": "integer", "value": "2"}, {"type": "text", "value": "Second note"}, {"type": "text", "value": "Another note."}]
      ],
      "rows_affected": "0"
    }
  ]
}
```

语句按顺序执行，无需添加 `BEGIN` 或 `COMMIT`。`sql` / `params` 与非空 `statements` 数组二选一。

语句数组沿用写事务语义，即使只包含查询也需要写权限；不能使用 `--max-rows`、快照或提交令牌等只读选项。

输入示例：

```sh
# 将令牌替换为先前写入返回的 commit_token
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id" \
  --consistency at_least --commit-token COMMIT_TOKEN
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": [
        "id",
        "title",
        "content"
      ],
      "rows": [
        [
          {
            "type": "integer",
            "value": "1"
          },
          {
            "type": "text",
            "value": "First note"
          },
          {
            "type": "text",
            "value": "Hello, ponydb!"
          }
        ],
        [
          {
            "type": "integer",
            "value": "2"
          },
          {
            "type": "text",
            "value": "Second note"
          },
          {
            "type": "text",
            "value": "Another note."
          }
        ]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**修改与删除**：接续上面的事务示例，修改再删除 `id = 2` 的笔记；每次写入都产生新版本。

输入示例：

```sh
# 修改指定笔记的正文
ponydb db notes execute "UPDATE notes SET content = ? WHERE id = ?" \
  --param '{"type":"text","value":"Updated note."}' \
  --param '{"type":"integer","value":"2"}'
```

输出示例：

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "5",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"}
  ]
}
```

输入示例：

```sh
# 删除指定笔记
ponydb db notes execute "DELETE FROM notes WHERE id = ?" \
  --param '{"type":"integer","value":"2"}'
```

输出示例：

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "6",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"}
  ]
}
```

两次写入的 `rows_affected` 均为 `"1"`，表示各影响一行。删除后再次查询该笔记：

```sh
ponydb db notes execute "SELECT id, title, content FROM notes WHERE id = ?" \
  --param '{"type":"integer","value":"2"}'
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "6",
  "results": [
    {"columns": ["id", "title", "content"], "rows": [], "rows_affected": "0"}
  ],
  "truncated": false
}
```

#### db DATABASE_NAME schema

查看用户表、索引、视图和触发器的定义。省略筛选项时返回全部非内部对象。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--table TABLE_NAME` |  |  | 精确匹配存在的非内部表名，区分大小写；包含其索引和触发器，不解析视图依赖。 |
| `--type TYPE` |  |  | `table`、`index`、`view` 或 `trigger`；与 `--table` 取交集。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 只查看 notes 表的建表语句
ponydb db notes schema --table notes --type table
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": ["type", "name", "tbl_name", "sql"],
      "rows": [
        [
          {"type": "text", "value": "table"},
          {"type": "text", "value": "notes"},
          {"type": "text", "value": "notes"},
          {"type": "text", "value": "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT, content TEXT)"}
        ]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

`columns` 列出列名，`rows` 中每行的单元格按相同顺序对应：

| 列名 | 含义 | 本例的值 |
| --- | --- | --- |
| `type` | 对象类型：表、索引、视图或触发器。 | `table`（表） |
| `name` | 对象名称。 | `notes` |
| `tbl_name` | 对象所属的表或视图名称；表和视图自身的此值与 `name` 相同。 | `notes` |
| `sql` | 创建该对象的 SQL 定义。 | `CREATE TABLE notes(...)` |

指定的表不存在时返回 `TABLE_NOT_FOUND`。表存在但没有匹配类型时返回空结果；Schema 结果也受查询预算限制。

### 快照

#### db DATABASE_NAME snapshot list

分页列出保留快照，用于历史查询和导出。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--limit NUMBER` |  | 50 | 每页快照数，范围 1–1000。 |
| `--cursor CURSOR` |  | 从第一页开始 | 上次响应的 `next_cursor`。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 最多列出 20 个保留快照
ponydb db notes snapshot list --limit 20
```

输出示例（节选）：

```json
{
  "outcome": "read_only",
  "snapshots": [
    {
      "snapshot_id": "SNAPSHOT_ID",
      "data_version": "3"
    }
  ],
  "next_cursor": "CURSOR",
  "retention_days": 14,
  "time_authority": "system_clock",
  "history_gaps": []
}
```

快照条目仅摘录一个示例及部分字段。将后续 `SNAPSHOT_ID` 替换为实际返回值。

需要继续列举时，将 `next_cursor` 原样传给 `--cursor`。响应中的 `retention_days` 是该数据库当前的快照保留期。

#### db DATABASE_NAME snapshot get SNAPSHOT_ID

读取保留快照的描述信息和 Manifest 引用。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `SNAPSHOT_ID` | 是 |  | 快照 ID；也可通过 `--input` 中的 `snapshot_id` 提供。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 读取所选快照的描述信息
ponydb db notes snapshot get SNAPSHOT_ID
```

输出示例（节选）：

```json
{
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "3",
  "retired_at_upper_ms": null,
  "observed_at_ms": 1794182400000
}
```

以上节选省略 `manifest`。

#### db DATABASE_NAME execute --consistency snapshot --snapshot-id SNAPSHOT_ID

要查询快照中的数据，使用 `execute` 的只读参数：

输入示例：

```sh
# 固定读取指定快照中的数据
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id" \
  --consistency snapshot --snapshot-id SNAPSHOT_ID
```

输出示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

### 备份与恢复

#### db DATABASE_NAME export SQLITE_FILE

将当前数据库版本导出为独立 SQLite 文件。导出期间的后续写入不会改变本次导出内容。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `SQLITE_FILE` | 是 |  | 目标文件；也可由 `--input` 的 `file` 提供。不支持 `-`，父目录必须存在，已有文件不会被覆盖。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 导出当前版本到本地文件
ponydb db notes export ./notes-backup.sqlite
```

输出示例：

```json
{
  "outcome": "read_only",
  "file": "./notes-backup.sqlite",
  "snapshot_id": "CURRENT_SNAPSHOT_ID",
  "data_version": "4",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST"
}
```

#### db DATABASE_NAME snapshot export SNAPSHOT_ID SQLITE_FILE

将指定保留快照导出为独立 SQLite 文件。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `SNAPSHOT_ID` | 是 |  | 保留快照 ID；也可由 `--input` 的 `snapshot_id` 提供。 |
| `SQLITE_FILE` | 是 |  | 目标文件；也可由 `--input` 的 `file` 提供。与当前版本导出遵循相同的路径和不覆盖规则。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 导出所选历史快照
ponydb db notes snapshot export SNAPSHOT_ID ./notes-history.sqlite
```

输出示例：

```json
{
  "outcome": "read_only",
  "file": "./notes-history.sqlite",
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "3",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST"
}
```

两个导出命令的 JSON 请求分别为：

```json
{"database": "notes", "file": "./notes-backup.sqlite"}
```

```json
{"database": "notes", "snapshot_id": "SNAPSHOT_ID", "file": "./notes-history.sqlite"}
```

`total_bytes` 是字符串形式的字节长度。`sha256` 为 64 个十六进制字符，示例中的 `SHA256_DIGEST` 是占位值。

#### db DATABASE_NAME restore file SQLITE_FILE

从本地 SQLite 备份恢复到对象存储，仅支持 CLI。目标数据库必须不存在或已删除，不能覆盖活动数据库。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `DATABASE_NAME` | 是 |  | 恢复目标数据库名；不要预先创建该库。 |
| `SQLITE_FILE` | 是 |  | 本地位置参数；输入须稳定、完整且使用 DELETE journal 模式。不支持 stdin 或 DTO 替代。 |

输入示例：

```sh
# 从备份恢复到不存在或已删除的 restored_notes
ponydb db restored_notes restore file ./notes-backup.sqlite
```

stdout 最终输出示例：

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "restored_notes",
  "data_version": "1",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST",
  "page_size": 4096
}
```

恢复需要本地临时磁盘空间，无总执行截止时间，不接受 `--deadline-ms`，也不支持断点续传。

阶段和进度写入 stderr；请等待 stdout 的最终成功结果后再视为完成。

### 在线维护

#### db DATABASE_NAME compact

在线合并存储段，保持逻辑数据版本不变。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--budget-ms MILLISECONDS` |  | 10000 毫秒 | 本次工作预算，范围 1–60000。 |
| `--max-segments NUMBER` |  | 32 | 最多处理的存储段数，范围 1–32。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 限制本次压缩的时间和处理数量
ponydb db notes compact --budget-ms 5000 --max-segments 16
```

输出示例：

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "segments_processed": 1,
  "data_version": "4"
}
```

实际处理数量取决于数据库状态。省略选项时使用默认值；DTO 与显式参数合并后，再补齐未提供的默认字段。

#### db DATABASE_NAME gc

在线清理过期历史和不可达对象。预算耗尽后可再次调用。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--budget-ms MILLISECONDS` |  | `maintenance.gc_default_budget_ms`（60000） | 本次工作预算；须为正数且不超过 `requests.max_timeout_ms`。 |
| `--max-objects NUMBER` |  | `maintenance.gc_max_objects_per_call`（1000） | 最多尝试删除的对象数；部署上限也由 `maintenance.gc_max_objects_per_call` 配置，须为正数。 |
| `--input REQUEST_FILE\|-` |  |  | 从 JSON 文件或 stdin 读取完整请求；遵循通用输入规则。 |

输入示例：

```sh
# 使用默认时间预算和对象数量上限
ponydb db notes gc
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

输入示例：

```sh
# 分配 300 秒工作预算，最多尝试删除 1000 个对象，请求超时设为 330 秒
ponydb db notes gc --budget-ms 300000 --max-objects 1000 --deadline-ms 330000
```

预算或对象数量上限耗尽时的可能输出（退出码 `6`）：

```json
{
  "outcome": "partial",
  "complete": false,
  "database_available": true,
  "phase": "sweeping",
  "deleted_objects": 1000
}
```

压缩或 GC 返回 `outcome: "partial"`、退出码 `6` 时，再次执行相同操作继续。GC 没有暂停、恢复或断点续传子命令。

历史快照在保留期结束后通过显式 GC 清理。请保持系统时钟准确，并导出需要长期保存的历史。

### 服务与 AI Agent

#### server

启动可选 HTTP/MCP 服务。需要对象存储配置及至少一个有效的服务认证身份。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--listen ADDRESS` |  | `server.listen`（内置值 `127.0.0.1:8080`） | 覆盖服务监听地址。 |

输入示例：

```sh
# 使用已配置的认证身份启动服务
ponydb server --listen 127.0.0.1:8080
```

命令持续运行，stdout 不返回一次性业务结果；诊断写入 stderr。它不接受 `--output` 或 `--deadline-ms`。

可在另一个终端检查健康接口：

```sh
# 检查监听状态
curl --silent --show-error http://127.0.0.1:8080/healthz
```

健康接口的 JSON 输出示例（由 curl 接收）：

```json
{
  "status": "ok"
}
```

认证、部署和 Docker 配置见 [HTTP 指南](http.zh-CN.md)。

#### mcp

通过 stdin/stdout 提供 MCP，由 AI Agent 客户端启动和管理；直接使用对象存储凭证。

| 参数 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--config CONFIG_FILE` |  | `PONYDB_CONFIG` 或平台默认路径 | 通用参数；建议使用绝对路径，便于客户端启动。 |
| `--deadline-ms MILLISECONDS` |  | `requests.default_timeout_ms`（90000） | 通用参数；单个 MCP 请求的截止时间，不是进程总运行时间。 |

输入示例：

```sh
# 启动 stdio MCP；请将配置路径替换为实际绝对路径
ponydb --config /absolute/path/ponydb.toml mcp --deadline-ms 90000
```

stdout 只包含 MCP JSON-RPC 协议消息；没有一次性 CLI 结果，不接受 `--output`。以下为一条协议交互示例，实际传输时每条 JSON 消息占一行。

客户端发送到 stdin 的请求：

```json
{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
```

stdout 返回的协议响应：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "resultType": "complete"
  }
}
```

客户端连接配置、支持的协议版本和工具调用方式见 [AI Agent 接入](agent-clients.zh-CN.md)。

## 输出与脚本集成

成功结果写入 stdout，诊断日志、错误和恢复进度写入 stderr。帮助始终为文本；Server 和 MCP 的输出行为见各自命令。

脚本可直接保存默认 JSON 结果：

输入示例：

```sh
# 将 JSON 结果写入文件，stderr 中的诊断信息不受影响
ponydb db notes execute "SELECT 1" > result.json
```

`result.json` 内容示例：

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": ["1"],
      "rows": [
        [{"type": "integer", "value": "1"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

脚本应检查退出码。ETag 不匹配时的 stderr JSON 示例：

输出示例：

```json
{
  "request_id": "REQUEST_ID",
  "code": "METADATA_PRECONDITION_FAILED",
  "message": "Management ETag does not match",
  "retryable": false,
  "outcome": "not_committed"
}
```

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
