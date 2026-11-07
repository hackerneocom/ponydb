use database_service::{Service, ServiceOptions};
use object_store_api::*;
use object_store_test_support::MemoryStore;
use protocol::{ApiError, Principal};
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{
    ops::Range,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;
use versioned_store::DatabaseStore;

#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}
impl Gate {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    async fn enter(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .unwrap();
    }
    fn resume(&self) {
        self.release.notify_one();
    }
}
#[derive(Default)]
struct Controlled {
    inner: MemoryStore,
    claim: Gate,
    list: Gate,
    delete: Gate,
    read: Gate,
    missing_delete: AtomicBool,
    heads: AtomicUsize,
    deletes: Mutex<Vec<usize>>,
    fail_delete: AtomicUsize,
    unknown_delete: AtomicBool,
    lose_claim: AtomicBool,
    advance_after_claim: AtomicBool,
    foreign_listing: AtomicBool,
}
impl BlobStore for Controlled {
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }
    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        if key.ends_with("/head.json") {
            self.heads.fetch_add(1, Ordering::SeqCst);
        }
        if key.contains("/deltas/") {
            self.read.enter().await;
        }
        self.inner.read_current(key).await
    }
    async fn read_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
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
        let value: Value = serde_json::from_slice(&body).unwrap();
        if !value["gc"].is_null() {
            self.claim.enter().await;
            if self.lose_claim.swap(false, Ordering::SeqCst) {
                let result = self.inner.compare_and_swap(key, expected, body).await;
                if matches!(result, CasOutcome::Applied { .. }) {
                    if self.advance_after_claim.swap(false, Ordering::SeqCst) {
                        let current = self.inner.read_current(key).await.unwrap();
                        let mut head: storage_format::Head =
                            serde_json::from_slice(&current.body).unwrap();
                        head.head_nonce = "concurrent-metadata-write".into();
                        head.revision += 1;
                        head.description = "preserved".into();
                        assert!(matches!(
                            self.inner
                                .compare_and_swap(
                                    key,
                                    &current.revision,
                                    serde_json::to_vec(&head).unwrap()
                                )
                                .await,
                            CasOutcome::Applied { .. }
                        ));
                    }
                    return CasOutcome::Unknown;
                }
                return result;
            }
        }
        self.inner.compare_and_swap(key, expected, body).await
    }
    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        self.list.enter().await;
        if self.foreign_listing.swap(false, Ordering::SeqCst) {
            return Ok(ObjectPage {
                objects: vec![
                    "databases/notes_old/incarnations/inc/generations/0000000000000001/deltas/x"
                        .into(),
                ],
                common_prefixes: vec![],
                next_cursor: None,
            });
        }
        self.inner.list(request).await
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.delete.enter().await;
        let number = {
            let mut deletes = self.deletes.lock().unwrap();
            deletes.push(self.heads.load(Ordering::SeqCst));
            deletes.len()
        };
        if self.fail_delete.load(Ordering::SeqCst) == number {
            return if self.unknown_delete.load(Ordering::SeqCst) {
                DeleteOutcome::Unknown
            } else {
                DeleteOutcome::NotApplied(StoreError::Transport("injected DELETE failure".into()))
            };
        }
        let result = self.inner.delete(key).await;
        if self.missing_delete.load(Ordering::SeqCst) {
            DeleteOutcome::Missing
        } else {
            result
        }
    }
}
fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(15))
}
async fn call(s: &Service<Controlled>, operation: &str, input: Value) -> Result<Value, ApiError> {
    s.call(
        &Principal::direct(),
        serde_json::from_value(json!({"operation":operation,"input":input})).unwrap(),
        ctx(),
    )
    .await
}
fn service(store: Arc<Controlled>) -> Arc<Service<Controlled>> {
    Arc::new(
        Service::new(
            store,
            "gc-test".into(),
            ServiceOptions {
                workers: 1,
                max_read_ms: 1000,
                ..Default::default()
            },
        )
        .unwrap(),
    )
}
async fn setup() -> (Arc<Controlled>, Arc<Service<Controlled>>) {
    let store = Arc::new(Controlled::default());
    let s = service(store.clone());
    call(&s, "database_create", json!({"name":"notes"}))
        .await
        .unwrap();
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"CREATE TABLE t(x)"}),
    )
    .await
    .unwrap();
    (store, s)
}
async fn orphan(store: &Arc<Controlled>, count: usize) {
    let db = DatabaseStore::new(store.clone(), "notes").unwrap();
    let head = db.read_head().await.unwrap().head;
    for _ in 0..count {
        db.put_bytes(
            &head.incarnation_id,
            head.epoch,
            "deltas",
            vec![42; 4096],
            &ctx(),
        )
        .await
        .unwrap();
    }
}
fn gc(s: Arc<Service<Controlled>>) -> tokio::task::JoinHandle<Result<Value, ApiError>> {
    tokio::spawn(async move {
        call(
            &s,
            "database_gc",
            json!({"database":"notes","budget_ms":10000}),
        )
        .await
    })
}
async fn rows(store: &Arc<Controlled>) -> Value {
    // New cache: a deleted live object cannot be hidden by the writer's cache.
    call(
        &service(store.clone()),
        "sql_execute",
        json!({"database":"notes","sql":"SELECT x FROM t ORDER BY x"}),
    )
    .await
    .unwrap()["results"]
        .clone()
}

