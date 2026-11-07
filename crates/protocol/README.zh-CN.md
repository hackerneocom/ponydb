[English](README.md) | **中文**

# protocol

11 个数据库操作的、与传输无关的输入、错误和 MCP 工具 Schema。HTTP、MCP 和 CLI 解码成这些类型，并把一个 `Operation` 传给 `database-service`。整数 SQL 值不以 JSON 数字传输。

## 用途

所有入口共用 DTO 校验。64 位整数使用十进制字符串，二进制使用 base64，未知字段被拒绝。

## 工作原理

`SqlValue` 是带标签的联合：`null`、`integer`（字符串）、`real`（仅有限的 `f64`）、`text` 或 `blob`（base64）。`to_sql` 和 `from_sql` 与 rusqlite 互相转换。非有限实数、不是 `i64` 的整数，或错误的 base64，都会使转换失败。

每个操作都有一个以 `deny_unknown_fields` 派生的结构体：`Create`、`List`、`Update`、`Delete`、`Execute`、`Compact`、`Gc`、快照输入，以及共用的 `Database` ID 包装。`Operation` 用 `operation` 和 `input` 给它们加标签，名称是 snake_case（`database_create`、`sql_execute` 等）。`Operation::name` 是业务操作名和 MCP 工具名。

`Update` 和 `Delete` 的 `if_match` 可选：提供时由服务校验管理 ETag，省略时基于当前数据库状态执行。生成的 MCP Schema 也将该字段标记为可选。

`Create::validate` 和 `Update::validate` 将传入的 `metadata` 限制为 `MAX_DATABASE_METADATA_BYTES`（紧凑 JSON 序列化后的 65,536 个 UTF-8 字节，包含键名和转义），`description` 限制为 `MAX_DATABASE_DESCRIPTION_BYTES`（16,384 个 UTF-8 字节）。紧凑 JSON 大小不计算输入排版空白。CLI 和服务入口在访问 S3 前校验，超限返回 `INVALID_REQUEST`；MCP 输入 Schema 描述字节限制。这是请求限制：已有 HEAD 仍可读取，也可通过 update 缩小元数据。

`Execute` 把 `consistency` 默认为 `strong`。每次写入独立执行。读取可以设置 `snapshot_id`、`commit_token` 和 `max_rows`。哪些组合合法由服务决定，不由这个 crate 决定。

`Principal` 是一个 ID、一份数据库 ID 列表和一份作用域列表。`allows` 匹配数据库和作用域，任一列表上的 `*` 都是通配符。`Principal::direct` 是 CLI 为已经持有 S3 凭证的进程使用的主体。

`ApiError` 是 `{ request_id, code, message, retryable, outcome }`。提交可能已经落地时，`outcome` 为 `unknown`，否则为 `not_committed`。入口把 `code` 映射为 HTTP 状态或进程退出码。

`tools` 构建 MCP 工具列表。每个工具的 `inputSchema` 来自 schemars。`outputSchema` 是手写的 JSON Schema，描述该工具的 `{ ok, result }` / `{ ok, error }` 信封，包括哪些结果字段是十进制字符串。注解标出只读工具，只读工具标记为幂等，变更工具标记为非幂等。

`strict_json` 解析业务载荷。前导 UTF-8 BOM 会被去掉。值必须是 UTF-8 JSON；任意深度的重复对象键被拒绝，非有限数字被拒绝，一个值之后的尾随字节也被拒绝。服务、CLI 文件加载器和 MCP 都使用这个解析器，因此重复键不能在不同传输之间改变含义。

数据库唯一标识为不可修改的 `name`。创建使用 `databases create notes` 或
`{"name":"notes"}`；创建结果和 get/list 的数据库 meta 使用调用方提供的 `name` 标识数据库。其他操作通过 `db notes` /
`database: "notes"` 选择同名数据库。CLI 创建必须提供位置参数 `DATABASE_NAME`，DTO 中的 `name` 必须与之相同。更新不接受独立显示名称。

名称遵循 [MySQL 数据库标识符规则](https://dev.mysql.com/doc/refman/8.0/en/identifiers.html)
及 [64 字符限制](https://dev.mysql.com/doc/refman/8.0/en/identifier-length.html)：
1–64 个 Unicode BMP 字符，禁止 NUL、`/`、`\`、`.` 和末尾 ASCII 空格。
传入原始名称，无需 SQL 反引号；与 MySQL 引用标识符一样，支持纯数字、保留字、
中文、名称内部空格和连字符。名称区分大小写，原样保存，不自动去空格或转换大小写。
持久化 HEAD 使用同一 `name` 标识数据库。

`Execute` 在反序列化时校验 `sql`/`params` 与非空 `statements` 数组互斥，验证类型化参数，并拒绝批量请求的查询专用选项。数组每项独立绑定参数；SQL 不按分号拆分。

`schema_get` 使用专用 DTO：`database` 必填，`table` 与 `type` 可选。`table` 非空，按 `sqlite_schema.tbl_name` 区分大小写精确匹配，包含关联索引和触发器，不解析视图依赖；`type` 为 `table`、`index`、`view`、`trigger` 之一。条件取交集。指定的表必须是存在的非内部表，否则返回 `TABLE_NOT_FOUND`（HTTP 404、CLI 退出码 1）。表存在但没有匹配类型，或仅按类型筛选无匹配时，返回空结果。存在性与定义在同一次查询、同一快照中读取。HTTP 使用同名查询参数，CLI 使用 `--table` 和 `--type`，MCP Schema 同步暴露这些字段。查询使用绑定参数，保留内部对象排除、排序和结果预算。
