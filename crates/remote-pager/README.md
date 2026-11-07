**English** | [中文](README.zh-CN.md)

# remote-pager

A private copy-on-write file over one immutable snapshot. SQLite sees a normal byte-addressable file. Remote storage is touched only for pages this candidate actually reads, and a freeze returns the full pages that would be published.

## Purpose

A SQL statement must run against a fixed version of the database, then either discard its writes or hand the dirty pages to `versioned-store`. The pager is that private layer. It does not upload anything. Publishing is a later step, after SQLite has committed locally.

## How it works

### Snapshot

`Snapshot::read_exact_at` returns exactly the requested bytes of one immutable version. A missing object is an I/O error, not end of file. `CowFile::new` requires a power-of-two page size from 512 to 65536 bytes and a base length that is a multiple of that size.

### Copy-on-write file

`CowFile` keeps the base length, a high-water `base_limit`, and a map of dirty pages. A read copies from a dirty page when one exists. Otherwise it fills a page from the base up to `base_limit` and treats the rest as zeros. A short read is reported only when the logical file ends inside the caller's buffer; the unread tail of that buffer is zero. A missing base page is never turned into zeros.

A write computes every page it will touch, including holes created by writing past the current end. It loads those pages first. If a remote read fails, or if the new pages would exceed `max_dirty_bytes`, the map and the length stay unchanged. Only after every page is staged does the file install them and extend its length.

Truncate shrinks the logical length and drops dirty pages past the new end. The partial last page is zeroed. `base_limit` falls to the new length, so bytes that used to live beyond the cut cannot reappear if the file grows again. Growing the file is a write of a zero at the new last byte, which materializes the hole as dirty pages.

`freeze` requires a page-aligned length. It marks the file frozen, moves the dirty map out, and returns `FrozenPages`: page size, final file size, and the full after-image of every dirty page. Further reads of the logical file still work from those images only through the returned map; further writes fail with `Frozen`.

### Synchronous bridge

SQLite's VFS callbacks are synchronous, and the snapshot lives on async object storage. `bridge` splits that:

- `BridgeSnapshot` implements `Snapshot` and runs on the database worker. `read_exact_at` checks the request deadline, serves a small exact-range cache, then `try_send`s one read on a channel of capacity 1. It waits on a synchronous reply channel in 10 ms slices, re-checking the deadline between slices. A saturated channel is an I/O error rather than an unbounded queue.
- `BridgeServer::serve` runs on Tokio, never on the worker that is blocked in `read_exact_at`. It performs `AsyncSnapshot::read_at` and sends the bytes back. Dropping that future cancels the in-flight read.

`StoreSnapshot` in `versioned-store` is the production `AsyncSnapshot`: its length and bytes are fixed to one manifest for the life of the read.

## Runtime diagnostics

Trace records page read/write completion and bridge-cache hit/miss/fill/eviction events without inspecting page contents. Pager and remote-snapshot reads are counted separately in request summaries. Libraries do not install a global subscriber.
