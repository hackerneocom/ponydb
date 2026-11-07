[English](README.md) | **中文**

# database-service

HTTP、MCP 和原生 CLI 背后的 11 个业务操作。传输代码认证调用方并构建 `protocol::Operation`。这个 crate 检查授权，与 `versioned-store` 通信，并通过 `engine-sqlite` 运行 SQL。

## 用途

所有入口共享事务原子性、授权、提交和错误语义。SQL 每次调用独立执行，不提供请求去重、账本或原响应重放。

`Service::call` 是唯一的分发入口。它检查请求截止时间，运行操作，并在返回时取消上下文，使残留的快照读取停止。

## 工作原理

`Service::new` 要求 `BlobStore` 的能力通过校验，然后启动 `WorkerPool`。`ServiceOptions` 携带工作线程和队列大小、SQL 执行限制、段大小、快照保留以及覆盖层硬上限。`runtime-config` 从共用的 TOML 文件填充这些选项。

授权使用 `Principal::allows`。作用域是具体的（`db:list`、`db:read`、`data:read`、`data:write`、`schema:write`、`schema:read`、`db:maintenance`、`db:restore`、`db:backup`）。CLI 主体是 S3 凭证的直接持有者，并被授予 `*`。

HEAD 竞争返回 `TX_CONFLICT`。提交可能成功但不能核验时返回 `OUTCOME_UNKNOWN`，`retryable: false`、`outcome: unknown`；调用方必须根据业务数据决定是否再次提交。

ETag 直接使用同一次读取 `head.json` 时对象存储返回的 ETag，原样传递，不增加存储请求。元数据更新和删除可选地把该值作为 `if_match` 发送；提供时若与读到的 HEAD 不一致，以 `METADATA_PRECONDITION_FAILED` 失败。省略 `if_match` 时基于当前 HEAD 执行，发布仍使用原子条件写拒绝并发 HEAD 变更。提交令牌是命名空间、数据库、实例代和 `data_version` 的不透明编码，供 `at_least` 读取使用。

服务实例持有一个共享 `ObjectReader`：不同调用以及 SQL 检查/执行复用已验证的不可变 Manifest、索引和数据对象，正文容量上限为 16 MiB，最多 8192 项。缓存绑定后端和完整物理对象身份，不缓存 HEAD；新 CLI 进程从空缓存开始。

### SQL

`sql_execute` 加载 HEAD，然后请工作线程池检查语句。检查会编译语句并读取 SQLite 的只读标志，但不执行语句。

只读语句需要 `data:read`。一致性选择清单：

- `strong`：当前 HEAD，它必须是 `active`
- `snapshot`：由 `snapshot_id` 指向的历史记录，需要 `db:backup`
- `at_least`：当前 HEAD，并检查 `commit_token` 指向这个命名空间、数据库和实例代，且 `data_version` 不比 HEAD 更新

读取返回 `outcome: read_only` 和类型化结果，不发布版本。`max_rows` 只能收紧上限，超限报错而不截断。

写入需要 `data:write` 或 `schema:write`，拒绝查询专用选项。工作线程执行 SQL、冻结页，服务写入数据依赖与快照，再通过 `publish_head` 原子发布。重复 DDL 正常编译，已存在的表按 SQLite 规则报错。`SQL_ERROR` 带上 SQLite 诊断，例如表不存在、约束失败、`not authorized` 或多语句。语句执行过程中被取消时返回 `DEADLINE_EXCEEDED`。工作区和快照失败使用稳定文案，不包含对象键或本地路径。

`sql_execute` 的 `statements` 数组沿用同一条发布路径，始终作为写入，拒绝查询专用选项。`schema_get` 是对 `sqlite_schema` 的只读 `sql_execute`。

### 数据库生命周期

创建要求目标不存在或已删除；GC 不阻止生命周期变更，发布仍由同一 HEAD CAS 仲裁。首次创建使用 create-if-absent，墓碑复用使用 CAS；内部生成新 incarnation 和空快照历史。删除保留墓碑，重建后旧快照不再可查。客户端按不可修改的名称访问当前逻辑数据库；迟到请求可能操作同名的新代际。

### 维护

`database_compact` 在预算内把增量页合并进基线段。`database_gc` 在线执行：每次领取新代际，重新标记当前清单及保留快照根，完整列举旧代际对象，然后在读取宽限期结束后删除不可达候选。可达集、前沿和候选集存于本地临时 SQLite 文件，不上传 GC 对象。结束时只清除自己的 HEAD.gc 记录，保留并发提交；残留记录由下次 GC 自动接管，不阻止访问或生命周期操作。`complete: false` / `outcome: "partial"` 表示可再发起独立调用。详见[在线 GC](../../docs/online-gc.md)。

压缩读取源数据时保留错误类型：请求超时和取消返回 `DEADLINE_EXCEEDED`，存储访问和传输失败保留 `STORAGE_UNAVAILABLE` 的诊断与可重试属性，引用对象缺失和完整性校验失败返回 `STORAGE_CORRUPT`。源数据读取失败不会发布新 HEAD。

### 恢复

