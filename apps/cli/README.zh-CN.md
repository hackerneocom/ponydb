[English](README.md) | **中文**

# ponydb

面向 Windows、Linux 和 macOS 的原生 CLI。它加载与 `ponydb server` 相同的配置，从本进程打开 S3，并直接调用 `database-service`。不需要服务进程。

[共享配置示例](../../examples/ponydb.toml) 仅列出用户可调整且实际生效的配置，只有四项必需的 S3 配置未注释。覆盖可选设置时，同时取消节标题和对应设置的注释。token、认证身份和目录的示例值与内置默认值分开标注。

## 用途

所有入口调用共同业务层，共有 11 个操作。每次写入独立执行，不保存或重放原响应。结果未知时检查业务数据后再决定是否重试。

主体是 `Principal::direct`，即本进程中 S3 凭证的持有者。网络授权是桶策略，不是 `server.auth`。

## 工作原理

数据库名称只通过显式位置参数提供。通用选项为 `--config`、`--output` 和 `--deadline-ms`；后两者按最终命令校验：server/MCP 不接受输出格式，config/version/server/restore 不接受截止时间。截止时间沿用 requests 配置。

创建与列表使用 `databases`，详情与单库操作使用 `db DATABASE_NAME`，另有 `config` 和 `version`。JSON 业务命令接受常用字段标志，并用 `--input` 接受完整 DTO；文件恢复必须使用文件位置参数。`--sql-file`、`--params-file` 和 `--metadata-file` 读取 UTF-8，可以带 BOM。最多一个参数可以是 `-` 以读取标准输入。文件用 `protocol::strict_json` 解析。重复键、未知字段，以及与文件不一致的标志，都会在任何 S3 调用之前失败。

创建和更新时，`metadata` 最多为**紧凑 JSON 序列化后的 64 KiB（65,536 个 UTF-8 字节）**，包含键名、标点和转义，不计算输入排版空白；`description` 最多为 **16 KiB 的 UTF-8 字节**。`--metadata-file METADATA_FILE|-` 和 `--input REQUEST_FILE|-` 使用相同限制。超限在访问 S3 前返回 `INVALID_REQUEST`（退出码 2）。这些限制为 HEAD 内部字段预留空间，完整 HEAD 仍受 1 MiB 上限约束；读取持久化 HEAD 时使用存储格式约束，update 可缩小超出请求上限的元数据。HTTP 和 MCP 使用相同校验。

`config validate` 先检查本地配置（`--target server|all` 还检查 `server.auth`），再使用配置中的凭证、桶和前缀执行 S3 探测：条件创建（`If-None-Match: *`）、匹配/过期令牌 CAS（`If-Match`）、四轮并发创建/CAS、立即读取与完整性校验、Range GET、最大 2 MiB 的探测对象 PUT/GET，以及带或不带 `/` 分隔符的分页 `ListObjectsV2`。探测使用 `storage.prefix` 下随机的 `_ponydb-validation/<uuid>/`，需要 List/Get/Put/Delete 权限，不修改数据库对象。只删除本轮尝试写入的键，并通过 GET 和 LIST 确认清理。成功返回 `storage_capabilities_verified: true`；失败返回 `INVALID_CONFIG`（退出码 2），指出失败项目及脱敏后的服务端原因。清理失败或写入结果不确定时，还会提示需检查的相对探测前缀。探测预算使用 `requests.default_timeout_ms`，随后最多用 30 秒清理；进程被强杀或超时后 PUT 迟到可能留下探测对象。有限探测不验证服务端的最大对象大小，也不能证明所有故障和未来负载下的行为。`config show` 仍为离线操作。`version` 打印程序版本、捆绑 SQLite 的版本和源标识、MCP 协议版本、布局版本 1，以及操作数量。


成功的 JSON 以两空格缩进、分行格式打印到标准输出。`--output table` 把行渲染成文本表。压缩或 GC 返回 `outcome: "partial"` 时仍然打印正文，并以 6 退出。压缩使用 `complete` 表示完成情况，不返回 `resume_required`；未完成时再次调用 compact。错误以相同的美化 JSON 格式写到标准错误。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 1 | 其他错误 |
| 2 | 配置或请求无效 |
| 3 | `UNAUTHENTICATED` 或 `FORBIDDEN` |
| 4 | `TX_CONFLICT` 或 `METADATA_PRECONDITION_FAILED` |
| 5 | `OUTCOME_UNKNOWN`：根据业务数据判断结果，不自动重试 |
| 6 | 在本次调用的预算内停止；再次运行该操作以继续 |

