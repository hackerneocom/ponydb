**English** | [中文](README.zh-CN.md)

# protocol

Transport-independent inputs, errors, and MCP tool schemas for the 11 database operations. HTTP, MCP, and the CLI decode into these types and pass an `Operation` to `database-service`. Integer SQL values never travel as JSON numbers.

## Purpose

All entries share DTO validation. 64-bit integers use decimal strings, blobs use base64, and unknown fields are rejected.

## How it works

`SqlValue` is a tagged union: `null`, `integer` (string), `real` (finite `f64` only), `text`, or `blob` (base64). `to_sql` and `from_sql` convert to and from rusqlite. A non-finite real, an integer that is not `i64`, or bad base64 fails conversion.

Each operation has a struct derived with `deny_unknown_fields`: `Create`, `List`, `Update`, `Delete`, `Execute`, `Compact`, `Gc`, snapshot inputs, and the common `Database` id wrapper. `Operation` tags them with `operation` and `input` using snake_case names (`database_create`, `sql_execute`, …). `Operation::name` is the business operation name and MCP tool name.

`Update` and `Delete` accept optional `if_match`: when supplied, the service checks the management ETag; when omitted, it operates on the current database state. The generated MCP schemas also mark this field as optional.

`Create::validate` and `Update::validate` limit supplied `metadata` to `MAX_DATABASE_METADATA_BYTES` (65,536 compact JSON UTF-8 bytes, including keys and escaping) and `description` to `MAX_DATABASE_DESCRIPTION_BYTES` (16,384 UTF-8 bytes). Input formatting whitespace does not count toward compact JSON size. CLI and service entrypoints validate before S3 access and return `INVALID_REQUEST` on oversized fields; MCP input schemas describe the byte limits. These are request limits: existing HEADs are still readable and can have their metadata reduced through update.

`Execute` defaults `consistency` to `strong`. Writes execute independently. Reads may set `snapshot_id`, `commit_token`, and `max_rows`. The service, not this crate, decides which combinations are legal.

`Principal` is an id, a list of database ids, and a list of scopes. `allows` matches a database and a scope, with `*` as a wildcard on either list. `Principal::direct` is the principal the CLI uses for the process that already holds the S3 credentials.

`ApiError` is `{ request_id, code, message, retryable, outcome }`. `outcome` is `unknown` when the commit may have landed and `not_committed` otherwise. Entry points map `code` onto HTTP status or a process exit code.

`tools` builds the MCP tool list. Each tool's `inputSchema` comes from schemars. `outputSchema` is a hand-written JSON Schema for the `{ ok, result }` / `{ ok, error }` envelope of that tool, including which result fields are decimal strings. Annotations mark read-only tools and mark only read-only tools as idempotent.

`strict_json` parses a business payload. A leading UTF-8 BOM is stripped. The value must be UTF-8 JSON, duplicate object keys at any depth are rejected, non-finite numbers are rejected, and trailing bytes after one value are rejected. The server, the CLI file loader, and MCP all use this parser so a repeated key cannot change meaning between transports.

Database identity is the immutable `name`: create with `databases create notes` or
`{"name":"notes"}`. Create results and database metadata (get/list) expose only
`name`, supplied by the caller at creation. Other operations
use `db notes` / `database: "notes"` to select this same name. CLI creation requires positional `DATABASE_NAME`; a DTO `name` must match it. Updates cannot rename the database.

Names follow [MySQL database identifier rules](https://dev.mysql.com/doc/refman/8.0/en/identifiers.html)
and the [64-character limit](https://dev.mysql.com/doc/refman/8.0/en/identifier-length.html):
1–64 Unicode BMP characters, excluding NUL, `/`, `\`, `.`, and trailing ASCII space.
Pass the literal name without SQL backticks; numbers, reserved words, Chinese,
spaces within names and hyphens are accepted as for quoted MySQL identifiers.
Names are case-sensitive, preserved verbatim, and never trimmed or case-folded.
The persistent HEAD identifies the database by the same `name`.

`Execute` validates mutually exclusive `sql`/`params` and nonempty `statements` inputs during deserialization, checks typed parameter values, and rejects read options on batches. Each array entry binds its own parameters; SQL is not split on semicolons.

`schema_get` uses a dedicated DTO: required `database`, optional nonempty `table`, and optional `type` (`table`, `index`, `view`, `trigger`). `table` matches `sqlite_schema.tbl_name` exactly and case-sensitively, including associated indexes and triggers, without resolving view dependencies. Filters combine with AND. A specified table must exist as a non-internal table; otherwise `TABLE_NOT_FOUND` is returned (HTTP 404, CLI exit 1). An existing table with no matching object type, or a type-only filter with no matches, returns empty rows. Existence and definitions are read in one query from the same snapshot. HTTP uses the same query parameter names, CLI uses `--table` and `--type`, and MCP advertises these fields. Queries bind filter values and retain internal-object exclusion, ordering, and result budgets.
