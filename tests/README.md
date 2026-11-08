# CLI S3 端到端测试

发布工具回归检查使用 `python3 -m unittest discover -s tests/release -v`，不需要 S3。
发布工作流另在两种原生 Linux 架构上运行完整 CLI 测试，以及容器、静态链接、
Alpine/Debian、TLS/DNS 验证；执行和恢复方式见[发布指南](../docs/releases.zh-CN.md)。

`cli_e2e.py` 是统一运行入口，具体测试按命令放在 `cli_e2e/test_*.py` 中，使用 Python 标准库 `unittest` 自动发现。测试使用真实 CLI 子进程和 S3 兼容存储，覆盖全部 CLI 命令，包括 HTTP server、stdio MCP、11 个业务操作、当前版本/快照导出及本地文件恢复。只依赖 Python 3.9+ 标准库和编译好的 CLI。

可以运行全部测试，也可以按文件对应的测试组选择运行；选择多个测试组时重复 `--suite`：

```sh
python3 tests/cli_e2e.py --list
python3 tests/cli_e2e.py --offline
ACCESS_KEY=... SECRET_KEY=... python3 tests/cli_e2e.py --suite execute --suite schema
ACCESS_KEY=... SECRET_KEY=... python3 tests/cli_e2e.py --suite server --suite mcp
```

`--list` 不要求 CLI、凭据或 S3。`--offline` 显式跳过需要 S3 的场景，运行帮助文本、参数校验、版本和配置展示测试；不能将其通过视为真实 S3 测试通过。默认执行全部测试，需要传入凭据。默认会继续运行其他独立场景并汇总失败；使用 `--failfast` 可在首次失败后停止，仍执行当前场景清理。入口返回 0 表示所选测试通过，1 表示失败，非法运行参数返回 2。

[GitHub Actions CI](../.github/workflows/ci.yml) 在 push 和 pull request 时运行独立的 `e2e` 任务（显示为 `CLI, server and MCP end-to-end`）。任务使用 Ubuntu 24.04、Rust 1.95.0 和 Python 3.11，启动与开发容器相同、固定 digest 的 RustFS 1.0.4 镜像，等待 `/health` 就绪，再用 runner 自带的 AWS CLI 创建 `ponydb-test` 桶。端口仅绑定 runner 的 `127.0.0.1:9000`，使用临时容器内的测试凭据，无需仓库 Secrets 或人工建桶。构建 CLI 后执行 `python3 tests/cli_e2e.py`，不传 `--offline` 或 `--suite`，因此全部命令、server、HTTP MCP 和 stdio MCP 场景都会运行。测试失败会使任务失败，并输出 RustFS 日志；任务结束总会尝试删除容器及其临时卷。任务总上限 30 分钟，测试步骤上限 10 分钟。

命令与测试文件对应如下；`db` 行代表 `db DATABASE_NAME`，其他 `db` 行同样要求显式数据库名称：

| 命令 | 测试文件 | 主要校验 |
| --- | --- | --- |
| `version` | `test_version.py` | 操作数、JSON / 表格输出 |
| `config show` | `test_config_show.py` | 凭据脱敏、默认值、环境覆盖 |
| `config validate` | `test_config_validate.py` | 存储契约、探测对象清理、认证错误 |
| `databases create` | `test_databases_create.py` | 位置参数、DTO、元数据文件/stdin、重复创建、关键词/中文名称 |
| `databases list` | `test_databases_list.py` | 分页、DTO、逻辑删除过滤、布尔开关默认值、存储错误 |
| `db` | `test_database_get.py` | 显式名称、DTO 一致性、不存在及 manifest 损坏 |
| `db update` | `test_database_update.py` | 可选 ETag、冲突、元数据大小/UTF-8、保留期 |
| `db delete` | `test_database_delete.py` | 可选/显式/DTO ETag、冲突、删除后的读取行为 |
| `db execute` | `test_execute.py` | 类型、参数/文件/stdin、SQL 边界、事务、数学、JSON/JSONB、历史一致性 |
| `db schema` | `test_schema.py` | 表/类型过滤、DTO、缺失表错误 |
| `db export` | `test_database_export.py` | 当前数据导出、DTO/文件、摘要、不覆盖、导出后恢复 |
| `db snapshot list` | `test_snapshot_list.py` | 历史、分页/DTO、旧 HEAD 格式诊断 |
| `db snapshot get` | `test_snapshot_get.py` | 快照描述、DTO 替代位置参数及冲突 |
| `db snapshot export` | `test_snapshot_export.py` | 历史数据导出、DTO、摘要、不覆盖、导出后恢复 |
| `db compact` | `test_compact.py` | 默认值、预算/段数、DTO 冲突、数据/历史完整性 |
| `db gc` | `test_gc.py` | 分批续作、退出码 6、默认值、冲突、数据/历史完整性 |
| `db restore file` | `test_restore.py` | 文件恢复、进度、无总时限、非法备份、新代际及存储布局 |
| `server` | `test_server.py` | 启动/停止/重启、健康检查、认证/权限、ETag、事务/NDJSON、快照下载、HTTP MCP |
| `mcp` | `test_mcp.py` | 全部工具、事务/历史、协议错误恢复、现代/旧协议混用、取消/截止时间/EOF、stdout 隔离 |
| 跨命令 | `test_arguments.py`、`test_logging.py` | 实际帮助文本/命令覆盖、参数约束/严格 DTO、日志分级与隐私 |

