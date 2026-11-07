use super::*;
use object_store_test_support::MemoryStore;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Semaphore;

struct Controlled {
    inner: MemoryStore,
    reads: AtomicUsize,
    put_mode: AtomicUsize,
    cancel_after_put: Mutex<Option<RequestContext>>,
    block: AtomicBool,
    fail_next: AtomicBool,
    started: Semaphore,
    release: Semaphore,
}
impl Default for Controlled {
    fn default() -> Self {
        Self {
            inner: MemoryStore::default(),
            reads: AtomicUsize::new(0),
            put_mode: AtomicUsize::new(0),
            cancel_after_put: Mutex::new(None),
            block: AtomicBool::new(false),
            fail_next: AtomicBool::new(false),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}
impl BlobStore for Controlled {
    fn cache_namespace(&self) -> Option<String> {
        Some("test-controlled".into())
    }
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }
    async fn read_current(&self, key: &str) -> std::result::Result<VersionedObject, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        if self.block.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(StoreError::Transport("injected read failure".into()));
        }
        self.inner.read_current(key).await
    }
    async fn read_range(
        &self,
        key: &str,
        range: std::ops::Range<u64>,
    ) -> std::result::Result<Vec<u8>, StoreError> {
        self.inner.read_range(key, range).await
    }
    async fn create_if_absent(&self, key: &str, body: Vec<u8>) -> CasOutcome {
        match self.put_mode.swap(0, Ordering::SeqCst) {
            1 => self.inner.lose_next_response.store(true, Ordering::SeqCst),
            2 => return CasOutcome::Unknown,
            3 => {
                return CasOutcome::NotApplied {
                    reason: StoreError::Transport("rejected".into()),
                    retryable: true,
                }
            }
            4 => return CasOutcome::PreconditionFailed,
            5 => {
                self.inner
                    .create_if_absent(key, b"different".to_vec())
                    .await;
                return CasOutcome::Unknown;
            }
            _ => {}
        }
        let result = self.inner.create_if_absent(key, body).await;
        if let Some(ctx) = self.cancel_after_put.lock().unwrap().take() {
            ctx.cancel();
        }
        result
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> CasOutcome {
        self.inner.compare_and_swap(key, expected, body).await
    }
    async fn list(&self, request: ListRequest) -> std::result::Result<ObjectPage, StoreError> {
        self.inner.list(request).await
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.inner.delete(key).await
    }
}

fn key(n: usize) -> Key {
    (format!("object/{n}"), 0, String::new())
}
fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(10))
}
fn leaf(prefix: &str, value: Value) -> IndexNode {
    IndexNode {
        version: FORMAT_VERSION,
        prefix: prefix.into(),
        value: Some(value),
        children: BTreeMap::new(),
    }
}
fn fixture() -> (
    Arc<Controlled>,
    Arc<ObjectReader<Controlled>>,
    DatabaseStore<Controlled>,
) {
    let backend = Arc::new(Controlled::default());
    let reader = Arc::new(ObjectReader::new(backend.clone()));
    let db = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
    (backend, reader, db)
}

#[test]
fn cache_evicts_lru_at_byte_and_entry_limits() {
    let mut cache = Cache::new(MAX_OBJECT_BYTES, MAX_OBJECT_ENTRIES);
    for n in 0..16 {
        cache.insert(key(n), Arc::new(vec![0; 1024 * 1024]), 1024 * 1024);
    }
    assert!(cache.get(&key(0)).is_some());
    cache.insert(key(16), Arc::new(vec![0; 1024 * 1024]), 1024 * 1024);
    assert!(cache.get(&key(1)).is_none());
    assert!(cache.get(&key(0)).is_some());
    assert_eq!(cache.weight, MAX_OBJECT_BYTES);
    cache.insert(
        key(17),
        Arc::new(vec![0; MAX_OBJECT_BYTES + 1]),
        MAX_OBJECT_BYTES + 1,
    );
    assert!(cache.get(&key(17)).is_none());
    let mut cache = Cache::new(MAX_OBJECT_BYTES, MAX_OBJECT_ENTRIES);
    for n in 0..8193 {
        cache.insert(key(n), Arc::new(Vec::<u8>::new()), 0);
    }
    assert!(cache.get(&key(0)).is_none());
    assert_eq!(cache.entries.len(), MAX_OBJECT_ENTRIES);
    assert_eq!(cache.order.len(), MAX_OBJECT_ENTRIES);
}