#[tokio::test]
async fn claim_conflict_refreshes_roots_and_preserves_concurrent_commit() {
    let (store, s) = setup().await;
    orphan(&store, 2).await;
    store.claim.arm();
    let task = gc(s.clone());
    store.claim.wait().await;
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(42)"}),
    )
    .await
    .unwrap();
    let expected = rows(&store).await;
    store.claim.resume();
    assert_eq!(task.await.unwrap().unwrap()["complete"], true);
    assert_eq!(rows(&store).await, expected);
    assert!(DatabaseStore::new(store, "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head
        .gc
        .is_none());
}

#[tokio::test]
async fn online_writes_compaction_and_metadata_survive_cleanup() {
    let (store, s) = setup().await;
    orphan(&store, 3).await;
    store.list.arm();
    let task = gc(s.clone());
    store.list.wait().await;
    let head = DatabaseStore::new(store.clone(), "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head;
    assert!(head.gc.is_some());
    let details = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(details["status"], "active");
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(7)"}),
    )
    .await
    .unwrap();
    call(
        &s,
        "database_compact",
        json!({"database":"notes","budget_ms":1000,"max_segments":32}),
    )
    .await
    .unwrap();
    call(
        &s,
        "database_update",
        json!({"database":"notes","description":"new metadata"}),
    )
    .await
    .unwrap();
    let expected = rows(&store).await;
    store.list.resume();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["database_available"], true);
    assert_eq!(result["complete"], true);
    assert_eq!(rows(&store).await, expected);
    let head = DatabaseStore::new(store, "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head;
    assert_eq!(head.description, "new metadata");
    assert!(head.gc.is_none());
}

#[tokio::test]
async fn takeover_supersedes_old_gc_without_blocking_database() {
    let (store, s) = setup().await;
    store.list.arm();
    let old = gc(s.clone());
    store.list.wait().await;
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":100}),
    )
    .await
    .unwrap();
    assert_eq!(result["database_available"], true);
    store.list.resume();
    let result = old.await.unwrap().unwrap();
    assert_eq!(result["phase"], "superseded");
    assert_eq!(result["outcome"], "partial");
    rows(&store).await;
}

#[tokio::test]
async fn deleting_gc_can_outlive_recreation_without_deleting_new_objects() {
    let (store, mut s) = setup().await;
    // The old lifecycle keeps its short drain, but replacement SQL is not a
    // deadline test and needs scheduling headroom under concurrent test load.
    Arc::get_mut(&mut s).unwrap().options.max_read_ms = 5000;
    orphan(&store, 3).await;
    store.delete.arm();
    let task = gc(s.clone());
    store.delete.wait().await;
    let db = DatabaseStore::new(store.clone(), "notes").unwrap();
    let old_epoch = db.read_head().await.unwrap().head.epoch;
    call(&s, "database_delete", json!({"database":"notes"}))
        .await
        .unwrap();
    call(&s, "database_create", json!({"name":"notes"}))
        .await
        .unwrap();
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"CREATE TABLE t(x)"}),
    )
    .await
    .unwrap();
    let new_head = db.read_head().await.unwrap().head;
    assert_eq!(new_head.epoch, old_epoch + 2);
    assert_eq!(
        new_head
            .manifest_ref
            .relative_key()
            .unwrap()
            .object_parts()
            .map(|object| object.epoch),
        Some(new_head.epoch)
    );
    store.delete.resume();
    task.await.unwrap().unwrap();
    rows(&store).await;
    assert_eq!(
        db.read_head().await.unwrap().head.incarnation_id,
        new_head.incarnation_id
    );
}