## 参数约定

捆绑 SQLite 已启用数学函数，无需额外配置。`sqrt(-1)` 返回 NULL；`exp(1000)` 等非有限结果返回 `SQL_ERROR`（退出码 1）。

CLI/S3 端到端测试覆盖 JSON / JSONB 的持久化、更新、遍历及非法 JSON 导致的事务回滚。本地 RustFS 环境的运行方式见[测试说明](../../tests/README.md)。原始 JSONB 结果使用协议的 `blob` 类型（base64）；需要 JSON 文本时使用 `json(...)` 转换。

端到端测试按命令拆分在 `tests/cli_e2e/`，覆盖全部 CLI 命令，包含 HTTP server 和 stdio MCP 的启动、请求及退出。在仓库根目录运行 `python3 tests/cli_e2e.py --list` 查看测试组；设置 `ACCESS_KEY` 和 `SECRET_KEY` 后，可通过 `python3 tests/cli_e2e.py --suite execute --suite schema` 选择测试组；`python3 tests/cli_e2e.py --offline` 执行无需 S3 的检查。各测试方法使用独立夹具，断言失败也清理本方法的 S3 前缀，并通过实际帮助文本核对命令覆盖。

`--suite server --suite mcp` 专项检查 HTTP 认证/ETag、SQL 回滚和 NDJSON、快照下载、server 重启、HTTP MCP 请求头，以及 stdio MCP 全部业务工具、旧版握手、协议错误恢复、取消、请求截止时间和 EOF 退出。

[CI 端到端任务](../../.github/workflows/ci.yml) 在 push 和 pull request 时构建 CLI，自动初始化 RustFS 容器并执行所有测试组。

操作对象使用位置参数，行为配置使用选项。数据库名统一显示为 `DATABASE_NAME`，包括创建和全部单库操作。文件占位符按用途区分为 `CONFIG_FILE`、`REQUEST_FILE`、`SQL_FILE`、`PARAMS_FILE`、`METADATA_FILE`、`SQLITE_FILE`；快照使用 `SNAPSHOT_ID`。每个命令只接受自身支持的参数：

```sh
ponydb databases create notes --description "My notes"
ponydb db notes
ponydb db notes update --if-match ETAG --description "Updated notes"
ponydb db notes update --description "Updated notes"
ponydb db notes delete --if-match ETAG
ponydb db notes compact
ponydb db notes compact --budget-ms 5000 --max-segments 16
ponydb db notes gc --budget-ms 60000 --max-objects 1000
ponydb db notes schema
ponydb db notes schema --table notes --type index
ponydb db notes execute "SELECT 1" --max-rows 10
ponydb db notes execute "SELECT sqrt(9), pow(2, 3), log(100)"
ponydb db notes execute "SELECT json_extract(jsonb_object('count', 2), '$.count')"
ponydb db notes snapshot get SNAPSHOT_ID
ponydb db notes restore file backup.sqlite
```

`schema` 支持 `--table TABLE_NAME` 和 `--type table|index|view|trigger`，也可通过 `--input` 的 `table`、`type` 字段提供。表名精确匹配 `sqlite_schema.tbl_name`，区分大小写、不支持通配符，包含表及其关联索引和触发器；不解析视图依赖。两个条件取交集。指定的表必须是存在的非内部表，否则返回 `TABLE_NOT_FOUND`（HTTP 404、CLI 退出码 1）。表存在但没有匹配类型，或仅按类型筛选无匹配时，返回空结果。存在性与定义在同一次查询、同一快照中读取。省略筛选项返回全部非内部对象，结果仍受查询预算限制。

压缩的 `--budget-ms` 可选（默认 10000 毫秒，范围 1–60000），`--max-segments` 可选（默认 32，范围 1–32）。`--input` DTO 与显式选项合并后，逐项为缺失字段补入默认值；同一字段值冲突时报错。GC 的 `--budget-ms` 和 `--max-objects` 均可省略：预算默认 60000 毫秒，由 `maintenance.gc_default_budget_ms` 配置；删除尝试数默认值和部署上限由 `maintenance.gc_max_objects_per_call` 配置，默认 1000。显式预算可大于默认值，但不能超过 `requests.max_timeout_ms`（系统最高 1800000 毫秒）。这些维护字段也可由 DTO 提供。

