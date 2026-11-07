[English](README.md) | **中文**

# runtime-config

`ponydb` CLI 和 `ponydb server` 共用的严格配置解析器。两个入口加载同一份 TOML，应用相同的环境变量覆盖，并拒绝未知键。

## 用途

服务和 CLI 必须从同一个文件构造相同的 `database-service` 限制和同一个 S3 客户端。不支持的组合（另一种后端、WAL、缺少原子写要求）在加载时失败。`AppConfig::validate` 检查本地配置。CLI 的 `config validate` 还会构造 `config.s3()`，执行 [CLI README](../../apps/cli/README.zh-CN.md) 中的在线存储探测；探测与清理成功才返回 `storage_capabilities_verified: true`。`config show` 保持离线。

## 工作原理

`AppConfig` 是一棵带默认值且 `deny_unknown_fields` 的配置节树：`storage`、`engine`、`cache`、`runtime`、`transactions`、`query`、`requests`、`maintenance`、`snapshots`、`exports`、`logging`、`server`、`mcp` 和 `cli`。不存在数据库上下文配置节。`config_version` 必须为 1。存储布局必须为 1。引擎必须是 `s3_versioned` VFS 上的捆绑 SQLite，使用 `DELETE` 日志、`synchronous=FULL`、外键，以及 `mmap_size=0`。工作线程数、队列深度、脏字节上限、查询限制、超时、维护预算各自有允许范围，在 `validate` 中检查。

`load` 按以下顺序解析文件：

1. 路径参数（`--config`）。
2. `PONYDB_CONFIG`。
3. 平台默认路径：Linux 为 `$XDG_CONFIG_HOME/ponydb/config.toml`，macOS 为 `~/Library/Application Support/ponydb/config.toml`，Windows 为 `%APPDATA%\ponydb\config.toml`。

缺少默认文件意味着使用内置默认值。缺少显式指定的文件是错误。文件按 TOML 解析。缓存目录的相对路径，相对于配置文件所在目录解析。

[`examples/ponydb.toml`](../../examples/ponydb.toml) 仅列出用户可调整且实际生效的配置。仅 `storage.endpoint`、`storage.bucket` 和两把密钥保持生效；可选设置及节标题均已注释，并写出内置默认值。覆盖时同时取消节标题和赋值行的注释。可选 session token 和缓存目录默认未设置，其赋值行仅为示例。`ponydb server` 至少需要一条认证身份，示例里该段保持注释，默认身份数组为空。`config show` 和 `version` 不需要访问 S3。

示例省略程序内部固定的协议、引擎约束，以及未接入运行逻辑的字段。

### 环境变量覆盖

所有配置字段都支持环境变量覆盖，无须维护字段白名单。`PONYDB_<SECTION>__<KEY>` 和 `PONYDB_<SECTION>.<KEY>` 指向同一路径（名称转小写）。Shell 脚本推荐使用 `__`，因为 shell 变量赋值不接受点号。字符串和可选字符串/路径按原始文本读取；布尔值和无符号整数按对应类型解析。对象接受 JSON，并递归合并已有字段；数组接受 JSON，并整体替换。数字路径段可定位已有数组元素，例如 `PONYDB_SERVER__AUTH__PRINCIPALS__0__TOKEN`；如果元素尚不存在，需要先设置整个数组。

环境变量按路径排序应用，父级先于子级，因此具体字段优先于包含它的对象。同一路径对应多个环境变量名称时会报错。`PONYDB_CONFIG` 仅选择配置文件。未知键和错误类型都会报错。最后一张覆盖用的 `BTreeMap` 使用相同的值解析规则，服务用它处理 `--listen`。合并后的文档被反序列化并再次 `validate`。

例如，`PONYDB_STORAGE='{"bucket":"my-bucket"}'` 保留其他存储配置；`PONYDB_SERVER__AUTH__PRINCIPALS='[...]'` 替换身份数组。认证对象和其他对象使用完全相同的 JSON 覆盖机制。

`storage.credentials` 包含普通配置项 `access_key_id`、`secret_access_key`（默认空字符串）和可选 `session_token`（默认未设置）。环境变量分别为 `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`、`PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY` 和 `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`，按字段覆盖 TOML。创建 S3 客户端时要求非空的 Access Key 和 Secret Key；不单独读取 `AWS_*` 变量，也不探测其他账号。`s3` 构建一个对象上限为 8 MiB、操作超时为 10 秒的 `object-store-s3::S3Store`。`config show`、Debug 和解析错误不会显示凭证值。

