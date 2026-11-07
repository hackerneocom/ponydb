**English** | [中文](README.zh-CN.md)

# versioned-store

Immutable objects, COW indexes, and the sole HEAD publication point. All data dependencies must be durable before the HEAD CAS publishes them.

## Purpose

Many writers may prepare SQL against the same snapshot. Exactly one of them may advance the database. This crate implements that rule on top of `BlobStore`: page images and metadata are created once, indexes are persistent radix trees of those objects, and `publish_head` is the single compare-and-swap of `head.json`. It also turns a manifest back into the byte stream `remote-pager` reads.

## How it works

`DatabaseStore` is rooted at `databases/{database}/`. Every relative key is checked with `storage-namespace` before it is joined to that root. The backend must pass `StoreCapabilities::validate`.

`read_head` returns `Error::DatabaseNotFound(name)` for a missing HEAD. `get_bytes` returns `Error::MissingObject(name)` when a referenced immutable object is missing, including manifest and index objects. Callers can distinguish an absent database from incomplete storage without exposing object keys in business errors.

`read_head` returns `Error::InvalidHead { database, reason }` for invalid HEAD metadata; the service maps it to `STORAGE_CORRUPT`, with the database name, `HEAD validation failed`, and a bounded reason. For example: `Database 'test' HEAD validation failed: unknown field 'restore'; stored HEAD may be incompatible with this version`. Reasons distinguish invalid JSON, oversized metadata, unknown or missing fields, invalid field types/values, and violated HEAD constraints. Field names are shown only for ASCII letters, digits and underscores up to 64 characters; other names are redacted. Stored field values, object keys and raw Serde errors are excluded. HEAD validation is strict. These errors have `retryable: false`, `outcome: not_committed`, and CLI exit code 1.

### Immutable puts

`put_bytes` chooses `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`, hashes the body, and calls `create_if_absent`. `Applied` returns the `ObjectRef`. `Unknown` reads the key back and returns the same reference only when the stored bytes are identical; otherwise the upload is unconfirmed. `PreconditionFailed` on a fresh uuid is treated as corruption. `put` is the same path for a JSON value, rejected above `MAX_METADATA_BYTES`. `get_bytes` requires `ObjectRef::matches` on the first read, then may reuse verified bytes for that immutable reference. Verification of Unknown uploads still reads the backend directly.

### Indexes

Index keys are 1 to 64 hex characters. `index_get` walks `IndexNode`s from a root. Each step must lengthen the prefix. A missing edge or a prefix mismatch is a missing key. A walk deeper than 64 nodes is corrupt.

`index_insert` copies the path. It writes a new leaf when the root is empty, splits a node when the key diverges in the middle of a prefix, and replaces a node when the key continues through a child. Old nodes stay reachable until compaction and GC decide they are unused. `index_remove` writes a path that omits the key. `index_list` walks in key order from an optional exclusive cursor and stops at a caller limit of at most 1000 entries.

Snapshot history, delta page maps, and base segment maps share this index structure.

The shared `ObjectReader` retains parsed, intrinsically validated `Arc<IndexNode>` values in an independent LRU, with an 8 MiB estimated node weight budget and at most 4096 entries. The key includes the bound database's full physical path, size, and SHA-256; the reader binds the backend. `index_get`, `index_list`, and COW updates reuse these nodes across calls and snapshots, even if their raw bodies have been evicted. Updates clone fields into private dirty nodes only when a change is needed. Parent-prefix relationships, lookup matches, and traversal limits are checked on each walk.

Node fills use a separate set of 64 striped locks: after acquiring a lock the reader checks the node cache again, then obtains verified bytes through the body cache, decodes JSON, and validates the node before admission. Node fills can wait for body fills, never the reverse; neither cache mutex is held during parsing or I/O. Failed loads and invalid nodes do not enter the node cache, although verified raw bytes may remain cached. Cancellation releases fill locks so a waiter can retry. Read leases also apply to node-cache hits.

Node weight estimates inline storage and recursively owned string/array capacities, child references, and JSON map entries, allowing 64 bytes of overhead per BTreeMap entry. Saturating arithmetic prevents weight overflow. Nodes exceeding this budget are returned without admission; evicted nodes remain valid while callers hold an `Arc`. The estimate excludes cache bookkeeping, in-flight buffers, caller-retained evicted nodes, and allocator-specific overhead. The separate 16 MiB body budget and 8 MiB node estimate are not a process memory limit. Neither budget is currently configurable.

The asynchronous index write methods share an `IndexWriteTarget` containing the incarnation, epoch, and category; the request context is passed separately.

`index_update` accepts a batch of inserts and removals, validates all keys first, materializes only changed paths in memory, then uploads each changed node once. Unchanged subtrees retain their original references. `apply_pages` batches page mappings across the entire transaction; compaction batches base updates and delta removals across the call. Only final changed nodes are uploaded; intermediate index roots do not generate PUTs.

### Publish

`publish_head` sets a fresh nonce and timestamp after all dependencies are durable, and increments an existing HEAD's revision. Initial creation uses create-if-absent; updates and tombstone reuse use CAS.

Applied confirms success; PreconditionFailed returns Conflict. Unknown is confirmed only when the current HEAD has this candidate's exact nonce; otherwise it remains Unknown. A later version cannot prove that this candidate was never committed. No business request or response is stored, and SQL is never automatically re-executed.


### Page images

`apply_pages` checks that every frozen page matches the manifest page size and lies inside the new file length. It packs pages into objects of about 256 KiB, stores each pack under `deltas`, and inserts a `PageLocation` (object, offset, page hash) at the zero-padded hex page number. Overlay byte and transaction counters increase. The new logical size is the frozen file size. The updated manifest is stored under `manifests` and returned as an `ObjectRef` for the caller to put on the head.

