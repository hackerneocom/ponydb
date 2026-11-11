**English** | [中文](agent-clients.zh-CN.md)

# AI Agent clients

Give an AI Agent persistent SQL context through the CLI or MCP. Claude Code, Codex, WorkBuddy and DeepSeek Harness can launch a local stdio MCP process; HTTP support also depends on the client's protocol implementation.

## Install the PonyDB skill

The [PonyDB skill](../SKILL.md) guides installation, configuration and ongoing
database work, and includes the [setup runbook](setup.md) and these shared usage
guides. Installing the skill adds instructions to an Agent; installing and
configuring PonyDB makes the database connection available. Either can be done
independently.

Project installation:

```sh
npx skills add hackerneocom/ponydb --skill ponydb
```

Global installation:

```sh
npx skills add hackerneocom/ponydb --skill ponydb -g
```

Alternatively, copy the entire `skills/ponydb` directory from a checkout into the
chosen Agent's skill directory at the agreed project or user scope. Keep
`SKILL.md` and `references/` together. Reload the Agent if required and check that
it discovers the `ponydb` skill; discovery does not verify database connectivity.

## Let your AI Agent install it

Paste this into your AI Agent:

```text
Read https://raw.githubusercontent.com/hackerneocom/ponydb/main/skills/ponydb/references/setup.md and help me install ponydb for this AI Agent. Choose CLI, stdio MCP or an existing HTTP MCP service for my environment, configure it, and verify a query. Ask me for any missing connection details.
```

## Choose a connection

