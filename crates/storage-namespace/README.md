**English** | [中文](README.zh-CN.md)

# storage-namespace

Layout v1 path rules. Every persistent reference in a database is a root-relative key, and this crate is the only place that turns those keys into object names.

## Purpose

Object storage is a flat key space. ponydb keeps many databases under one bucket prefix, and metadata stores further keys inside each database. Those keys must stay inside that database: a reference cannot escape with `..`, an absolute path, or a prefix that overlaps a neighbor such as `notes` and `notes_old`.

## How it works

`DatabaseName` validates the user-chosen, immutable logical name: 1–64 Unicode BMP characters, excluding NUL, `/`, `\\`, `.`, supplementary characters and trailing ASCII space. Names are case-sensitive and preserved literally. `IncarnationId` validates one non-empty ASCII relative-key segment; it identifies a creation or restore lifecycle and does not require a UUID spelling.

`RelativeKey` validates database-root-relative references. Segments must be non-empty and cannot be `.` or `..`; bytes are limited to ASCII alphanumerics and `/-_.`. Validation rejects absolute paths, backslashes, percent escapes, URL schemes and trailing slashes. General safe keys, such as `head.json`, need not be immutable object paths.

`StorageNamespace` holds an optional validated prefix and parses database candidates with `database_name`. `database(DatabaseName)` creates a `DatabasePath` binding all operations to one exact database root, including its trailing `/`. Runtime stores use the empty namespace: the S3 adapter alone adds the configured deployment prefix, once.

| Method | Object name |
| --- | --- |
| `StorageNamespace::databases_prefix` | `{prefix}databases/` |
| `DatabasePath::prefix` | `{prefix}databases/{name}/` |
| `DatabasePath::head_key` | `{prefix}databases/{name}/head.json` |
| `DatabasePath::incarnations_prefix` | `{prefix}databases/{name}/incarnations/` |
| `DatabasePath::resolve` | `{prefix}databases/{name}/{relative-key}` |

`DatabasePath::relative_key` removes only that exact root and validates the remainder. It rejects neighboring databases such as `notes_old` when bound to `notes`.

`object_key` constructs `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`. `RelativeKey::object_parts` parses the same v1 layout into incarnation, positive epoch, category and object suffix. Epoch text is exactly 16 lowercase hexadecimal digits. GC rejects unknown layouts. Layout v1 accepts safe nested object suffixes.

The crate performs no storage I/O. Golden-path tests verify layout v1 paths.
