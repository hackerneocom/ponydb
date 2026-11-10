[English](sqlite.md) | **中文**

# SQLite 兼容性

ponydb 当前内置 SQLite 3.54.1，不使用系统安装的版本。可运行 `ponydb version` 查看引擎版本。CLI、HTTP API 与 MCP 使用相同的 SQL 行为。

## 可用功能

| 功能 | 用法 |
| --- | --- |
| 关系型 SQL | 普通表、索引、视图、触发器、连接、CTE 与窗口函数 |
| 约束 | 主键、唯一约束、检查约束与外键；已启用外键检查 |
| 事务 | 单条语句或原子 `statements` 数组；语句失败时回滚整个请求 |
| JSON / JSONB | 构建、提取、更新、`->` / `->>`、`json_each` / `json_tree` 及 `jsonb_` 变体 |
| 数学函数 | `sqrt`、`pow`、`log`、`ln`、`sin` 等内置 SQLite 数学函数 |
| 版本化读取 | 当前强一致读取、保留快照读取、不早于指定提交令牌的读取 |
| 可迁移备份 | 导出独立 SQLite 文件；通过 CLI 恢复受支持的 SQLite 备份 |

JSON 与数学函数均已内置，无需安装扩展。例如，对已有的 `notes` 数据库执行：

```sh
ponydb db notes execute "SELECT json_extract(jsonb_object('count', 2), '$.count')"
ponydb db notes execute "SELECT key, value FROM json_each('[1,2,3]')"
ponydb db notes execute "SELECT sqrt(9), pow(2, 3), log(100)"
```

JSONB 结果使用协议的 base64 `blob` 表示；需要文本时，用 `json(...)` 包裹 JSONB 表达式。`sqrt(-1)` 等定义域错误返回 NULL；`exp(1000)` 等非有限数值结果会被拒绝。

## 暂不支持

| 功能或语法 | 替代方式 |
| --- | --- |
| `ATTACH`、`DETACH`、跨库事务 | 每个请求显式指定一个数据库 |
| 用户 `BEGIN`、`COMMIT`、`ROLLBACK`、`SAVEPOINT`、`RELEASE` | 通过 `execute --input` 或 HTTP/MCP 请求提交 `statements` 数组 |
| 一个 SQL 字符串或文件中包含多条语句 | 使用上述语句数组，每项包含一条语句 |
| 用户 `PRAGMA`，包括修改日志模式或页大小 | 使用支持的 ponydb 配置和 Schema 操作 |
| API 中的 WAL 模式与 `VACUUM` | 通过 ponydb `compact`、`gc` 维护存储 |
| `CREATE VIRTUAL TABLE`，包括 FTS3/4/5 与 R-tree 表 | 使用普通表和索引；这些虚表扩展未向用户开放 |
| `load_extension`，包括自行安装向量扩展 | 使用内置 SQL 功能，或导出数据后交给外部工具处理 |
| 跨请求保留连接状态 | 将相关操作放入同一个原子请求；临时表仅在请求内有效 |

导出数据库不会使 ponydb 支持额外扩展。从文件恢复时会验证 Schema，依赖不受支持 Schema 功能的备份不能原样导入。

## 限制与并发

默认单次请求限制包括 100 条语句、16 MiB SQLite 脏页、1000 行结果和 1 MiB 序列化结果。结果超限会报错，不会截断；请使用 SQL `LIMIT` 与分页，并通过[配置模板](https://github.com/hackerneocom/ponydb/blob/main/examples/ponydb.toml)调整受支持的设置。

多个客户端可以读写共享数据库。提交使用乐观并发控制，冲突时返回 `TX_CONFLICT`。重复写入会再次执行，结果未知时应先检查数据再决定是否重试。不提供离线写入、自动合并冲突，也不承诺专用数据库服务器的吞吐能力。

备份导出限制与恢复前提见 [CLI 指南](cli.zh-CN.md#备份与恢复)。
