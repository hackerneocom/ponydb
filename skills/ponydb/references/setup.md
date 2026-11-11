# Install PonyDB for an AI Agent

Use this runbook when a user asks you to install PonyDB, configure its storage, or connect it to an AI Agent client. Follow the user's language and complete only the requested scope.

For configuration, follow this sequence: reuse or create minimal settings, guide local secret entry when needed, validate, and hand off the observed results.

Use this runbook to track setup progress. Read linked references only for the
current step, then continue here from that step. Links back to this runbook or
its external entry point do not require rereading it or restarting setup. The
copyable installation prompts in the client guide are for users starting a new
request; do not execute them as instructions within an ongoing setup.

## General rules

- Inspect the environment before making changes. Prefer existing installations and configurations.
- Preserve unrelated AI Agent client settings. Never overwrite user configuration without explicit permission.
- Use documentation matching the selected PonyDB release.
- Ask only for missing non-secret information required to proceed. Never ask users to send credentials, API keys, access tokens, or completed credential files through chat. Guide them to enter secrets locally or through the client's credential mechanism.
- Treat accessible credential files and environment variables as sensitive. Pass configuration paths or environment variable names to tools instead of reading secret values into the conversation. Use `config show` for redacted inspection; never print, log, or commit secrets, including through command text or shell tracing.
- Do not create or delete databases without authorization. Installation alone does not authorize a setup database.
- Distinguish PonyDB installation, configuration, storage validation, Agent connectivity, query verification, and skill installation. Report only outcomes actually verified.
- If tools, permissions, or credentials are unavailable, explain the blocker and report completed steps without claiming full success.

### Handle execution restrictions

Keep the intended installation environment and persistent destinations when an operation is blocked. Authorization to install PonyDB does not itself grant the Agent's tools access outside their sandbox.

- If filesystem access, PATH updates, or network requests are blocked by the execution environment, use its approval mechanism for the specific required operation when supported and permitted. Explain the operation, destination or network endpoint, and why access is needed. Retry after approval; do not default to administrator access or disabling the sandbox.
- If approval is unavailable, prohibited by session policy, or denied, explain the restriction and provide exact commands or local editing steps for the user to perform in the intended environment. Continue independent work and leave dependent steps pending. Do not repeatedly request unavailable permissions or treat an unanswered request as approval.
- Temporary directories may hold downloads and extracted packages. Do not silently substitute a temporary directory, workspace, or application-private location for the intended persistent executable or configuration location. If a different destination or installation scope is needed, explain the change and obtain agreement unless already authorized.
- Distinguish execution restrictions from other failures such as an invalid URL, TLS errors, or rejected storage credentials. Request broader execution access only when evidence indicates it is needed; never expose secrets while diagnosing failures.

### Ask questions users can answer directly

Prefer short questions in normal conversation so the user can reply directly. Use a dedicated question tool only when the current client clearly displays the question and choices. If its presentation is unknown or questions appear only in a queued-input indicator, include the question in the visible handoff message.

- Ask one focused question per turn, covering only the next missing fact or decision. Do not combine configuration paths, provider and bucket details, alternative service URLs, or skill scope into one question. Use any details the user volunteers without asking for them again.
- Recommend a suitable path and explain it briefly. Ask only for information that inspection or known provider details cannot establish; do not ask the user to design the configuration.
- Allow answers such as “I don't know” or “I don't have one.” Follow the corresponding preparation step instead of repeating a checklist of required settings.
- A tool receipt such as `accepted: true` means the question was submitted, not answered or authorized. Continue independent work while waiting; leave dependent actions pending until the required answer arrives.

## 1. Inspect and choose a connection

Identify the OS, CPU architecture, AI Agent client and version, and where its tools run. Check whether `ponydb version` already works in that environment. Inspect existing PonyDB configuration and client connections without exposing secrets.

Identify the intended runtime and user account: the host terminal, Agent shell, WSL, container, MCP client process, or service host. Establish persistent executable and configuration destinations and whether the user needs `ponydb` available by name in a host terminal. Infer these from the request and existing setup where possible; ask only when the target remains ambiguous. Inspect available filesystem, network, and approval capabilities before dependent operations. A working command in the Agent's environment does not establish host availability.