| Mode | Prerequisites | Credentials | Use when |
| --- | --- | --- | --- |
| CLI (recommended) | CLI installed where the AI Agent runs shell commands | S3 credentials | Recommended when the AI Agent can run shell commands; no MCP setup needed |
| stdio MCP | Local CLI installed and storage configured | S3 credentials on the AI Agent's machine | Recommended when the CLI can be installed but the AI Agent is not allowed to run shell commands; the client launches a local CLI process without an always-on HTTP service |
| HTTP MCP | A running `ponydb server` or Docker service | Service URL and Bearer token | When the CLI cannot be installed, deploy the HTTP service through [Docker](http.md#docker) on a serverless platform and connect through HTTP MCP; the client must support ponydb's HTTP MCP protocol |

For CLI or stdio MCP, start with [CLI installation](cli.md#install) and [object storage configuration](configuration.md). For HTTP MCP, install the CLI on the service host and start the HTTP service, or deploy it with [Docker](http.md#docker).

## Use the CLI from an AI Agent

After installation and configuration, have your AI Agent follow the [CLI guide](cli.md) to create databases, read, write and back up data.

## stdio MCP

The client starts `ponydb --config /absolute/path/ponydb.toml mcp`. Use absolute paths for both files, especially with desktop clients. No HTTP server or server token is required.

In a Windows JSON configuration, write the executable path as `"C:/path/to/ponydb.exe"` or `"C:\\path\\to\\ponydb.exe"`. Backslashes are escape characters in JSON, so each `\` in a path must be written as `\\`; forward slashes (`/`) need no escaping. Replace the example with your actual installation path. The same rules apply to the configuration file path.

stdio supports `2026-07-28` and legacy versions `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`. `mcp.enabled` controls HTTP only.

### Claude Code

```sh
claude mcp add --transport stdio ponydb -- \
  /absolute/path/ponydb --config /absolute/path/ponydb.toml mcp
claude mcp list
```

Use `/mcp` in the client to inspect the connection. See [Claude Code MCP documentation](https://code.claude.com/docs/en/mcp).

### Codex

```sh
codex mcp add ponydb -- \
  /absolute/path/ponydb --config /absolute/path/ponydb.toml mcp
codex mcp list
```

Alternatively, add this to `~/.codex/config.toml`:

```toml
[mcp_servers.ponydb]
command = "/absolute/path/ponydb"
args = ["--config", "/absolute/path/ponydb.toml", "mcp"]
```

See the [official Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

### WorkBuddy

Add a local stdio server in the client's MCP configuration, using these fields. Merge with existing entries:

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

Enable the connection in WorkBuddy and check its tool list. Configuration fields are described in the [official connector guide](https://open.workbuddy.cn/docs/connector).

### DeepSeek Harness

Add one client entry in your Harness configuration's intended scope:

```yaml
- id: mcp-ponydb
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: ponydb
    transport: stdio
    command: /absolute/path/ponydb
    args: ['--config', '/absolute/path/ponydb.toml', 'mcp']
```

Restart or reload Harness and check the discovered tools. See the [official MCP client configuration](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/mcp/mcp-client/README.md).

## HTTP MCP

First deploy the [HTTP service](http.md). Its `/mcp` endpoint is enabled by default and uses the same principals as the REST API.

**Check protocol support before configuring a client.** This endpoint requires MCP `2026-07-28`, modern per-request metadata, and method-specific headers. A client supporting only legacy Streamable HTTP initialization will not connect. Adding static headers cannot supply the changing method and tool name for each request. Use stdio if your client does not implement this protocol.

| Request field | Required value |
| --- | --- |
| `Authorization` | `Bearer TOKEN` |
| `Content-Type` | `application/json` |
| `Accept` | Both `application/json` and `text/event-stream` |
| `MCP-Protocol-Version` | `2026-07-28` |
| `MCP-Method` | The JSON-RPC method of this request |
| `MCP-Name` | Tool name, for `tools/call` only |
| `params._meta` | Protocol version and client capabilities, as in the probe below |

Requests with an `Origin` header are rejected. There is no browser origin allow-list or legacy HTTP handshake in this build.

Test authentication and tool discovery independently of the client:

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

For a client version implementing those requirements, configure the URL and authentication using its format:

| Client | HTTP connection settings |
| --- | --- |
| Claude Code | `claude mcp add --transport http ponydb https://db.example.com/mcp --header "Authorization: Bearer $MCP_TOKEN"` |
| Codex | `[mcp_servers.ponydb]` with `url = "https://db.example.com/mcp"` and `bearer_token_env_var = "MCP_TOKEN"` |
| WorkBuddy | `mcpServers.ponydb` with `type: "streamableHttp"`, `url` and `headers.Authorization` |
| DeepSeek Harness | `@deepseek-ai/dsh-mcp-client` with `serverName: ponydb`, `transport: streamable-http`, `url` and `headers.Authorization` |

These are connection settings, not a guarantee of protocol support in every client version. Use your real service URL and configure the token in the client environment. The earlier official client links describe their supported configuration fields.

## Verify MCP access

For both stdio and HTTP MCP, discover the available tools from the actual client.
Call `database_list` only if authorized. Listing requires `db:list`; a principal
with `db:read` and `data:read` on a specific database can query it without listing
permission. A `FORBIDDEN` listing is an authorization restriction, not a connection
failure.

Use an explicitly authorized existing database and call `sql_execute` with
`{"database":"AUTHORIZED_DATABASE","sql":"SELECT 1"}`. Check that the result is
the integer `1`. If the database name is unknown, ask the user; do not require a
successful database listing. Report tool discovery, authorization, and query
execution separately. If no authorized database is available, report query
verification as pending.

## Try it after setup

Ask the AI Agent:

> Use ponydb to create an `agent_context` database with a `memories` table containing an ID, topic and content. Save my preference: documentation should be concise. Then query that preference back.

In a later session, or on another configured device:

> Query `agent_context` for my documentation preferences and use them for this task.

MCP exposes database management, SQL, schema, snapshots, compaction and GC tools. Verify that the client lists the tools and completes a query; a saved connection alone does not establish access. SQLite file restore and exports use the CLI. See [SQLite limits](sqlite.md) before asking an AI Agent to load extensions or manage transactions manually.