CLI 更新和删除可选地提供 `--if-match ETAG` 或 DTO 的 `if_match`，值取自 `db DATABASE_NAME` 返回的完整对象 etag（保留值内双引号）。省略时基于当前 HEAD 执行；发布仍通过条件写拒绝并发变更。DTO 仍可补全其余必需字段，但数据库名必须显式提供；文件恢复必须提供 SQLITE_FILE。SQL 位置参数与 `--sql-file FILE|-` 互斥，多语句使用 `--input`。

数据库名称只通过显式位置参数提供。通用选项为 `--config`、`--output` 和 `--deadline-ms`；后两者按最终命令校验：server/MCP 不接受输出格式，config/version/server/restore 不接受截止时间。截止时间沿用 requests 配置。


多语句通过 `db DATABASE_NAME execute --input statements.json` 原子执行。请求中的 `sql`/`params` 与 `statements` 二选一；数组不能为空，每项使用独立的类型化参数。无需 `BEGIN`/`COMMIT`，任一语句失败则全部回滚，结果按语句顺序返回。数组沿用写事务语义，需要写权限并发布一个版本，即使只包含查询；不接受 `max_rows`、快照或提交令牌等查询选项。`--sql-file` 仍只接收单条 SQL，不按分号拆分。

```json
{
  "database": "personal_notes",
  "statements": [
    {"sql": "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"},
    {"sql": "INSERT INTO notes VALUES(?, ?)", "params": [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}]},
    {"sql": "SELECT * FROM notes"}
  ]
}
```

## 通过 stdio MCP 接入 agent

运行 `ponydb --config /absolute/path/ponydb.toml mcp`。由 agent 客户端启动并管理这个常驻进程，无需 HTTP 服务或 bearer 令牌。它沿用普通 CLI 的 S3 凭证和直接访问身份，暴露全部 11 个工具。`mcp.enabled` 只控制 HTTP 接口。

支持 `mcpServers` 配置的客户端可使用：

```json
{
  "mcpServers": {
    "ponydb": {
      "command": "/absolute/path/ponydb",
      "args": ["--config", "/absolute/path/ponydb.toml", "mcp"]
    }
  }
}
```

各客户端的配置格式可能不同，进程命令和参数如上。也可通过已有的 `PONYDB_STORAGE__CREDENTIALS__*` 环境变量提供存储凭证。

stdio 支持当前 `2026-07-28` 协议，以及旧版 `2025-11-25`、`2025-06-18`、`2025-03-26`、`2024-11-05`。

新版请求无需初始化握手。每次请求的 `params._meta` 必须提供 `io.modelcontextprotocol/protocolVersion` 和 `io.modelcontextprotocol/clientCapabilities`，`io.modelcontextprotocol/clientInfo` 可选。例如：

```json
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
```

`server/discover` 返回全部支持版本和工具能力。客户端也可直接携带相同元数据调用 `tools/list` 或 `tools/call`。缺失必填元数据返回 `-32602`；不支持的版本返回 `-32022`，通过 `data.supported` 和 `data.requested` 给出支持列表与请求版本。新版结果保留 `resultType: complete`，已取消的新版请求不再发送响应。

旧版客户端仍使用 `initialize` 和 `notifications/initialized`。旧版握手中出现未知版本时，返回 `2025-11-25`，由客户端接受或断开；旧版本响应省略不支持的结构化结果和 schema 字段。同一进程可处理新旧请求，响应格式按每次请求的版本生成。`ponydb version` 会报告 stdio 最新版本和全部支持版本。

实现依据官方 [版本兼容规则](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) 和 [stdio 规范](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)，提供工具调用能力。

传输使用每行一条 JSON-RPC 消息；stdout 只输出协议消息，启动错误写入 stderr。stdin 关闭时取消在途请求并退出。每个请求使用配置中的超时时间，也可用 `--deadline-ms` 覆盖；最多同时处理 32 个请求，单条输入消息上限为 8 MiB。取消为协作式，不会撤销已提交写入；结果不确定时检查业务数据后决定是否重新提交。

数据库名称只通过显式位置参数提供。通用选项为 `--config`、`--output` 和 `--deadline-ms`；后两者按最终命令校验：server/MCP 不接受输出格式，config/version/server/restore 不接受截止时间。截止时间沿用 requests 配置。


