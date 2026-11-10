**English** | [中文](sqlite.zh-CN.md)

# SQLite compatibility

ponydb currently bundles SQLite 3.54.1 rather than using your system installation. Run `ponydb version` to inspect the engine version. The CLI, HTTP API and MCP share the same SQL behavior.

## Available features

| Feature | Usage |
| --- | --- |
| Relational SQL | Ordinary tables, indexes, views, triggers, joins, CTEs and window functions |
| Constraints | Primary keys, unique constraints, checks and foreign keys; foreign-key enforcement is enabled |
| Transactions | One statement or an atomic `statements` array; a failed statement rolls back the request |
| JSON / JSONB | JSON construction, extraction and updates, `->` / `->>`, `json_each` / `json_tree` and `jsonb_` variants |
| Math functions | `sqrt`, `pow`, `log`, `ln`, `sin` and other bundled SQLite math functions |
| Versioned reads | Current strong reads, retained snapshot reads and reads at least as recent as a commit token |
| Portable backups | Export a standalone SQLite file; restore a supported SQLite backup through the CLI |

JSON and math functions are built in; no extension installation is needed. For example, against an existing `notes` database:

```sh
ponydb db notes execute "SELECT json_extract(jsonb_object('count', 2), '$.count')"
ponydb db notes execute "SELECT key, value FROM json_each('[1,2,3]')"
ponydb db notes execute "SELECT sqrt(9), pow(2, 3), log(100)"
```

JSONB results use the protocol's base64 `blob` representation; wrap a JSONB expression in `json(...)` for text output. A math domain error such as `sqrt(-1)` returns NULL; non-finite output such as `exp(1000)` is rejected.

## Currently unsupported

| Feature or syntax | Alternative |
| --- | --- |
| `ATTACH`, `DETACH`, cross-database transactions | Operate on one explicitly named database per request |
| User `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE` | Submit a `statements` array through `execute --input` or the HTTP/MCP request |
| Multiple statements in one SQL string or SQL file | Use the same statement-array format; each entry contains one statement |
| User `PRAGMA`, including changing journal mode or page size | Use supported ponydb configuration and the schema operation |
| WAL mode and `VACUUM` through the API | Use ponydb `compact` and `gc` for storage maintenance |
| `CREATE VIRTUAL TABLE`, including FTS3/4/5 and R-tree tables | Use ordinary tables and indexes; these virtual-table extensions are not exposed |
| `load_extension`, including user-installed vector extensions | Use the bundled SQL features or export data for external processing |
| Persistent connection state across requests | Keep related work in a single atomic request; temporary tables are request-local |

Exporting a database does not make unsupported extensions available through ponydb. File restore validates the schema; a backup depending on unsupported schema features cannot be imported as-is.

## Limits and concurrency

Default request limits include 100 statements, 16 MiB of dirty SQLite pages, 1000 result rows and 1 MiB of serialized results. Oversized results fail rather than truncate. Use SQL `LIMIT` and pagination; tune supported settings in the [configuration template](https://github.com/hackerneocom/ponydb/blob/main/examples/ponydb.toml).

Multiple clients may read and write the shared database. Commits use optimistic concurrency control; a conflicting publication returns `TX_CONFLICT`. Repeated writes execute again, and uncertain outcomes must be checked before retrying. This does not provide offline writes, automatic conflict merging or the throughput guarantees of a dedicated database server.

For backup/export limits and restore prerequisites, see the [CLI guide](cli.md#backup-and-restore).
