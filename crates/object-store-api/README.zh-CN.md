[English](README.md) | **中文**

# object-store-api

所有后端共用的存储契约。调用方只能在对象不存在时创建它，或在仍然持有上次读到的修订时替换它。不存在无条件覆盖。

## 用途

ponydb 数据库的正确性依赖一个原子发布点（`head.json`），以及不会被悄悄替换的不可变对象。协议层必须能区分四种写结果：已生效、竞争失败、生效前失败、结果未知。后端专有的修订令牌不能泄漏进可移植的清单。

## 工作原理

`BlobStore` 是每个适配器实现的 trait：

| 方法 | 契约 |
| --- | --- |
| `read_current` | 返回同一次读取中的正文、对象 ETag 和不透明修订。 |
| `read_range` | 读取不可变对象的精确字节范围。缺失或截断的范围是错误。 |
| `create_if_absent` | 仅当键不存在时创建。 |
| `compare_and_swap` | 仅当键仍是期望修订时替换。 |
| `list` | 返回某个前缀下的一页，可选分隔符。 |
| `delete` | 删除一个对象。 |

`StoreCapabilities` 必须声明强一致读、原子创建、原子比较并交换、范围读、分页列举、逐对象删除，以及一个大于零的单次写入上限。`validate` 拒绝缺少其中任一项的后端。`database-service` 在运行前调用 `validate`。

`ObjectRevision` 是不透明令牌。适配器为一个后端和一个命名空间（端点、桶、前缀）创建一个 `RevisionScope`。`issue` 把键和类型擦除后的令牌绑定成修订。`decode` 仅在作用域、键和向下转型的类型全部匹配时成功。`Debug` 不打印令牌。修订只在进程内有效，不会序列化进清单；持久身份是对象键加上 SHA-256 和大小。

`CasOutcome` 给写入分类：

- `Applied`：写入已落地。适配器可以返回新修订。
- `PreconditionFailed`：后端因前置条件不成立而拒绝写入。
- `Unknown`：写入可能已经落地。之后的 412 不能抹掉这种不确定性，调用方必须读取当前状态来对账。
- `NotApplied`：写入没有落地，附带 `StoreError` 和是否可重试的标志。

`DeleteOutcome` 对删除使用同一思路：`Deleted`、`Missing`、`Unknown` 或 `NotApplied`。

列举游标是适配器定义的字符串。适配器必须把每个游标绑定到产生它的命名空间、前缀和分隔符，并拒绝与另一次查询一起提交的游标。

对象存储错误使用统一的 JSON 错误字段和业务错误码。可用时，`message` 包含 S3 操作、HTTP 状态、服务端错误码及消息，例如 `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` 配置中的 AccessKey ID、Secret AccessKey 和 Session Token 替换为 `[REDACTED]`，控制字符替换为空格，服务端错误码/消息分别限制为 128/1024 个字符，超长追加省略号。这种最小脱敏不会隐藏服务端回显的所有资源信息。不附带请求头、响应头、原始响应正文或 SDK 错误链。远端 HTTP 错误中，408、429、5xx 可重试，其他状态不可重试；对象不存在、条件写和提交不确定的专门处理优先。CLI、HTTP 和 MCP 共享这些诊断。