#[test]
fn evicted_bytes_live_until_the_last_caller_releases_them() {
    let mut cache = Cache::new(MAX_OBJECT_BYTES, MAX_OBJECT_ENTRIES);
    let bytes = Arc::new(vec![7; MAX_OBJECT_BYTES]);
    let weak = Arc::downgrade(&bytes);
    cache.insert(key(0), Arc::clone(&bytes), bytes.len());
    let hit = cache.get(&key(0)).unwrap();
    assert!(Arc::ptr_eq(&bytes, &hit));
    cache.insert(key(1), Arc::new(vec![1]), 1);
    assert!(cache.get(&key(0)).is_none());
    assert_eq!(cache.weight, 1);
    assert!(bytes.iter().all(|&byte| byte == 7));
    drop(bytes);
    assert!(weak.upgrade().is_some());
    drop(hit);
    assert!(weak.upgrade().is_none());
}

#[test]
fn node_cache_bounds_replacements_and_evicted_lifetimes() {
    let node = Arc::new(leaf("a", json!(1)));
    let weight = node_weight(&node);
    let mut cache = Cache::new(weight * 2, MAX_NODE_ENTRIES);
    cache.insert(key(0), node.clone(), weight);
    cache.insert(key(1), node.clone(), weight);
    let held = cache.get(&key(0)).unwrap();
    let weak = Arc::downgrade(&held);
    cache.insert(key(2), Arc::new(leaf("b", json!(2))), weight);
    assert!(cache.get(&key(1)).is_none());
    assert_eq!(cache.weight, weight * 2);

    // Replacement must remove both the old weight and the old LRU record.
    let larger = Arc::new(leaf("a", json!("x".repeat(weight / 2))));
    let larger_weight = node_weight(&larger);
    assert!(larger_weight > weight && larger_weight <= weight * 2);
    cache.insert(key(0), larger, larger_weight);
    assert!(cache.get(&key(2)).is_none());
    assert_eq!(cache.weight, larger_weight);
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.order.len(), 1);
    cache.insert(key(0), node.clone(), usize::MAX);
    assert_eq!(cache.weight, larger_weight);
    drop(node);
    assert_eq!(held.value, Some(json!(1)));
    drop(held);
    assert!(weak.upgrade().is_none());

    let node = Arc::new(leaf("a", Value::Null));
    let weight = node_weight(&node);
    let mut cache = Cache::new(MAX_NODE_WEIGHT, MAX_NODE_ENTRIES);
    for n in 0..=MAX_NODE_ENTRIES {
        cache.insert(key(n), node.clone(), weight);
    }
    assert!(cache.get(&key(0)).is_none());
    assert_eq!(cache.entries.len(), MAX_NODE_ENTRIES);
    assert_eq!(cache.order.len(), MAX_NODE_ENTRIES);
    assert_eq!(cache.weight, weight * MAX_NODE_ENTRIES);
}