#[tokio::test]
async fn deleting_gc_cannot_damage_restore_or_clear_successor_ownership() {
    let (store, s) = setup().await;
    orphan(&store, 3).await;
    store.delete.arm();
    let old_gc = gc(s.clone());
    store.delete.wait().await;
    let db = DatabaseStore::new(store.clone(), "notes").unwrap();
    let old = db.read_head().await.unwrap().head;
    call(&s, "database_delete", json!({"database":"notes"}))
        .await
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let connection = rusqlite::Connection::open(file.path()).unwrap();
    connection
        .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(42)")
        .unwrap();
    drop(connection);
    s.restore_file(
        &Principal::direct(),
        "notes",
        file.path().into(),
        RequestContext::unbounded(),
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let restored = db.read_head().await.unwrap().head;
    assert_ne!(restored.incarnation_id, old.incarnation_id);
    assert!(restored.epoch > old.epoch);
    // Hold a successor in LIST, after it has published ownership of the new lifecycle.
    store.list.arm();
    let successor = gc(s.clone());
    store.list.wait().await;
    let claimed = db.read_head().await.unwrap().head;
    assert!(claimed.gc.is_some());
    store.delete.resume();
    old_gc.await.unwrap().unwrap();
    let after_old_gc = db.read_head().await.unwrap().head;
    assert_eq!(after_old_gc.gc, claimed.gc);
    assert_eq!(after_old_gc.incarnation_id, restored.incarnation_id);
    store.list.resume();
    successor.await.unwrap().unwrap();
    let result = rows(&store).await;
    assert_eq!(
        result[0]["rows"][0][0],
        json!({"type":"integer","value":"42"})
    );
}

#[tokio::test]
async fn foreign_database_listing_is_rejected_before_any_gc_delete() {
    let (store, s) = setup().await;
    call(&s, "database_create", json!({"name":"notes_old"}))
        .await
        .unwrap();
    let neighbor = DatabaseStore::new(store.clone(), "notes_old")
        .unwrap()
        .read_head()
        .await
        .unwrap();
    store.foreign_listing.store(true, Ordering::SeqCst);
    let error = gc(s.clone()).await.unwrap().unwrap_err();
    assert_eq!(error.code, "STORAGE_CORRUPT");
    assert!(error.message.contains("GC key escaped database"));
    assert!(store.deletes.lock().unwrap().is_empty());
    assert_eq!(
        DatabaseStore::new(store.clone(), "notes_old")
            .unwrap()
            .read_head()
            .await
            .unwrap()
            .head
            .head_nonce,
        neighbor.head.head_nonce
    );
    rows(&store).await;
}

#[tokio::test]
async fn cancelled_caller_awaits_admitted_delete_then_cleans_latest_head() {
    let (store, s) = setup().await;
    orphan(&store, 3).await;
    store.delete.arm();
    let task = gc(s.clone());
    store.delete.wait().await;
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(9)"}),
    )
    .await
    .unwrap();
    let expected = rows(&store).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    store.delete.resume();
    s.wait_for_gc().await;
    assert_eq!(store.deletes.lock().unwrap().len(), 1);
    assert_eq!(rows(&store).await, expected);
    assert!(DatabaseStore::new(store, "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head
        .gc
        .is_none());
}

#[tokio::test]
async fn delete_limit_counts_attempts_and_does_not_read_head_per_object() {
    let (store, s) = setup().await;
    orphan(&store, 8).await;
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","max_objects":3}),
    )
    .await
    .unwrap();
    assert_eq!(result["deleted_objects"], 3);
    assert_eq!(result["complete"], false);
    assert_eq!(result["database_available"], true);
    let reads = store.deletes.lock().unwrap();
    assert_eq!(reads.len(), 3);
    assert!(
        reads.iter().all(|n| *n == reads[0]),
        "HEAD was fetched between DELETEs"
    );
    assert_eq!(
        reads[0],
        store.heads.load(Ordering::SeqCst) - 1,
        "only latest-HEAD cleanup follows deletion"
    );
}

