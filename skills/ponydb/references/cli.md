**English** | [中文](cli.zh-CN.md)

# CLI guide

The CLI can connect directly to object storage to operate on databases; no HTTP service is required.

## Install

Download a prebuilt CLI from [GitHub Releases](https://github.com/hackerneocom/ponydb/releases); Rust is not required:

1. Select a version and download the Linux x86_64/ARM64, macOS Intel/Apple Silicon or Windows x86_64 archive for your system, along with `SHA256SUMS`.
2. Place `ponydb` (`ponydb.exe` on Windows) in a directory on your `PATH`.
3. Run `ponydb version` to verify installation; see its input and output in the command reference below.

SQLite is bundled. See the [installation guide](installation.md) for download, checksum and installation commands, or [Docker](http.md#docker) for container deployment.

## Before you start

Follow [Object storage and configuration](configuration.md) to prepare a bucket, configure a file or environment variables, and check your settings.

Examples use a POSIX shell; adapt the syntax for Windows. Topics can be read independently.

## Command reference

| Topic | Function | Command |
| --- | --- | --- |
| Configuration and diagnostics | Show versions | [`version`](#version) |
| | Show effective configuration | [`config show`](#config-show) |
| | Validate configuration and storage capabilities | [`config validate`](#config-validate) |
| | Show command help | [`help` / `--help`](#help----help) |
| Database management | Create a database | [`databases create DATABASE_NAME`](#databases-create-database_name) |
| | List databases | [`databases list`](#databases-list) |
| | Show database details | [`db DATABASE_NAME`](#db-database_name) |
| | Update database settings and metadata | [`db DATABASE_NAME update`](#db-database_name-update) |
| | Logically delete a database | [`db DATABASE_NAME delete`](#db-database_name-delete) |
| SQL and schema | Execute SQL | [`db DATABASE_NAME execute`](#db-database_name-execute) |
| | Show schema | [`db DATABASE_NAME schema`](#db-database_name-schema) |
| Snapshots | List retained snapshots | [`db DATABASE_NAME snapshot list`](#db-database_name-snapshot-list) |
| | Show snapshot details | [`db DATABASE_NAME snapshot get SNAPSHOT_ID`](#db-database_name-snapshot-get-snapshot_id) |
| | Query data from a selected snapshot | [`db DATABASE_NAME execute --consistency snapshot --snapshot-id SNAPSHOT_ID`](#db-database_name-execute---consistency-snapshot---snapshot-id-snapshot_id) |
| Backup and restore | Export the current version | [`db DATABASE_NAME export SQLITE_FILE`](#db-database_name-export-sqlite_file) |
| | Export a selected snapshot | [`db DATABASE_NAME snapshot export SNAPSHOT_ID SQLITE_FILE`](#db-database_name-snapshot-export-snapshot_id-sqlite_file) |
| | Restore from a SQLite file | [`db DATABASE_NAME restore file SQLITE_FILE`](#db-database_name-restore-file-sqlite_file) |
| Online maintenance | Compact fragmented storage segments to improve read efficiency | [`db DATABASE_NAME compact`](#db-database_name-compact) |
| | Clean up expired history and unreachable objects to reduce object storage usage | [`db DATABASE_NAME gc`](#db-database_name-gc) |
| Server and MCP | Start the HTTP service (including HTTP MCP) | [`server`](#server) |
| | Start stdio MCP | [`mcp`](#mcp) |

Commands in the table omit the `ponydb` prefix. Replace `DATABASE_NAME`, `SNAPSHOT_ID`, and `SQLITE_FILE` with actual values.

### Common conventions

#### Global parameters

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--config CONFIG_FILE` |  | `PONYDB_CONFIG` or the platform default path | Select a TOML configuration file. Accepted by all commands; `version` does not read configuration. |
| `--output FORMAT` |  | `cli.output` (built-in `json`) | `json` or `table`. Rejected by `server` and `mcp`. |
| `--deadline-ms MILLISECONDS` |  | `requests.default_timeout_ms` (90000) | Per-request timeout; must be positive and within the configured maximum (built-in 1800000). Accepted by business commands and `mcp`, but rejected by `config`, `version`, `server`, and `restore file`. |
| `-h, --help` |  |  | Print help for the current command and exit. |

Global parameters may appear before or after subcommands; their applicability is checked against the final subcommand.

#### Database names and file inputs

Database names must be explicit positional arguments. They are case-sensitive and immutable. Quote names containing spaces.

`--input REQUEST_FILE|-` in a parameter table reads a complete JSON request. It may supply other parameters, but the database name must remain explicit and match `database` in the request (`name` for creation).

Place `--input` after the action, as in `db notes execute --input request.json`. `db notes --input details.json` is for database details only.

Unknown fields, duplicate keys, invalid types, and conflicting request/argument values are errors. At most one input can read stdin per invocation.

`--sql-file`, `--params-file`, and `--metadata-file` also accept `-`. SQLite export destinations and restore source files do not; restore also rejects `--input`.

### Configuration and diagnostics

#### version

Show program, SQLite, and protocol versions. Requires no storage configuration and has no command-specific parameters.

Example input:

```sh
# Check installed versions
ponydb version
```

Example output (excerpt):

```json
{
  "program": "0.1.1",
  "sqlite_version": "SQLITE_VERSION",
  "mcp_protocol_version": "2026-07-28"
}
```

`SQLITE_VERSION` is a version string placeholder.

#### config show

Show effective configuration with credentials and authentication tokens always redacted. Works offline.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--target SCOPE` |  | `cli` | `cli`, `server`, or `all`. Marks configuration sections inactive for that entry point. |

Example input:

```sh
# Show effective CLI configuration with secrets redacted
ponydb config show --target cli
```

Example output (excerpt):

```json
{
  "config": {
    "cli": {
      "output": "json"
    },
    "storage": {
      "credentials": {
        "access_key_id": "[REDACTED]",
        "secret_access_key": "[REDACTED]"
      }
    }
  },
  "entry": "cli",
  "inactive": [
    "server",
    "server.auth",
    "mcp"
  ]
}
```

#### config validate

Validate configuration and probe the configured storage prefix with real reads, writes, conditional writes, listing, and deletion.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--target SCOPE` |  | `cli` | `cli`, `server`, or `all`. `server` and `all` also check server authentication configuration. |

Example input:

```sh
# Validate CLI settings and storage capabilities
ponydb config validate --target cli
```

Example output:

```json
{
  "valid": true,
  "target": "cli",
  "storage_capabilities_verified": true
}
```

Probes require List/Get/Put/Delete permissions and include a temporary 2 MiB object. Probe objects are cleaned up after completion; interruption may leave them behind.

#### help / --help

Show command or subcommand help. Help uses native text and is unaffected by JSON output settings.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `help [COMMAND...]` |  |  | Show help for a command path, such as `help db execute`. |
| `-h, --help` |  |  | Append to a command; `--help` shows full help. |

Example input:

```sh
# List available commands
ponydb --help

# Inspect SQL parameters; ponydb help db execute also works
ponydb db notes execute --help
```

Example top-level help output (text excerpt):

```text
Usage: ponydb [OPTIONS] <COMMAND>

Commands:
  server     Serve HTTP and MCP.
  mcp        Serve MCP over stdin/stdout for agent clients.
  databases  Create and list databases.
  db         Inspect or operate on an explicitly named database.
  config     Show and validate the effective configuration.
  version    Show program, SQLite and protocol versions.
  help       Print this message or the help of the given subcommand(s)
```

### Database management

#### databases create DATABASE_NAME

Create a database at data version `1`. Snapshot retention defaults to `snapshots.retention_days` (7 days).

Database names contain 1–64 Unicode BMP characters and cannot contain NUL, `/`, `\`, `.`, or trailing ASCII spaces.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `DATABASE_NAME` | Yes |  | Unique, immutable database name. |
| `--description TEXT` |  | Empty string | Description; at most 16 KiB of UTF-8. |
| `--metadata-file METADATA_FILE\|-` |  | `null` | Custom metadata JSON; at most 64 KiB of compact UTF-8 JSON. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Save this as `metadata.json`:

```json
{"project": "notes"}
```

Example input:

```sh
# Create a database with a description and metadata
ponydb databases create notes --description "My notes" --metadata-file ./metadata.json
```

Example output:

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "notes",
  "data_version": "1"
}
```

#### databases list

List databases in the current storage namespace with pagination.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--limit NUMBER` |  | 50 | Maximum databases per page; range 1–1000. |
| `--cursor CURSOR` |  | Start from the first page | The previous response's `next_cursor`. |
| `--include-deleted` |  | `false` (excluded) | Include logically deleted databases; takes no value. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# List up to 20 databases, including deleted entries
ponydb databases list --limit 20 --include-deleted
```

Example output (excerpt):

```json
{
  "outcome": "read_only",
  "databases": [
    {
      "name": "notes",
      "description": "My notes",
      "status": "active",
      "data_version": "1"
    }
  ],
  "next_cursor": null
}
```

A `null` `next_cursor` means there is no next page; otherwise pass it unchanged with `--cursor`. Database entries contain the same fields as the details command.

#### db DATABASE_NAME

Omit the action to read database details, including its data version, management ETag, and commit token.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `DATABASE_NAME` | Yes |  | Database name; there is no implicit current database. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Read notes details and its latest ETag
ponydb db notes
```

Example output (excerpt):

```json
{
  "name": "notes",
  "description": "My notes",
  "status": "active",
  "metadata_version": "1",
  "data_version": "1",
  "etag": "\"ETAG_FROM_DETAILS\"",
  "metadata": {
    "project": "notes"
  },
  "snapshot_retention_days": 7,
  "commit_token": "COMMIT_TOKEN"
}
```

#### db DATABASE_NAME update

Update description, custom metadata, or snapshot retention without changing the database name or SQL data version.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--description TEXT` |  | Keep the current value | New description, at most 16 KiB of UTF-8. |
| `--metadata-file METADATA_FILE\|-` |  |  | Replace custom metadata; at most 64 KiB of compact UTF-8 JSON. |
| `--snapshot-retention-days DAYS` |  | Keep the current value | Snapshot retention in days, range 0–36500. |
| `--if-match ETAG` |  | Skip matching a previously read ETag | Update only if the management ETag still matches. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Replace the ETag placeholder with the latest value, keeping embedded quotes
ponydb db notes update --if-match '"ETAG_FROM_DETAILS"' \
  --description "Personal notes" --snapshot-retention-days 14
```

Example output:

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "notes",
  "metadata_version": "2"
}
```

Retention changes do not immediately reclaim objects; explicit GC cleans up history. An ETag mismatch returns `METADATA_PRECONDITION_FAILED` and exit code `4`.

#### db DATABASE_NAME delete

Logically delete a database, retaining its tombstone and history within retention.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--if-match ETAG` |  |  | Delete only if the management ETag from the latest details still matches. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Fetch the latest ETag again, then logically delete the database
ponydb db notes delete --if-match '"ETAG_FROM_DETAILS"'
```

Example output:

```json
{
  "outcome": "committed",
  "committed": true,
  "database": "notes",
  "status": "deleted"
}
```

### SQL and schema

#### db DATABASE_NAME execute

Execute one SQL statement, or an atomic statements array supplied through a complete JSON request.

See [SQLite compatibility](sqlite.md) for supported SQL features, unsupported syntax, and usage limits.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `SQL` |  |  | Positional SQL statement; mutually exclusive with `--sql-file`. SQL beginning with `-` may follow `--`. |
| `--sql-file SQL_FILE\|-` |  |  | Read one UTF-8 SQL statement; does not split statements on semicolons. |
| `--param PARAM_JSON` |  |  | Typed parameter; repeatable, mutually exclusive with `--params-file`. |
| `--params-file PARAMS_FILE\|-` |  |  | Read a JSON array of typed parameters, bound in order. |
| `--consistency MODE` |  | `strong` | `strong`, `snapshot`, or `at_least`; the latter two are query-only. |
| `--snapshot-id SNAPSHOT_ID` | With `snapshot` consistency |  | Snapshot ID; obtain from the snapshot list. |
| `--commit-token TOKEN` | With `at_least` |  | Commit token; reads must be at least the version in the token. |
| `--max-rows NUMBER` |  | `query.max_rows` (built-in 1000) | Query-only, positive and within `query.max_rows`. Exceeding the limit is an error, not silent truncation. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

**Create and insert**: run these commands in order in the new `notes` database.

Example input:

```sh
# Create a notes table; its integer primary key id may be omitted on insert
ponydb db notes execute "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT, content TEXT)"
```

Example output:

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "2",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {
      "columns": [],
      "rows": [],
      "rows_affected": "0"
    }
  ]
}
```

Example input:

```sh
# Bind the title and content as typed parameters
ponydb db notes execute "INSERT INTO notes(title, content) VALUES (?, ?)" \
  --param '{"type":"text","value":"First note"}' \
  --param '{"type":"text","value":"Hello, ponydb!"}'
```

Alternatively, include the values directly in SQL. Choose either this form or the parameterized form above:

```sh
ponydb db notes execute "INSERT INTO notes(title, content) VALUES ('First note', 'Hello, ponydb!')"
```

Example output:

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "3",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {
      "columns": [],
      "rows": [],
      "rows_affected": "1"
    }
  ]
}
```

Parameters and result cells use these types; integers use strings to preserve 64-bit precision.

| Type | JSON example |
| --- | --- |
| Null | `{"type":"null"}` |
| Integer | `{"type":"integer","value":"42"}` |
| Real | `{"type":"real","value":3.14}` |
| Text | `{"type":"text","value":"A note"}` |
| Blob | `{"type":"blob","value":"SGVsbG8="}` (base64) |

Example input:

```sh
# Limit result size in SQL and set a returned-row budget
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id LIMIT 10" --max-rows 10
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

`results` follows statement order and contains `columns`, typed `rows`, and string-valued `rows_affected`. Writes also return `committed` and a `commit_token`.

**Files and stdin**: save `SELECT count(*) FROM notes` as `query.sql`, then choose either input form.

Example input:

```sh
# Read one SQL statement from a file
ponydb db notes execute --sql-file ./query.sql

# Read the same SQL from stdin
printf 'SELECT count(*) FROM notes' | ponydb db notes execute --sql-file -
```

Both forms produce the same output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["count(*)"],
      "rows": [
        [{"type": "integer", "value": "1"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**Bind parameters from a file**: save this as `params.json`.

```json
[{"type": "integer", "value": "1"}]
```

Example input:

```sh
# Bind query parameters from an array file
ponydb db notes execute "SELECT id, title, content FROM notes WHERE id = ?" --params-file ./params.json
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**Transaction input**: save this as `statements.json`.

```json
{
  "database": "notes",
  "statements": [
    {
      "sql": "INSERT INTO notes(title, content) VALUES (?, ?)",
      "params": [
        {"type": "text", "value": "Second note"},
        {"type": "text", "value": "Another note."}
      ]
    },
    {"sql": "SELECT id, title, content FROM notes ORDER BY id"}
  ]
}
```

Example input:

```sh
# Insert and query in one request; any failure rolls back all statements
ponydb db notes execute --input ./statements.json
```

Example output:

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "4",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"},
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}],
        [{"type": "integer", "value": "2"}, {"type": "text", "value": "Second note"}, {"type": "text", "value": "Another note."}]
      ],
      "rows_affected": "0"
    }
  ]
}
```

Statements run in order; do not add `BEGIN` or `COMMIT`. Choose either `sql` / `params` or a nonempty `statements` array.

Statement arrays use write transaction semantics and require write permission even for queries only. They reject read-only options such as `--max-rows`, snapshots, and commit tokens.

Example input:

```sh
# Replace the token with commit_token from a prior write
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id" \
  --consistency at_least --commit-token COMMIT_TOKEN
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": [
        "id",
        "title",
        "content"
      ],
      "rows": [
        [
          {
            "type": "integer",
            "value": "1"
          },
          {
            "type": "text",
            "value": "First note"
          },
          {
            "type": "text",
            "value": "Hello, ponydb!"
          }
        ],
        [
          {
            "type": "integer",
            "value": "2"
          },
          {
            "type": "text",
            "value": "Second note"
          },
          {
            "type": "text",
            "value": "Another note."
          }
        ]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

**Update and delete**: continue from the transaction example above to update, then delete, the note with `id = 2`. Each write produces a new version.

Example input:

```sh
# Update the selected note's content
ponydb db notes execute "UPDATE notes SET content = ? WHERE id = ?" \
  --param '{"type":"text","value":"Updated note."}' \
  --param '{"type":"integer","value":"2"}'
```

Example output:

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "5",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"}
  ]
}
```

Example input:

```sh
# Delete the selected note
ponydb db notes execute "DELETE FROM notes WHERE id = ?" \
  --param '{"type":"integer","value":"2"}'
```

Example output:

```json
{
  "outcome": "committed",
  "database": "notes",
  "committed": true,
  "data_version": "6",
  "commit_token": "COMMIT_TOKEN",
  "results": [
    {"columns": [], "rows": [], "rows_affected": "1"}
  ]
}
```

Both writes return `rows_affected: "1"`, indicating one affected row each. Query the note again after deletion:

```sh
ponydb db notes execute "SELECT id, title, content FROM notes WHERE id = ?" \
  --param '{"type":"integer","value":"2"}'
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "6",
  "results": [
    {"columns": ["id", "title", "content"], "rows": [], "rows_affected": "0"}
  ],
  "truncated": false
}
```

#### db DATABASE_NAME schema

Inspect user table, index, view, and trigger definitions. With no filters, returns all non-internal objects.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--table TABLE_NAME` |  |  | Exact case-sensitive name of an existing non-internal table; includes its indexes and triggers, without resolving view dependencies. |
| `--type TYPE` |  |  | `table`, `index`, `view`, or `trigger`; intersected with `--table`. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Show only the CREATE TABLE statement for notes
ponydb db notes schema --table notes --type table
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": ["type", "name", "tbl_name", "sql"],
      "rows": [
        [
          {"type": "text", "value": "table"},
          {"type": "text", "value": "notes"},
          {"type": "text", "value": "notes"},
          {"type": "text", "value": "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT, content TEXT)"}
        ]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

`columns` lists the column names, and each row in `rows` contains cells in that same order:

| Column | Meaning | Value in this example |
| --- | --- | --- |
| `type` | Object type: table, index, view, or trigger. | `table` |
| `name` | Object name. | `notes` |
| `tbl_name` | Name of the table or view the object belongs to; for tables and views themselves, this equals `name`. | `notes` |
| `sql` | SQL definition that creates the object. | `CREATE TABLE notes(...)` |

A missing named table returns `TABLE_NOT_FOUND`. An existing table with no matching object type returns an empty result; schema results also obey query budgets.

### Snapshots

#### db DATABASE_NAME snapshot list

List retained snapshots with pagination for historical queries and export.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--limit NUMBER` |  | 50 | Snapshots per page; range 1–1000. |
| `--cursor CURSOR` |  | Start from the first page | The previous response's `next_cursor`. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# List up to 20 retained snapshots
ponydb db notes snapshot list --limit 20
```

Example output (excerpt):

```json
{
  "outcome": "read_only",
  "snapshots": [
    {
      "snapshot_id": "SNAPSHOT_ID",
      "data_version": "3"
    }
  ],
  "next_cursor": "CURSOR",
  "retention_days": 14,
  "time_authority": "system_clock",
  "history_gaps": []
}
```

One snapshot entry is excerpted with selected fields. Replace later `SNAPSHOT_ID` placeholders with a returned value.

To continue listing, pass `next_cursor` unchanged with `--cursor`. `retention_days` is the database's current snapshot retention period.

#### db DATABASE_NAME snapshot get SNAPSHOT_ID

Read a retained snapshot descriptor and its Manifest reference.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `SNAPSHOT_ID` | Yes |  | Snapshot ID; may instead be supplied as `snapshot_id` via `--input`. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Read the selected snapshot descriptor
ponydb db notes snapshot get SNAPSHOT_ID
```

Example output (excerpt):

```json
{
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "3",
  "retired_at_upper_ms": null,
  "observed_at_ms": 1794182400000
}
```

The excerpt omits `manifest`.

#### db DATABASE_NAME execute --consistency snapshot --snapshot-id SNAPSHOT_ID

To query the snapshot data, use the read-only parameters on `execute`:

Example input:

```sh
# Read data pinned to the specified snapshot
ponydb db notes execute "SELECT id, title, content FROM notes ORDER BY id" \
  --consistency snapshot --snapshot-id SNAPSHOT_ID
```

Example output:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "3",
  "results": [
    {
      "columns": ["id", "title", "content"],
      "rows": [
        [{"type": "integer", "value": "1"}, {"type": "text", "value": "First note"}, {"type": "text", "value": "Hello, ponydb!"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

### Backup and restore

#### db DATABASE_NAME export SQLITE_FILE

Export the current database version as a standalone SQLite file. Later writes during export do not change the exported contents.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `SQLITE_FILE` | Yes |  | Destination file; may instead be supplied as `file` via `--input`. No `-`; the parent directory must exist and existing files are never overwritten. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Export the current version to a local file
ponydb db notes export ./notes-backup.sqlite
```

Example output:

```json
{
  "outcome": "read_only",
  "file": "./notes-backup.sqlite",
  "snapshot_id": "CURRENT_SNAPSHOT_ID",
  "data_version": "4",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST"
}
```

#### db DATABASE_NAME snapshot export SNAPSHOT_ID SQLITE_FILE

Export a selected retained snapshot as a standalone SQLite file.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `SNAPSHOT_ID` | Yes |  | Retained snapshot ID; may instead be supplied as `snapshot_id` via `--input`. |
| `SQLITE_FILE` | Yes |  | Destination file; may instead be supplied as `file` via `--input`. Uses the same path and no-overwrite rules as current-version export. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Export the selected historical snapshot
ponydb db notes snapshot export SNAPSHOT_ID ./notes-history.sqlite
```

Example output:

```json
{
  "outcome": "read_only",
  "file": "./notes-history.sqlite",
  "snapshot_id": "SNAPSHOT_ID",
  "data_version": "3",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST"
}
```

The two export commands accept these JSON requests, respectively:

```json
{"database": "notes", "file": "./notes-backup.sqlite"}
```

```json
{"database": "notes", "snapshot_id": "SNAPSHOT_ID", "file": "./notes-history.sqlite"}
```

`total_bytes` is the byte length as a string. `sha256` contains 64 hexadecimal characters; `SHA256_DIGEST` is a placeholder.

#### db DATABASE_NAME restore file SQLITE_FILE

Restore a local SQLite backup to object storage through the CLI only. The target must be absent or deleted; active databases cannot be overwritten.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `DATABASE_NAME` | Yes |  | Destination database name; do not create the database first. |
| `SQLITE_FILE` | Yes |  | Local positional file; must be stable, complete, and in DELETE journal mode. No stdin or DTO replacement. |

Example input:

```sh
# Restore the backup into absent or deleted restored_notes
ponydb db restored_notes restore file ./notes-backup.sqlite
```

Final stdout example:

```json
{
  "committed": true,
  "outcome": "committed",
  "name": "restored_notes",
  "data_version": "1",
  "total_bytes": "8192",
  "sha256": "SHA256_DIGEST",
  "page_size": 4096
}
```

Restore needs local temporary disk space, has no total execution deadline, rejects `--deadline-ms`, and cannot resume interrupted work.

Stages and progress go to stderr; wait for the final successful stdout result before treating restore as complete.

### Online maintenance

#### db DATABASE_NAME compact

Merge storage segments online without changing the logical data version.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--budget-ms MILLISECONDS` |  | 10000 milliseconds | Work budget for this call; range 1–60000. |
| `--max-segments NUMBER` |  | 32 | Maximum segments to process; range 1–32. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Bound compaction work by time and segment count
ponydb db notes compact --budget-ms 5000 --max-segments 16
```

Example output:

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "segments_processed": 1,
  "data_version": "4"
}
```

The processed count depends on database state. Omitted options use defaults, applied after merging DTO fields and explicit arguments.

#### db DATABASE_NAME gc

Reclaim expired history and unreachable objects online. Call again if the work budget is exhausted.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--budget-ms MILLISECONDS` |  | `maintenance.gc_default_budget_ms` (60000) | Work budget for this call; must be positive and within `requests.max_timeout_ms`. |
| `--max-objects NUMBER` |  | `maintenance.gc_max_objects_per_call` (1000) | Maximum deletion attempts; the deployment cap also comes from `maintenance.gc_max_objects_per_call`, and the value must be positive. |
| `--input REQUEST_FILE\|-` |  |  | Read a complete JSON request from a file or stdin; common input rules apply. |

Example input:

```sh
# Use the default time budget and object limit
ponydb db notes gc
```

Example output:

```json
{
  "outcome": "applied",
  "complete": true,
  "database_available": true,
  "phase": "complete",
  "deleted_objects": 0
}
```

Example input:

```sh
# Allocate 300 seconds, at most 1000 deletion attempts, and a 330-second timeout
ponydb db notes gc --budget-ms 300000 --max-objects 1000 --deadline-ms 330000
```

Possible output when a budget or object limit is reached (exit code `6`):

```json
{
  "outcome": "partial",
  "complete": false,
  "database_available": true,
  "phase": "sweeping",
  "deleted_objects": 1000
}
```

If compaction or GC returns `outcome: "partial"` with exit code `6`, run the same operation again to continue. GC has no pause, recovery, or resume subcommands.

Explicit GC cleans up historical snapshots after retention expires. Keep system clocks accurate and export history you need to keep.

### Server and AI Agents

#### server

Start the optional HTTP/MCP service. Requires storage configuration and at least one valid server authentication identity.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--listen ADDRESS` |  | `server.listen` (built-in `127.0.0.1:8080`) | Override the server listen address. |

Example input:

```sh
# Start the service with configured authentication identities
ponydb server --listen 127.0.0.1:8080
```

The command keeps running without a one-shot business result on stdout; diagnostics go to stderr. It rejects `--output` and `--deadline-ms`.

Check the health endpoint from another terminal:

```sh
# Check that the service is listening
curl --silent --show-error http://127.0.0.1:8080/healthz
```

Example health endpoint JSON output (received by curl):

```json
{
  "status": "ok"
}
```

See [HTTP deployment](http.md) for authentication, deployment, and Docker configuration.

#### mcp

Serve MCP over stdin/stdout, launched and managed by an AI Agent client, using storage credentials directly.

| Parameter | Required | Default | Meaning |
| --- | --- | --- | --- |
| `--config CONFIG_FILE` |  | `PONYDB_CONFIG` or the platform default path | Global parameter; an absolute path is recommended for client launches. |
| `--deadline-ms MILLISECONDS` |  | `requests.default_timeout_ms` (90000) | Global parameter; per-request deadline, not total process lifetime. |

Example input:

```sh
# Start stdio MCP; replace the configuration path with a real absolute path
ponydb --config /absolute/path/ponydb.toml mcp --deadline-ms 90000
```

Stdout contains only MCP JSON-RPC messages, without a one-shot CLI result; `--output` is rejected. The following illustrates one protocol exchange; each JSON message occupies one line on the wire.

Client request sent to stdin:

```json
{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
```

Protocol response on stdout:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "resultType": "complete"
  }
}
```

See [AI Agent setup](agent-clients.md) for client configuration, supported protocol versions, and tool calls.

## Output and scripting

Successful results go to stdout; diagnostics, errors, and restore progress go to stderr. Help is always text; see Server and MCP for their output behavior.

Scripts can save the default JSON result directly:

Example input:

```sh
# Write the JSON result to a file; diagnostics still go to stderr
ponydb db notes execute "SELECT 1" > result.json
```

Example contents of `result.json`:

```json
{
  "outcome": "read_only",
  "database": "notes",
  "data_version": "4",
  "results": [
    {
      "columns": ["1"],
      "rows": [
        [{"type": "integer", "value": "1"}]
      ],
      "rows_affected": "0"
    }
  ],
  "truncated": false
}
```

Scripts should check exit codes. Example stderr JSON for an ETag mismatch:

Example output:

```json
{
  "request_id": "REQUEST_ID",
  "code": "METADATA_PRECONDITION_FAILED",
  "message": "Management ETag does not match",
  "retryable": false,
  "outcome": "not_committed"
}
```

| Exit code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Other error |
| `2` | Invalid configuration or request |
| `3` | Authentication or permission denied |
| `4` | Transaction conflict or ETag mismatch |
| `5` | Write outcome unknown; inspect data before retrying |
| `6` | Maintenance stopped within its work budget; call again |

Writes execute again when repeated. If a response is lost or the result is `OUTCOME_UNKNOWN`, inspect the database before retrying; use business keys or unique constraints to detect duplicate work.
