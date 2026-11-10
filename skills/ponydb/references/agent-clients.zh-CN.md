[English](agent-clients.md) | **中文**

# AI Agent 接入

通过 CLI 或 MCP 为 AI Agent 提供持久化 SQL 上下文。Claude Code、Codex、WorkBuddy 和 DeepSeek Harness 均可启动本地 stdio MCP 进程；HTTP 接入还取决于客户端的协议实现。

## 安装 PonyDB skill

[PonyDB skill](../SKILL.md) 为长期数据库操作提供引导，并附带这些共用使用文档。
安装 skill 是向 Agent 添加操作指导；安装和配置 PonyDB 则是让数据库连接可用。
两者可以独立完成。

为用户安装 skill 前，先询问仅用于当前项目，还是全局供该用户的多个项目使用；
用户已经指定范围时，无需重复询问。根据选择使用下方对应命令，然后在安装工具中
选择目标 Agent。运行前提和支持的 Agent 见 [skills CLI 参考](https://github.com/vercel-labs/skills/blob/main/docs/commands.md)。

当前项目安装：

```sh
npx skills add hackerneocom/ponydb --skill ponydb
```

全局安装：

```sh
npx skills add hackerneocom/ponydb --skill ponydb -g
```

也可从仓库检出中复制整个 `skills/ponydb` 目录，放入目标 Agent 对应的 skill 目录，
采用已确定的项目或用户范围。保持 `SKILL.md` 和 `references/` 在一起。
按需重载 Agent，并确认能发现 `ponydb` skill；发现 skill 不代表数据库连接已验证。

## 让 AI Agent 帮你安装

把以下提示词交给 AI Agent：

```text
请阅读 https://raw.githubusercontent.com/hackerneocom/ponydb/main/docs/install-ponydb-with-ai.md ，帮我为当前 AI Agent 安装 ponydb。根据我的环境选择 CLI、stdio MCP 或已有的 HTTP MCP 服务，完成配置并验证一次查询；缺少连接信息时请向我询问。
```

[AI Agent 安装指引](https://github.com/hackerneocom/ponydb/blob/main/docs/install-ponydb-with-ai.md)覆盖安装包选择、对象存储配置、客户端配置与验证。

## 选择接入方式

| 方式 | 前提 | 凭证 | 适用场景 |
| --- | --- | --- | --- |
| CLI（推荐） | 在 AI Agent 执行 shell 命令的环境安装 CLI | S3 凭证 | AI Agent 能执行 shell 命令时推荐使用，无需配置 MCP |
| stdio MCP | 本地安装 CLI 并配置对象存储 | AI Agent 所在机器的 S3 凭证 | 可以安装 CLI，但不允许 AI Agent 执行 shell 命令时推荐使用；由客户端启动本地 CLI 进程，无需常驻 HTTP 服务 |
| HTTP MCP | 已启动 `ponydb server` 或 Docker 服务 | 服务地址与 Bearer token | 无法安装 CLI 时，推荐在 Serverless 平台通过 [Docker](http.zh-CN.md#docker) 部署 HTTP 服务后接入；客户端需支持 ponydb 的 HTTP MCP 协议 |

使用 CLI 或 stdio MCP 时，请先完成 [CLI 安装](cli.zh-CN.md#安装)和[对象存储配置](configuration.zh-CN.md)。使用 HTTP MCP 时，可在服务主机安装 CLI 并启动 HTTP 服务，也可通过 [Docker](http.zh-CN.md#docker) 部署。

## 在 AI Agent 中使用 CLI

安装和配置完成后，让 AI Agent 按 [CLI 指南](cli.zh-CN.md)创建数据库、读写和备份。

## stdio MCP

客户端启动 `ponydb --config /absolute/path/ponydb.toml mcp`。建议程序和配置文件都使用绝对路径，尤其是桌面客户端。无需启动 HTTP 服务，也不需要服务端 token。

在 Windows 的 JSON 配置中，程序路径可写为 `"C:/path/to/ponydb.exe"` 或 `"C:\\path\\to\\ponydb.exe"`。JSON 中的反斜杠是转义符，因此路径中的每个 `\` 都需要写成 `\\`；使用 `/` 则无需转义。请将示例替换为实际安装路径，配置文件路径也遵循相同规则。

stdio 支持 `2026-07-28`，以及旧版 `2025-11-25`、`2025-06-18`、`2025-03-26`、`2024-11-05`。`mcp.enabled` 仅控制 HTTP 端点。

### Claude Code

```sh
claude mcp add --transport stdio ponydb -- \
  /absolute/path/ponydb --config /absolute/path/ponydb.toml mcp
claude mcp list
```

在客户端通过 `/mcp` 检查连接。见 [Claude Code 官方 MCP 文档](https://code.claude.com/docs/en/mcp)。

### Codex

```sh
codex mcp add ponydb -- \
  /absolute/path/ponydb --config /absolute/path/ponydb.toml mcp
codex mcp list
```

也可在 `~/.codex/config.toml` 中添加：

```toml
[mcp_servers.ponydb]
command = "/absolute/path/ponydb"
args = ["--config", "/absolute/path/ponydb.toml", "mcp"]
```

见 [Codex 官方 MCP 文档](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)。

### WorkBuddy

在客户端 MCP 配置中添加本地 stdio 服务，字段如下；与已有条目合并：

```json
{
  "mcpServers": {
    "ponydb": {
      "type": "stdio",
      "command": "/absolute/path/ponydb",
      "args": ["--config", "/absolute/path/ponydb.toml", "mcp"]
    }
  }
}
```

在 WorkBuddy 中启用连接并查看工具列表。字段说明见[官方连接器指南](https://open.workbuddy.cn/docs/connector)。

### DeepSeek Harness

在 Harness 配置所需作用域中添加一个客户端条目：

```yaml
- id: mcp-ponydb
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: ponydb
    transport: stdio
    command: /absolute/path/ponydb
    args: ['--config', '/absolute/path/ponydb.toml', 'mcp']
```

重启或重载 Harness 后检查发现的工具。见[官方 MCP 客户端配置](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/mcp/mcp-client/README.md)。

## HTTP MCP

先部署 [HTTP 服务](http.zh-CN.md)。`/mcp` 默认启用，与 REST API 共用认证身份。

**配置客户端前先检查协议支持。** 该端点要求 MCP `2026-07-28`、逐请求元数据和与方法对应的请求头。仅支持旧版 Streamable HTTP 初始化流程的客户端无法连接。静态请求头无法替代每次请求变化的方法名和工具名；客户端未实现该协议时，请使用 stdio。

| 请求字段 | 必需值 |
| --- | --- |
| `Authorization` | `Bearer TOKEN` |
| `Content-Type` | `application/json` |
| `Accept` | 同时包含 `application/json` 和 `text/event-stream` |
| `MCP-Protocol-Version` | `2026-07-28` |
| `MCP-Method` | 本次请求的 JSON-RPC 方法名 |
| `MCP-Name` | 工具名，仅 `tools/call` 需要 |
| `params._meta` | 协议版本与客户端能力，见下方探测示例 |

携带 `Origin` 的请求会被拒绝。本版本没有浏览器来源白名单，也不支持旧版 HTTP 握手。

可先独立于客户端验证认证与工具发现：

```sh
export MCP_TOKEN='replace-with-your-server-token'
curl --fail-with-body http://127.0.0.1:8080/mcp \
  -H "Authorization: Bearer $MCP_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'MCP-Method: tools/list' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
```

客户端版本满足上述要求时，按相应格式配置地址和认证：

| 客户端 | HTTP 连接设置 |
| --- | --- |
| Claude Code | `claude mcp add --transport http ponydb https://db.example.com/mcp --header "Authorization: Bearer $MCP_TOKEN"` |
| Codex | `[mcp_servers.ponydb]` 中配置 `url = "https://db.example.com/mcp"` 和 `bearer_token_env_var = "MCP_TOKEN"` |
| WorkBuddy | `mcpServers.ponydb` 中配置 `type: "streamableHttp"`、`url`、`headers.Authorization` |
| DeepSeek Harness | `@deepseek-ai/dsh-mcp-client` 中配置 `serverName: ponydb`、`transport: streamable-http`、`url`、`headers.Authorization` |

这些是连接字段，不代表每个客户端版本都已兼容协议。请替换为实际服务地址，并在客户端环境中配置 token。各客户端的配置字段见前面的官方文档链接。

## 验证 MCP 访问

stdio 和 HTTP MCP 均应从实际客户端发现可用工具。仅在获得授权时调用
`database_list`。列出数据库需要 `db:list`；仅拥有指定数据库的 `db:read` 和
`data:read` 权限的身份，可以查询该数据库而无需列举权限。列举返回 `FORBIDDEN`
表示授权受限，不应视为连接失败。

选择一个明确获准访问的已有数据库，调用 `sql_execute`，参数为
`{"database":"AUTHORIZED_DATABASE","sql":"SELECT 1"}`，并确认返回整数 `1`。
数据库名称未知时，向用户询问，无需先成功列出数据库。分别报告工具发现、授权和
查询执行的结果。没有可用的已授权数据库时，将查询验证标记为待完成。

## 安装后的使用示例

向 AI Agent 提出：

> 使用 ponydb 创建 `agent_context` 数据库，添加含 ID、主题、内容的 `memories` 表。记录我的偏好：文档应该简洁。然后查询并返回这条偏好。

在后续会话或另一台已配置的设备中：

> 从 `agent_context` 查询我的文档偏好，并应用到当前任务。

MCP 提供数据库管理、SQL、Schema、快照、压缩与 GC 工具。请确认客户端能列出工具并完成查询；仅保存连接配置不足以确认可用。SQLite 文件恢复与导出使用 CLI。让 AI Agent 加载扩展或手动管理事务前，请先查看 [SQLite 限制](sqlite.zh-CN.md)。
