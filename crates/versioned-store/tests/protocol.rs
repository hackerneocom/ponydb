use object_store_api::*;
use object_store_test_support::MemoryStore;
use request_context::RequestContext;
use std::{sync::Arc, time::Duration};
use storage_format::*;
use versioned_store::*;
fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(10))
}

#[tokio::test]
async fn layout_one_head_and_reference_strings_are_readable() {
    let backend = Arc::new(MemoryStore::default());
    let body = br#"{
        "protocol_version":1,"storage_layout_version":1,"name":"notes",
        "incarnation_id":"inc1","revision":"7","data_version":"3",
        "metadata_version":"2","epoch":"4","head_nonce":"existing-head",
        "description":"existing","metadata":{},"status":"active",
        "manifest_ref":{"key":"incarnations/inc1/generations/0000000000000004/manifests/object1","size":0,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
        "snapshot_index_root":null,"snapshot_retention_days":7,"snapshot_count":0,
        "safety_policy":{"max_read_ms":10000,"drain_ms":11000},"gc":null,
        "created_at_ms":1,"updated_at_ms":2
    }"#.to_vec();
    backend
        .create_if_absent("databases/notes/head.json", body.clone())
        .await;
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let view = store.read_head().await.unwrap();
    assert_eq!(view.head.revision, 7);
    assert_eq!(view.head.incarnation_id, "inc1");
    assert_eq!(
        store.physical(&view.head.manifest_ref.relative_key().unwrap()),
        "databases/notes/incarnations/inc1/generations/0000000000000004/manifests/object1"
    );
    assert_eq!(
        backend
            .read_current("databases/notes/head.json")
            .await
            .unwrap()
            .body,
        body
    );
}
async fn initial(s: &DatabaseStore<MemoryStore>) -> Head {
    let context = ctx();
    let manifest = s
        .put(
            "inc1",
            1,
            "manifests",
            &Manifest {
                version: 1,
                page_size: 4096,
                logical_size: 0,
                segment_size: 262144,
                generation_id: "g1".into(),
                base_root: None,
                delta_root: None,
                overlay_bytes: 0,
                overlay_transactions: 0,
            },
            &context,
        )
        .await
        .unwrap();
    Head {
        protocol_version: 1,
        storage_layout_version: LAYOUT_VERSION,
        incarnation_id: "inc1".into(),
        revision: 1,
        data_version: 1,
        metadata_version: 1,
        epoch: 1,
        head_nonce: unique(),
        name: "notes".into(),
        description: String::new(),
        metadata: serde_json::json!({}),
        status: "active".into(),
        manifest_ref: manifest,
        snapshot_index_root: None,
        snapshot_retention_days: 7,
        snapshot_count: 1,
        safety_policy: SafetyPolicy {
            max_read_ms: 10000,
            drain_ms: 11000,
        },
        gc: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    }
}
#[tokio::test]
async fn missing_head_and_referenced_objects_have_distinct_errors() {
    let store = DatabaseStore::new(Arc::new(MemoryStore::default()), "notes").unwrap();
    assert!(matches!(
        store.read_head().await,
        Err(Error::DatabaseNotFound(database)) if database == "notes"
    ));
    let head = initial(&store).await;
    assert!(matches!(
        store
            .backend
            .delete(&store.physical(&head.manifest_ref.relative_key().unwrap()))
            .await,
        DeleteOutcome::Deleted
    ));
    // Use a cold reader: confirmed PUTs now retain valid immutable metadata.
    let store = DatabaseStore::new(store.backend.clone(), "notes").unwrap();
    assert!(matches!(
        store.manifest(&head).await,
        Err(Error::MissingObject(database)) if database == "notes"
    ));
    let root = store
        .index_insert(
            None,
            "a",
            serde_json::json!(1),
            IndexWriteTarget {
                incarnation: "inc1",
                epoch: 1,
                category: "index",
            },
            &ctx(),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .backend
            .delete(&store.physical(&root.relative_key().unwrap()))
            .await,
        DeleteOutcome::Deleted
    ));
    assert!(matches!(
        store.index_get(Some(root), "a").await,
        Err(Error::MissingObject(database)) if database == "notes"
    ));
}