`db DATABASE_NAME restore file SQLITE_FILE` 仅在 CLI 提供。CLI 将稳定、完整且使用 DELETE 日志模式的独立 SQLite 备份复制到本地临时文件，检查 SQLite 完整性、外键和结构兼容性，计算 SHA-256，再直接向 S3 上传正式数据段，建立索引、manifest 和首个快照，最后条件发布 HEAD。不使用暂存上传、文件回下载、恢复会话或续作文件。目标必须不存在或已删除，不能覆盖活动数据库。

恢复没有总执行时限，拒绝 `--deadline-ms`，不使用请求超时默认值；单次 S3 操作仍有超时。不配置恢复文件大小或磁盘占用配额，临时磁盘不足时直接报错并清理临时文件。Ctrl+C 停止本地工作和后续上传；已进入发布的操作仍需确认结果。未发布对象可在数据库 HEAD 存在后由后续 GC 回收。明确失败后重新运行完整命令；`OUTCOME_UNKNOWN` 时先检查数据库，再决定是否重试。

阶段和进度始终写入 stderr；stdout 仅返回最终 JSON/表格结果（`committed`、`outcome`、`name`、`data_version`、`total_bytes`、`sha256`、`page_size`）。交互终端原地刷新，最多每 200 毫秒一次；重定向时阶段变化立即输出，同阶段进度最多每 5 秒一行。复制、哈希和已确认的数据段上传显示字节数及百分比；校验、索引构建和发布显示阶段及耗时。上传 100% 不代表完成，必须等待 HEAD 发布成功。HTTP 和 MCP 不提供恢复。

### 操作

| 操作 | 对 HEAD 的影响 |
| --- | --- |
| `database_create` | 创建 |
| `database_list`、`database_get`、`schema_get` | 读取 |
| `database_update`、`database_delete` | 元数据比较并交换 |
| `sql_execute` | 读取；语句产生写入时则发布 |
| `database_compact`、`database_gc` | 发布，可能只完成一部分 |
| `snapshot_list`、`snapshot_get` | 读取历史 |

对象存储错误使用统一的 JSON 错误字段和业务错误码。可用时，`message` 包含 S3 操作、HTTP 状态、服务端错误码及消息，例如 `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` 配置中的 AccessKey ID、Secret AccessKey 和 Session Token 替换为 `[REDACTED]`，控制字符替换为空格，服务端错误码/消息分别限制为 128/1024 个字符，超长追加省略号。这种最小脱敏不会隐藏服务端回显的所有资源信息。不附带请求头、响应头、原始响应正文或 SDK 错误链。远端 HTTP 错误中，408、429、5xx 可重试，其他状态不可重试；对象不存在、条件写和提交不确定的专门处理优先。CLI、HTTP 和 MCP 共享这些诊断。

### 二进制快照导出

`Service::export_database` 接收数据库名、主体、请求上下文和异步写入目标，读取一次 HEAD 并固定活动数据库的当前 Manifest 和版本。已删除数据库返回 `DATABASE_DELETED`。该方法仅由 CLI `db DATABASE_NAME export SQLITE_FILE` 调用，与历史快照导出共享权限（`db:backup`）、大小与读取时限、校验及 SHA-256 逻辑；不增加 JSON Operation 或远程接口。

`Service::export_snapshot` 接收严格的 `protocol::SnapshotExport` 请求、主体、请求上下文和异步写入目标，要求 `db:backup` 权限。它按最多 1 MiB 的已校验范围重建保留的 Manifest，返回快照 ID、数据版本、长度和 SHA-256。调用方管理临时文件，成功后才发布。保留存储错误类型映射，取消会停止等待中的读写。读取受请求和 HEAD 读取租约限制，遵守 GC 等待窗口；允许已删除库的保留历史，允许在线 GC 期间导出。该二进制接口独立于 11 个 JSON 操作，只提供 CLI 和 HTTP 入口。

显式 GC 为非当前快照补记 `retired_at_upper_ms`，后续按 HEAD 的保留天数裁剪；首次观察后至少保留 N 天，0 天立即解除非当前历史保护。当前快照及其他根引用的数据始终保留，物理删除仍等待读者宽限期。时间使用与时区无关的 Unix 毫秒，部署方负责系统时钟准确。

`schema_get` 使用专用 DTO：`database` 必填，`table` 与 `type` 可选。`table` 非空，按 `sqlite_schema.tbl_name` 区分大小写精确匹配，包含关联索引和触发器，不解析视图依赖；`type` 为 `table`、`index`、`view`、`trigger` 之一。条件取交集。指定的表必须是存在的非内部表，否则返回 `TABLE_NOT_FOUND`（HTTP 404、CLI 退出码 1）。表存在但没有匹配类型，或仅按类型筛选无匹配时，返回空结果。存在性与定义在同一次查询、同一快照中读取。HTTP 使用同名查询参数，CLI 使用 `--table` 和 `--type`，MCP Schema 同步暴露这些字段。查询使用绑定参数，保留内部对象排除、排序和结果预算。

路径与边界统一由 `storage-namespace` 管理：单库引用、缓存和 GC 均绑定完整数据库根前缀。删除后恢复切换 incarnation，并重建快照索引；旧快照在发布后不可查询，即使底层对象尚未被 GC 回收。旧读者在原租约内可继续，GC 保留跨生命周期的排空窗口。

## 运行诊断

业务调用、导出和 CLI 恢复共享请求 ID，debug 输出阶段与结果汇总，不记录 SQL 输入或结果内容。 库不安装全局 subscriber。