#[tokio::test]
async fn index_operations_share_parses_without_mutating_historical_nodes() {
    let (backend, reader, db) = fixture();
    let root = db
        .index_update(
            None,
            BTreeMap::from([
                ("a".into(), Some(json!(1))),
                ("ab".into(), Some(json!(2))),
                ("b".into(), Some(json!(3))),
            ]),
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "index",
            },
            &ctx(),
        )
        .await
        .unwrap();
    let expected = vec![
        ("a".into(), json!(1)),
        ("ab".into(), json!(2)),
        ("b".into(), json!(3)),
    ];
    assert_eq!(
        db.index_list(root.clone(), None, 100).await.unwrap(),
        expected
    );
    let parses = reader.parses.load(Ordering::SeqCst);
    let reads = backend.reads.load(Ordering::SeqCst);
    // Parsed nodes remain useful even after the independent body cache is cleared.
    *reader.cache.lock().unwrap() = Cache::new(0, 0);
    let other = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
    for (key, value) in &expected {
        assert_eq!(
            other.index_get(root.clone(), key).await.unwrap().as_ref(),
            Some(value)
        );
    }
    assert_eq!(
        other.index_list(root.clone(), None, 100).await.unwrap(),
        expected
    );
    let reference = root.as_ref().unwrap();
    let original = db.get_index_node(reference).await.unwrap();
    let hit = other.get_index_node(reference).await.unwrap();
    assert!(Arc::ptr_eq(&original, &hit));
    let original_json = serde_json::to_value(original.as_ref()).unwrap();
    let next = db
        .index_update(
            root.clone(),
            BTreeMap::from([("a".into(), Some(json!(9)))]),
            IndexWriteTarget {
                incarnation: "inc",
                epoch: 1,
                category: "index",
            },
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(reader.parses.load(Ordering::SeqCst), parses);
    assert_eq!(backend.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        serde_json::to_value(original.as_ref()).unwrap(),
        original_json
    );
    assert_eq!(db.index_get(next, "a").await.unwrap(), Some(json!(9)));
    assert_eq!(db.index_list(root, None, 100).await.unwrap(), expected);
}

#[test]
fn node_weight_accounts_for_nested_allocations_and_spare_capacity() {
    let mut text = String::with_capacity(4096);
    text.push_str("small");
    let mut array = Vec::with_capacity(1024);
    array.push(Value::String(text));
    let mut object = serde_json::Map::new();
    object.insert("nested".into(), Value::Array(array));
    let node = leaf("a", Value::Object(object));
    let array_and_string = 1024 * size_of::<Value>() + 4096;
    assert!(node_weight(&node) >= size_of::<IndexNode>() + array_and_string);
    assert!(serde_json::to_vec(&node).unwrap().len() < 128);

    let mut reference_key = String::with_capacity(2048);
    reference_key.push_str("object");
    let reference = ObjectRef {
        key: reference_key,
        size: 0,
        sha256: digest(&[]),
    };
    let mut with_child = node.clone();
    with_child.children.insert('b', reference);
    let child_weight = node_weight(&with_child);
    // Cloning the JSON can shrink spare capacity, so compare its owned reference
    // against an otherwise identical clone rather than against the original.
    with_child.children.clear();
    assert!(child_weight >= node_weight(&with_child) + 2048 + 64);
}

