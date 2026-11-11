**English** | [中文](README.zh-CN.md)

# object-store-s3

S3 implementation of `object-store-api::BlobStore`. Conditional puts are the only writes, and the AWS SDK is not allowed to retry them.

## Purpose

ponydb treats S3 as the source of truth. `head.json` moves forward only with `If-Match`. Immutable pages and metadata appear only with `If-None-Match: *`. If a successful put response is lost, an automatic SDK retry could observe the object it just created and report a precondition failure. That would hide a committed write, so write retries are disabled and the outcome stays `Unknown` until the caller reconciles.

## How it works

`S3Store::new` accepts an `http` or `https` endpoint with no username, password, query, fragment, or extra path. Addressing follows `S3Config::force_path_style`. The bucket name is lowercase letters, digits, `-`, and `.`. The configured prefix is appended to every key and is included in the revision scope, whose identity is `endpoint|bucket|prefix`. Keys that contain empty segments, `.`, `..`, or characters outside a small ASCII set are rejected before a request is sent.

A revision token is the object ETag, wrapped by `RevisionScope`. `compare_and_swap` decodes that token and sends `PutObject` with `If-Match`. `create_if_absent` sends the same put with `If-None-Match: *`. Each put also sets the SHA-256 checksum header and a `sha256` user-metadata value. The SDK retry policy is one attempt, and the operation timeout is the configured timeout.

`classify_write` maps the HTTP result:

| Status | Outcome |
| --- | --- |
| 412 | `PreconditionFailed` |
| 401, 403 | `NotApplied(AccessDenied)` |
| 429 | `NotApplied(Throttled)`, retryable |
| 400, 404, 405, 411, 413, 416, 422 | `NotApplied(InvalidInput)` |
| anything else, including 409, 5xx, timeout, and dispatch failure | `Unknown` |

A construction failure is `NotApplied` because the request never left the client.

`read_current` checks the content length, requires `sha256` metadata, and hashes the body before returning it with the unchanged object ETag and the opaque ETag revision. `read_range` sends an HTTP Range and accepts the body only when `Content-Range` starts with the requested bounds and the length matches. Listing uses `ListObjectsV2`. The continuation cursor is JSON that repeats the scope id, prefix, and delimiter; a cursor from another query is `InvalidInput`. `delete` removes one key and classifies transport failures as `Unknown` when the outcome cannot be known.

`S3Config::max_object_bytes` is the capability limit (the runtime config currently passes 8 MiB). Live contract tests in `tests/live.rs` are ignored unless `P0_S3_ENDPOINT` and AWS credentials are set.

Object storage errors use the shared JSON error fields and business error codes. When available, `message` includes the S3 operation, HTTP status, provider error code and provider message, for example `S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access.` Configured access keys, secret keys and session tokens are replaced with `[REDACTED]`; control characters become spaces, and provider codes/messages are limited to 128/1024 characters plus an ellipsis. URL userinfo, paths and queries, and sensitive authorization/signature/token assignments are also redacted; other resource information echoed by the provider may remain. Business errors do not include request/response headers, raw response bodies or SDK error chains. Remote HTTP errors are retryable for 408, 429 and 5xx; other statuses are not retryable. Not-found, conditional-write and uncertain-commit handling takes precedence. CLI, HTTP and MCP share these diagnostics.

`storage.force_path_style` defaults to `true`, preserving path-style addressing for MinIO. Set it to `false` for virtual-hosted-style addressing required by OSS; keep the regional endpoint (for example `https://oss-cn-beijing.aliyuncs.com`) and bucket as separate settings. The SDK constructs the bucket hostname. Environment override: `PONYDB_STORAGE__FORCE_PATH_STYLE=false`. The value must be a boolean.

## Runtime diagnostics

Debug emits `s3_request_failed` before translating an SDK failure into a business error. It includes the enclosing request ID, operation, elapsed milliseconds, SDK attempt count, endpoint, actual request hostname/port, environment proxy selection (including `NO_PROXY`), SDK variant and dispatch category (`timeout`, `io`, `user`, `other`). Proxy selection follows the locked Smithy default client; proxy URLs and credentials are not logged. A construction failure may have zero attempts and no hostname.

`error_chain` is a JSON-encoded array containing the top-level `Display` and every `Error::source()` in order, without truncation. I/O entries also include `io_kind` and, when available, `raw_os_error`. Configured credentials and their URL-encoded forms, URL userinfo/paths/queries, and sensitive authorization/signature/token assignments are redacted before output. Control characters become spaces. Requests, response bodies, object contents and header maps are never formatted. Only HTTP status and allowlisted server request-ID headers are observed (`x-amz-request-id`, `x-oss-request-id`, `x-request-id`).

`timeout_config_observed=true` means the four `*_timeout_ms` fields come from the SDK's merged runtime configuration; an absent timeout field then means disabled. Otherwise the configuration is unavailable. In the current locked SDK, `BehaviorVersion::latest()` supplies a 3100ms connect timeout, read timeout is disabled, and the CLI configures operation and operation-attempt timeouts to 10000ms. The diagnostics do not change these timeouts or the one-attempt retry policy. A connect timeout can cover DNS, TCP, proxy negotiation and TLS, so its duration alone cannot identify the failing layer.

Trace also emits one completion event per adapter call: operation, actual prefixed key (LIST prefix), observed HTTP status, outcome, confirmed byte count and elapsed time. Libraries do not install a global subscriber. Raw dependency logs remain filtered; DNS/TCP/TLS phase tracing is not enabled. See the [CLI logging instructions](../../apps/cli/README.md#runtime-logs).
