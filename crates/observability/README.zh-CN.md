# observability

CLI、HTTP/MCP、业务层、SQLite 线程及存储库共享的请求诊断设施。由二进制安装 subscriber，本库不修改全局日志状态。

`Request` 管理一次请求的开始与完成汇总；嵌套调用复用关联 ID 和计数器，`ApiError.request_id` 使用该 ID。`Request::scope` 仅在 future 的每次 poll 和析构期间进入捕获的 dispatcher、span 与计数上下文。异步派生任务使用 `capture().scope(future)`；工作线程和 blocking 任务在分派前 capture，在内部使用 `capture().run(|| work())`。该机制保持取消语义，避免并发请求的上下文互相污染。

`phase` 输出阶段转换和前一阶段耗时。`PageIo`、`cache`、`count` 独立于 trace 过滤维护 debug 汇总。计数表示操作次数，pager 访问与远程页读取分开统计；缓存合并等待计为初次未命中，磁盘旁路包含读写尝试。S3 确认字节数不包含未完成读取和结果未知的写入。任务被丢弃记录 `aborted`，不推断提交结果。

日志 target 为 `ponydb` 和 `ponydb::*`，仅使用受控标签与数值。数据库标识使用进程私有随机盐匿名化；对象 key 明确允许记录，由 text/JSON 格式器转义。禁止记录参数、DTO、SQL、结果、请求头、本地路径、凭据、令牌、校验和及原始错误。二进制在所有等级屏蔽依赖库 target。
