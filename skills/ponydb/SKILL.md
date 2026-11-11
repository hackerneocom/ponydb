---
name: ponydb
description: >
  Install, configure, and use PonyDB, a SQLite database backed by S3-compatible
  object storage. Use for PonyDB CLI or MCP setup, database management, SQL,
  snapshots, backups, maintenance, and troubleshooting.
---

# PonyDB

Use PonyDB for the user's requested database work. The references are the same
usage guides read by people; do not load them all for every task.

If there is no usable connection, a required local executable is missing, storage needs
initial configuration, or the user requests installation or reconfiguration, read
[Setup](references/setup.md) before making setup changes. Follow that runbook
only for the requested scope, loading technical references as needed. Reuse this
skill when it is already installed in the target Agent; skill installation alone
does not install the executable or configure database access.

## Choose a connection

Reuse an existing working CLI or MCP connection. When setting up a connection,
prefer the CLI if the Agent can execute shell commands, stdio MCP if the client
can launch a local process, or an existing HTTP MCP service for remote access.
Only deploy a service when requested. Remote MCP clients need the service URL
and token, not a local PonyDB binary or S3 credentials.

Use the explicitly selected database. If the task leaves the database ambiguous,
list databases only if authorized and ask which one to use before changing data.
Without listing permission, ask for an explicitly authorized database name.

## Read for the task

Read only the relevant English reference or section below. Chinese translations
are available for people; answering in Chinese does not require reading both.

| Task | Read |
| --- | --- |
| Install, configure, or connect PonyDB | [Setup workflow](references/setup.md), then the relevant references below |
| Install PonyDB | [Installation](references/installation.md) |
| Configure storage | [Configuration](references/configuration.md) |
| Connect an AI Agent | [Agent clients](references/agent-clients.md) |
| Diagnose configuration or storage access | [Configuration](references/configuration.md#check-and-adjust-settings) |
| Use CLI arguments, request files, or database management | [CLI conventions](references/cli.md#common-conventions) and the relevant command under [database management](references/cli.md#database-management) |
| Execute SQL or inspect schema | [SQL and schema](references/cli.md#sql-and-schema), [SQLite compatibility](references/sqlite.md); for MCP, use the discovered tool's input schema |
| Read history, export, restore, or reclaim storage | [Snapshots](references/cli.md#snapshots), [backup and restore](references/cli.md#backup-and-restore), or [maintenance](references/cli.md#online-maintenance), as requested |
| Connect or troubleshoot MCP | [Agent clients](references/agent-clients.md#choose-a-connection), then the selected transport's section; check HTTP protocol compatibility before configuring a client |
| Deploy an explicitly requested HTTP service or use its REST API | [HTTP guide](references/http.md) |
| Interpret CLI results, exit codes, or uncertain writes | [Output and scripting](references/cli.md#output-and-scripting) |

Check `ponydb version` when using a local executable. These references describe
the version of the checkout they came from. If the installed release differs,
consult that release's documentation and CLI help or discovered MCP tool schemas.
For external repository links using `main`, select the matching tag or commit
when working with a release. If required documentation is unavailable, report
that limitation rather than guessing unsupported commands or protocol behavior.

## Operate and verify

- Stay within the user's request and existing authorization. Installation alone
  does not authorize creating a test database, modifying tables, or deleting data.
  Ask before destructive changes that are not already authorized.
- Preserve unrelated configuration. Have the user supply secrets through local
  configuration or the client's credential mechanism; do not reveal or commit
  credentials. Use `config show` for redacted diagnostics.
- Bind SQL values with typed parameters. Submit related statements as an atomic
  `statements` array; do not split SQL on semicolons or issue `BEGIN`/`COMMIT`.
  Use SQL limits or pagination because oversized results fail rather than truncate.
- Repeated writes execute again. If a response is lost or returns
  `OUTCOME_UNKNOWN`, inspect business data before deciding whether to retry.
  For conflicts, reread the relevant state before deciding on another write.
- Verify the requested operation through the selected connection and inspect its
  actual result. A version check, saved client configuration, or successful HTTP
  probe alone does not establish that the Agent can query a database. Distinguish
  verified results from pending checks and explain any remaining blocker.
- For MCP, discover tools from the actual client and call `database_list` only if
  authorized. A `FORBIDDEN` listing is an authorization restriction, not a connection
  failure. Verify query access on an explicitly authorized database and report
  tool discovery, authorization, and query execution separately.