### Reading a snapshot

`snapshot` builds a `StoreSnapshot` bound to one manifest and one `RequestContext`. Page `n` is loaded from `delta_root` when that key exists. The packed object is checked against its `ObjectRef`, and the page bytes are checked against `PageLocation.sha256`. Otherwise the page is sliced out of the base segment `offset / segment_size`. `ObjectReader` provides an LRU cache shared across snapshots on one backend, retaining up to 16 MiB of object bodies and 8192 entries (excluding in-flight reads and caller buffers). It caches manifests, index nodes, segments, and delta packs. Identity includes the full physical key, size, and SHA-256; the reader owns its backend, isolating databases, incarnations, and storage namespaces. `DatabaseStore::new` creates an independent cache shared by clones; `with_reader` lets service calls share a reader. A fixed set of 64 striped locks coalesces concurrent misses for the same object; unrelated objects may wait on the same stripe. Failures are not cached. If the downloader is cancelled, a waiter can retry; no background download outlives its request. HEAD and CAS verification always access the backend directly. `read` assembles an arbitrary range from whole pages and refuses a range past `logical_size` or a single read above 1 MiB.

`DatabaseStore::get_bytes` returns `Result<Arc<Vec<u8>>>`. The verified backend `Vec` is moved into the shared allocation without copying its body. Cache fills and hits share that allocation with callers; metadata decoding borrows it, and snapshot page reads copy only the selected page. Objects larger than 16 MiB are returned through the same shared type but bypass the cache. Eviction drops only the cache's reference: callers can keep evicted bodies alive, and those bytes are outside the cache's retained-body budget. Consumers needing a byte slice can use `as_slice()`; explicitly cloning the inner `Vec` creates an owned copy.

`StoreSnapshot` implements `AsyncSnapshot`. The engine's bridge calls it from Tokio while SQLite runs on a worker thread. The manifest does not change for the life of the snapshot, so a SQL statement sees one version even while another commit publishes a new head.

## Request count regression

`cargo test --locked -p versioned-store --test io_counts -- --nocapture` uses a counting memory backend: indexing 64 adjacent pages takes 69 PUTs in a batch versus 172 sequentially; the first 64-page read after writing takes one GET (the data pack), since the 69 confirmed index PUTs fill the cache, and repeating it through another snapshot adds zero data/index GETs. Manifest and HEAD reads are counted separately. These are fixed-fixture request counts, not real S3 latency or capacity benchmarks. Tests also cover historical roots, mixed-prefix inserts/removals, cache isolation, concurrent misses, cancellation, and uncached HEAD reads.

All immutable writes carry an explicit birth epoch, including index flushes. Layout 1 includes `generations/{epoch:016x}/` in each incarnation path; the birth epoch does not require separate object writes. Service read leases are anchored before HEAD, tightened without restarting, and checked before and after cached or remote reads; stalled reads can expire. GC alone uses an unleased store and its own work/request budgets.

Path construction and parsing use `DatabasePath`, `DatabaseName`, `IncarnationId` and `RelativeKey` from `storage-namespace`. The cache resolves references against the bound database path rather than accepting an independently assembled physical key. Database names in HEAD and keys in JSON object references are encoded as strings. The storage layout version is 1.

The application enables a shared disk cache for verified immutable objects by default. Reads use parsed-node memory, object-body memory, disk, then S3; HEAD and CAS verification always use S3. Separate CLI processes can reuse `objects-v1.sqlite3`. Linux uses `$XDG_CACHE_HOME/ponydb` (absolute paths only), falling back to `$HOME/.cache/ponydb`; macOS uses `$HOME/Library/Caches/ponydb`, Windows `%LOCALAPPDATA%/ponydb`. `cache.directory` overrides this location; `cache.disk_budget_mib = 0` disables disk caching. `config show` reports the resolved directory without creating files.

`cache.disk_budget_mib` limits cached body bytes (default 2048 MiB), shared across all namespaces in a directory, with at most 65,536 entries. SQLite metadata, journals and free pages are additional; incremental reclamation may lag. Processes sharing a directory should use the same budget. `runtime.work_disk_budget_mib` limits local SQLite execution workspace bytes (default 2048 MiB). Restore temporary files remain outside these quotas.

Disk entries are isolated by backend endpoint/bucket/prefix, full object key, length and SHA-256. Every disk read verifies the body, and invalid entries are refetched. Approximate LRU touches are coalesced and throttled to 60 seconds. Disk lookup and fill share a 50 ms wait budget excluding S3 time; busy/failed caches fall back with bounded stderr diagnostics. One blocking task per reader may finish after timeout or cancellation, retaining its execution permit; maintenance is throttled and best effort. Cache contents are disposable and should be stored in a directory trusted by the same users as the database credentials. Library readers remain memory-only unless explicitly configured with `DiskCacheOptions`.

Confirmed immutable metadata PUTs also fill the body cache and, when enabled, the disk cache. Admission covers `manifests`, `segment-index`, `delta-index` and `snapshots`, up to 1 MiB per object. It uses spare capacity without evicting existing entries; data segments, delta packs and custom categories remain demand-loaded. An unknown PUT outcome must first be confirmed by a direct backend read with identical bytes. Cache failure or cancellation skips the optional fill without changing the confirmed upload result. Index nodes are parsed and validated on first use, then shared through the existing node cache.

## Runtime diagnostics

Debug records HEAD publication and confirmation outcomes. Trace records remote-page reads and memory/node/disk cache activity, including backend-relative object keys. Disk degradation remains deduplicated and uses structured warn events. Blocking cache work preserves the originating request context. Libraries do not install a global subscriber.
