[English](README.md) | **中文**

# mcp-server

把一条 MCP JSON-RPC 消息适配到 `database-service` 的无状态适配器。协议版本为 `2026-07-28`。HTTP 认证和 `Origin` 检查位于 `ponydb-server`；这个 crate 只解释消息。

## 用途

所有入口调用共同业务层，共有 11 个操作。每次写入独立执行，不保存或重放原响应。结果未知时检查业务数据后再决定是否重试。

## 工作原理

`dispatch` 读取一个 JSON-RPC 2.0 对象。`id` 必须是字符串或数字。`params._meta` 必须携带：

- 等于 `2026-07-28` 的 `io.modelcontextprotocol/protocolVersion`
- 可选的 `io.modelcontextprotocol/clientInfo`，提供字符串 `name` 和 `version`
- 对象 `io.modelcontextprotocol/clientCapabilities`

缺失或无效的元数据返回 JSON-RPC `-32602`。不支持的版本返回 `-32022`，附带 `data.supported` 和 `data.requested`。

| 方法 | 结果 |
| --- | --- |
| `server/discover` | `supportedVersions` 是这个版本，宣告 `tools` 能力，说明中写明重复写入会再次执行，部分完成的维护需要另一次同步调用。 |
| `tools/list` | `protocol::tools()`。`cursor` 被拒绝；列表放得进一页。 |
| `tools/call` | 工具名必须是这些工具之一。参数解码为 `Operation`（`operation` 为工具名，`input` 为 `arguments`）。未知名称和无法反序列化的模式是 `-32602`。 |

`tools/call` 用调用方的 `Principal` 和 `RequestContext` 等待 `Service::call`。成功变成 `{ "ok": true, "result": ... }` 的 `structuredContent`，以及同一份 JSON 文本内容块，`isError` 为 false。业务 `ApiError` 使用同一形状，`ok` 为 false，`isError` 为 true。这是一次完成的工具结果，不是 JSON-RPC 传输错误。传输失败（错误的 JSON-RPC、未知方法）使用 `rpc_error`，不调用服务。

响应始终包含 `resultType: complete`。服务进程在调用 `dispatch` 之前负责 `MCP-Protocol-Version` 和 `MCP-Method` 头。

## Stdio

`stdio::serve` 提供逐行 JSON 传输，支持无状态的 `2026-07-28` 请求、旧版初始化、工具调用和协作式取消。它与 HTTP 共用操作分发，无需 HTTP 请求元数据。见 [CLI AI Agent 配置](../../apps/cli/README.zh-CN.md#通过-stdio-mcp-接入-ai-agent)。

## 运行诊断

MCP 请求诊断共享业务层关联 ID，使用宿主 subscriber；stdio stdout 仅输出协议消息。不记录客户端 ID、客户端元数据、工具参数或结果，ping 不产生请求日志。 库不安装全局 subscriber。
