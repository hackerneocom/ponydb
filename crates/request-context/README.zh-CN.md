[English](README.md) | **中文**

# request-context

一次 ponydb 请求共用的取消信号和截止时间。SQL 进度检查、工作线程准入、同步 VFS 桥接，以及对象存储等待，都调用同一个 `RequestContext`。

## 用途

一个请求会在 Tokio 任务、专用 SQLite 线程和阻塞的快照读取之间切换。这些位置需要同一套时钟和同一个取消信号。调用方丢弃请求，或任一时钟用尽时，后续阶段停止，避免发布一个已经过期的结果。

## 工作原理

`RequestContext::new` 记录三件事：

- 单调时钟起点（`Instant`）
- 墙上时钟起点（`SystemTime`）
- 一个 `CancellationToken`

令牌被取消时，`check` 返回 `Stop::Cancelled`。任一时钟达到预算时返回 `Stop::Deadline`；墙上时间相对起点倒退时同样返回 `Stop::Deadline`。两套时钟都要检查：在部分平台上，单调时钟不计系统休眠，而墙上时钟倒退意味着截止时间已经不可信。

`with_max_duration` 返回一个克隆，其预算是当前预算与新上限中的较小值。起点不变，因此更紧的 SQL 读取上限不能把一个已经接近截止时间的请求再延长。`cancel` 触发令牌。`cancelled` 等待该信号。

何时取消由调用方决定。`database-service` 在 `Service::call` 返回时取消上下文；SQLite 工作线程池在 prepare future 于工作线程结束前被丢弃时取消上下文。

`RequestContext::unbounded()` 创建没有总截止时间、但仍可取消的上下文；`check` 仅检查取消信号，`with_max_duration` 可基于原起点添加时限。CLI 文件恢复使用此模式，取消后仍等待本地工作线程退出；单次 S3 操作保留自身超时。