Choose based on the user's goal and the capabilities of the environment where tools run:

| Connection | Use when | Requirements |
| --- | --- | --- |
| CLI | The Agent can execute shell commands | PonyDB and S3 credentials where those commands run |
| stdio MCP | The client can launch a local PonyDB process | PonyDB and S3 credentials accessible to that process |
| HTTP MCP | The client will connect to an existing or explicitly requested HTTP service | Service URL, token source, and a compatible client; remote clients need no local PonyDB binary or S3 credentials |

CLI and MCP can coexist. Do not modify a working connection solely to switch transport modes.

HTTP MCP requires protocol version `2026-07-28`, per-request metadata, and method-specific headers. Check the [HTTP MCP requirements](agent-clients.md#http-mcp) against the actual client version before configuring it. A saved URL or successful curl probe does not prove client compatibility.

After inspection, recommend a connection mode and ask only about its next missing prerequisite. For CLI or stdio MCP without a usable configuration, ask: “Do you already have an S3 bucket available for PonyDB?” Wait for the answer before asking for provider or bucket details. Do not also ask for a configuration path or HTTP service URL when following this path. Adapt to an existing configuration or service when one is discovered or the user mentions it.

For direct storage access, resolve missing settings progressively: provider, existing bucket, endpoint, intended prefix, signing region, and credential source. Ask for only the next unresolved value, deriving settings from known provider details where possible. Preserve an existing prefix; when choosing a new one, explain the proposed value, including an empty prefix, and confirm the user's intent.

If the user has no bucket, recommend managed cloud object storage first and guide preparation using [object storage configuration](configuration.md#object-storage). Start with one question, such as “Do you already have a cloud provider account?” Reuse that provider if explicitly supported and suitable; otherwise recommend a supported provider based on known location and access requirements, asking only for missing information needed to choose. Once the provider is selected, guide bucket preparation before requesting connection details or local credential entry.

Limit provider recommendations and examples in questions to the [explicitly supported services](configuration.md#explicitly-supported-services). S3 compatibility alone does not establish PonyDB support. If the user mentions an unlisted provider, explain that it is outside the documented support list and do not promise compatibility. A successful probe alone does not add a provider to that list.

Suggest local or self-hosted storage only when the user requests it or known requirements call for it. A missing bucket or a desire to try PonyDB does not by itself justify defaulting to a local RustFS deployment. Do not make the user choose between local and cloud storage before recommending the cloud path.

Obtain authorization before creating cloud resources or deploying storage, reusing any authorization already given for that scope. Continue independent installation work, but leave storage configuration and live checks pending until the required resources are available.

For an existing HTTP service, establish the service URL and token source without requesting the token value. Configure storage on the service host only when deploying or changing that service is requested.

## 2. Install where needed

Reuse a working installation unless the requested setup requires a different version. For a new installation, establish the requested release or select a published stable release before reading detailed setup references. For CLI or stdio MCP, install where the Agent's commands or client process run. For a new HTTP service, install on the service host.

Follow [CLI installation](installation.md): select the release package for the OS and architecture, verify it against `SHA256SUMS`, extract the binary, and place it in the intended persistent installation directory. If no suitable package is available, follow the source-build instructions from the selected release's checkout. Verify the installed executable with its absolute path and the `version` subcommand. Running a binary from the download or extraction directory verifies that binary, not a completed installation.

For command-name CLI use, add the installation directory to the intended user's persistent PATH, preserving existing entries and avoiding duplicates. A change to the current process PATH is only a session convenience. Verify command resolution and `ponydb version` from a fresh terminal in the intended environment; check that the resolved executable is the installed one. On Windows, prefer user PATH over system PATH unless the requested scope requires otherwise. An Agent-launched child shell can inherit its parent's environment, so it is not sufficient proof that an independently opened host terminal will resolve the command.

For stdio MCP, configure an absolute executable path; PATH modification is not required solely for that connection. If required PATH access is blocked, follow [execution restrictions](#handle-execution-restrictions). Continue checks using the installed absolute path where possible, but report PATH setup separately as pending until verified.

Read linked documentation from the same release tag or checkout as the selected binary. Read bundled references locally when available. When following links remotely, resolve them relative to this file in that version of the PonyDB repository. Read only the references needed for the selected mode and current step; CLI setup does not require reviewing every MCP client's settings or maintainer release procedures.

## 3. Configure the selected mode

Use only the settings needed for the selected mode. Keep built-in defaults where they fit the environment; do not copy every optional setting from the annotated template.

Before changing an existing PonyDB or client configuration, prepare the exact changes. Apply changes already covered by the user's authorization without asking again; obtain permission before overwriting values outside that scope. Merge only the intended settings and preserve unrelated entries. Do not replace an entire configuration file to add a connection.

### CLI or stdio MCP

Follow [object storage configuration](configuration.md). Reuse a usable configuration. Before preparing a new file, check the provider's endpoint, signing region, and addressing requirements in that reference.

Before local credential entry, explain that the credential's identity needs List/Get/Put/Delete access to the intended bucket and prefix; read-only access is insufficient. Give the user the provider's official authorization guide, such as [TOS IAM policies (Chinese)](https://docs.volcengine.com/docs/torch-object-storage/access-control-iam-policy?lang=zh). If they need help, provide scoped policy and binding steps before the live probe. Use the provider's own action names and resource syntax; creating an IAM policy alone does not grant access until it is attached to the intended identity. Avoid unrestricted administrative grants.

When a new configuration is needed:

1. Reuse the user's credential mechanism when available. Otherwise, recommend the documented [platform default path](configuration.md#configuration-file) for the intended runtime and user account rather than asking the user to choose between files and environment variables. Once the endpoint, existing bucket, and intended prefix are established, create the file there, or use an agreed persistent private location outside the repository selected with an absolute `PONYDB_CONFIG` or `--config` path. Follow [execution restrictions](#handle-execution-restrictions) if access is blocked. Restrict file access to the intended user or service account.
2. Fill in known non-secret settings, including the endpoint, existing bucket, and intended prefix. Add region or addressing overrides only when the defaults do not fit the provider. Preserve the intended prefix even when it differs from the default.
3. Leave clearly marked credential placeholders for local editing, or omit credential fields when credentials will be injected through PonyDB's supported environment variables.
4. Give the user the absolute file path and exact field names or environment variable names to fill in. Have them edit the file locally or use their existing secret mechanism. Do not ask them to paste the resulting file or secret values into chat. Explain that they can reply “credentials entered” when done and that you will then validate storage.
5. If user entry is needed, wait for confirmation that it is complete before running live checks. Placeholders are not working credentials. Continue independent setup while waiting and report dependent checks as pending. Do not present a configuration with an unknown endpoint or bucket as ready for credential entry.

On Windows, verify the configuration destination before directing the user to enter secrets. Packaged desktop applications can redirect AppData writes into application-private storage. A path containing `LocalCache\Roaming`, for example, is a reason to check visibility; do not assume every client or Windows version behaves identically. Expanding `%APPDATA%`, using an absolute path, or successfully writing and reading the file from the same Agent environment does not prove it exists at the host user's expected location.

For a host installation, check file visibility from an independently opened host terminal and use the intended PonyDB executable's redacted `config show --target cli` output to check the non-secret settings. Use the same configuration selection that ongoing use will rely on, including default discovery when applicable. Ask the user to perform this check if no suitable host execution tool is available; do not request file contents or credentials. Merely launching another shell from the desktop app or receiving sandbox approval does not prove that application-level redirection has ended.

If the default location is redirected, arrange creation in the intended host location or agree on a persistent private location accessible to both the user and the runtime, using `--config` or `PONYDB_CONFIG`. Update the actual CLI or MCP invocation consistently. Do not hand off an unverified application-private path as the host default. Until checked, report “configuration accessible in the Agent environment; host visibility unverified.”

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

Follow [HTTP deployment](http.md) for a release binary or container. Reuse an existing service when it meets the user's goal. On the service host, prepare minimal storage and server authentication settings using the same local secret-entry procedure. Prefer a principal limited to the intended databases and operations; do not copy unrestricted administrative grants unless needed for the requested use.

Use placeholders or supported environment injection for both S3 credentials and server tokens. Tell the user where to supply each secret and which client must receive the matching Bearer token. When generating a token locally, write it directly to its intended private destination without displaying it in tool output. Validate the server configuration after any required user entry is complete.

## 4. Validate storage

For direct storage access, inspect the effective configuration with the redacted `config show` output. Run the live probe only after required credentials are available:

```sh
ponydb --config /absolute/path/ponydb.toml config show --target cli
ponydb --config /absolute/path/ponydb.toml config validate --target cli
```

Run these checks in the intended runtime with the installed executable and configuration selection that ongoing use will rely on. Use its absolute executable path if PATH is not yet verified. Use `--target server` on the service host when validating a deployment; it also checks server authentication. Adapt these commands if the setup uses environment variables or default file discovery instead of an explicit file path. A successful probe in a temporary setup environment does not verify the target runtime.

`config show` works offline and redacts secrets. Success means the configuration loaded, not that the endpoint, credentials, permissions, or storage capabilities are usable. Report “configuration loaded; live storage validation pending” until the live probe succeeds, and report file creation separately.

`config validate` performs real reads, writes, conditional writes, listing, and deletion under a temporary probe prefix within the configured storage prefix. It requires List/Get/Put/Delete permissions and cleans up its probe objects; an interrupted run can leave probe objects behind. It does not create a database.

If validation fails, resolve the failed permission, addressing, or conditional-write check before continuing with that storage setup. Never bypass a failed storage contract. When connecting to an existing HTTP service without service-host access, report storage validation as not independently verified.

## 5. Verify Agent connectivity

Follow [AI Agent clients](agent-clients.md) for the selected client's configuration, preserving existing entries:

- **CLI:** run `ponydb --config /absolute/path/ponydb.toml databases list` through the Agent's shell tools. A version check alone does not establish storage access.
- **stdio MCP:** use absolute executable and configuration paths, with `mcp` as the final argument. Let the client launch the process. Reload if needed and discover the available MCP tools from the actual client.
- **HTTP MCP:** use the [HTTP probe](agent-clients.md#http-mcp) to diagnose authentication and protocol access, then discover the available MCP tools from the actual client. Confirm actual client compatibility before reporting a successful connection.

For either MCP transport, call `database_list` only if authorized. It requires `db:list`; a principal with `db:read` and `data:read` on a specific database can query that database without permission to list databases. If `database_list` returns `FORBIDDEN`, report the authorization restriction without treating it as a connection failure. Continue with an explicitly authorized database in Section 6. Report tool discovery, authorization, and query execution as separate results.

If the actual client cannot be exercised, report the checks completed and leave Agent connectivity unverified.

## 6. Verify a query

Use a user-selected existing database that the principal is explicitly authorized to query and run `SELECT 1` through the selected Agent connection. Database listing is not a prerequisite:

- **CLI:** `ponydb --config /absolute/path/ponydb.toml db USER_SELECTED_DATABASE execute 'SELECT 1'`.
- **MCP:** call `sql_execute` with `{"database":"USER_SELECTED_DATABASE","sql":"SELECT 1"}`.

Check that the response contains the integer result `1`. Do not modify user tables to test connectivity.

Once the connection is ready, if no authorized database has been identified, ask which existing database to use and explicitly allow “I don't have one” or “I don't know.” A forbidden database listing does not mean no suitable database exists. If no suitable database exists, propose a setup database named `ponydb_setup_tmp` and ask for authorization to create it, unless the user has already authorized it. Without authorization, leave databases unchanged and report query verification as pending. Do not delete a setup database without authorization.

## 7. Reuse or install the PonyDB skill

If the target Agent is already using this installed skill, reuse it and skip installation. Do not ask for its installation scope again. Install a separate copy only when the requested setup targets another Agent or discovery environment that lacks the skill.

For a full Agent setup, include installation of the [PonyDB skill](agent-clients.md#install-the-ponydb-skill) to guide ongoing database work. Do not ask whether to install it. Reuse an existing installation in the intended scope. If the user requested only a storage configuration or connection change, keep skill installation outside that limited scope unless requested.

Before installing, ask only whether to install globally across the user's projects or only in the current project, unless the user has already specified the scope. Recommend global installation so the skill is available across projects, and present it first. For example: “I recommend installing the PonyDB skill globally so you can use it across projects. Should I install it globally or only in the current project?” Once the user selects the scope, proceed without a separate installation confirmation.

Present this scope choice separately from required connection questions, preferably after the connection works or while waiting for local credential entry. Wait for the scope selection before installing; a recommendation alone is not a selection. Continue independent storage validation or query verification while waiting.

Install the skill in the environment where the target AI Agent discovers skills. This may differ from the environment where the PonyDB CLI or HTTP service runs. For example, an Agent running on Windows may discover skills on Windows while operating PonyDB inside a WSL2 container.

Follow the linked guide's matching command or manual installation procedure in the skill discovery environment, and select only the intended Agent. Keep the complete skill directory, including its references, together.

Reload the Agent if required and verify that it discovers the `ponydb` skill. If the Agent does not support skills, report this step as not applicable. If discovery cannot be checked in the actual Agent, report it as pending. Skill discovery and database query verification are separate checks; neither proves the other succeeded.

## 8. Hand off

Report each stage as verified, pending, failed, or not applicable: PonyDB installation, configuration, storage validation, Agent connectivity, query verification, and skill installation. Include the observed version, selected mode, configuration location, database used, and query result where verified. Report the skill's project or global scope, installation location, and discovery result when applicable. For an existing HTTP service, distinguish the service version from any local CLI version.

For local installations, include the persistent executable path, intended runtime and user account, and environment in which checks ran. Report executable verification, persistent PATH availability when required, and configuration visibility in the target environment separately. A temporary binary, process-only PATH change, or configuration visible only inside the Agent cannot establish a completed host installation. Name any remaining permission or visibility blocker and the exact next action needed.

For MCP, report tool discovery, operation authorization (including any `database_list` denial or skipped check), and query execution separately. A forbidden database listing must not mark Agent connectivity as failed when tool discovery succeeds; query access is verified only by a successful query on an explicitly authorized database.

Name any setup database created and describe the non-secret settings applied. Give absolute configuration paths where applicable. For unfinished secret entry, name the exact fields or environment variables and their local destination, leaving the dependent validation and connection checks pending. Explain remaining blockers and the next required action without including secret values.

When waiting for input, keep the immediate question or local action visible in the handoff and state what you will do after the reply. Ask only for the next missing information rather than repeating the full setup questionnaire. Distinguish a proposed configuration location from a file actually created, and a prepared file from a validated configuration.

End a successful setup handoff with 3–5 natural-language prompts the user can send next, introduced by “You can say to me:” in the user's language. Choose examples that fit the verified connection and available permissions. Use known database and table names where available; clearly mark any placeholders the user must replace. Examples include:

- “Show me the tables in `DATABASE_NAME` and explain their structure.”
- “Show me up to 10 rows from `TABLE_NAME` in `DATABASE_NAME`.”
- “Use PonyDB to create an `agent_context` database with a `memories` table containing an ID, topic and content. Save my preference: documentation should be concise. Then query that preference back.”
- “Query `agent_context` for my documentation preferences and use them for this task.”

Present the [context-saving example](agent-clients.md#try-it-after-setup) only when the connection permits creating databases and writing data. Explain that the later-session retrieval example requires the context to have been saved and PonyDB to be configured in that session. These are suggestions for the user's next request; do not execute them as part of installation without authorization. If setup remains incomplete, prioritize the next step needed to unblock it and make any usage examples conditional on completing setup.

For subsequent use, see [CLI usage](cli.md) and [SQLite compatibility](sqlite.md). Repeated writes execute again; inspect business data before deciding to retry a write that returns `OUTCOME_UNKNOWN`. Use typed SQL parameters and statement arrays for atomic work; user SQL cannot manage transactions or load extensions directly.
