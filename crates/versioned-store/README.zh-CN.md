[English](README.md) | **中文**

# versioned-store

不可变对象、COW 索引和唯一 HEAD 发布点。全部数据依赖持久化后，通过 HEAD CAS 原子发布。

## 用途

多个写入者可以针对同一份快照准备 SQL。其中恰好一个可以推进数据库。这个 crate 在 `BlobStore` 之上实现该规则：页镜像和元数据只创建一次，索引是这些对象的持久基数树，`publish_head` 是对 `head.json` 的唯一一次比较并交换。它还把一份清单还原成 `remote-pager` 读取的字节流。

## 工作原理

`DatabaseStore` 以 `databases/{database}/` 为根。每个相对键在拼到这个根上之前，都用 `storage-namespace` 检查。后端必须通过 `StoreCapabilities::validate`。

`read_head` 在 HEAD 不存在时返回 `Error::DatabaseNotFound(name)`。`get_bytes` 在引用的不可变对象（包括 manifest 和索引）缺失时返回 `Error::MissingObject(name)`。调用方可区分数据库不存在和存储不完整，业务错误无需暴露对象键。

`read_head` 对非法 HEAD 元数据返回 `Error::InvalidHead { database, reason }`，服务层映射为 `STORAGE_CORRUPT`，消息包含数据库名、`HEAD validation failed` 和受限的原因。例如：`Database 'test' HEAD validation failed: unknown field 'restore'; stored HEAD may be incompatible with this version`。原因区分 JSON 非法、元数据过大、未知或缺失字段、字段类型或值非法，以及 HEAD 约束不满足。字段名仅在不超过 64 字符且只含 ASCII 字母、数字、下划线时显示，其他名称脱敏；不输出存储字段值、对象键或 Serde 原始错误。HEAD 使用严格校验；错误为 `retryable: false`、`outcome: not_committed` 和 CLI 退出码 1。

### 不可变写入

`put_bytes` 选择 `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`，对正文做哈希，并调用 `create_if_absent`。`Applied` 返回 `ObjectRef`。`Unknown` 会把键读回来，仅当已存储的字节完全一致时返回同一引用；否则上传未确认。全新 uuid 上的 `PreconditionFailed` 视为损坏。`put` 是 JSON 值的同一条路径，超过 `MAX_METADATA_BYTES` 会被拒绝。`get_bytes` 首次读取被引用的键时要求 `ObjectRef::matches`，之后可复用同一不可变引用的已验证字节。上传结果为 Unknown 时的核验仍直接访问后端。

### 索引

索引键是 1 到 64 个十六进制字符。`index_get` 从根开始遍历 `IndexNode`。每一步都必须加长前缀。缺少边或前缀不匹配就是键不存在。深于 64 个节点的遍历视为损坏。

`index_insert` 复制路径。根为空时写入新叶子；键在前缀中部分叉时分裂节点；键穿过某个子节点继续时替换该节点。旧节点保持可达，直到压缩和 GC 判定它们不再使用。`index_remove` 写入一条省略该键的路径。`index_list` 从一个可选的排除游标开始按键序遍历，并在调用方限制处停止，上限为 1000 项。

快照历史、增量页映射和基线段映射共用该索引结构。

共享的 `ObjectReader` 通过独立 LRU 保留已解析并通过节点自身校验的 `Arc<IndexNode>`，上限为 8 MiB 节点估算权重、4096 项。键包含绑定数据库的完整物理路径、长度和 SHA-256，后端由 reader 绑定。`index_get`、`index_list` 和 COW 更新跨调用与快照复用节点，即使原始正文已被淘汰也可命中。更新只在需要修改时将字段复制到私有脏节点；父子前缀关系、查询匹配及遍历限制逐次检查。

节点填充使用独立的 64 个分片锁：获取锁后再次检查节点缓存，再通过正文缓存读取已校验字节、解析 JSON 并校验节点，最后按预算入缓存。节点填充可以等待正文填充，反向不等待；解析或 I/O 期间不持有缓存互斥锁。加载失败或节点非法时不写节点缓存，但已校验的原始字节可继续留在正文缓存。取消会释放填充锁，等待者可重试；节点缓存命中也受读取租约约束。