`support.py` 集中管理 CLI 调用、超时、缓存诊断、SQLite 历史数据夹具、S3 签名请求及清理。`catalog.py` 自动发现测试模块，无需手动维护运行列表。新增命令时新增对应 `test_*.py`，声明 `COMMANDS` 并编写 `CliTestCase` 测试；`test_arguments.py` 递归读取实际 `--help`，将命令树与各模块的 `COMMANDS` 对比，遗漏命令或保留已删除命令都会失败。每个测试方法应自行创建所需数据，不能依赖其他方法或模块的执行顺序。

`transports.py` 提供真实 HTTP/JSON-RPC 客户端和有界的 server/MCP 进程夹具。`server` 测试组覆盖 REST 与 HTTP MCP：无效/缺失令牌、只读身份和数据库范围、ETag 与 CLI 一致性及旧 ETag 的 412、事务失败回滚、NDJSON 行流、历史快照与 SQLite 下载、压缩/GC、严格输入与 404/410 错误、HTTP MCP 的协议版本/方法/工具名请求头、Origin、Accept 和启用配置。还会停止并重启 server，验证已提交数据和停机期间的 CLI 写入。

`mcp` 测试组通过 stdin/stdout 调用 `tools/list` 中的每一个业务工具，并检查业务内容在文本和结构化结果中一致。覆盖 SQL 事务回滚、跨进程持久化、历史读取、维护后数据完整性、业务错误、非法 JSON 后继续服务、请求独立元数据、未知版本/方法/工具、旧版握手与现代请求混用。长查询分别验证主动取消、请求超时和 EOF 取消；ping 及后续请求仍可使用，EOF 后在 10 秒内退出且无额外 stdout。禁用 HTTP MCP 不影响 stdio MCP。所有场景均使用真实 S3，`--offline` 会明确跳过。

devcontainer 默认运行 RustFS。首次启动后，先在控制台 `http://localhost:9001` 使用 `rustfsadmin` / `rustfsadmin` 登录，创建 `ponydb-test` 桶，不启用版本保留或对象锁定。控制台地址位于开发容器的网络命名空间，宿主机浏览器需使用编辑器端口转发或 SSH 隧道。然后在开发容器内运行：

```sh
cargo build --locked -p ponydb
export ACCESS_KEY=rustfsadmin
export SECRET_KEY=rustfsadmin
python3 tests/cli_e2e.py
```

默认地址 `http://localhost:9000`，bucket `ponydb-test`，region `us-east-1`。可通过 `E2E_ENDPOINT`、`E2E_BUCKET`、`E2E_REGION`、`E2E_CLI` 覆盖。脚本不自动读取 `.env`，需要调用方传入凭证环境变量。