#[tokio::test]
async fn delete_failures_preserve_partial_and_unknown_outcomes() {
    for unknown in [false, true] {
        let (store, s) = setup().await;
        orphan(&store, 3).await;
        store.fail_delete.store(2, Ordering::SeqCst);
        store.unknown_delete.store(unknown, Ordering::SeqCst);
        let error = gc(s).await.unwrap().unwrap_err();
        assert_eq!(error.outcome, if unknown { "unknown" } else { "partial" });
        assert!(error.message.contains("confirmed deleted_objects=1"));
        assert!(DatabaseStore::new(store, "notes")
            .unwrap()
            .read_head()
            .await
            .unwrap()
            .head
            .gc
            .is_none());
    }
}

#[tokio::test]
async fn uncertain_claim_is_confirmed_by_owner_after_concurrent_head_update() {
    let (store, s) = setup().await;
    store.lose_claim.store(true, Ordering::SeqCst);
    store.advance_after_claim.store(true, Ordering::SeqCst);
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":100}),
    )
    .await
    .unwrap();
    assert_eq!(result["database_available"], true);
    let head = DatabaseStore::new(store, "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head;
    assert_eq!(head.description, "preserved");
    assert!(head.gc.is_none());
}

#[tokio::test]
async fn stale_record_is_replaced_and_epoch_overflow_never_publishes() {
    let (store, s) = setup().await;
    let db = DatabaseStore::new(store, "notes").unwrap();
    let base = db.read_head().await.unwrap();
    let mut head = base.head.clone();
    head.gc = Some(storage_format::GcState {
        owner_id: "killed-process".into(),
        cutoff_epoch: head.epoch,
    });
    db.publish_head(Some(&base), head, &ctx()).await.unwrap();
    rows(&s.store).await;
    call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":100}),
    )
    .await
    .unwrap();
    let base = db.read_head().await.unwrap();
    assert!(base.head.gc.is_none());
    assert_eq!(base.head.epoch, 2);
    let mut head = base.head.clone();
    head.epoch = u64::MAX;
    db.publish_head(Some(&base), head, &ctx()).await.unwrap();
    assert_eq!(
        call(&s, "database_gc", json!({"database":"notes"}))
            .await
            .unwrap_err()
            .code,
        "RESOURCE_LIMIT"
    );
    assert_eq!(
        call(&s, "database_delete", json!({"database":"notes"}))
            .await
            .unwrap_err()
            .code,
        "RESOURCE_LIMIT"
    );
    assert_eq!(db.read_head().await.unwrap().head.epoch, u64::MAX);
}

#[test]
fn gc_defaults_limits_and_old_fields() {
    let input = serde_json::from_value::<protocol::Gc>(json!({"database":"notes"})).unwrap();
    assert_eq!(
        ServiceOptions::default().gc_limits(&input).unwrap(),
        (60000, 1000)
    );
    for value in [
        json!({"database":"notes","allow_pause":true}),
        json!({"database":"notes","pass_id":"old"}),
    ] {
        assert!(serde_json::from_value::<protocol::Gc>(value).is_err());
    }
    let mut input = input;
    input.budget_ms = Some(300000);
    assert_eq!(
        ServiceOptions::default().gc_limits(&input).unwrap().0,
        300000
    );
    input.budget_ms = Some(1800001);
    assert!(ServiceOptions::default().gc_limits(&input).is_err());
    input.budget_ms = Some(1);
    input.max_objects = Some(1001);
    assert!(ServiceOptions::default().gc_limits(&input).is_err());
}

#[tokio::test]
async fn missing_delete_consumes_budget_without_counting_as_deleted() {
    let (store, s) = setup().await;
    orphan(&store, 4).await;
    store.missing_delete.store(true, Ordering::SeqCst);
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","max_objects":1}),
    )
    .await
    .unwrap();
    assert_eq!(result["deleted_objects"], 0);
    assert_eq!(result["complete"], false);
    assert_eq!(store.deletes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stalled_baseline_reader_expires_before_gc_deletion() {
    let (store, s) = setup().await;
    orphan(&store, 2).await;
    store.read.arm();
    let reader = service(store.clone());
    let reading = tokio::spawn(async move {
        call(
            &reader,
            "sql_execute",
            json!({"database":"notes","sql":"SELECT x FROM t"}),
        )
        .await
    });
    store.read.wait().await;
    let collecting = gc(s.clone());
    let failure = tokio::time::timeout(Duration::from_millis(1500), reading)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.code, "DEADLINE_EXCEEDED");
    assert!(store.deletes.lock().unwrap().is_empty());
    store.read.resume();
    collecting.await.unwrap().unwrap();
    rows(&store).await;
}