数据库唯一标识为不可修改的 `name`。创建使用 `databases create notes` 或
`{"name":"notes"}`；创建结果和 get/list 的数据库 meta 使用调用方提供的 `name` 标识数据库。所有单库 CLI 操作使用 `db DATABASE_NAME [ACTION]`，省略 ACTION 获取详情。创建使用 `databases create DATABASE_NAME`，列表使用 `databases list`。数据库名称必须显式提供；DTO 中同名字段必须与位置参数一致。

名称遵循 [MySQL 数据库标识符规则](https://dev.mysql.com/doc/refman/8.0/en/identifiers.html)
及 [64 字符限制](https://dev.mysql.com/doc/refman/8.0/en/identifier-length.html)：
1–64 个 Unicode BMP 字符，禁止 NUL、`/`、`\`、`.` 和末尾 ASCII 空格。
传入原始名称，无需 SQL 反引号；与 MySQL 引用标识符一样，支持纯数字、保留字、
中文、名称内部空格和连字符。名称区分大小写，原样保存，不自动去空格或转换大小写。
持久化 HEAD 使用同一 `name` 标识数据库。


布局 v1 使用以下路径：`{prefix}databases/{name}/head.json` 是唯一权威发布点，不可变对象位于 `incarnations/{incarnation_id}/generations/{epoch:016x}/{category}/{uuid}`。名称是不可修改、可复用的逻辑标识；删除后恢复生成新代际，不继承旧快照索引，旧 token 失效。没有独立数据库 ID、全局清单或改名入口。

对象存储错误使用统一的 JSON 错误字段和业务错误码。可用时，`message` 包含 S3 操作、HTTP 状态、服务端错误码及消息，例如 `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` 配置中的 AccessKey ID、Secret AccessKey 和 Session Token 替换为 `[REDACTED]`，控制字符替换为空格，服务端错误码/消息分别限制为 128/1024 个字符，超长追加省略号。这种最小脱敏不会隐藏服务端回显的所有资源信息。不附带请求头、响应头、原始响应正文或 SDK 错误链。远端 HTTP 错误中，408、429、5xx 可重试，其他状态不可重试；对象不存在、条件写和提交不确定的专门处理优先。CLI、HTTP 和 MCP 共享这些诊断。

数据库 HEAD 不存在时返回 `DATABASE_NOT_FOUND`，消息为 `Database 'test3' not found`（包含请求的数据库名称）；请检查名称以及存储端点、bucket 和 prefix 配置。现有数据库引用的对象缺失时返回 `STORAGE_CORRUPT`，消息为 `Database 'test3' storage is incomplete: a referenced object is missing`，不暴露对象键。两种错误均为 `retryable: false`、`outcome: not_committed`。

HEAD 校验失败也返回 `STORAGE_CORRUPT`，消息包含数据库名、`HEAD validation failed` 和受限的原因。例如：`Database 'test' HEAD validation failed: unknown field 'restore'; stored HEAD may be incompatible with this version`。原因区分 JSON 非法、元数据过大、未知或缺失字段、字段类型或值非法，以及 HEAD 约束不满足。字段名仅在不超过 64 字符且只含 ASCII 字母、数字、下划线时显示，其他名称脱敏；不输出存储字段值、对象键或 Serde 原始错误。HEAD 使用严格校验；错误为 `retryable: false`、`outcome: not_committed` 和 CLI 退出码 1。

`storage.force_path_style` 默认为 `true`，保留 MinIO 的 path-style 寻址。OSS 使用 virtual-hosted-style 时设为 `false`；区域端点（如 `https://oss-cn-beijing.aliyuncs.com`）和 bucket 仍分别配置，由 SDK 拼接桶域名。环境变量覆盖方式：`PONYDB_STORAGE__FORCE_PATH_STYLE=false`。配置值必须为布尔类型。

## 数据库与快照导出

无需查询快照 ID 即可导出数据库当前版本：

```sh
ponydb db notes export backup.sqlite
ponydb db notes export --input current-export.json
```

此命令的请求文件为 `{"database":"notes","file":"backup.sqlite"}`。命令只读取一次 HEAD，固定活动数据库当时的 Manifest 和数据版本；后续写入或压缩不改变本次导出的内容。已删除数据库返回 `DATABASE_DELETED`，保留历史仍使用 `snapshot export`。当前版本入口仅在 CLI 提供，沿用下述限制、文件发布方式和结果格式。

将保留的历史版本导出为独立 SQLite 文件，可直接使用 SQLite 打开，也可通过 `db DATABASE_NAME restore file` 恢复：

```sh
ponydb db notes snapshot export SNAPSHOT_ID backup.sqlite
ponydb db notes snapshot export --input export.json
```

CLI 请求文件为 `{"database":"notes","snapshot_id":"SNAPSHOT_ID","file":"backup.sqlite"}`。命令必须显式提供数据库名；显式字段与 DTO 相同时可合并，不一致时报错。`--input -` 从 stdin 读取 JSON。导出目标必须是文件，不支持输出到 stdout（拒绝 `-`），父目录必须存在，已有文件不会被覆盖。导出先写入目标目录的临时文件，成功后原子发布；stdout 按 JSON/table 输出文件路径、快照 ID、数据版本、字节长度及 SHA-256 摘要。

HTTP 使用 `GET /v1/databases/{database}/snapshots/{snapshot_id}/export`，沿用 Bearer 认证并要求 `db:backup` 权限。服务端先生成完整临时文件，再返回 `application/vnd.sqlite3`、`Content-Length` 和附件文件名。下载前的失败沿用 JSON 错误格式；失败或响应体释放时清理临时文件。

`[exports].max_file_bytes` 限制单次文件大小，默认 10737418240 字节（10 GiB）；多个并发导出各自需要临时磁盘空间。生成文件遵守请求截止时间及数据库最大读取时长，因此大文件或慢速读取即使未超过大小限制也可能超时；HTTP 传输使用请求剩余时间。每次导出固定一个 Manifest，不发布新版本。逻辑删除后尚保留的快照仍可导出，重建后不可用；恢复到已删除目标时，新 HEAD 发布后旧快照失效。不支持 MCP 导出和断点续传。

GC 在线执行，可直接使用默认值：`ponydb db notes gc`。每次重新标记，预算耗尽时数据库仍可读写。`max_objects` 限制 DELETE 尝试数，`deleted_objects` 只统计已确认删除的对象。部分完成返回 `outcome: "partial"`，退出码为 6；`complete` 仅表示本次发现的候选已处理完毕，不代表以后不会产生垃圾。不需要 pass ID、暂停开关、恢复命令或续作标志。请求超时独立配置，默认 90000 毫秒；五分钟预算示例：`ponydb db notes gc --budget-ms 300000 --deadline-ms 330000`。只支持布局 1，其他布局版本会报错。

显式 GC 为非当前快照补记 `retired_at_upper_ms`，后续按 HEAD 的保留天数裁剪；首次观察后至少保留 N 天，0 天立即解除非当前历史保护。当前快照及其他根引用的数据始终保留，物理删除仍等待读者宽限期。时间使用与时区无关的 Unix 毫秒，部署方负责系统时钟准确。

## SQLite 文件恢复

`db DATABASE_NAME restore file SQLITE_FILE` 仅在 CLI 提供。CLI 将稳定、完整且使用 DELETE 日志模式的独立 SQLite 备份复制到本地临时文件，检查 SQLite 完整性、外键和结构兼容性，计算 SHA-256，再直接向 S3 上传正式数据段，建立索引、manifest 和首个快照，最后条件发布 HEAD。不使用暂存上传、文件回下载、恢复会话或续作文件。目标必须不存在或已删除，不能覆盖活动数据库。

恢复没有总执行时限，拒绝 `--deadline-ms`，不使用请求超时默认值；单次 S3 操作仍有超时。不配置恢复文件大小或磁盘占用配额，临时磁盘不足时直接报错并清理临时文件。Ctrl+C 停止本地工作和后续上传；已进入发布的操作仍需确认结果。未发布对象可在数据库 HEAD 存在后由后续 GC 回收。明确失败后重新运行完整命令；`OUTCOME_UNKNOWN` 时先检查数据库，再决定是否重试。

阶段和进度始终写入 stderr；stdout 仅返回最终 JSON/表格结果（`committed`、`outcome`、`name`、`data_version`、`total_bytes`、`sha256`、`page_size`）。交互终端原地刷新，最多每 200 毫秒一次；重定向时阶段变化立即输出，同阶段进度最多每 5 秒一行。复制、哈希和已确认的数据段上传显示字节数及百分比；校验、索引构建和发布显示阶段及耗时。上传 100% 不代表完成，必须等待 HEAD 发布成功。HTTP 和 MCP 不提供恢复。

应用默认启用已校验不可变对象的共享磁盘缓存。读取顺序为已解析索引节点内存、对象正文内存、磁盘、S3；HEAD 和 CAS 结果确认始终访问 S3。独立 CLI 进程通过 `objects-v1.sqlite3` 复用正文。Linux 默认使用绝对路径 `$XDG_CACHE_HOME/ponydb`，未设置时使用 `$HOME/.cache/ponydb`；macOS 使用 `$HOME/Library/Caches/ponydb`，Windows 使用 `%LOCALAPPDATA%/ponydb`。`cache.directory` 可覆盖目录，`cache.disk_budget_mib = 0` 禁用磁盘缓存。`config show` 显示解析后的目录，但不创建文件。

`cache.disk_budget_mib` 限制缓存正文容量（默认 2048 MiB），同一目录下所有命名空间共享预算，条目上限为 65,536。SQLite 元数据、日志和空闲页额外占用空间，增量回收允许滞后。共享目录的进程应使用相同预算。`runtime.work_disk_budget_mib` 限制本地 SQLite 执行工作区容量，默认 2048 MiB。恢复临时文件仍不受这些配额限制。

磁盘键包含后端 endpoint/bucket/prefix、完整对象键、长度及 SHA-256，每次读取都验证正文，损坏条目重新下载。近似 LRU 合并访问记录，最多每 60 秒更新一次。磁盘查询及回填共享 50 ms 等待预算，不计 S3 耗时；繁忙或故障时降级，并限频写入 stderr 提示。因此成功命令的 stderr 也可能非空，应结合退出码和结果判断业务是否成功。每个 reader 最多一个阻塞任务，已开始的任务可在超时或取消后完成，期间仍持有执行许可；维护限频且尽力执行。缓存可随时丢弃，目录应仅向拥有数据库访问权限的可信用户开放。库默认仅使用内存缓存，需要显式传入 `DiskCacheOptions` 才启用磁盘层。

已确认成功的不可变元数据 PUT 会回填正文内存缓存及已启用的磁盘缓存，范围为 `manifests`、`segment-index`、`delta-index` 和 `snapshots`，单对象不超过 1 MiB。回填只使用剩余容量，不淘汰已有条目；数据段、增量包及自定义类别仍按需读取缓存。PUT 结果不确定时，必须直接读取后端并确认字节完全相同，才能回填。缓存故障或请求取消时跳过可选回填，不改变已确认的上传结果。索引节点首次使用时仍解析并校验，随后复用现有节点内存缓存。

## 运行日志

`logging.level` 默认 `warn`；`logging.format` 默认 `json`，也支持 `text`。CLI、HTTP 和 MCP 的诊断日志统一写 stderr，业务结果按所选格式写 stdout；MCP stdout 仅输出协议消息。

```toml
[logging]
level = "debug"
format = "json"
```

```sh
PONYDB_LOGGING__LEVEL=debug ponydb db notes execute "SELECT 1"
PONYDB_LOGGING__LEVEL=trace PONYDB_LOGGING__FORMAT=text ponydb db notes execute "SELECT 1"
```

Debug 记录请求 ID、数据库匿名标识、操作与阶段转换、排队和执行耗时、发布结果、数据版本及请求级 I/O/缓存汇总。Trace 额外逐条记录 pager/远程页操作、S3 请求和缓存查询、填充、淘汰、旁路。S3 日志包含实际对象 key（LIST 为前缀），含配置的 storage prefix；缓存日志使用后端相对 key。Key 中的控制字符会转义。Trace 保留完整明细，debug 不输出这些逐项事件。健康检查、ping 和进度刷新不产生请求日志。

所有等级均不记录 SQL、参数、结果内容、metadata、description、凭据、令牌、请求头、URL、本地路径及内容校验和。对象 key 可能包含数据库名称，按约定允许输出；独立数据库标识使用私有随机盐匿名化，在同一进程内保持一致。Trace 也不会开启依赖库日志。API 错误和恢复进度独立写入 stderr；JSON 日志每行一个对象。

汇总统计操作次数，不代表不同页或对象的数量。Pager 访问与远程页读取分别计数；S3 字节数只计成功校验的读取和已确认写入。缓存合并等待计为初次未命中；磁盘旁路包含不可用的读写尝试。请求被丢弃记录 `aborted`，不据此判断写入是否提交；未知发布结果在确认前保持 `unknown`。
