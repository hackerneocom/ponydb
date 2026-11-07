**English** | [中文](README.zh-CN.md)

# sqlite-vfs

A custom SQLite VFS that serves one database file from a `remote-pager::CowFile` and keeps the rollback journal on a private local directory. This crate never publishes remote objects.

## Purpose

Official SQLite, linked through rusqlite's bundled build, needs a VFS to read and write the main database and a journal. ponydb wants SQLite's own pager, locking, and `DELETE` journal, while the durable bytes stay in the copy-on-write file until the engine freezes them. The local journal and temporary files are disposable. They are not a second copy of the database and they are not used to recover a snapshot.

## How it works

`Workspace::open_with_limits` creates a temporary directory and an empty `main.db` sentinel. The sentinel exists so SQLite's native journal code can read path permissions. Its bytes are never the database. The workspace registers a uniquely named VFS (`ponydb-p0-N`) with `makeDflt` set to 0, so it does not replace the process default. It then opens exactly one connection on that VFS with read-write, create, and no-mutex flags.

`xOpen` of the main database is accepted once, and only for the sentinel path. The `sqlite3_file` methods read, write, truncate, sync, and lock against the `CowFile`. The lock state lives in that file object because the workspace owns a single connection. A second main-database open returns `SQLITE_CANTOPEN`.

Every other open goes to `local`. Those files live under the same temporary directory. `WorkspaceLimits` caps total local bytes and the number of files. The budget is charged on grow and released on delete. The request context is checked on local I/O. `xDelete` accepts only the journal path that belongs to this workspace. `xAccess` reports the main database as present and the journal according to the local file. Shared-memory and WAL methods are not installed, and the VFS does not load extensions.

All C callbacks run inside `catch_unwind`. A panic becomes `SQLITE_IOERR` instead of crossing the FFI boundary.

`Workspace::freeze` requires the connection to be in autocommit, freezes the `CowFile`, closes the connection, and returns `FrozenPages`. `Drop` closes any remaining connection and unregisters the VFS. The temporary directory disappears with the workspace. Callers must not use the connection after `freeze` or after the workspace is dropped.

`connection` exposes the rusqlite connection; `connection_mut` lets `engine-sqlite` install authorizer and cancellation hooks. SQL policy (authorizer, page size, journal mode) is enforced there, not in the VFS.
