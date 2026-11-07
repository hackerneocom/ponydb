[English](README.md) | **中文**

# ponydb server

在一个端口上提供 HTTP `/v1` 和 MCP。进程加载 `runtime-config`，打开 S3 存储，并对外提供 `database-service`。它不实现第二套数据库规则。

## 用途

所有入口调用共同业务层，共有 11 个操作。每次写入独立执行，不保存或重放原响应。结果未知时检查业务数据后再决定是否重试。

## 工作原理

不带子命令的 `ponydb server` 直接启动服务。`config validate --target server` 先校验认证配置，再执行 [S3 在线契约探测](README.zh-CN.md)，在配置前缀下创建和删除临时对象。`config show` 保持离线。启动时检查捆绑的 SQLite 是否带有授权器，加载认证配置，并用 `config.s3()` 和 `config.service_options()` 构建 `Service`。`--listen` 覆盖 `server.listen`。Ctrl-C 或 `SIGTERM` 停止接受连接，并等待已经在运行的请求。`/healthz` 只报告进程存活。处于维护中的数据库不会使该路由失败。

认证配置是 JSON `{ "principals": [ { "id", "token", "databases", "scopes" } ] }`。令牌至少 16 字节，ID 和令牌各自唯一，并且至少需要一个主体。比较 bearer 令牌时走完两个字节串并折叠异或差，因此不会在第一处不匹配时返回。

`router` 挂载：

| 路由 | 传输 |
| --- | --- |
| `GET /healthz` | 存活 JSON |
| `/v1/databases` 及其嵌套路径 | HTTP API |
| `POST /mcp` | MCP，当 `mcp.enabled` 时 |

正文上限为 8 MiB。HTTP JSON 用 `protocol::strict_json` 解析。路径参数、查询参数和 `If-Match` 头（映射为 `if_match`）写入同一个对象。与正文不一致的值是 `INVALID_REQUEST`，并在访问 S3 之前被拒绝。得到的对象被反序列化为 `Operation`，并以 `requests.default_timeout_ms` 的 `RequestContext` 传给 `Service::call`。`ApiError.code` 选择 HTTP 状态（`TX_CONFLICT` 为 409，`OUTCOME_UNKNOWN` 为 503，依此类推）。`database_create` 返回 201。JSON 结果含有 `etag` 时，该值被复制到 `ETag` 头。`Accept` 含有 `application/x-ndjson` 的只读 `sql_execute` 以 `result_start`、`rows` 和 `done` 行流式返回。

`POST /mcp` 要求 `MCP-Protocol-Version: 2026-07-28`，以及与 JSON-RPC 方法相等的 `MCP-Method` 头。`tools/call` 还要求 `MCP-Name` 等于工具名。携带 `Origin` 头的请求以 403 拒绝，包括 localhost，因为这个构建没有浏览器来源允许列表。正文随后交给 `mcp-server::dispatch`。

### 通过环境变量配置服务认证

设置 `PONYDB_SERVER__AUTH`，值为完整的认证 JSON：

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
ponydb config validate --target server
```

对应配置键为 `server.auth`，可以直接写在主 TOML 配置文件中；环境变量优先于配置文件。认证内容在启动时加载，变更需要重启。`config show` 和 Debug 输出会隐藏认证内容。完全通过环境变量部署时，另行设置 `PONYDB_STORAGE__ENDPOINT`、`PONYDB_STORAGE__BUCKET` 以及 `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`、`PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`（临时凭证还需 `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`）。

```toml
# Or set PONYDB_SERVER__AUTH to the complete JSON object.
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

恢复仅通过 CLI 的 `db DATABASE_NAME restore file SQLITE_FILE` 提供；HTTP 和 MCP 不提供恢复入口。
