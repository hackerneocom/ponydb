**English** | [中文](README.zh-CN.md)

# object-store-test-support

An in-memory `BlobStore` and a contract test that any real backend can run. This crate is a test fixture. It is not a persistence backend and it does not survive the process.

## Purpose

`versioned-store`, `database-service`, and the server tests need a store that implements the same conditional-write rules as S3, including a way to simulate a lost success response. The contract function checks those rules through the public trait only, so the same assertions can run against `MemoryStore` and against a live S3 bucket.

## How it works

`MemoryStore` keeps a `BTreeMap` of keys to bodies behind a mutex. Each instance gets its own `RevisionScope`, so a revision issued by one store cannot be decoded by another. The revision token is a pair: a monotonic counter and the constant `17`. `create_if_absent` succeeds only when the key is missing. `compare_and_swap` succeeds only when `decode` yields that pair and the stored counter still matches. Exactly one of two concurrent creates or swaps can observe `Applied`; the loser sees `PreconditionFailed`. A stale token after a successful swap also sees `PreconditionFailed`.

`lose_next_response` is a one-shot fault. The next successful put still stores the body, then returns `CasOutcome::Unknown`. Callers can test the reconcile path: read the object back and accept it only when the bytes match.

`read_range` returns an error for an inverted range or a range past the object. `list` pages a sorted set of keys, with optional `/` delimiter grouping, and uses a numeric cursor. `delete` reports `Deleted` or `Missing`.

`contract(store)` validates capabilities, then for 16 keys races two creates and two compare-and-swaps, retries the stale revision, and checks range reads, listing, and deletion. It deletes the keys it created when the assertions pass. Call it with an isolated prefix.
