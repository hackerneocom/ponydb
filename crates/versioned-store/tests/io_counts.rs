use object_store_api::*;
use object_store_test_support::MemoryStore;
use remote_pager::{bridge::AsyncSnapshot, FrozenPages};
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use storage_format::*;
use tokio::sync::Semaphore;
use versioned_store::{DatabaseStore, IndexWriteTarget, ObjectReader};

struct Counted {
    inner: MemoryStore,
    gets: AtomicUsize,
    puts: AtomicUsize,
    last_body_ptr: AtomicUsize,
    max_single_put_bytes: Option<u64>,
    block: AtomicBool,
    started: Semaphore,
    release: Semaphore,
}
impl Default for Counted {
    fn default() -> Self {
        Self {
            inner: MemoryStore::default(),
            gets: AtomicUsize::new(0),
            puts: AtomicUsize::new(0),
            last_body_ptr: AtomicUsize::new(0),
            max_single_put_bytes: None,
            block: AtomicBool::new(false),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}
impl BlobStore for Counted {
    fn capabilities(&self) -> StoreCapabilities {
        let mut capabilities = self.inner.capabilities();
        if let Some(limit) = self.max_single_put_bytes {
            capabilities.max_single_put_bytes = limit;
        }
        capabilities
    }
    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        if self.block.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        let object = self.inner.read_current(key).await?;
        self.last_body_ptr
            .store(object.body.as_ptr() as usize, Ordering::SeqCst);
        Ok(object)
    }
    async fn read_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        self.inner.read_range(key, range).await
    }
    async fn create_if_absent(&self, key: &str, body: Vec<u8>) -> CasOutcome {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.inner.create_if_absent(key, body).await
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> CasOutcome {
        self.inner.compare_and_swap(key, expected, body).await
    }
    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        self.inner.list(request).await
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.inner.delete(key).await
    }
}
fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(30))
}
fn manifest() -> Manifest {
    Manifest {
        version: 1,
        page_size: 4096,
        logical_size: 0,
        segment_size: 262144,
        generation_id: "g1".into(),
        base_root: None,
        delta_root: None,
        overlay_bytes: 0,
        overlay_transactions: 0,
    }
}

#[tokio::test]
async fn batch_writes_only_final_nodes_and_preserves_old_roots() {
    let backend = Arc::new(Counted::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let context = ctx();
    let updates: BTreeMap<_, _> = (0..64)
        .map(|n| (format!("{n:016x}"), Some(json!(n))))
        .collect();
    let root = store
        .index_update(
            None,
            updates.clone(),
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "delta-index",
            },
            &context,
        )
        .await
        .unwrap();
    let batch_puts = backend.puts.load(Ordering::SeqCst);
    // 64 leaves, four nibble branches and one common root, each written once.
    assert_eq!(batch_puts, 69);
    let mut sequential = None;
    for (key, value) in updates {
        sequential = Some(
            store
                .index_insert(
                    sequential,
                    &key,
                    value.unwrap(),
                    IndexWriteTarget {
                        incarnation: "inc",
                        epoch: 1,
                        category: "delta-index",
                    },
                    &context,
                )
                .await
                .unwrap(),
        );
    }
    let sequential_puts = backend.puts.load(Ordering::SeqCst) - batch_puts;
    assert!(batch_puts < sequential_puts);
    assert_eq!(
        store.index_list(root.clone(), None, 100).await.unwrap(),
        store.index_list(sequential, None, 100).await.unwrap()
    );
    let mut changes = BTreeMap::from([
        ("f".into(), Some(json!(999))),
        ("0000000000000001".into(), Some(json!(1001))),
    ]);
    for n in 16..32 {
        changes.insert(format!("{n:016x}"), None);
    }
    let next = store
        .index_update(
            root.clone(),
            changes,
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "delta-index",
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .index_get(root.clone(), "0000000000000001")
            .await
            .unwrap(),
        Some(json!(1))
    );
    assert_eq!(
        store
            .index_get(next.clone(), "0000000000000001")
            .await
            .unwrap(),
        Some(json!(1001))
    );
    for n in 16..32 {
        assert!(store
            .index_get(next.clone(), &format!("{n:016x}"))
            .await
            .unwrap()
            .is_none());
    }
    assert_eq!(
        store.index_get(next.clone(), "f").await.unwrap(),
        Some(json!(999))
    );
    let count = backend.puts.load(Ordering::SeqCst);
    // Splitting above a stored subtree reuses that subtree without reuploading it.
    store
        .index_update(
            root,
            BTreeMap::from([("e".into(), Some(json!(1)))]),
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "delta-index",
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(backend.puts.load(Ordering::SeqCst) - count, 2);
    let all = store
        .index_list(next.clone(), None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|(key, _)| (key, None))
        .collect();
    assert!(store
        .index_update(
            next,
            all,
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "delta-index"
            },
            &context
        )
        .await
        .unwrap()
        .is_none());
    eprintln!("64-page index: batch {batch_puts} PUT vs sequential {sequential_puts} PUT");
}

