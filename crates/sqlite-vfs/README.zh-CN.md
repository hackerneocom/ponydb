[English](README.md) | **中文**

# sqlite-vfs

自定义 SQLite VFS。它从一个 `remote-pager::CowFile` 提供数据库主文件，并把回滚日志放在私有的本地目录里。这个 crate 不发布远程对象。

## 用途

通过 rusqlite 捆绑构建链接的官方 SQLite，需要一个 VFS 来读写主数据库和日志。ponydb 要使用 SQLite 自己的分页器、锁和 `DELETE` 日志，同时让持久字节留在写时复制文件里，直到引擎把它们冻结。本地日志和临时文件可以丢弃。它们不是数据库的第二份副本，也不用来恢复快照。

## 工作原理

`Workspace::open_with_limits` 创建一个临时目录和一个空的 `main.db` 哨兵文件。哨兵的存在是为了让 SQLite 原生日志代码能读取路径权限。它的字节从来不是数据库。工作区注册一个唯一命名的 VFS（`ponydb-p0-N`），`makeDflt` 设为 0，因此不会替换进程默认 VFS。然后它以读写、创建和无互斥标志，在该 VFS 上恰好打开一个连接。

主数据库的 `xOpen` 只接受一次，并且只接受哨兵路径。`sqlite3_file` 的方法针对 `CowFile` 执行读、写、截断、同步和加锁。锁状态放在这个文件对象里，因为工作区只拥有一个连接。第二次打开主数据库返回 `SQLITE_CANTOPEN`。

其他打开都进入 `local`。这些文件位于同一个临时目录下。`WorkspaceLimits` 限制本地字节总量和文件数量。增长时扣减预算，删除时释放预算。本地 I/O 会检查请求上下文。`xDelete` 只接受属于这个工作区的日志路径。`xAccess` 把主数据库报告为存在，并按本地文件报告日志。共享内存和 WAL 方法没有安装，VFS 也不加载扩展。

所有 C 回调都在 `catch_unwind` 内运行。panic 变成 `SQLITE_IOERR`，不会穿过 FFI 边界。

`Workspace::freeze` 要求连接处于自动提交状态，冻结 `CowFile`，关闭连接，并返回 `FrozenPages`。`Drop` 关闭剩余连接并注销 VFS。临时目录随工作区一起消失。调用方在 `freeze` 之后或工作区被丢弃之后不能再使用该连接。

`connection` 暴露 rusqlite 连接；`connection_mut` 供 `engine-sqlite` 注册授权器和取消回调。SQL 策略（授权器、页大小、日志模式）在那里执行，不在 VFS 里执行。
