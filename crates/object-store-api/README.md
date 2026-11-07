**English** | [中文](README.zh-CN.md)

# object-store-api

The storage contract used by every backend. Callers can create an object only if it is absent, or replace it only if they still hold the revision they read. There is no unconditional overwrite.

## Purpose

Correctness of a ponydb database depends on one atomic publication point (`head.json`) and on immutable objects that are never silently replaced. The protocol layer must be able to tell four write results apart: applied, lost the race, failed before apply, and unknown. Backend-specific revision tokens must not leak into portable manifests.

## How it works

`BlobStore` is the trait every adapter implements:

| Method | Contract |
| --- | --- |
| `read_current` | Return the body, object ETag, and opaque revision from the same read. |
| `read_range` | Read an exact byte range of an immutable object. A missing or truncated range is an error. |
| `create_if_absent` | Create the key only when it does not exist. |
| `compare_and_swap` | Replace the key only when it still has the expected revision. |
| `list` | Return one page under a prefix, optionally with a delimiter. |
| `delete` | Delete one object. |

`StoreCapabilities` must advertise strong reads, atomic create, atomic compare-and-swap, range reads, paginated listing, per-object delete, and a positive maximum put size. `validate` rejects a backend that is missing any of those. `database-service` calls `validate` before it will run.

`ObjectRevision` is an opaque token. An adapter creates a `RevisionScope` for one backend and one namespace (endpoint, bucket, and prefix). `issue` binds a key and a type-erased token into a revision. `decode` succeeds only when the scope, the key, and the downcast type all match. `Debug` does not print the token. Revisions are process-local and are never serialized into manifests; durable identity is the object key plus SHA-256 and size.

`CasOutcome` classifies a write:

- `Applied` — the write landed. The adapter may return the new revision.
- `PreconditionFailed` — the backend refused the write because the precondition did not hold.
- `Unknown` — the write may have landed. A later 412 cannot erase that uncertainty, so callers must reconcile by reading current state.
- `NotApplied` — the write did not land, with a `StoreError` and a retryable flag.

`DeleteOutcome` is the same idea for deletes: `Deleted`, `Missing`, `Unknown`, or `NotApplied`.

List cursors are adapter-defined strings. The adapter must bind each cursor to the namespace, prefix, and delimiter that produced it, and reject a cursor presented with a different query.

Object storage errors use the shared JSON error fields and business error codes. When available, `message` includes the S3 operation, HTTP status, provider error code and provider message, for example `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` Configured access keys, secret keys and session tokens are replaced with `[REDACTED]`; control characters become spaces, and provider codes/messages are limited to 128/1024 characters plus an ellipsis. This minimal redaction does not hide all resource information echoed by the provider. Request/response headers, raw response bodies and SDK error chains are not included. Remote HTTP errors are retryable for 408, 429 and 5xx; other statuses are not retryable. Not-found, conditional-write and uncertain-commit handling takes precedence. CLI, HTTP and MCP share these diagnostics.
