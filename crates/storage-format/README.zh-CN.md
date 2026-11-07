[English](README.md) | **中文**

# storage-format

版本化元数据的 JSON 类型。每条不可变对象引用都带有键、字节长度和 SHA-256。必须精确往返的整数计数器使用十进制字符串。

## 用途

清单、页索引、快照历史和 `head.json` 都以对象形式存储。其他进程中的读者，以及崩溃后的读者，需要一个有界的模式：未知字段被拒绝，引用可以对照它所指向的字节检查，`u64` 版本也不会被 JSON 数字解析器损坏。

`FORMAT_VERSION` 为 1。`LAYOUT_VERSION` 为 1。大于 `MAX_METADATA_BYTES`（1 MiB）的元数据文档视为损坏。

创建和更新请求另将用户 `metadata` 限制为紧凑 JSON 序列化后的 64 KiB UTF-8 字节，`description` 限制为 16 KiB UTF-8 字节，为 JSON 转义和 HEAD 内部字段（包括 GC 所有权及计数器增长）预留空间。持久化 HEAD 按存储格式约束校验，完整文档上限为 1 MiB；用户元数据的请求大小限制适用于创建和更新输入。

## 工作原理

`digest` 是小写十六进制 SHA-256。`ObjectRef` 是 `{ key, size, sha256 }`。`validate` 要求一个 `storage-namespace` 相对键和 64 个十六进制字符。仅当键有效、`size` 等于缓冲区长度、且 `sha256` 等于该缓冲区的 `digest` 时，`matches` 为真。调用方在每次读取被引用对象后用这个辅助函数比较字节。

`IndexNode` 是压缩基数树的一个节点。`prefix` 是到目前为止共享的键，最多 64 个十六进制字符。`value` 是可选的小型 JSON 文档，不是内嵌的页映射或 SQL 载荷。`children` 把一个十六进制数字映射到另一个节点的 `ObjectRef`，最多 16 个子节点。节点不可变；插入写入新节点，旧节点留在原地。

`Manifest` 描述一份完整的数据库映像：

- `page_size`：2 的幂，512 到 65536
- `logical_size`：页大小的整数倍，至多 `i64::MAX`
- `segment_size`：页大小的整数倍，从一页到 1 MiB
- `base_root`：基线段的基数索引，键为段号
- `delta_root`：被覆盖页的基数索引，键为页号
- `overlay_bytes` 与 `overlay_transactions`：这份映像携带多少尚未压缩的增量
- `generation_id`：这一代基线的身份

读页时优先查增量索引。未命中则落到包含该页的基线段。

`PageLocation` 指向一个打包的增量对象：`ObjectRef`、页在对象内的字节偏移，以及该页单独的 SHA-256。

`History` 是一条保留的快照：快照 ID、实例代、十进制 `data_version`、清单引用和观察时间。`retired_at_upper_ms` 可选。新的历史记录留空该字段，GC 后续补记退役时间并按保留期裁剪历史。

`Head` 是唯一的可变文档（`head.json`）。它携带协议版本和布局版本、不可修改的数据库名称、创建/恢复代际、十进制的 `revision`、`data_version`、`metadata_version` 和 `epoch`、一个 `head_nonce`、展示用元数据、状态（`active` 或 `deleted`）、当前清单、可选的快照索引根、快照保留策略，以及安全策略。`gc` 保存在线 GC 的所有权与 epoch。`validate` 检查数据库名称与正在读取的目录一致、代际是安全路径段、清单引用格式正确，以及 `drain_ms` 在允许窗口内不小于 `max_read_ms`。

`decimal` 模块把 `u64` 序列化为规范的十进制字符串：只有数字，除了零本身之外没有前导零。反序列化拒绝任何其他形状。


布局 1 的不可变对象路径为 `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`。HEAD.gc 仅记录 owner_id 与 cutoff_epoch，不阻止数据库访问；epoch 跨生命周期单调增加。其他布局版本拒绝读取。

显式 GC 为非当前快照补记 `retired_at_upper_ms`，后续按 HEAD 的保留天数裁剪；首次观察后至少保留 N 天，0 天立即解除非当前历史保护。当前快照及其他根引用的数据始终保留，物理删除仍等待读者宽限期。时间使用与时区无关的 Unix 毫秒，部署方负责系统时钟准确。

布局构造和解析由 `storage-namespace` 统一负责。`ObjectRef::relative_key` 将现有字符串键转换为已验证的相对引用；一般相对引用的安全校验与 v1 对象布局识别分开。`incarnation_id` 表示生命周期，`epoch` 表示 GC/写入隔离序号，Manifest 的 `generation_id` 表示基线表示版本，三者用途不同。
