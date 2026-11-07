[English](README.md) | **中文**

# object-store-test-support

内存中的 `BlobStore`，以及任何真实后端都可以运行的契约测试。这个 crate 是测试夹具。它不是持久化后端，进程结束后数据也不保留。

## 用途

`versioned-store`、`database-service` 和服务测试需要一个与 S3 遵守相同条件写规则的存储，并且要能模拟成功响应丢失。契约函数只通过公开 trait 检查这些规则，因此同一组断言既能跑在 `MemoryStore` 上，也能跑在真实 S3 桶上。

## 工作原理

`MemoryStore` 用互斥锁保护一张从键到正文的 `BTreeMap`。每个实例有自己的 `RevisionScope`，因此一个存储发出的修订不能被另一个存储解码。修订令牌是一对值：单调计数器和常数 `17`。`create_if_absent` 仅在键不存在时成功。`compare_and_swap` 仅在 `decode` 得到这一对值、且已存储的计数器仍然匹配时成功。两次并发创建或交换中，恰好一方观察到 `Applied`，失败方看到 `PreconditionFailed`。成功交换之后的过期令牌也会看到 `PreconditionFailed`。

`lose_next_response` 是一次性故障。下一次成功的 Put 仍然保存正文，然后返回 `CasOutcome::Unknown`。调用方可以测试对账路径：把对象读回来，仅在字节一致时接受它。

`read_range` 对反向范围或超出对象的范围返回错误。`list` 对排好序的键分页，可选按 `/` 分隔符分组，并使用数字游标。`delete` 报告 `Deleted` 或 `Missing`。

`contract(store)` 先校验能力，然后对 16 个键各发起两次创建竞争和两次比较并交换竞争，重试过期修订，并检查范围读、列举和删除。断言通过后，它删除自己创建的键。调用时使用隔离的前缀。