`service_options` 把引擎、缓存、运行时、事务、查询、请求、维护、快照和导出节中已接入运行逻辑的字段映射到 `database_service::ServiceOptions`。`namespace` 是 `s3:{endpoint}:{bucket}:{prefix}`，并成为提交令牌命名空间的一部分。`effective(entry)` 返回配置，以及该入口不使用的节：CLI 忽略 `server` 和 `mcp`，服务忽略 `cli`。

### 通过环境变量配置服务认证

设置 `PONYDB_SERVER__AUTH`，值为完整的认证 JSON：

```sh
export PONYDB_SERVER__AUTH='{"principals":[{"id":"admin","token":"replace-with-a-long-random-token","databases":["*"],"scopes":["*"]}]}'
ponydb config validate --target server
```

对应配置键为 `server.auth`，可以直接写在主 TOML 配置文件中；环境变量优先于配置文件。认证内容在启动时加载，变更需要重启。`config show` 和 Debug 输出会隐藏认证内容。完全通过环境变量部署时，另行设置 `PONYDB_STORAGE__ENDPOINT`、`PONYDB_STORAGE__BUCKET` 以及 `PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID`、`PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY`（临时凭证还需 `PONYDB_STORAGE__CREDENTIALS__SESSION_TOKEN`）。

```toml
# Or set PONYDB_SERVER__AUTH to the complete JSON object.
[[server.auth.principals]]
id = "admin"
token = "replace-with-a-long-random-token"
databases = ["*"]
scopes = ["*"]
```

`storage.force_path_style` 默认为 `true`，保留 MinIO 的 path-style 寻址。OSS 使用 virtual-hosted-style 时设为 `false`；区域端点（如 `https://oss-cn-beijing.aliyuncs.com`）和 bucket 仍分别配置，由 SDK 拼接桶域名。环境变量覆盖方式：`PONYDB_STORAGE__FORCE_PATH_STYLE=false`。配置值必须为布尔类型。

## 导出预算

`[exports].max_file_bytes` 为 CLI/HTTP 快照导出的单文件字节上限，必须为正数，默认 10737418240（10 GiB），映射到 `ServiceOptions::export_max_bytes`。每个并发请求需要最多相当于文件大小的临时磁盘空间。文件生成同时遵守请求截止时间和数据库 HEAD 的读取租约；HTTP 下载受请求剩余时间限制。提高大小上限不会延长读取租约。

应用默认启用已校验不可变对象的共享磁盘缓存。读取顺序为已解析索引节点内存、对象正文内存、磁盘、S3；HEAD 和 CAS 结果确认始终访问 S3。独立 CLI 进程通过 `objects-v1.sqlite3` 复用正文。Linux 默认使用绝对路径 `$XDG_CACHE_HOME/ponydb`，未设置时使用 `$HOME/.cache/ponydb`；macOS 使用 `$HOME/Library/Caches/ponydb`，Windows 使用 `%LOCALAPPDATA%/ponydb`。`cache.directory` 可覆盖目录，`cache.disk_budget_mib = 0` 禁用磁盘缓存。`config show` 显示解析后的目录，但不创建文件。

`cache.disk_budget_mib` 限制缓存正文容量（默认 2048 MiB），同一目录下所有命名空间共享预算，条目上限为 65,536。SQLite 元数据、日志和空闲页额外占用空间，增量回收允许滞后。共享目录的进程应使用相同预算。`runtime.work_disk_budget_mib` 限制本地 SQLite 执行工作区容量，默认 2048 MiB。恢复临时文件仍不受这些配额限制。

磁盘键包含后端 endpoint/bucket/prefix、完整对象键、长度及 SHA-256，每次读取都验证正文，损坏条目重新下载。近似 LRU 合并访问记录，最多每 60 秒更新一次。磁盘查询及回填共享 50 ms 等待预算，不计 S3 耗时；繁忙或故障时降级，并限频写入 stderr 提示。每个 reader 最多一个阻塞任务，已开始的任务可在超时或取消后完成，期间仍持有执行许可；维护限频且尽力执行。缓存可随时丢弃，目录应仅向拥有数据库访问权限的可信用户开放。库默认仅使用内存缓存，需要显式传入 `DiskCacheOptions` 才启用磁盘层。

## 运行诊断

二进制在配置加载成功后启用 `logging.level` / `logging.format`，默认为 warn/json；使用 PONYDB_LOGGING__LEVEL 和 PONYDB_LOGGING__FORMAT 覆盖，不另接入 RUST_LOG。 库不安装全局 subscriber。
