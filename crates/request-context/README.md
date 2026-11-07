**English** | [中文](README.zh-CN.md)

# request-context

Shared cancellation and deadline for one ponydb request. SQL progress checks, worker admission, the synchronous VFS bridge, and object-store waits all call the same `RequestContext`.

## Purpose

A request can spend time in a Tokio task, on a dedicated SQLite thread, and inside blocking snapshot reads. Those sites need one clock and one cancel signal. When the caller drops the request, or either clock runs out, later stages stop instead of publishing a late result.

## How it works

`RequestContext::new` records three things:

- a monotonic start (`Instant`)
- a wall-clock start (`SystemTime`)
- a `CancellationToken`

`check` fails with `Stop::Cancelled` when the token is cancelled. It fails with `Stop::Deadline` when either clock has reached the budget, and also when wall time has moved backwards relative to the start. Both clocks are required because monotonic time can exclude system suspension on some platforms, while a backwards wall clock means the deadline can no longer be trusted.

`with_max_duration` returns a clone whose budget is the minimum of the current budget and the new limit. The start times stay the same, so a tighter SQL read limit cannot extend a request that is already near its deadline. `cancel` fires the token. `cancelled` awaits that signal.

Callers own the policy for when to cancel. `database-service` cancels the context when `Service::call` returns, and the SQLite worker pool cancels it if the prepare future is dropped before the worker finishes.

`RequestContext::unbounded()` creates a cancellable context without a total deadline. Its `check` only checks cancellation; `with_max_duration` can add a deadline anchored to the original start. CLI file restore uses this mode and still awaits local workers on cancellation. Individual S3 operations retain their own timeouts.