节点权重统计内联大小，并递归估算字符串与数组容量、子引用及 JSON 映射条目，每个 BTreeMap 条目额外计入 64 字节开销；采用饱和算术防止溢出。超出预算的合法节点正常返回但不入缓存，已淘汰节点在调用方持有 `Arc` 时仍有效。估算不包含缓存记账结构、在途缓冲区、调用方持有的已淘汰节点及分配器特有开销。独立的 16 MiB 正文预算与 8 MiB 节点估算预算都不是进程内存上限，目前也不提供配置项。

异步索引写入方法共用 `IndexWriteTarget`，包含 incarnation、epoch 和 category；请求上下文单独传入。

`index_update` 批量接受插入与删除，先验证全部键，只在内存中展开变更路径，最后将每个变更节点上传一次，未修改的子树保留原引用。`apply_pages` 合并整笔事务的页映射更新；压缩合并本次调用的基线更新和增量删除。仅上传最终变更节点，中间索引根不产生 PUT。

### 发布

`publish_head` 在所有依赖持久化后设置新的 nonce 和更新时间，并递增已有 HEAD 的 revision。首次创建使用 create-if-absent，更新和墓碑复用使用 CAS。

Applied 确认成功，PreconditionFailed 返回 Conflict。Unknown 仅在重读 HEAD 的 nonce 与本次候选一致时确认成功，否则仍返回 Unknown；后续版本不能证明本次未提交。该方法不保存请求或响应，不自动重新执行 SQL。


### 页镜像

`apply_pages` 检查每个冻结页都与清单页大小一致，并且落在新文件长度之内。它把页打包成大约 256 KiB 的对象，把每个包存到 `deltas` 下，并在零填充的十六进制页号处插入 `PageLocation`（对象、偏移、页哈希）。覆盖层字节计数和事务计数增加。新的逻辑大小是冻结后的文件大小。更新后的清单存到 `manifests` 下，并作为 `ObjectRef` 返回，供调用方放到 HEAD 上。

### 读取快照

`snapshot` 构建一个绑定到一份清单和一个 `RequestContext` 的 `StoreSnapshot`。页 `n` 在 `delta_root` 存在该键时从那里加载。打包对象对照其 `ObjectRef` 检查，页字节对照 `PageLocation.sha256` 检查。否则该页从基线段 `offset / segment_size` 中切出。`ObjectReader` 为同一后端提供跨快照的 LRU 缓存，最多保留 16 MiB 对象正文和 8192 条记录（不含在途读取及调用方缓冲区）。缓存覆盖 Manifest、索引节点、Segment 和 delta 包；身份包含完整物理键、长度和 SHA-256，缓存实例绑定后端，避免跨库、跨实例代或跨存储命名空间混用。`DatabaseStore::new` 创建独立缓存，clone 共享；`with_reader` 允许服务的多次调用共享同一缓存。64 个固定分片锁合并相同对象的并发缺失，不同对象碰撞时会等待同一锁；失败不缓存，下载者取消后等待者可重试，没有脱离请求的后台下载。HEAD 和 CAS 核验始终直接访问后端。`read` 用整页拼出任意范围，并拒绝超过 `logical_size` 的范围或单次超过 1 MiB 的读取。

`DatabaseStore::get_bytes` 返回 `Result<Arc<Vec<u8>>>`。后端返回的 `Vec` 校验后直接转入共享分配，无需复制正文。缓存填充和命中均与调用方共享同一分配；元数据解析直接借用，快照页读取仅复制所需页。超过 16 MiB 的对象仍以相同共享类型返回，但不进入缓存。淘汰仅释放缓存持有的引用：调用方仍可持有已淘汰正文，这部分不计入缓存保留正文的预算。需要字节切片的调用方可使用 `as_slice()`；显式克隆内部 `Vec` 才会创建独立副本。

`StoreSnapshot` 实现 `AsyncSnapshot`。引擎的桥接在 Tokio 上调用它，SQLite 则运行在工作线程上。快照的生命周期内清单不变，因此即使另一次提交正在发布新的 HEAD，一条 SQL 语句看到的仍是一个版本。