#[tokio::test]
async fn evicted_nodes_reparse_verified_bytes_and_preserve_held_values() {
    let (backend, reader, db) = fixture();
    *reader.nodes.lock().unwrap() = Cache::new(MAX_NODE_WEIGHT, 1);
    let first_ref = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    let second_ref = db
        .put("inc", 1, "index", &leaf("b", json!(2)), &ctx())
        .await
        .unwrap();
    let held = db.get_index_node(&first_ref).await.unwrap();
    let weak = Arc::downgrade(&held);
    db.get_index_node(&second_ref).await.unwrap();
    let reloaded = db.get_index_node(&first_ref).await.unwrap();
    assert!(!Arc::ptr_eq(&held, &reloaded));
    assert_eq!(held.value, Some(json!(1)));
    assert_eq!(backend.reads.load(Ordering::SeqCst), 2);
    assert_eq!(reader.parses.load(Ordering::SeqCst), 3);
    drop(held);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn invalid_nodes_are_not_cached_and_transport_and_missing_errors_can_retry() {
    let (backend, reader, db) = fixture();
    for body in [
        b"{invalid".to_vec(),
        br#"{"version":1,"prefix":"a","value":1,"children":{},"unknown":0}"#.to_vec(),
        serde_json::to_vec(&IndexNode {
            version: 999,
            ..leaf("a", json!(1))
        })
        .unwrap(),
        serde_json::to_vec(&leaf("!", json!(1))).unwrap(),
        serde_json::to_vec(&IndexNode {
            children: BTreeMap::from([(
                'a',
                ObjectRef {
                    key: "../bad".into(),
                    size: 0,
                    sha256: digest(&[]),
                },
            )]),
            ..leaf("a", json!(1))
        })
        .unwrap(),
    ] {
        let reference = db.put_bytes("inc", 1, "index", body, &ctx()).await.unwrap();
        let parses = reader.parses.load(Ordering::SeqCst);
        let reads = backend.reads.load(Ordering::SeqCst);
        for _ in 0..2 {
            assert!(matches!(
                db.get_index_node(&reference).await,
                Err(Error::Corrupt)
            ));
        }
        assert_eq!(reader.parses.load(Ordering::SeqCst), parses + 2);
        assert_eq!(backend.reads.load(Ordering::SeqCst), reads + 1);
        assert!(reader.nodes.lock().unwrap().entries.is_empty());
    }
    let reference = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    let parses = reader.parses.load(Ordering::SeqCst);
    assert!(matches!(
        db.get_index_node(&reference).await,
        Err(Error::Store(StoreError::Transport(_)))
    ));
    assert_eq!(reader.parses.load(Ordering::SeqCst), parses);
    assert_eq!(
        db.get_index_node(&reference).await.unwrap().value,
        Some(json!(1))
    );

    let body = serde_json::to_vec(&leaf("b", json!(2))).unwrap();
    let missing = ObjectRef {
        key: "incarnations/inc/generations/0000000000000001/index/missing".into(),
        size: body.len() as u64,
        sha256: digest(&body),
    };
    assert!(
        matches!(db.get_index_node(&missing).await, Err(Error::MissingObject(name)) if name == "notes")
    );
    backend
        .inner
        .create_if_absent(&db.physical(&missing.relative_key().unwrap()), body)
        .await;
    assert_eq!(
        db.get_index_node(&missing).await.unwrap().value,
        Some(json!(2))
    );
}

#[tokio::test]
async fn node_identity_includes_database_backend_incarnation_size_and_checksum() {
    let (backend, reader, db) = fixture();
    let reference = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    let old = db.get_index_node(&reference).await.unwrap();
    let other = DatabaseStore::with_reader(reader.clone(), "other").unwrap();
    assert!(matches!(
        other.get_index_node(&reference).await,
        Err(Error::MissingObject(_))
    ));
    let isolated = DatabaseStore::new(Arc::new(Controlled::default()), "notes").unwrap();
    assert!(matches!(
        isolated.get_index_node(&reference).await,
        Err(Error::MissingObject(_))
    ));
    let recreated = db
        .put("new-inc", 1, "index", &leaf("a", json!(2)), &ctx())
        .await
        .unwrap();
    assert_eq!(
        db.get_index_node(&recreated).await.unwrap().value,
        Some(json!(2))
    );
    let wrong_size = ObjectRef {
        size: reference.size + 1,
        ..reference.clone()
    };
    assert!(matches!(
        db.get_index_node(&wrong_size).await,
        Err(Error::Corrupt)
    ));
    let changed = serde_json::to_vec(&leaf("a", json!(3))).unwrap();
    let physical = db.physical(&reference.relative_key().unwrap());
    let revision = backend
        .inner
        .read_current(&physical)
        .await
        .unwrap()
        .revision;
    assert!(matches!(
        backend
            .inner
            .compare_and_swap(&physical, &revision, changed.clone())
            .await,
        CasOutcome::Applied { .. }
    ));
    let changed_ref = ObjectRef {
        size: changed.len() as u64,
        sha256: digest(&changed),
        ..reference.clone()
    };
    assert_eq!(
        db.get_index_node(&changed_ref).await.unwrap().value,
        Some(json!(3))
    );
    assert!(Arc::ptr_eq(
        &old,
        &db.get_index_node(&reference).await.unwrap()
    ));

    let reads = backend.reads.load(Ordering::SeqCst);
    for invalid in [
        ObjectRef {
            key: "../bad".into(),
            ..reference.clone()
        },
        ObjectRef {
            sha256: "bad".into(),
            ..reference.clone()
        },
        ObjectRef {
            size: MAX_METADATA_BYTES as u64 + 1,
            ..reference.clone()
        },
    ] {
        assert!(matches!(
            db.get_index_node(&invalid).await,
            Err(Error::Corrupt)
        ));
    }
    assert_eq!(backend.reads.load(Ordering::SeqCst), reads);
}

#[tokio::test]
async fn cached_nodes_still_require_valid_parent_prefixes() {
    let (_, reader, db) = fixture();
    let child = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    db.get_index_node(&child).await.unwrap();
    let parent = db
        .put(
            "inc",
            1,
            "index",
            &IndexNode {
                prefix: "a".into(),
                value: None,
                children: BTreeMap::from([('b', child)]),
                version: FORMAT_VERSION,
            },
            &ctx(),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            db.index_get(Some(parent.clone()), "ab").await,
            Err(Error::Corrupt)
        ));
        assert!(matches!(
            db.index_list(Some(parent.clone()), None, 100).await,
            Err(Error::Corrupt)
        ));
    }
    assert_eq!(reader.parses.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn expanded_json_values_bypass_the_node_budget_but_reuse_verified_bytes() {
    let (backend, reader, db) = fixture();
    // Small JSON numbers expand into many inline Value slots after parsing.
    let reference = db
        .put(
            "inc",
            1,
            "index",
            &leaf("a", json!(vec![0; 300_000])),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(reference.size < MAX_METADATA_BYTES as u64);
    let first = db.get_index_node(&reference).await.unwrap();
    assert!(node_weight(&first) > MAX_NODE_WEIGHT);
    let second = db.get_index_node(&reference).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(reader.parses.load(Ordering::SeqCst), 2);
    assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    assert!(reader.nodes.lock().unwrap().entries.is_empty());
}

#[tokio::test]
async fn concurrent_node_misses_share_parsing_and_cancelled_owners_can_be_replaced() {
    for cancel in [false, true] {
        let (backend, reader, db) = fixture();
        let reference = db
            .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
            .await
            .unwrap();
        backend.block.store(true, Ordering::SeqCst);
        let owner = tokio::spawn({
            let db = db.clone();
            let reference = reference.clone();
            async move { db.get_index_node(&reference).await }
        });
        tokio::time::timeout(Duration::from_secs(2), backend.started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let waiter = tokio::spawn({
            let db = db.clone();
            let reference = reference.clone();
            async move { db.get_index_node(&reference).await }
        });
        let first = if cancel {
            owner.abort();
            assert!(owner.await.unwrap_err().is_cancelled());
            None
        } else {
            backend.release.add_permits(1);
            Some(
                tokio::time::timeout(Duration::from_secs(2), owner)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            )
        };
        backend.release.add_permits(1);
        let second = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(first) = first {
            assert!(Arc::ptr_eq(&first, &second));
        }
        assert!(Arc::ptr_eq(
            &second,
            &db.get_index_node(&reference).await.unwrap()
        ));
        assert_eq!(reader.parses.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend.reads.load(Ordering::SeqCst),
            if cancel { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn node_cache_hits_and_stalled_fills_obey_read_leases() {
    let (backend, reader, db) = fixture();
    let reference = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    db.get_index_node(&reference).await.unwrap();
    for expired in [false, true] {
        let leased = DatabaseStore::with_reader(reader.clone(), "notes")
            .unwrap()
            .with_read_lease();
        if expired {
            *leased.read_lease.as_ref().unwrap().lock().unwrap() =
                RequestContext::new(Duration::ZERO);
        } else {
            leased.read_lease.as_ref().unwrap().lock().unwrap().cancel();
        }
        assert!(matches!(
            leased.get_index_node(&reference).await,
            Err(Error::Stopped(_))
        ));
    }
    assert_eq!(reader.parses.load(Ordering::SeqCst), 1);
    assert_eq!(backend.reads.load(Ordering::SeqCst), 1);

    let reference = db
        .put("inc", 1, "index", &leaf("b", json!(2)), &ctx())
        .await
        .unwrap();
    let leased = DatabaseStore::with_reader(reader.clone(), "notes")
        .unwrap()
        .with_read_lease();
    let lease = leased.read_lease.as_ref().unwrap().lock().unwrap().clone();
    // Drain the notification from the previous successful download.
    backend.started.acquire().await.unwrap().forget();
    backend.block.store(true, Ordering::SeqCst);
    let pending = tokio::spawn({
        let reference = reference.clone();
        async move { leased.get_index_node(&reference).await }
    });
    tokio::time::timeout(Duration::from_secs(2), backend.started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    lease.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Stopped(_))
    ));
    backend.block.store(false, Ordering::SeqCst);
    assert_eq!(
        db.get_index_node(&reference).await.unwrap().value,
        Some(json!(2))
    );
}

#[tokio::test]
async fn disk_hit_promotes_bodies_and_parsed_nodes_without_backend_reads() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Controlled::default());
    let db = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let reference = db
        .put("inc", 1, "index", &leaf("a", json!(1)), &ctx())
        .await
        .unwrap();
    let options = crate::DiskCacheOptions {
        directory: dir.path().into(),
        max_bytes: 1024 * 1024,
    };
    // Seed through the disk API with a generous test budget; production uses 50 ms.
    let disk =
        crate::disk_cache::DiskCache::new(options.clone(), backend.cache_namespace()).unwrap();
    let k = cache_key(&db.paths, &reference).unwrap();
    let bytes = db.get_bytes(&reference).await.unwrap();
    disk.write(
        &k,
        bytes,
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await;
    disk.wait_idle().await;
    // Construct an independent reader after optional maintenance.
    let reader = Arc::new(ObjectReader::with_disk_cache(
        backend.clone(),
        Some(options),
    ));
    let fresh = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
    let before = backend.reads.load(Ordering::SeqCst);
    let first = fresh.get_index_node(&reference).await.unwrap();
    let second = fresh.get_index_node(&reference).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(backend.reads.load(Ordering::SeqCst), before);
    assert_eq!(reader.parses.load(Ordering::SeqCst), 1);
    assert!(reader.cache.lock().unwrap().get(&k).is_some());
}

#[tokio::test]
async fn confirmed_metadata_writes_fill_bodies_without_extra_gets() {
    for mode in [0, 1] {
        for category in ["manifests", "segment-index", "delta-index", "snapshots"] {
            let (backend, reader, db) = fixture();
            backend.put_mode.store(mode, Ordering::SeqCst);
            let reference = db
                .put("inc", 1, category, &leaf("a", json!(1)), &ctx())
                .await
                .unwrap();
            assert_eq!(backend.reads.load(Ordering::SeqCst), mode);
            let bytes = db.get_bytes(&reference).await.unwrap();
            assert!(reference.matches(&bytes));
            assert_eq!(
                db.get_index_node(&reference).await.unwrap().value,
                Some(json!(1))
            );
            assert_eq!(backend.reads.load(Ordering::SeqCst), mode);
            assert_eq!(reader.parses.load(Ordering::SeqCst), 1);
        }
    }
}
#[tokio::test]
async fn unsuccessful_writes_never_populate_either_cache() {
    for mode in [2, 3, 4, 5] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        let backend = Arc::new(Controlled::default());
        let reader = Arc::new(ObjectReader::with_disk_cache(
            backend.clone(),
            Some(crate::DiskCacheOptions {
                directory: path.clone(),
                max_bytes: 1024,
            }),
        ));
        let db = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
        backend.put_mode.store(mode, Ordering::SeqCst);
        assert!(db
            .put_bytes("inc", 1, "manifests", b"expected".to_vec(), &ctx())
            .await
            .is_err());
        assert!(reader.cache.lock().unwrap().entries.is_empty());
        assert!(reader.nodes.lock().unwrap().entries.is_empty());
        assert!(!path.exists());
    }
}
#[tokio::test]
async fn bulk_writes_preserve_hot_bodies_and_data_is_demand_loaded() {
    let (backend, reader, db) = fixture();
    *reader.cache.lock().unwrap() = Cache::new(64, 1);
    let hot = db
        .put_bytes("inc", 1, "segments", vec![1; 64], &ctx())
        .await
        .unwrap();
    assert!(reader.cache.lock().unwrap().entries.is_empty());
    let held = db.get_bytes(&hot).await.unwrap();
    for n in 0..100 {
        db.put("inc", 1, "manifests", &json!(n), &ctx())
            .await
            .unwrap();
    }
    assert!(Arc::ptr_eq(&held, &db.get_bytes(&hot).await.unwrap()));
    assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    assert_eq!(reader.cache.lock().unwrap().entries.len(), 1);
}
#[tokio::test]
async fn written_metadata_is_reused_by_an_independent_disk_reader() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Controlled::default());
    let options = crate::DiskCacheOptions {
        directory: dir.path().into(),
        max_bytes: 4096,
    };
    let reader = Arc::new(ObjectReader::with_disk_cache(
        backend.clone(),
        Some(options.clone()),
    ));
    let db = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
    let reference = db
        .put("inc", 1, "manifests", &json!({"metadata":true}), &ctx())
        .await
        .unwrap();
    let disk = reader.disk.as_ref().unwrap();
    disk.wait_idle().await;
    let key = cache_key(&db.paths, &reference).unwrap();
    // Admission is best effort under the production 50 ms budget. Retry the same
    // confirmed bytes if the test machine was busy, without any remote GET.
    for _ in 0..20 {
        if disk
            .read(&key, tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .is_some()
        {
            break;
        }
        reader
            .admit_written(
                &db.paths,
                &reference,
                serde_json::to_vec(&json!({"metadata":true})).unwrap(),
                &ctx(),
            )
            .await;
        disk.wait_idle().await;
    }
    disk.wait_idle().await;
    let other = Arc::new(ObjectReader::with_disk_cache(
        backend.clone(),
        Some(options),
    ));
    // Exercise disk lookup with a generous budget, then verify production promotion.
    assert!(other
        .disk
        .as_ref()
        .unwrap()
        .read(&key, tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .is_some());
    other.disk.as_ref().unwrap().wait_idle().await;
    let fresh = DatabaseStore::with_reader(other, "notes").unwrap();
    assert!(reference.matches(&fresh.get_bytes(&reference).await.unwrap()));
    assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cache_failure_or_cancellation_preserves_confirmed_put_result() {
    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        if !cancel {
            std::fs::write(&path, b"not a directory").unwrap();
        }
        let backend = Arc::new(Controlled::default());
        let reader = Arc::new(ObjectReader::with_disk_cache(
            backend.clone(),
            Some(crate::DiskCacheOptions {
                directory: path.clone(),
                max_bytes: 1024,
            }),
        ));
        let db = DatabaseStore::with_reader(reader.clone(), "notes").unwrap();
        let ctx = ctx();
        if cancel {
            *backend.cancel_after_put.lock().unwrap() = Some(ctx.clone());
        }
        let reference = db
            .put_bytes("inc", 1, "manifests", b"confirmed".to_vec(), &ctx)
            .await
            .unwrap();
        assert!(reference.matches(
            &backend
                .inner
                .read_current(&db.physical(&reference.relative_key().unwrap()))
                .await
                .unwrap()
                .body
        ));
        if cancel {
            assert!(ctx.check().is_err());
            assert!(reader.cache.lock().unwrap().entries.is_empty());
            assert!(!path.exists());
        } else {
            assert!(reference.matches(&db.get_bytes(&reference).await.unwrap()));
            assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
        }
    }
}