#[tokio::test]
async fn malformed_generation_key_stops_gc_before_any_delete() {
    let (store, s) = setup().await;
    store
        .inner
        .create_if_absent(
            "databases/notes/incarnations/bad/generations/not-an-epoch/deltas/x",
            vec![1],
        )
        .await;
    let failure = gc(s).await.unwrap().unwrap_err();
    assert_eq!(failure.code, "STORAGE_CORRUPT");
    assert!(store.deletes.lock().unwrap().is_empty());
    assert!(DatabaseStore::new(store, "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head
        .gc
        .is_none());
}

#[tokio::test]
async fn larger_budget_supports_sixty_one_second_grace_without_waiting_when_cancelled() {
    let (store, s) = setup().await;
    orphan(&store, 1).await;
    let db = DatabaseStore::new(store.clone(), "notes").unwrap();
    let base = db.read_head().await.unwrap();
    let mut head = base.head.clone();
    head.safety_policy = storage_format::SafetyPolicy {
        max_read_ms: 60000,
        drain_ms: 61000,
    };
    db.publish_head(Some(&base), head, &ctx()).await.unwrap();
    // Default budget may end before this grace: partial is a normal, available result.
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":100}),
    )
    .await
    .unwrap();
    assert_eq!(result["complete"], false);
    assert_eq!(result["database_available"], true);
    store.list.arm();
    let context = ctx();
    let child = context.clone();
    let task = tokio::spawn(async move {
        s.call(
            &Principal::direct(),
            protocol::Operation::DatabaseGc(protocol::Gc {
                database: "notes".into(),
                budget_ms: Some(300000),
                max_objects: None,
            }),
            child,
        )
        .await
    });
    store.list.wait().await;
    context.cancel();
    store.list.resume();
    assert_eq!(task.await.unwrap().unwrap_err().code, "DEADLINE_EXCEEDED");
    assert!(db.read_head().await.unwrap().head.gc.is_none());
}

#[tokio::test]
async fn empty_candidate_set_finishes_without_waiting_for_grace() {
    let store = Arc::new(Controlled::default());
    let s = service(store.clone());
    call(&s, "database_create", json!({"name":"notes"}))
        .await
        .unwrap();
    let result = call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":500}),
    )
    .await
    .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["phase"], "complete");
    assert_eq!(result["deleted_objects"], 0);
    assert_eq!(result.as_object().unwrap().len(), 5);
}

#[tokio::test]
async fn unsupported_layout_is_explicit_and_never_modified() {
    let (store, s) = setup().await;
    let key = "databases/notes/head.json";
    let before = store.inner.read_current(key).await.unwrap();
    let mut head: Value = serde_json::from_slice(&before.body).unwrap();
    head["storage_layout_version"] = json!(2);
    let old_bytes = serde_json::to_vec(&head).unwrap();
    assert!(matches!(
        store
            .inner
            .compare_and_swap(key, &before.revision, old_bytes.clone())
            .await,
        CasOutcome::Applied { .. }
    ));
    assert_eq!(
        call(&s, "database_gc", json!({"database":"notes"}))
            .await
            .unwrap_err()
            .code,
        "UNSUPPORTED_LAYOUT"
    );
    assert_eq!(store.inner.read_current(key).await.unwrap().body, old_bytes);
    assert!(store.deletes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gc_observes_retirement_then_zero_retention_prunes_only_history() {
    let (store, s) = setup().await;
    gc(s.clone()).await.unwrap().unwrap();
    let db = DatabaseStore::new(store, "notes").unwrap();
    let head = db.read_head().await.unwrap().head;
    let entries = db
        .index_list(head.snapshot_index_root.clone(), None, 1000)
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    for (_, value) in entries {
        let h: storage_format::History = serde_json::from_value(value).unwrap();
        assert_eq!(
            h.retired_at_upper_ms.is_none(),
            h.data_version == head.data_version
        );
    }
    call(
        &s,
        "database_update",
        json!({"database":"notes","snapshot_retention_days":0}),
    )
    .await
    .unwrap();
    gc(s.clone()).await.unwrap().unwrap();
    let head = db.read_head().await.unwrap().head;
    assert_eq!(head.snapshot_count, 1);
    let entries = db
        .index_list(head.snapshot_index_root, None, 1000)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1["data_version"], head.data_version.to_string());
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"SELECT * FROM t"}),
    )
    .await
    .unwrap();
}
