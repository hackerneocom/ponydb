<div align="center">

# PonyDB

**Serverless SQLite for AI Agents.**

<img src="docs/assets/logo.png" alt="ponydb 标志" width="520">

![CI](https://github.com/hackerneocom/ponydb/actions/workflows/ci.yml/badge.svg)
![License: AGPL-3.0-only](https://img.shields.io/badge/license-AGPL--3.0--only-blue)

[English](README.md) · **简体中文**

[CLI](skills/ponydb/references/cli.zh-CN.md) · [HTTP / Docker](skills/ponydb/references/http.zh-CN.md) · [AI Agent 接入](skills/ponydb/references/agent-clients.zh-CN.md) · [配置](skills/ponydb/references/configuration.zh-CN.md)

</div>

ponydb 是基于 SQLite 和对象存储的轻量级事务 SQL 数据库，也是可自行托管的 Cloudflare D1 开源替代，提供 CLI、HTTP API 和供 AI Agent 使用的 MCP 工具。

## 为什么选择 ponydb

- **跨设备使用同一个数据库。** 数据实时持久化在对象存储桶中，其他设备使用相同配置即可实时读写相同的数据库。
- **多个客户端并发读写。** 支持参数化 SQL 和原子事务，通过乐观并发控制提交写入。
- **数据库数量理论上不限。** 每个数据库以轻量的头文件管理，彼此独立，可创建成千上万个。
- **无需常驻数据库服务。** 支持 cli 直接读写数据库。使用对象存储时，只需要为存储和请求按用量计费；闲置时仅产生数据存储费用。

| 交互形式                          | 接入方式                                                   | 使用场景                                                                                                                                                   |
| ----------------------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [CLI](skills/ponydb/references/cli.zh-CN.md)      | —                                                      | 在终端或脚本中直接读写数据库。                                                                                                                                        |
| [HTTP 服务](skills/ponydb/references/http.zh-CN.md) | —                                                      | 为应用提供远程 HTTP API，由服务端统一管理对象存储凭证与访问权限。                                                                                                                  |
| **AI Agent**                     | [CLI](skills/ponydb/references/agent-clients.zh-CN.md#在-ai-agent-中使用-cli)     | 可执行 Shell 命令时，推荐直接使用 CLI。                                                                                                                              |
|                               | [HTTP MCP](skills/ponydb/references/agent-clients.zh-CN.md#http-mcp)       | 无法安装 ponydb CLI 时，推荐在 Serverless 平台上通过 [Docker 容器](skills/ponydb/references/http.zh-CN.md#docker)部署 HTTP 服务，再通过 HTTP MCP 接入。AI Agent 客户端需支持 ponydb 的 HTTP MCP 协议，只需配置服务地址与令牌。 |
|                               | [CLI stdio MCP](skills/ponydb/references/agent-clients.zh-CN.md#stdio-mcp) | 可以安装 ponydb CLI，但不允许 AI Agent 执行 Shell 命令时，推荐由 AI Agent 客户端启动本地 CLI 进程，通过 stdio 调用 MCP 工具，无需常驻 HTTP 服务。                                                      |

适用场景：

- **AI Agent 上下文：** 持久化保存记忆、任务、事实和工具结果。
- **个人数据库：** 跨设备使用的个人应用数据集，例如笔记、待办、财务数据等。

## 快速开始

### 通过 AI Agent 安装

把下面的提示词交给 Claude Code、Codex、WorkBuddy 或 DeepSeek Harness：

```text
请阅读 https://raw.githubusercontent.com/hackerneocom/ponydb/main/skills/ponydb/references/setup.md ，帮我为当前 AI Agent 安装 ponydb。根据我的环境选择 CLI、stdio MCP 或已有的 HTTP MCP 服务，完成配置并验证一次查询；缺少连接信息时请向我询问。
```

手动操作见 [AI Agent 接入指南](skills/ponydb/references/agent-clients.zh-CN.md)，覆盖 CLI 调用、各客户端 stdio MCP 配置、HTTP MCP 要求和使用提示词。远程 HTTP MCP 客户端只需要服务地址与令牌；在服务主机安装 CLI，或使用 Docker 部署即可。

### 手动安装

#### 1. 准备对象存储

创建存储桶、访问凭证，并访问凭证授予 List/Get/Put/Delete 权限。

支持以下对象存储：

- [RustFS](https://rustfs.com/)（自托管推荐）
- [火山引擎 TOS](https://www.volcengine.com/product/tos)（中国区域推荐）
- [Cloudflare R2](https://www.cloudflare.com/products/r2/)（非中国区域推荐）
- [MinIO](https://www.min.io/)
- [AWS S3](https://aws.amazon.com/s3/)

> ponydb 依赖对象存储的条件写入、强写后读一致性等特性，并非所有对象存储服务均支持。详情见[存储要求与服务商设置](skills/ponydb/references/configuration.zh-CN.md#对象存储)。

#### 2. 安装 CLI

从 [GitHub Releases](https://github.com/hackerneocom/ponydb/releases) 下载适合你的操作系统（Linux、macOS 或 Windows）和 CPU 架构的 CLI 安装包。解压并将 `ponydb`（Windows 为 `ponydb.exe`）加入 `PATH`，然后验证安装：

```sh
ponydb version
```

平台选择与校验见[安装指南](skills/ponydb/references/installation.zh-CN.md#下载-cli)。

#### 3. 配置并查询

创建 `ponydb.toml`，填入以下存储配置，并将端点、存储桶和凭证替换为实际值：

```toml
[storage]
endpoint = "https://<s3-endpoint>"
bucket = "my-bucket"
# 签名地域，默认为 us-east-1；按服务商要求修改，Cloudflare R2 使用 auto。
region = "us-east-1"
# 路径式寻址，默认为 true；端点要求虚拟主机式寻址时设为 false。
force_path_style = true
# 桶内对象键前缀，默认为空（不添加前缀）。
prefix = ""

[storage.credentials]
access_key_id = "<access-key-id>"
secret_access_key = "<secret-access-key>"
# 仅使用临时凭证时需要，默认不设置。
# session_token = "<session-token>"
```

更多内容见[配置](skills/ponydb/references/configuration.zh-CN.md)。

以下命令使用 POSIX shell；Windows 请使用对应终端的环境变量语法。

```sh
export PONYDB_CONFIG="$PWD/ponydb.toml"
ponydb config validate --target cli
ponydb databases create notes --description "My notes"
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"
ponydb db notes execute "INSERT INTO notes(title) VALUES (?)" \
  --param '{"type":"text","value":"First note"}'
ponydb db notes execute "SELECT id, title FROM notes" --output table
```

查询返回 `1` 和 `First note`。数据库管理、事务、快照、备份和维护的完整用法见 [CLI 指南](skills/ponydb/references/cli.zh-CN.md)。

## SQLite 特性与边界

内置 SQLite 支持 **JSON / JSONB** 和**数学函数**，可使用普通表、索引、视图、触发器、外键与原子语句数组。示例与限制见 [SQLite 兼容性指南](skills/ponydb/references/sqlite.zh-CN.md)。

用户 SQL 暂不支持 `ATTACH` / `DETACH`、`PRAGMA`、显式事务控制、`VACUUM`、动态加载扩展和 `CREATE VIRTUAL TABLE`（包括 FTS 与 R-tree 表）。需要本地 SQLite 工具时，可导出标准 SQLite 文件。桶内保存的是 ponydb 的版本化格式，不能直接挂载为 `.sqlite` 文件；在线访问需要网络连接。

## 与 LiteFS、Litestream 的比较


|      | ponydb                     | LiteFS             | Litestream                |
| ---- | -------------------------- | ------------------ | ------------------------- |
| 主要用途 | 对象存储上的事务 SQLite            | 在应用节点之间复制 SQLite   | 持续备份 SQLite               |
| 在线数据 | 对象存储；本地工作区可丢弃              | 节点间复制的本地 SQLite 文件 | 本地 SQLite 文件；异步备份到副本存储    |
| 写入方式 | 多客户端提交，冲突的提交失败             | 由单个主节点处理写入         | 应用写本地 SQLite；备份复制不协调独立写入方 |
| 常驻组件 | CLI 配合对象存储无需常驻进程；HTTP 服务可选 | LiteFS 节点与主节点租约管理  | 后台复制进程                    |
| 适用场景 | 跨设备的 AI Agent 上下文与个人数据        | 需要本地只读副本的应用        | 现有 SQLite 应用的灾难恢复         |


## 开发

环境搭建与验证见[开发环境](.devcontainer/README.md)、[测试指南](tests/README.md)和[仓库指南](AGENTS.md)。模块职责见各 crate 的 README，可从 [database-service](crates/database-service/README.zh-CN.md)、[protocol](crates/protocol/README.zh-CN.md)和 [runtime-config](crates/runtime-config/README.zh-CN.md) 开始。

## 许可证

[AGPL-3.0-only](LICENSE)。

> 可免费使用，ponydb 不收取软件许可费用。使用、修改与再分发须遵守 AGPL-3.0-only。