使用其他 S3 兼容存储时，将上述凭据、端点和桶替换为实际配置。RustFS 的镜像版本、数据卷和重建步骤见 [devcontainer 说明](../.devcontainer/README.md#rustfs)。

底层存储契约测试会创建独立测试桶，覆盖 16 轮并发创建/CAS、范围读取、校验和、分页、删除和响应丢失后的结果分类，可在同一 RustFS 实例上运行：

```sh
P0_S3_ENDPOINT=http://localhost:9000 \
AWS_ACCESS_KEY_ID=rustfsadmin \
AWS_SECRET_ACCESS_KEY=rustfsadmin \
cargo test --locked -p object-store-s3 --test live -- --ignored
```

每个测试方法使用独立的随机 `cli-e2e/<uuid>/` 存储前缀、临时配置、缓存和进程工作目录。通过 `unittest.addCleanup` 注册清理，断言失败也会删除当前前缀内的对象，并重新列举确认前缀为空；S3 夹具接口限制访问当前前缀，不会删除 bucket。清理失败会作为测试错误报告。server/MCP 子进程先停止，再清理存储和临时目录。强制终止测试进程可能跳过清理，测试开头会打印场景名称和前缀。测试账号需要该前缀的 List/Get/Put/Delete 权限；测试使用未启用版本保留的 bucket，清理不遍历历史对象版本。

成功命令的 stderr 可以包含已定义的磁盘缓存降级诊断。诊断使用默认 JSON 格式的 `WARN` / `ponydb::cache` / `disk_cache_degraded` 事件，脚本按事件与受控 reason 精确匹配，并以 `WARN <检查名称>:` 保留在测试 stderr 中；仍严格校验退出码、JSON/表格结果、恢复进度和其他 stderr。缓存超时本身不会令 GC 后的数据校验失败，行数和历史查询仍须通过。

校验内容包括：

- 数据库创建、查询、列举、元数据更新、可选 ETag（命令行或 DTO 提供）、更新/删除旧 ETag 冲突、逻辑删除；数据库和 schema 命令使用位置参数 `DATABASE_NAME`。
- `db DATABASE_NAME` 的显式名称、DTO 合并和冲突校验；更新和删除可以省略 ETag，提供时严格校验。
- SQL 持久化、参数类型（含最大 i64、中文、Blob、NULL）、BOM 文件和 stdin、表格输出。
- 捆绑数学函数 `sqrt`、`pow`、`log` 的结果和类型、定义域错误返回 NULL、非有限结果返回 `SQL_ERROR`。
- JSON / JSONB 的参数化写入、跨进程读取、字段更新与数组遍历；JSONB 作为 Blob 返回并重新绑定参数；非法 JSON 导致整个事务回滚且持久化数据不变。
- 重复写入实际执行、约束冲突、事务提交和失败回滚。
- 快照列表与详情、历史读取、commit token 一致性、压缩、GC 分批续作以及维护后数据与历史完整性。
- 当前版本与历史快照导出、请求文件与 stdin、文件内容及摘要、不覆盖文件、当前版本导出拒绝已删除数据库，以及导出文件恢复。
- CLI 本地文件恢复、stderr 进度、忽略默认请求总超时、非法备份拒绝；删除后旧快照可读，恢复后新代际只含首个快照、HEAD revision 继续递增、旧 ETag/令牌/快照失效。
- 实际对象保持布局 v1，部署 prefix 只追加一次；名称仍为不可修改的逻辑标识，无独立数据库 ID 或清单。
- 对象存储认证失败的操作、HTTP 状态、服务端原因、凭证脱敏和不可重试语义。
- 数据库不存在时消息包含请求名称；删除测试库的 manifest 后返回 `STORAGE_CORRUPT`，与数据库不存在区分。
- 向隔离测试库的 HEAD 添加旧版恢复字段后，快照列表报错包含数据库名、HEAD 校验阶段和未知字段，并提示可能的版本不兼容。
- `config validate` 使用最大 2 MiB 的探测对象在线检查存储契约并清空隔离探测前缀；无效凭证校验失败包含具体阶段和 HTTP 状态，且不泄露凭证。
- 配置凭证脱敏、重复 JSON 键、未知字段、参数冲突、多个 stdin 来源、非法截止时间、退出码 2/4/6。

为缩短 GC 的读者排空等待，测试配置将 `maintenance.max_read_duration_ms` 设为 3000，其余业务配置使用默认值。同一测试方法内部的生命周期步骤按顺序执行，不同方法互相独立。不通过故障注入模拟 `OUTCOME_UNKNOWN`、进程中断或网络分区。

日志测试覆盖 debug 汇总、trace 的页/S3/缓存明细、实际 key、跨线程请求关联、敏感内容排除、发布冲突与未知结果，以及 HTTP/MCP 错误关联和 stdio stdout 隔离。`cli_e2e.py` 另以真实存储验证 debug/trace 的输出分级。
