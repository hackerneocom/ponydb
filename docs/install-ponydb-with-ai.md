# Install PonyDB for an AI Agent

Use this runbook when a user asks you to install PonyDB, configure its storage, or connect it to an AI Agent client. Follow the user's language and complete only the requested scope.

For configuration, follow this sequence: reuse or create minimal settings, guide local secret entry when needed, validate, and hand off the observed results.

## General rules

- Inspect the environment before making changes. Prefer existing installations and configurations.
- Preserve unrelated AI Agent client settings. Never overwrite user configuration without explicit permission.
- Use documentation matching the selected PonyDB release.
- Ask only for missing non-secret information required to proceed. Never ask users to send credentials, API keys, access tokens, or completed credential files through chat. Guide them to enter secrets locally or through the client's credential mechanism.
- Treat accessible credential files and environment variables as sensitive. Pass configuration paths or environment variable names to tools instead of reading secret values into the conversation. Use `config show` for redacted inspection; never print, log, or commit secrets, including through command text or shell tracing.
- Do not create or delete databases without authorization. Installation alone does not authorize a setup database.
- Distinguish PonyDB installation, configuration, storage validation, Agent connectivity, query verification, and skill installation. Report only outcomes actually verified.
- If tools, permissions, or credentials are unavailable, explain the blocker and report completed steps without claiming full success.

## 1. Inspect and choose a connection

Identify the OS, CPU architecture, AI Agent client and version, and where its tools run. Check whether `ponydb version` already works in that environment. Inspect existing PonyDB configuration and client connections without exposing secrets.

Choose based on the user's goal and the capabilities of the environment where tools run:

| Connection | Use when | Requirements |
| --- | --- | --- |
| CLI | The Agent can execute shell commands | PonyDB and S3 credentials where those commands run |
| stdio MCP | The client can launch a local PonyDB process | PonyDB and S3 credentials accessible to that process |
| HTTP MCP | The client will connect to an existing or explicitly requested HTTP service | Service URL, token source, and a compatible client; remote clients need no local PonyDB binary or S3 credentials |

CLI and MCP can coexist. Do not modify a working connection solely to switch transport modes.

