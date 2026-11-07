[English](README.md) | **中文**

# engine-sqlite

工作区固定使用 `rusqlite` 0.41.1，捆绑编译 SQLite 3.54.1，不使用系统 SQLite 库。

仓库的 `.cargo/config.toml` 为 bundled 构建设置 `LIBSQLITE3_FLAGS=SQLITE_ENABLE_MATH_FUNCTIONS`，包含 CI 和 Docker 构建。SQL 可直接使用 `sqrt`、`pow`、`log`、`ln`、`sin` 等数学函数，无需加载扩展或创建虚表。若覆盖 `LIBSQLITE3_FLAGS`，必须保留此宏。诊断输出包含 `ENABLE_MATH_FUNCTIONS`，服务启动时也会核验。`sqrt(-1)` 等定义域错误返回 NULL；`exp(1000)` 等非有限结果由服务层现有的结果校验拒绝。

在一份不可变快照上运行参数化 SQL，并返回一个尚未发布的候选：冻结的脏页加上语句结果。调度、截止时间和远程提交属于调用方。

## 用途

SQLite 必须按它自己的事务规则执行，但 ponydb 的写入要到 `versioned-store` 发布新清单和 `head.json` 之后才持久。这个 crate 是这一拆分的本地一半。它还暴露捆绑 SQLite 的构建诊断，供服务和 CLI 在启动时打印。

## 工作原理

### 一次事务

`prepare_with_limits` 在调用方的 `Snapshot` 上构建 `CowFile`，打开一个 `sqlite-vfs` 工作区，并检查 SQLite 接受了这些配置：

- `page_size` 等于快照的页大小
- `journal_mode=DELETE`
- `synchronous=FULL`
- `foreign_keys=ON`
- `mmap_size=0`、`temp_store=FILE`、`threads=0`

然后它执行 `BEGIN IMMEDIATE` 并安装授权器。`ATTACH`、`DETACH`、`PRAGMA`、嵌套的 `BEGIN` / `SAVEPOINT`、虚表和 `load_extension` 被拒绝。读、写和模式变更仅在 `SqlPermissions` 允许时放行。调用方已经知道语句只读时，会打开 `query_only`。

每条语句都带绑定参数进行预编译。进度处理器每 1000 个虚拟机步调用一次 `RequestContext::check`，请求被取消或超过截止时间时中断 SQLite。行数和结果字节数对照 `max_result_rows` 与 `max_result_bytes` 计数。任一项超出都会使事务失败；结果不会被截断。语句结束后，引擎清除授权器，执行 `COMMIT`，确认连接回到自动提交，并冻结工作区。返回的 `PreparedTransaction` 带有 `read_only`、`FrozenPages`，以及每条语句一个 `StatementResult`。

`inspect` 把每条语句编译到能够读取 SQLite 只读标志为止，然后 `ROLLBACK`，不执行用户 SQL。`database-service` 用它判断一次 `sql_execute` 是查询还是写入。

语句数量和 SQL 总字节数超过限制时，会在打开工作区之前被拒绝。

### 工作线程池

`worker::WorkerPool` 拥有一组名为 `sqlite-N` 的固定线程和一条有界作业队列。`prepare` 对每个数据库 ID 最多准入一个活动作业，并对每个主体准入配置的数量。同一数据库的第二个作业，或已满的队列，返回 `Busy`。

线程池用 `remote-pager::bridge` 把异步快照接到工作线程上。Tokio 任务运行 `BridgeServer::serve`。工作线程运行同步 prepare。`tokio::select` 等待两边：桥接结束意味着工作线程已经丢弃快照发送端，任务仍会汇合工作线程的结果。如果 prepare future 被丢弃，请求上下文被取消，工作线程和桥接都会停止。工作线程 panic 变成 `Error::Worker`。

`Error::into_cause` 把这些失败归类给服务层。SQL 错误保留 rusqlite 的值，包括发生在 VFS 内部的语句失败。快照 I/O 和工作区失败不带内部原文。

`diagnostics` 打开一个内存连接，并报告 `sqlite3_libversion`、`sqlite_source_id` 和 `PRAGMA compile_options`。

JSON 和 JSONB 测试通过 VFS 和授权器覆盖绑定文本输入、嵌套提取、`->` / `->>`、更新、Unicode、NULL 和 64 位整数。还覆盖无需创建虚表即可查询的 `json_each` / `json_tree` 及其 `jsonb_` 版本。业务层和 CLI 测试验证更新持久化、JSONB Blob 参数往返、只读访问和非法 JSON 导致的事务回滚。

## 运行诊断

工作线程准入、排队等待和执行输出 debug 事件；任务显式把调用方日志上下文传入 SQLite 线程，不输出语句文本、参数或结果。 库不安装全局 subscriber。
