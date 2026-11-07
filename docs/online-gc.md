# 在线 GC（布局 1）

本文说明布局 1 下在线 GC 的调用方式、代际隔离、可达性标记和故障处理。

## 调用与结果

```sh
ponydb db notes gc
ponydb db notes gc --budget-ms 300000 --max-objects 1000 --deadline-ms 330000
```

所有传输的 GC DTO 都只包含 `database`、可选 `budget_ms` 和可选 `max_objects`。CLI 先严格解析 DTO 并合并显式参数，冲突即报错，然后使用部署默认值。默认预算为 `maintenance.gc_default_budget_ms = 60000`，显式预算上限为 `requests.max_timeout_ms`（系统最高 1800000）。`maintenance.gc_max_objects_per_call = 1000` 同时提供默认删除尝试数和部署上限，系统上限为 1000。预算不能为零。

请求超时独立配置，默认 90000 毫秒，不随 GC 预算自动延长。预算包括抢占、标记、列举、读取宽限期和删除；已开始的变更操作必须完成确认或得到未知结果后才能退出。所有权清理使用独立的 30 秒上下文，不计入工作预算。

结果只有 `outcome`、`complete`、`database_available`、`phase`、`deleted_objects`。`deleted_objects` 只计算已确认成功的删除，404 不计入，但消耗一次删除尝试预算。部分完成使用 `outcome: "partial"`，CLI 退出码为 6；`complete` 仅说明本次列举发现的候选已处理完毕。迟到的孤儿上传、之后的压缩和事务冲突仍可能产生垃圾。

不提供 pass ID、暂停开关、resume_required、retention_pruning 或 gc-recover。下一次调用重新标记并自动替换残留所有权。正常读写不依赖 GC 状态是否已清除。CLI 恢复直接构建新 incarnation，通过 epoch 和最终 HEAD CAS 与 GC 检测冲突，不预占目标。

## 代际与不可逆垃圾

不可变对象路径为：

```text
incarnations/{incarnation_id}/generations/{epoch:016x}/{category}/{uuid}
```

HEAD 的 epoch 在成功发布 GC 领取、删除、重建或恢复时单调增加，溢出报错。恢复在上传前计算旧 HEAD 的 epoch 加一作为候选对象的出生 epoch，但不会预先发布；最终 HEAD CAS 失败不会改变权威 epoch。所有页包、段、清单、COW 索引、快照索引和恢复块显式携带同一写入尝试的出生代际。正常业务提交通过 HEAD CAS 发布；失败不自动重放 SQL、不把旧页结果重新挂到新 HEAD。

只有基准 HEAD 可达对象和本次新对象才能成为提交依赖；不复用对象 key，不把已判定为垃圾的对象重新引用。布局版本不是 1 的 HEAD 明确拒绝，旧对象不自动删除。无法解析出生代际的对象使 GC 停止，不能猜测其可删除性。

对象 key 中的 `generations/{epoch:016x}/` 占 29 字节，每处序列化引用都包含该路径段。出生 epoch 编码在对象 key 中，不需要单独写入一个对象；固定 I/O 测试验证具体用例的存储请求次数。路径长度会影响元数据大小、缓存命中率和大小上限，实际请求量取决于负载。

## 领取、标记和删除

1. 读取 HEAD，用 CAS 将 epoch 从 E 增加到 E+1，并设置 `gc = {owner_id, cutoff_epoch: E+1}`。CAS 明确冲突后同时刷新 HEAD、epoch、当前清单和快照根。只有成功那次 CAS 对应的根集合可用于标记。一次调用最多领取成功一次。
2. CAS 响应丢失时按唯一 owner 和 cutoff 确认，不依赖可能已被业务写入改变的 HEAD nonce。不确认则返回未知结果，不盲目重新领取。
3. 从固定的当前清单和保留快照根完整遍历对象图。前沿、可达集、候选集保存在本地临时 SQLite 文件中，GC 不写远程索引或临时对象。
4. 完整列举数据库对象，只把出生 epoch 小于 cutoff 且不可达的对象加入候选。标记或列举不完整时，不能删除任何对象。
5. 标记和列举与读取宽限期重叠。删除最早发生在领取确认时间加 HEAD.safety_policy.drain_ms 之后。正常读取在获取 HEAD 之前启动计时，获得 HEAD 后收紧到 max_read_ms，缓存、索引和数据读取均检查到期；停滞的读操作可取消。
6. 标记、列举、删除入口各校验一次所有权，删除循环中不逐对象 GET HEAD。被接管后停止进入下一阶段并返回 partial/superseded。已经进入删除阶段的旧 GC 可以完成其已证明安全的候选；新代际对象不在这些候选中。

较新 GC 或生命周期替换可能移除旧 GC 的标记根；旧 GC 读取根失败时确认所有权，若已被替换，正常返回 superseded，绝不能凭不完整标记继续删除。

## 清理、取消和故障

结束时读取最新 HEAD，仅在 incarnation、owner 和 cutoff 都匹配时清除自己的 gc 字段，保留并发提交的清单、快照、业务版本和元数据。GC 元数据 CAS 冲突使用 10 毫秒起步、250 毫秒封顶的随机指数退避；业务提交不会因此获得透明重试。

调用方丢弃 future 会取消请求上下文，但 GC 的受跟踪任务继续等待已开始的 DELETE 并清理所有权。CLI 信号、MCP 取消或断开连接、服务正常关闭都遵循这个规则。进程被强杀可能留下记录，下一次 GC 自动接管；不检查旧进程是否存活，也不需要人工恢复。

确认删除过对象后的错误报告 `outcome: "partial"`；某次 DELETE 结果不确定时报告 `outcome: "unknown"`。错误消息保留已确认删除数。清理失败也必须报错，但残留记录不让数据库停止服务。

显式 GC 为非当前快照补记 `retired_at_upper_ms`，后续按 HEAD 的保留天数裁剪；首次观察后至少保留 N 天，0 天立即解除非当前历史保护。当前快照及其他根引用的数据始终保留，物理删除仍等待读者宽限期。时间使用与时区无关的 Unix 毫秒，部署方负责系统时钟准确。裁剪索引、计数与 GC 领取在同一次 HEAD CAS 中发布，冲突重新计算；GC 活动期间拒绝修改保留策略。

## 验证位置

`crates/database-service/tests/online_gc.rs` 覆盖领取冲突刷新根、在线写入及压缩、接管、生命周期替换、取消清理、删除尝试上限、缺失对象、不确定删除、响应丢失、残留状态、溢出、畸形路径、读期限和长宽限期配置。CLI、配置、存储 I/O、快照和恢复测试验证接口与数据完整性；真实对象存储测试使用 `tests/cli_e2e.py` 统一运行，各命令分别放在 `tests/cli_e2e/test_*.py`。GC 专项可通过 `python3 tests/cli_e2e.py --suite gc` 运行，覆盖分批续作、退出码和维护后的数据/历史完整性，并独立创建及清理测试前缀。

布局构造与解析由 `storage-namespace` 统一负责，GC 使用绑定单库的 `DatabasePath` 生成扫描前缀、反解候选并识别 category；越界键或畸形布局在删除前报错。测试还覆盖 `notes`/`notes_old` 越界列举、旧 GC 跨越文件恢复及新 GC 领取后不能清除其所有权。
