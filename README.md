<div align="center">

# PonyDB

**Serverless SQLite for AI Agents.**

<img src="docs/assets/logo.png" alt="ponydb logo" width="520">

[![CI](https://github.com/hackerneocom/ponydb/actions/workflows/ci.yml/badge.svg)](https://github.com/hackerneocom/ponydb/actions/workflows/ci.yml)
[![License: AGPL-3.0-only](https://img.shields.io/badge/license-AGPL--3.0--only-blue)](LICENSE)

**English** · [简体中文](README.zh-CN.md)

[CLI](skills/ponydb/references/cli.md) · [HTTP / Docker](skills/ponydb/references/http.md) · [AI Agent setup](skills/ponydb/references/agent-clients.md) · [Configuration](skills/ponydb/references/configuration.md)

</div>

A lightweight, transactional SQL database backed by SQLite and object storage. ponydb is an open-source, self-hostable alternative to Cloudflare D1, with a CLI, HTTP API and MCP tools for AI Agents.

## Why ponydb

- **Your database, across devices.** Data is persisted to an object storage bucket in real time. Other devices can use the same configuration to read and write the same databases in real time.
- **Read and write from multiple clients.** Run parameterized SQL and atomic transactions against shared storage. Concurrent writes use optimistic concurrency control.
- **Theoretically unlimited databases.** Each database is managed through a lightweight head file in object storage, so you can create thousands of independent databases.
- **No always-on database server.** Read and write databases directly through the CLI. With object storage, you only pay for storage and requests as used; when idle, only stored data incurs storage charges.

| Interface | Connection | Use case |
| --- | --- | --- |
| [CLI](skills/ponydb/references/cli.md) | — | Read and write databases directly from a terminal or scripts. |
| [HTTP service](skills/ponydb/references/http.md) | — | Provide a remote HTTP API for applications, with object storage credentials and access permissions managed on the server. |
| **AI Agents** | [CLI](skills/ponydb/references/agent-clients.md#use-the-cli-from-an-ai-agent) | Recommended when the AI Agent can run shell commands. |
| | [HTTP MCP](skills/ponydb/references/agent-clients.md#http-mcp) | When the ponydb CLI cannot be installed, deploy the HTTP service as a [Docker container](skills/ponydb/references/http.md#docker) on a serverless platform and connect through HTTP MCP. The AI Agent client must support ponydb's HTTP MCP protocol and only needs the service URL and token. |
| | [CLI stdio MCP](skills/ponydb/references/agent-clients.md#stdio-mcp) | Recommended when the ponydb CLI can be installed but the AI Agent is not allowed to run shell commands. The AI Agent client launches a local CLI process and calls MCP tools over stdio, without an always-on HTTP service. |

Use ponydb for:

- **AI Agent context:** persistent memories, tasks, facts and tool results.
- **Personal databases:** personal application datasets shared across devices, such as notes, to-do lists and financial data.

## Get started

### Install through an AI Agent

Ask Claude Code, Codex, WorkBuddy or DeepSeek Harness to set up ponydb:

```text
Read https://raw.githubusercontent.com/hackerneocom/ponydb/main/skills/ponydb/references/setup.md and help me install ponydb for this AI Agent. Choose CLI, stdio MCP or an existing HTTP MCP service for my environment, configure it, and verify a query. Ask me for any missing connection details.
```

For manual setup, see [AI Agent clients](skills/ponydb/references/agent-clients.md): shell-based CLI use, client-specific stdio MCP configuration, HTTP MCP requirements and example prompts. A remote HTTP MCP client needs only the service URL and token; install the CLI on the service host, or deploy the Docker image.

### Manual setup

#### 1. Prepare object storage

Create a bucket and access credentials, and grant the credentials List/Get/Put/Delete permissions.

Supported object storage services:

- [RustFS](https://rustfs.com/) (recommended for self-hosting)
- [Volcengine TOS](https://www.volcengine.com/product/tos) (recommended for China regions)
- [Cloudflare R2](https://www.cloudflare.com/products/r2/) (recommended for regions outside China)
- [MinIO](https://www.min.io/)
- [AWS S3](https://aws.amazon.com/s3/)

> ponydb requires object storage features such as conditional writes and strong read-after-write consistency, which not all object storage services support. See [storage requirements and provider settings](skills/ponydb/references/configuration.md#object-storage) for details.

#### 2. Install the CLI

Download the CLI package for your operating system (Linux, macOS or Windows) and CPU architecture from [GitHub Releases](https://github.com/hackerneocom/ponydb/releases). Extract it and add `ponydb` (`ponydb.exe` on Windows) to your `PATH`, then verify the installation:

```sh
ponydb version
```

See the [installation guide](skills/ponydb/references/installation.md#download-a-cli) for platform selection and checksum verification.

#### 3. Configure and query

Create `ponydb.toml` with this storage configuration, replacing the endpoint, bucket and credentials with your actual values:

```toml
[storage]
endpoint = "https://<s3-endpoint>"
bucket = "my-bucket"
# Signing region; defaults to us-east-1. Adjust for your provider; Cloudflare R2 uses auto.
region = "us-east-1"
# Path-style addressing; defaults to true. Set false if the endpoint requires virtual-hosted-style addressing.
force_path_style = true
# Object-key prefix within the bucket; defaults to empty (no prefix).
prefix = ""

[storage.credentials]
access_key_id = "<access-key-id>"
secret_access_key = "<secret-access-key>"
# Only needed for temporary credentials; unset by default.
# session_token = "<session-token>"
```

See [Configuration](skills/ponydb/references/configuration.md) for more details.

These commands use a POSIX shell; on Windows, use your shell's environment-variable syntax.

```sh
export PONYDB_CONFIG="$PWD/ponydb.toml"
ponydb config validate --target cli
ponydb databases create notes --description "My notes"
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT)"
ponydb db notes execute "INSERT INTO notes(title) VALUES (?)" \
  --param '{"type":"text","value":"First note"}'
ponydb db notes execute "SELECT id, title FROM notes" --output table
```

The query returns `1` and `First note`. Use the [CLI guide](skills/ponydb/references/cli.md) for database management, transactions, snapshots, backup and maintenance.

## SQLite features and limits

ponydb bundles SQLite with **JSON / JSONB** and **math functions**. Use standard tables, indexes, views, triggers, foreign keys and atomic statement arrays. Read the [SQLite compatibility guide](skills/ponydb/references/sqlite.md) for examples and limits.

User SQL currently cannot use `ATTACH` / `DETACH`, `PRAGMA`, explicit transaction control, `VACUUM`, loadable extensions or `CREATE VIRTUAL TABLE` (including FTS and R-tree tables). Export to a standard SQLite file when you need local SQLite tooling. Object storage contains ponydb's versioned format rather than a directly mountable `.sqlite` file; online access requires network connectivity.

## How it compares

| | ponydb | LiteFS | Litestream |
| --- | --- | --- | --- |
| Primary purpose | Transactional SQLite on object storage | Replicate SQLite across application nodes | Continuously back up SQLite |
| Live database | Object storage; disposable local workspace | Local SQLite files replicated between nodes | Local SQLite file; asynchronous backup to replica storage |
| Writes | Multiple clients submit writes; conflicting commits fail | Writes go to one primary node | Applications write to local SQLite; backup replication does not coordinate independent writers |
| Long-running components | None for CLI with object storage; HTTP service optional | LiteFS nodes and primary lease management | Background replication process |
| Best fit | AI Agent context and personal data across devices | Applications needing local read replicas | Recovery for existing SQLite applications |

## Development

For setup and validation, see the [development environment](.devcontainer/README.md), [test guide](tests/README.md) and [repository guide](AGENTS.md). Module responsibilities are documented in the crate READMEs, starting with [database-service](crates/database-service/README.md), [protocol](crates/protocol/README.md) and [runtime-config](crates/runtime-config/README.md).

## License

[AGPL-3.0-only](LICENSE).

> Free to use, with no ponydb license fee. Use, modification and redistribution are subject to the AGPL-3.0-only license.
