**English** | [中文](README.zh-CN.md)

# storage-format

JSON types for versioned metadata. Every immutable object reference carries its key, byte length, and SHA-256. Integer counters that must round-trip exactly are decimal strings.

## Purpose

Manifests, page indexes, snapshot history, and `head.json` are stored as objects. Readers in other processes, and readers after a crash, need a bounded schema: unknown fields are rejected, references can be checked against the bytes they name, and a `u64` version cannot be damaged by a JSON number parser.

`FORMAT_VERSION` is 1. `LAYOUT_VERSION` is 1. A metadata document larger than `MAX_METADATA_BYTES` (1 MiB) is corrupt.

Separately, create/update requests cap user `metadata` at 64 KiB of compact JSON UTF-8 bytes and `description` at 16 KiB of UTF-8 bytes. Those request limits leave room for JSON escaping and internal HEAD fields, including GC ownership and counter growth. Persisted HEADs are validated against storage-format constraints, including the 1 MiB document cap; the user metadata request limit applies to create/update inputs.

## How it works

`digest` is lowercase hex SHA-256. `ObjectRef` is `{ key, size, sha256 }`. `validate` requires a `storage-namespace` relative key and 64 hex characters. `matches` is true only when the key is valid, `size` equals the buffer length, and `sha256` equals `digest` of that buffer. Callers compare bytes with this helper after every read of a referenced object.

`IndexNode` is one node of a compressed radix tree. `prefix` is the shared key so far, at most 64 hex characters. `value` is an optional small JSON document, never an embedded page map or a SQL payload. `children` maps a single hex digit to an `ObjectRef` of another node, with at most 16 children. Nodes are immutable; an insert writes new nodes and leaves the old ones in place.

`Manifest` describes one complete database image:

- `page_size` — power of two, 512 through 65536
- `logical_size` — multiple of the page size, at most `i64::MAX`
- `segment_size` — multiple of the page size, from one page up to 1 MiB
- `base_root` — radix index of base segments, keyed by segment number
- `delta_root` — radix index of overwritten pages, keyed by page number
- `overlay_bytes` and `overlay_transactions` — how much uncompacted delta the image carries
- `generation_id` — identity of this base generation

A page read prefers the delta index. A miss falls through to the base segment that contains that page.

`PageLocation` points at a packed delta object: the `ObjectRef`, the byte offset of one page inside it, and the SHA-256 of that page alone.

`History` is one retained snapshot: snapshot id, incarnation, decimal `data_version`, manifest reference, and observation time. `retired_at_upper_ms` is optional. New history records leave it unset, and GC later records retirement and expires history.

`Head` is the single mutable document (`head.json`). It carries protocol and layout versions, immutable database name, creation/restore incarnation, decimal `revision`, `data_version`, `metadata_version`, and `epoch`, a `head_nonce`, display metadata, status (`active` or `deleted`), the current manifest, optional snapshot index root, snapshot retention, and the safety policy. `gc` holds only `{owner_id, cutoff_epoch}`; it does not make the database unavailable. Immutable keys use `incarnations/{incarnation}/generations/{epoch:016x}/{category}/{uuid}`. Epochs increase across GC and lifecycle changes; layout 1 is required. `validate` checks that the database name matches the directory being read and the incarnation is a safe path segment, that the manifest reference is well formed, and that `drain_ms` is at least `max_read_ms` within the allowed window.

The `decimal` module serializes `u64` as a canonical decimal string: digits only, no leading zeros except for zero itself. Deserialization rejects any other shape.


Explicit GC records `retired_at_upper_ms` for superseded snapshots and expires them on later calls after the HEAD retention period. The first observation starts the N-day wait; zero days removes all non-current history. Current and shared data remain protected, and physical deletion still waits for reader drain. Time uses timezone-independent Unix milliseconds; deployments must keep system clocks accurate.

Layout construction and parsing belong to `storage-namespace`. `ObjectRef::relative_key` converts the existing string key into a validated relative reference; relative-key safety and v1 object-layout recognition are separate checks. `incarnation_id` identifies a lifecycle, `epoch` provides GC/write fencing, and the manifest `generation_id` identifies a baseline representation.
