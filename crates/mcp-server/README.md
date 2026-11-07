**English** | [中文](README.zh-CN.md)

# mcp-server

A stateless adapter from one MCP JSON-RPC message to `database-service`. Protocol version `2026-07-28`. HTTP authentication and the `Origin` check live in `ponydb-server`; this crate only interprets the message.

## Purpose

All entries call the same business layer with 11 operations. Each write executes independently; responses are not stored or replayed. Inspect business data before deciding to retry an uncertain write.

## How it works

`dispatch` reads a JSON-RPC 2.0 object. `id` must be a string or a number. `params._meta` must carry:

- `io.modelcontextprotocol/protocolVersion` equal to `2026-07-28`
- optional `io.modelcontextprotocol/clientInfo` with string `name` and `version`
- an object `io.modelcontextprotocol/clientCapabilities`

Missing or invalid metadata is JSON-RPC `-32602`. Unsupported versions return `-32022` with `data.supported` and `data.requested`.

| Method | Result |
| --- | --- |
| `server/discover` | `supportedVersions` is this version, the `tools` capability is advertised, and the instructions state that repeated writes execute again and that partial maintenance needs another synchronous call. |
| `tools/list` | `protocol::tools()`. A `cursor` is rejected; the list fits in one page. |
| `tools/call` | The tool name must be one of those tools. Arguments are decoded as `Operation` (`operation` = tool name, `input` = `arguments`). Unknown names and schemas that do not deserialize are `-32602`. |

`tools/call` awaits `Service::call` with the caller's `Principal` and `RequestContext`. Success becomes `structuredContent` of `{ "ok": true, "result": ... }` and the same JSON as a text content block, with `isError: false`. A business `ApiError` uses the same shape with `ok: false` and `isError: true`. That is a completed tool result, not a JSON-RPC transport error. Transport failures (bad JSON-RPC, unknown method) use `rpc_error` and do not call the service.

The response always includes `resultType: complete`. The server process is responsible for the `MCP-Protocol-Version` and `MCP-Method` headers before it calls `dispatch`.

## Stdio

`stdio::serve` provides a newline-delimited transport supporting stateless `2026-07-28` requests, legacy initialization, tool calls and cooperative cancellation. It shares operation dispatch with HTTP, without requiring HTTP request metadata. See the [CLI agent configuration](../../apps/cli/README.md#agent-clients-over-stdio-mcp).

## Runtime diagnostics

MCP request diagnostics share service correlation IDs and use the host subscriber. Stdio stdout remains protocol-only. Client IDs, client metadata, tool arguments and results are excluded; ping does not produce request logs. Libraries do not install a global subscriber.