HTTP MCP requires protocol version `2026-07-28`, per-request metadata, and method-specific headers. Check the [HTTP MCP requirements](../skills/ponydb/references/agent-clients.md#http-mcp) against the actual client version before configuring it. A saved URL or successful curl probe does not prove client compatibility.

For direct storage access, establish the provider, endpoint, existing bucket, intended prefix, and credential source. Derive settings from known provider details and ask only for missing information; confirm the signing region when it cannot be determined. For an existing HTTP service, establish the service URL and token source without requesting the token value. Configure storage on the service host only when deploying or changing that service is requested.

## 2. Install where needed

Reuse a working installation unless the requested setup requires a different version. For CLI or stdio MCP, install where the Agent's commands or client process run. For a new HTTP service, install on the service host.

Follow [CLI installation](../skills/ponydb/references/installation.md): select the release package for the OS and architecture, verify it against `SHA256SUMS`, extract the binary, and put it on PATH. If no suitable package is available, follow the source-build instructions from the selected release's checkout. Run `ponydb version` to verify the executable.

Read linked documentation from the same release tag or checkout as the selected binary. When following links remotely, resolve them relative to this file in that version of the PonyDB repository.

## 3. Configure the selected mode

Use only the settings needed for the selected mode. Keep built-in defaults where they fit the environment; do not copy every optional setting from the annotated template.

Before changing an existing PonyDB or client configuration, prepare the exact changes and obtain permission to overwrite it. Merge only the intended settings and preserve unrelated entries.

### CLI or stdio MCP

Follow [object storage configuration](../skills/ponydb/references/configuration.md). Reuse a usable configuration. When a new configuration is needed:

1. Create it at the documented [platform default path](../skills/ponydb/references/configuration.md#configuration-file), or choose a private location outside the repository and select it with an absolute `PONYDB_CONFIG` or `--config` path. Restrict file access to the intended user or service account.
2. Fill in known non-secret settings, including the endpoint, existing bucket, and intended prefix. Add region or addressing overrides only when the defaults do not fit the provider. Preserve the intended prefix even when it differs from the default.
3. Leave clearly marked credential placeholders for local editing, or omit credential fields when credentials will be injected through PonyDB's supported environment variables.
4. Give the user the absolute file path and exact field names or environment variable names to fill in. Have them edit the file locally or use their existing secret mechanism. Do not ask them to paste the resulting file or secret values into chat.
5. If user entry is needed, wait for confirmation that it is complete before running live checks. Placeholders are not working credentials. Continue independent setup while waiting and report dependent checks as pending.

For example, with an AWS S3 bucket in `us-east-1`, default path-style addressing, and an intentionally empty prefix, the file needs only:

```toml
[storage]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket = "my-ponydb-bucket"

[storage.credentials]
access_key_id = "REPLACE_LOCALLY"
secret_access_key = "REPLACE_LOCALLY"
```

Replace the example endpoint and bucket with the user's values before handing over the file. The user fills in `storage.credentials.access_key_id` and `storage.credentials.secret_access_key` locally. Temporary credentials also require `storage.credentials.session_token`.

For environment injection, use `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`, `PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`, and, for temporary credentials, `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`. Ensure they are available to the process running PonyDB, including a client-launched stdio process or container. The CLI does not read `AWS_*` credentials or AWS credential files.

### Existing HTTP MCP service

Use the service URL and configure the Bearer token through the client's supported local credential mechanism. Give the user the destination setting or environment variable name and have them supply the value locally. No client-side storage configuration is required. Wait for any required local token entry before probing the service.

### Explicitly requested HTTP deployment

Follow [HTTP deployment](../skills/ponydb/references/http.md) for a release binary or container. Reuse an existing service when it meets the user's goal. On the service host, prepare minimal storage and server authentication settings using the same local secret-entry procedure. Prefer a principal limited to the intended databases and operations; do not copy unrestricted administrative grants unless needed for the requested use.

Use placeholders or supported environment injection for both S3 credentials and server tokens. Tell the user where to supply each secret and which client must receive the matching Bearer token. When generating a token locally, write it directly to its intended private destination without displaying it in tool output. Validate the server configuration after any required user entry is complete.

## 4. Validate storage

For direct storage access, inspect the effective configuration with the redacted `config show` output. Run the live probe only after required credentials are available:

```sh
ponydb --config /absolute/path/ponydb.toml config show --target cli
ponydb --config /absolute/path/ponydb.toml config validate --target cli
```

Use `--target server` on the service host when validating a deployment; it also checks server authentication. Adapt these commands if the setup uses environment variables instead of a file.

`config show` works offline and redacts secrets. `config validate` performs real reads, writes, conditional writes, listing, and deletion under a temporary probe prefix within the configured storage prefix. It requires List/Get/Put/Delete permissions and cleans up its probe objects; an interrupted run can leave probe objects behind. It does not create a database.

If validation fails, resolve the failed permission, addressing, or conditional-write check before continuing with that storage setup. Never bypass a failed storage contract. When connecting to an existing HTTP service without service-host access, report storage validation as not independently verified.

## 5. Verify Agent connectivity

Follow [AI Agent clients](../skills/ponydb/references/agent-clients.md) for the selected client's configuration, preserving existing entries:

- **CLI:** run `ponydb --config /absolute/path/ponydb.toml databases list` through the Agent's shell tools. A version check alone does not establish storage access.
- **stdio MCP:** use absolute executable and configuration paths, with `mcp` as the final argument. Let the client launch the process. Reload if needed and discover the available MCP tools from the actual client.
- **HTTP MCP:** use the [HTTP probe](../skills/ponydb/references/agent-clients.md#http-mcp) to diagnose authentication and protocol access, then discover the available MCP tools from the actual client. Confirm actual client compatibility before reporting a successful connection.

For either MCP transport, call `database_list` only if authorized. It requires `db:list`; a principal with `db:read` and `data:read` on a specific database can query that database without permission to list databases. If `database_list` returns `FORBIDDEN`, report the authorization restriction without treating it as a connection failure. Continue with an explicitly authorized database in Section 6. Report tool discovery, authorization, and query execution as separate results.

If the actual client cannot be exercised, report the checks completed and leave Agent connectivity unverified.

## 6. Verify a query

Use a user-selected existing database that the principal is explicitly authorized to query and run `SELECT 1` through the selected Agent connection. Database listing is not a prerequisite:

- **CLI:** `ponydb --config /absolute/path/ponydb.toml db USER_SELECTED_DATABASE execute 'SELECT 1'`.
- **MCP:** call `sql_execute` with `{"database":"USER_SELECTED_DATABASE","sql":"SELECT 1"}`.

Check that the response contains the integer result `1`. Do not modify user tables to test connectivity.

If no authorized database has been identified, ask the user for its name; a forbidden database listing does not mean no suitable database exists. If no suitable database exists, ask for authorization to create a named setup database, unless the user has already authorized it. Without authorization, leave databases unchanged and report query verification as pending. Do not delete a setup database without authorization.

## 7. Install the PonyDB skill

For a full Agent setup, use the [PonyDB skill](../skills/ponydb/references/agent-clients.md#install-the-ponydb-skill) to guide ongoing database work. Reuse an existing installation in the intended scope. If the user requested only a storage configuration or connection change, keep skill installation outside that limited scope unless requested.

Before installing, ask whether the skill should be available only in the current project or globally across the user's projects, unless the user has already specified the scope. Unless the user has already authorized skill installation, explain its purpose and intended installation scope and obtain authorization before proceeding.

Install the skill in the environment where the target AI Agent discovers skills. This may differ from the environment where the PonyDB CLI or HTTP service runs. For example, an Agent running on Windows may discover skills on Windows while operating PonyDB inside a WSL2 container.

Follow the linked guide's matching command or manual installation procedure in the skill discovery environment, and select only the intended Agent. Keep the complete skill directory, including its references, together.

Reload the Agent if required and verify that it discovers the `ponydb` skill. If the Agent does not support skills, report this step as not applicable. If discovery cannot be checked in the actual Agent, report it as pending. Skill discovery and database query verification are separate checks; neither proves the other succeeded.

## 8. Hand off

Report each stage as verified, pending, failed, or not applicable: PonyDB installation, configuration, storage validation, Agent connectivity, query verification, and skill installation. Include the observed version, selected mode, configuration location, database used, and query result where verified. Report the skill's project or global scope, installation location, and discovery result when applicable. For an existing HTTP service, distinguish the service version from any local CLI version.

For MCP, report tool discovery, operation authorization (including any `database_list` denial or skipped check), and query execution separately. A forbidden database listing must not mark Agent connectivity as failed when tool discovery succeeds; query access is verified only by a successful query on an explicitly authorized database.

Name any setup database created and describe the non-secret settings applied. Give absolute configuration paths where applicable. For unfinished secret entry, name the exact fields or environment variables and their local destination, leaving the dependent validation and connection checks pending. Explain remaining blockers and the next required action without including secret values. Offer the [context-saving prompt](../skills/ponydb/references/agent-clients.md#try-it-after-setup) as an optional next step; do not execute its writes as part of installation without authorization.

For subsequent use, see [CLI usage](../skills/ponydb/references/cli.md) and [SQLite compatibility](../skills/ponydb/references/sqlite.md). Repeated writes execute again; inspect business data before deciding to retry a write that returns `OUTCOME_UNKNOWN`. Use typed SQL parameters and statement arrays for atomic work; user SQL cannot manage transactions or load extensions directly.