## 请求次数回归

`cargo test --locked -p versioned-store --test io_counts -- --nocapture` 使用计数内存后端验证：64 个相邻页的索引批量写入为 69 PUT，逐项写入为 172 PUT；写入后首次读取这 64 页仅需 1 GET（数据包），69 个已确认写入的索引节点直接命中缓存，另一个快照重复读取新增 0 次数据/索引 GET。Manifest 与 HEAD 另计。这是固定用例的请求计数，不是真实 S3 延迟或容量基准。测试也覆盖历史根、混合前缀增删、缓存隔离、并发缺失、取消及 HEAD 不缓存。

全部不可变写入（包括索引刷新）显式携带出生 epoch。布局 1 的实例代路径包含 `generations/{epoch:016x}/`；出生 epoch 不需要单独写入对象。服务读取租约在读取 HEAD 前计时，收紧后不重置，缓存和远程读取前后都检查期限，停滞读取也能到期退出。GC 单独使用无读取租约的存储句柄，受自己的工作预算和请求期限限制。

路径构造与解析统一使用 `storage-namespace` 的 `DatabasePath`、`DatabaseName`、`IncarnationId` 和 `RelativeKey`。缓存从绑定单库的路径对象与引用计算键，不接受独立拼接的物理路径；HEAD 的名称字段及 JSON 引用的 key 字段使用字符串编码。存储布局版本为 1。

应用默认启用已校验不可变对象的共享磁盘缓存。读取顺序为已解析索引节点内存、对象正文内存、磁盘、S3；HEAD 和 CAS 结果确认始终访问 S3。独立 CLI 进程通过 `objects-v1.sqlite3` 复用正文。Linux 默认使用绝对路径 `$XDG_CACHE_HOME/ponydb`，未设置时使用 `$HOME/.cache/ponydb`；macOS 使用 `$HOME/Library/Caches/ponydb`，Windows 使用 `%LOCALAPPDATA%/ponydb`。`cache.directory` 可覆盖目录，`cache.disk_budget_mib = 0` 禁用磁盘缓存。`config show` 显示解析后的目录，但不创建文件。

`cache.disk_budget_mib` 限制缓存正文容量（默认 2048 MiB），同一目录下所有命名空间共享预算，条目上限为 65,536。SQLite 元数据、日志和空闲页额外占用空间，增量回收允许滞后。共享目录的进程应使用相同预算。`runtime.work_disk_budget_mib` 限制本地 SQLite 执行工作区容量，默认 2048 MiB。恢复临时文件仍不受这些配额限制。

磁盘键包含后端 endpoint/bucket/prefix、完整对象键、长度及 SHA-256，每次读取都验证正文，损坏条目重新下载。近似 LRU 合并访问记录，最多每 60 秒更新一次。磁盘查询及回填共享 50 ms 等待预算，不计 S3 耗时；繁忙或故障时降级，并限频写入 stderr 提示。每个 reader 最多一个阻塞任务，已开始的任务可在超时或取消后完成，期间仍持有执行许可；维护限频且尽力执行。缓存可随时丢弃，目录应仅向拥有数据库访问权限的可信用户开放。库默认仅使用内存缓存，需要显式传入 `DiskCacheOptions` 才启用磁盘层。

已确认成功的不可变元数据 PUT 会回填正文内存缓存及已启用的磁盘缓存，范围为 `manifests`、`segment-index`、`delta-index` 和 `snapshots`，单对象不超过 1 MiB。回填只使用剩余容量，不淘汰已有条目；数据段、增量包及自定义类别仍按需读取缓存。PUT 结果不确定时，必须直接读取后端并确认字节完全相同，才能回填。缓存故障或请求取消时跳过可选回填，不改变已确认的上传结果。索引节点首次使用时仍解析并校验，随后复用现有节点内存缓存。

## 运行诊断

debug 记录 HEAD 发布及确认结果；trace 记录远程页读取和内存、索引节点、磁盘缓存活动，包含后端相对对象 key。磁盘降级仍去重，通过结构化 warn 输出；blocking 缓存任务保留原请求上下文。 库不安装全局 subscriber。