#[tokio::test]
async fn cow_index_is_persistent_bounded_and_paginates() {
    let store = DatabaseStore::new(Arc::new(MemoryStore::default()), "notes").unwrap();
    let context = ctx();
    let mut root = None;
    for i in 0..100 {
        root = Some(
            store
                .index_insert(
                    root,
                    &format!("{i:016x}"),
                    serde_json::json!(i),
                    IndexWriteTarget {
                        incarnation: "inc1",
                        epoch: 1,
                        category: "segment-index",
                    },
                    &context,
                )
                .await
                .unwrap(),
        );
    }
    let old = root.clone();
    root = Some(
        store
            .index_insert(
                root,
                "0000000000000001",
                serde_json::json!(999),
                IndexWriteTarget {
                    incarnation: "inc1",
                    epoch: 1,
                    category: "segment-index",
                },
                &context,
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        store.index_get(old, "0000000000000001").await.unwrap(),
        Some(serde_json::json!(1))
    );
    assert_eq!(
        store
            .index_get(root.clone(), "0000000000000001")
            .await
            .unwrap(),
        Some(serde_json::json!(999))
    );
    let mut after = None;
    let mut count = 0;
    loop {
        let page = store
            .index_list(root.clone(), after.as_deref(), 7)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        count += page.len();
        after = Some(page.last().unwrap().0.clone());
    }
    assert_eq!(count, 100);
}
#[tokio::test]
async fn same_head_cas_has_one_winner_without_request_objects() {
    let backend = Arc::new(MemoryStore::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    store
        .publish_head(None, initial(&store).await, &ctx())
        .await
        .unwrap();
    let base = store.read_head().await.unwrap();
    let mut next = base.head.clone();
    next.data_version += 1;
    let context = ctx();
    let (a, b) = tokio::join!(
        store.publish_head(Some(&base), next.clone(), &context),
        store.publish_head(Some(&base), next, &context)
    );
    assert_eq!([a.is_ok(), b.is_ok()].into_iter().filter(|x| *x).count(), 1);
    assert_eq!(store.read_head().await.unwrap().head.data_version, 2);
    let objects = backend
        .list(ListRequest {
            prefix: "databases/notes/".into(),
            delimiter: None,
            cursor: None,
            limit: 1000,
        })
        .await
        .unwrap();
    assert!(!objects.objects.iter().any(|key| key.contains("/requests/")));
}
#[tokio::test]
async fn dependency_corruption_is_rejected_before_sql() {
    let backend = Arc::new(MemoryStore::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let reference = store
        .put_bytes("inc1", 1, "segments", vec![1; 4096], &ctx())
        .await
        .unwrap();
    let key = store.physical(&reference.relative_key().unwrap());
    let old = backend.read_current(&key).await.unwrap();
    backend
        .compare_and_swap(&key, &old.revision, vec![2; 4096])
        .await;
    assert!(matches!(
        store.get_bytes(&reference).await,
        Err(Error::Corrupt)
    ));
}

#[tokio::test]
async fn truncated_pages_stay_zero_after_regrowth_and_cold_reload() {
    struct Image(Vec<u8>);
    impl remote_pager::Snapshot for Image {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> remote_pager::Result<()> {
            out.copy_from_slice(&self.0[offset as usize..offset as usize + out.len()]);
            Ok(())
        }
    }
    let backend = Arc::new(MemoryStore::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let context = ctx();
    let segment = store
        .put_bytes("inc1", 1, "segments", vec![7; 2048], &context)
        .await
        .unwrap();
    let root = store
        .index_insert(
            None,
            "0000000000000000",
            serde_json::to_value(segment).unwrap(),
            IndexWriteTarget {
                incarnation: "inc1",
                epoch: 1,
                category: "segment-index",
            },
            &context,
        )
        .await
        .unwrap();
    let manifest = Manifest {
        version: 1,
        page_size: 512,
        logical_size: 2048,
        segment_size: 2048,
        generation_id: "g1".into(),
        base_root: Some(root),
        delta_root: None,
        overlay_bytes: 0,
        overlay_transactions: 0,
    };
    // Keep both base data and an old delta beyond the future truncation point.
    let reference = store
        .apply_pages(
            "inc1",
            1,
            manifest,
            remote_pager::FrozenPages {
                page_size: 512,
                final_file_size: 2048,
                pages: [(2, vec![9; 512])].into(),
            },
            &context,
        )
        .await
        .unwrap();
    let original: Manifest = store.get(&reference).await.unwrap();
    let original_bytes = store
        .snapshot(original.clone(), ctx())
        .read(0, 2048)
        .await
        .unwrap();
    assert_eq!(&original_bytes[1024..1536], &[9; 512]);

    let mut file =
        remote_pager::CowFile::new(Arc::new(Image(original_bytes.clone())), 512, 2048).unwrap();
    file.truncate(513).unwrap();
    file.truncate(1024).unwrap();
    let reference = store
        .apply_pages(
            "inc1",
            1,
            original.clone(),
            file.freeze().unwrap(),
            &context,
        )
        .await
        .unwrap();
    // Reopen through a fresh reader at each version to exercise persisted objects.
    let cold = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let shrunk: Manifest = cold.get(&reference).await.unwrap();
    let shrunk_snapshot = cold.snapshot(shrunk.clone(), ctx());
    assert!(matches!(
        shrunk_snapshot.read(1024, 1).await,
        Err(Error::Corrupt)
    ));
    let mut file = remote_pager::CowFile::new(
        Arc::new(Image(shrunk_snapshot.read(0, 1024).await.unwrap())),
        512,
        2048,
    )
    .unwrap();
    file.write(2047, &[5]).unwrap();
    let reference = cold
        .apply_pages("inc1", 1, shrunk, file.freeze().unwrap(), &context)
        .await
        .unwrap();
    let cold = DatabaseStore::new(backend, "notes").unwrap();
    let regrown: Manifest = cold.get(&reference).await.unwrap();
    let mut expected = vec![7; 513];
    expected.resize(2048, 0);
    expected[2047] = 5;
    assert_eq!(
        cold.snapshot(regrown, ctx()).read(0, 2048).await.unwrap(),
        expected
    );
    assert_eq!(
        cold.snapshot(original, ctx()).read(0, 2048).await.unwrap(),
        original_bytes
    );
}

#[tokio::test]
async fn head_only_publication_confirms_lost_response_without_ledger() {
    let backend = Arc::new(MemoryStore::default());
    let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
    let head = initial(&store).await;
    backend
        .lose_next_response
        .store(true, std::sync::atomic::Ordering::SeqCst);
    store.publish_head(None, head, &ctx()).await.unwrap();
    let base = store.read_head().await.unwrap();
    assert!(serde_json::to_value(&base.head)
        .unwrap()
        .get("request_index_root")
        .is_none());
    let mut next = base.head.clone();
    next.data_version += 1;
    store
        .publish_head(Some(&base), next.clone(), &ctx())
        .await
        .unwrap();
    assert!(matches!(
        store.publish_head(Some(&base), next, &ctx()).await,
        Err(Error::Conflict)
    ));
    assert_eq!(store.read_head().await.unwrap().head.data_version, 2);
}

struct UncertainStore {
    inner: MemoryStore,
    mode: u8,
}
impl BlobStore for UncertainStore {
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }
    async fn read_current(&self, key: &str) -> std::result::Result<VersionedObject, StoreError> {
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
        self.inner.create_if_absent(key, body).await
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> CasOutcome {
        if self.mode == 0 {
            return CasOutcome::Unknown;
        }
        let result = self.inner.compare_and_swap(key, expected, body).await;
        if !matches!(result, CasOutcome::Applied { .. }) {
            return result;
        }
        if self.mode == 2 {
            let current = self.inner.read_current(key).await.unwrap();
            let mut head: Head = serde_json::from_slice(&current.body).unwrap();
            head.head_nonce = unique();
            head.revision += 1;
            head.data_version += 1;
            assert!(matches!(
                self.inner
                    .compare_and_swap(key, &current.revision, serde_json::to_vec(&head).unwrap())
                    .await,
                CasOutcome::Applied { .. }
            ));
        }
        CasOutcome::Unknown
    }
    async fn list(&self, request: ListRequest) -> std::result::Result<ObjectPage, StoreError> {
        self.inner.list(request).await
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.inner.delete(key).await
    }
}
#[tokio::test]
async fn unknown_cas_only_succeeds_when_exact_candidate_is_current() {
    for mode in 0..=2 {
        let backend = Arc::new(UncertainStore {
            inner: MemoryStore::default(),
            mode,
        });
        let store = DatabaseStore::new(backend.clone(), "notes").unwrap();
        // Build the initial manifest and HEAD against the same backing store.
        let seed_store = DatabaseStore::new(Arc::new(MemoryStore::default()), "notes").unwrap();
        let mut head = initial(&seed_store).await;
        let manifest: Manifest = seed_store.get(&head.manifest_ref).await.unwrap();
        head.manifest_ref = store
            .put("inc1", 1, "manifests", &manifest, &ctx())
            .await
            .unwrap();
        store.publish_head(None, head, &ctx()).await.unwrap();
        let base = store.read_head().await.unwrap();
        let mut candidate = base.head.clone();
        candidate.data_version += 1;
        let result = store.publish_head(Some(&base), candidate, &ctx()).await;
        if mode == 1 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::Unknown)));
        }
        assert_eq!(
            store.read_head().await.unwrap().head.data_version,
            [1, 2, 3][mode as usize]
        );
    }
}