#[tokio::test]
async fn page_reads_reuse_index_and_data_across_snapshots() {
    let backend = Arc::new(Counted::default());
    let reader = Arc::new(ObjectReader::new(backend.clone()));
    let store = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
    let context = ctx();
    let pages = FrozenPages {
        page_size: 4096,
        final_file_size: 64 * 4096,
        pages: (0..64).map(|n| (n, vec![n as u8; 4096])).collect(),
    };
    let reference = store
        .apply_pages("inc", 1, manifest(), pages, &context)
        .await
        .unwrap();
    assert_eq!(backend.puts.load(Ordering::SeqCst), 71); // pack + 69 nodes + manifest
    let m: Manifest = store.get(&reference).await.unwrap();
    assert_eq!(backend.gets.load(Ordering::SeqCst), 0); // manifest admitted on PUT
    backend.gets.store(0, Ordering::SeqCst);
    let first = store.snapshot(m.clone(), context.clone());
    for n in 0..64 {
        assert_eq!(
            first.read_at(n * 4096, 4096).await.unwrap(),
            vec![n as u8; 4096]
        );
    }
    let cold = backend.gets.load(Ordering::SeqCst);
    assert_eq!(cold, 1); // confirmed index writes are cached; only the data pack needs GET
    let other = DatabaseStore::with_reader(reader, "notes").unwrap();
    let second = other.snapshot(m.clone(), ctx());
    for n in 0..64 {
        assert_eq!(
            second.read_at(n * 4096, 4096).await.unwrap(),
            vec![n as u8; 4096]
        );
    }
    assert_eq!(backend.gets.load(Ordering::SeqCst), cold);
    // Overwrite one page; both snapshots retain their own version.
    let next_ref = store
        .apply_pages(
            "inc",
            1,
            m,
            FrozenPages {
                page_size: 4096,
                final_file_size: 64 * 4096,
                pages: BTreeMap::from([(0, vec![99; 4096])]),
            },
            &context,
        )
        .await
        .unwrap();
    let next: Manifest = store.get(&next_ref).await.unwrap();
    assert_eq!(
        other.snapshot(next, ctx()).read_at(0, 4096).await.unwrap(),
        vec![99; 4096]
    );
    assert_eq!(second.read_at(0, 4096).await.unwrap(), vec![0; 4096]);
    eprintln!("64-page read: cold {cold} GET; repeat snapshot 0 additional GET");
}

