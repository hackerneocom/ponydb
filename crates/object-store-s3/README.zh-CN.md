[English](README.md) | **中文**

# object-store-s3

`object-store-api::BlobStore` 的 S3 实现。条件 Put 是仅有的写方式，并且不允许 AWS SDK 重试这些写请求。

## 用途

ponydb 把 S3 当作事实来源。`head.json` 只通过 `If-Match` 前进。不可变页和元数据只通过 `If-None-Match: *` 出现。如果一次成功的 Put 响应丢失，SDK 自动重试可能会看到自己刚创建的对象，并报告前置条件失败。那会把一次已提交的写入藏起来，因此写重试被关闭，结果保持为 `Unknown`，直到调用方完成对账。

## 工作原理

`S3Store::new` 接受没有用户名、密码、查询串、片段或额外路径的 `http` 或 `https` 端点。寻址方式由 `S3Config::force_path_style` 控制。桶名由小写字母、数字、`-` 和 `.` 组成。配置的前缀会追加到每个键上，并包含在修订作用域中，其身份是 `endpoint|bucket|prefix`。含有空段、`.`、`..`，或落在一小段 ASCII 集合之外的键，会在请求发出前被拒绝。

修订令牌是对象的 ETag，由 `RevisionScope` 包装。`compare_and_swap` 解码该令牌，并发送带 `If-Match` 的 `PutObject`。`create_if_absent` 发送带 `If-None-Match: *` 的同一种 Put。每次 Put 还设置 SHA-256 校验和头以及 `sha256` 用户元数据。SDK 重试策略是一次尝试，操作超时为配置的超时时间。

`classify_write` 映射 HTTP 结果：

| 状态 | 结果 |
| --- | --- |
| 412 | `PreconditionFailed` |
| 401、403 | `NotApplied(AccessDenied)` |
| 429 | `NotApplied(Throttled)`，可重试 |
| 400、404、405、411、413、416、422 | `NotApplied(InvalidInput)` |
| 其余情况，包括 409、5xx、超时和分发失败 | `Unknown` |

构造失败是 `NotApplied`，因为请求没有离开客户端。

`read_current` 检查内容长度，要求存在 `sha256` 元数据，并在返回正文、原始对象 ETag 和不透明 ETag 修订之前对正文做哈希。`read_range` 发送 HTTP Range，仅当 `Content-Range` 以请求的边界开头且长度吻合时接受正文。列举使用 `ListObjectsV2`。继续游标是重复作用域 ID、前缀和分隔符的 JSON；来自另一次查询的游标是 `InvalidInput`。`delete` 删除一个键；当结果无法确定时，把传输失败分类为 `Unknown`。

`S3Config::max_object_bytes` 是能力上限（运行时配置目前传入 8 MiB）。`tests/live.rs` 中的真实契约测试默认忽略，除非设置了 `P0_S3_ENDPOINT` 和 AWS 凭证。

对象存储错误使用统一的 JSON 错误字段和业务错误码。可用时，`message` 包含 S3 操作、HTTP 状态、服务端错误码及消息，例如 `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` 配置中的 AccessKey ID、Secret AccessKey 和 Session Token 替换为 `[REDACTED]`，控制字符替换为空格，服务端错误码/消息分别限制为 128/1024 个字符，超长追加省略号。URL 用户信息、路径和查询串，以及敏感的授权、签名、令牌赋值也会脱敏；服务端回显的其他资源信息仍可能保留。业务错误不附带请求头、响应头、原始响应正文或 SDK 错误链。远端 HTTP 错误中，408、429、5xx 可重试，其他状态不可重试；对象不存在、条件写和提交不确定的专门处理优先。CLI、HTTP 和 MCP 共享这些诊断。

`storage.force_path_style` 默认为 `true`，保留 MinIO 的 path-style 寻址。OSS 使用 virtual-hosted-style 时设为 `false`；区域端点（如 `https://oss-cn-beijing.aliyuncs.com`）和 bucket 仍分别配置，由 SDK 拼接桶域名。环境变量覆盖方式：`PONYDB_STORAGE__FORCE_PATH_STYLE=false`。配置值必须为布尔类型。

## 运行诊断

Debug 在将 SDK 失败转换为业务错误前记录 `s3_request_failed`，包含外围请求 ID、操作、耗时毫秒、SDK 尝试次数、endpoint、实际请求 hostname/端口、环境代理选择结果（考虑 `NO_PROXY`）、SDK 错误类型和 dispatch 分类（`timeout`、`io`、`user`、`other`）。代理判断遵循锁定版本 Smithy 默认客户端，不记录代理 URL 或凭证。构造失败可能为零次尝试，且没有 hostname。

`error_chain` 是 JSON 编码的数组，按顺序包含顶层 `Display` 和每一层 `Error::source()`，不截断。I/O 错误额外包含 `io_kind` 和可用时的 `raw_os_error`。输出前替换配置凭证及其 URL 编码形式、URL 用户信息/路径/查询串，以及敏感的授权、签名、令牌赋值；控制字符替换为空格。不格式化请求、响应正文、对象内容或请求头/响应头集合。仅观察 HTTP 状态和白名单中的服务端请求 ID 头（`x-amz-request-id`、`x-oss-request-id`、`x-request-id`）。

`timeout_config_observed=true` 表示四个 `*_timeout_ms` 字段来自 SDK 合并后的运行配置，此时缺少的超时字段表示禁用；否则表示无法获取配置。当前锁定 SDK 中，`BehaviorVersion::latest()` 提供 3100ms connect timeout，read timeout 禁用，CLI 配置的 operation 和 operation-attempt timeout 均为 10000ms。诊断不改变这些超时或单次尝试的重试策略。Connect timeout 可能覆盖 DNS、TCP、代理协商和 TLS，仅凭耗时不能定位失败层。

Trace 另为每次适配器调用输出一条完成事件：操作、含部署前缀的实际 key（LIST 为前缀）、实际 HTTP 状态、结果、确认字节数及耗时。库不安装全局 subscriber，依赖库原始日志仍被过滤，未启用 DNS/TCP/TLS 分阶段日志。用法见 [CLI 运行日志](../../apps/cli/README.zh-CN.md#运行日志)。