#[tokio::test]
async fn object_reads_share_the_downloaded_allocation_across_handles() {
    for body in [vec![7; 256 * 1024], vec![]] {
        let backend = Arc::new(Counted::default());
        let reader = Arc::new(ObjectReader::new(backend.clone()));
        let store = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
        let other = DatabaseStore::with_reader(reader, "notes").unwrap();
        let reference = store
            .put_bytes("inc", 1, "segments", body.clone(), &ctx())
            .await
            .unwrap();

        let first = store.get_bytes(&reference).await.unwrap();
        assert_eq!(first.as_slice(), body);
        assert_eq!(
            first.as_ptr() as usize,
            backend.last_body_ptr.load(Ordering::SeqCst)
        );
        let hit = store.get_bytes(&reference).await.unwrap();
        let shared = other.get_bytes(&reference).await.unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        assert!(Arc::ptr_eq(&first, &shared));
        assert_eq!(backend.gets.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn oversized_objects_transfer_the_download_without_entering_the_cache() {
    let backend = Arc::new(Counted {
        max_single_put_bytes: Some(32 * 1024 * 1024),
        ..Default::default()
    });
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let size = 16 * 1024 * 1024 + 1;
    let reference = store
        .put_bytes("inc", 1, "segments", vec![7; size], &ctx())
        .await
        .unwrap();
    let first = store.get_bytes(&reference).await.unwrap();
    assert_eq!(first.len(), size);
    assert!(first.iter().all(|&byte| byte == 7));
    assert_eq!(
        first.as_ptr() as usize,
        backend.last_body_ptr.load(Ordering::SeqCst)
    );
    let second = store.get_bytes(&reference).await.unwrap();
    assert_eq!(
        second.as_ptr() as usize,
        backend.last_body_ptr.load(Ordering::SeqCst)
    );
    assert_eq!(first, second);
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(backend.gets.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_misses_share_download_and_cancelled_owner_can_be_replaced() {
    for cancel_owner in [false, true] {
        let backend = Arc::new(Counted::default());
        let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
        let r = store
            .put_bytes("inc", 1, "segments", vec![7; 4096], &ctx())
            .await
            .unwrap();
        backend.block.store(true, Ordering::SeqCst);
        let owner = tokio::spawn({
            let s = store.clone();
            let r = r.clone();
            async move { s.get_bytes(&r).await }
        });
        backend.started.acquire().await.unwrap().forget();
        let waiter = tokio::spawn({
            let s = store.clone();
            let r = r.clone();
            async move { s.get_bytes(&r).await }
        });
        let owner_bytes = if cancel_owner {
            owner.abort();
            assert!(owner.await.unwrap_err().is_cancelled());
            None
        } else {
            backend.release.add_permits(1);
            Some(owner.await.unwrap().unwrap())
        };
        backend.release.add_permits(1);
        let waiter_bytes = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(waiter_bytes.as_slice(), vec![7; 4096]);
        if let Some(owner_bytes) = owner_bytes {
            assert!(Arc::ptr_eq(&owner_bytes, &waiter_bytes));
        }
        let hit = store.get_bytes(&r).await.unwrap();
        assert!(Arc::ptr_eq(&waiter_bytes, &hit));
        assert_eq!(
            backend.gets.load(Ordering::SeqCst),
            if cancel_owner { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn cache_isolates_databases_backends_and_reference_checksums_and_retries_failures() {
    let backend = Arc::new(Counted::default());
    let reader = Arc::new(ObjectReader::new(backend.clone()));
    let a = DatabaseStore::with_reader(reader.clone(), "a").unwrap();
    let b = DatabaseStore::with_reader(reader, "b").unwrap();
    let r = a
        .put_bytes("inc", 1, "segments", vec![1; 4096], &ctx())
        .await
        .unwrap();
    assert_eq!(a.get_bytes(&r).await.unwrap().as_slice(), vec![1; 4096]);
    assert!(b.get_bytes(&r).await.is_err());
    // The same relative key can have different contents in a different database.
    backend
        .inner
        .create_if_absent(&b.physical(&r.relative_key().unwrap()), vec![2; 4096])
        .await;
    let mut other = r.clone();
    other.sha256 = digest(&vec![2; 4096]);
    assert_eq!(b.get_bytes(&other).await.unwrap().as_slice(), vec![2; 4096]);
    assert!(a.get_bytes(&other).await.is_err());
    let isolated = DatabaseStore::new(Arc::new(Counted::default()), "a").unwrap();
    assert!(isolated.get_bytes(&r).await.is_err());
    let mut missing = r.clone();
    missing.key = "incarnations/inc/segments/missing".into();
    assert!(a.get_bytes(&missing).await.is_err());
    backend
        .inner
        .create_if_absent(&a.physical(&missing.relative_key().unwrap()), vec![1; 4096])
        .await;
    assert_eq!(
        a.get_bytes(&missing).await.unwrap().as_slice(),
        vec![1; 4096]
    );
    // Invalid input is rejected before object-store access, even in a mixed batch.
    let before = backend.gets.load(Ordering::SeqCst);
    assert!(a
        .index_update(
            None,
            BTreeMap::from([("a".into(), Some(Value::Null)), ("bad!".into(), None)]),
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "delta-index"
            },
            &ctx()
        )
        .await
        .is_err());
    assert_eq!(backend.gets.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn cache_isolates_incarnations_and_rejects_paths_before_io() {
    let backend = Arc::new(Counted::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let old = store
        .put_bytes("old", 1, "segments", vec![1; 4096], &ctx())
        .await
        .unwrap();
    assert_eq!(
        store.get_bytes(&old).await.unwrap().as_slice(),
        vec![1; 4096]
    );
    let mut new = old.clone();
    new.key = old.key.replace("incarnations/old/", "incarnations/new/");
    new.sha256 = digest(&vec![2; 4096]);
    backend
        .inner
        .create_if_absent(&store.physical(&new.relative_key().unwrap()), vec![2; 4096])
        .await;
    assert_eq!(
        store.get_bytes(&new).await.unwrap().as_slice(),
        vec![2; 4096]
    );
    let before = backend.gets.load(Ordering::SeqCst);
    assert_eq!(
        store.get_bytes(&old).await.unwrap().as_slice(),
        vec![1; 4096]
    );
    assert_eq!(
        store.get_bytes(&new).await.unwrap().as_slice(),
        vec![2; 4096]
    );
    for key in [
        "../notes_old/object",
        "/databases/notes/head.json",
        "a//b",
        "%2e%2e/x",
    ] {
        let invalid = ObjectRef {
            key: key.into(),
            ..old.clone()
        };
        assert!(matches!(
            store.get_bytes(&invalid).await,
            Err(versioned_store::Error::Corrupt)
        ));
    }
    for (incarnation, epoch, category) in [
        ("../bad", 1, "index"),
        ("inc", 0, "index"),
        ("inc", 1, "../bad"),
    ] {
        assert!(store
            .index_update(
                Some(old.clone()),
                BTreeMap::from([("a".into(), Some(Value::Null))]),
                IndexWriteTarget {
                    incarnation,
                    epoch,
                    category
                },
                &ctx()
            )
            .await
            .is_err());
    }
    assert_eq!(backend.gets.load(Ordering::SeqCst), before);
    assert_eq!(backend.puts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mixed_prefix_batches_match_ordered_map_and_empty_batch_is_free() {
    let backend = Arc::new(Counted::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let context = ctx();
    let mut root = None;
    let mut expected = BTreeMap::new();
    let keys = [
        "a", "ab", "abc", "abcd", "abd", "b", "b0", "b00", "0", "00", "001", "f", "ff",
    ];
    for round in 0..20 {
        let previous = root.clone();
        let old = expected.clone();
        let mut updates = BTreeMap::new();
        for (n, key) in keys.iter().enumerate() {
            let value = if (round + n) % 4 == 0 {
                None
            } else {
                Some(json!(round * 100 + n))
            };
            if let Some(value) = &value {
                expected.insert(key.to_string(), value.clone());
            } else {
                expected.remove(*key);
            }
            updates.insert(key.to_string(), value);
        }
        root = store
            .index_update(
                root,
                updates,
                IndexWriteTarget {
                    incarnation: "inc",
                    epoch: 1,
                    category: "delta-index",
                },
                &context,
            )
            .await
            .unwrap();
        assert_eq!(
            store.index_list(root.clone(), None, 100).await.unwrap(),
            expected.clone().into_iter().collect::<Vec<_>>()
        );
        assert_eq!(
            store.index_list(previous, None, 100).await.unwrap(),
            old.into_iter().collect::<Vec<_>>()
        );
        let before = (
            backend.gets.load(Ordering::SeqCst),
            backend.puts.load(Ordering::SeqCst),
        );
        assert_eq!(
            store
                .index_update(
                    root.clone(),
                    BTreeMap::new(),
                    IndexWriteTarget {
                        incarnation: "inc",
                        epoch: 1,
                        category: "delta-index"
                    },
                    &context
                )
                .await
                .unwrap(),
            root
        );
        assert_eq!(
            before,
            (
                backend.gets.load(Ordering::SeqCst),
                backend.puts.load(Ordering::SeqCst)
            )
        );
    }
}

#[tokio::test]
async fn head_reads_bypass_cache_and_keep_returning_current_revision() {
    let backend = Arc::new(Counted::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let context = ctx();
    let m = store
        .put("inc", 1, "manifests", &manifest(), &context)
        .await
        .unwrap();
    let head = Head {
        protocol_version: 1,
        storage_layout_version: LAYOUT_VERSION,
        incarnation_id: "inc".into(),
        revision: 1,
        data_version: 1,
        metadata_version: 1,
        epoch: 1,
        head_nonce: "seed".into(),
        name: "notes".into(),
        description: String::new(),
        metadata: json!({}),
        status: "active".into(),
        manifest_ref: m,
        snapshot_index_root: None,
        snapshot_retention_days: 7,
        snapshot_count: 1,
        safety_policy: SafetyPolicy {
            max_read_ms: 10000,
            drain_ms: 11000,
        },
        gc: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    store.publish_head(None, head, &context).await.unwrap();
    let base = store.read_head().await.unwrap();
    let mut next = base.head.clone();
    next.data_version += 1;
    store
        .publish_head(Some(&base), next, &context)
        .await
        .unwrap();
    assert_eq!(store.read_head().await.unwrap().head.data_version, 2);
    assert_eq!(store.read_head().await.unwrap().head.data_version, 2);
    assert_eq!(backend.gets.load(Ordering::SeqCst), 3);
}
