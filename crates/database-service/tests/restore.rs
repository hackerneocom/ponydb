use database_service::{
    restore::{Progress, RestoreStage},
    Service, ServiceOptions,
};
use object_store_api::*;
use object_store_test_support::MemoryStore;
use protocol::{Operation, Principal};
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{
    ops::Range,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
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
struct Recording {
    inner: MemoryStore,
    events: Mutex<Vec<(String, String)>>,
    segment: Gate,
    head: Gate,
    fail_segment: AtomicBool,
    head_response: AtomicU8,
    hide_head_once: AtomicBool,
    cancel_at_head: Mutex<Option<RequestContext>>,
}
impl Recording {
    fn event(&self, kind: &str, key: &str) {
        self.events.lock().unwrap().push((kind.into(), key.into()));
    }
    fn head_result(&self, result: CasOutcome) -> CasOutcome {
        if matches!(result, CasOutcome::Applied { .. }) {
            if let Some(ctx) = self.cancel_at_head.lock().unwrap().take() {
                ctx.cancel();
            }
            match self.head_response.swap(0, Ordering::SeqCst) {
                1 => return CasOutcome::Unknown,
                2 => {
                    self.hide_head_once.store(true, Ordering::SeqCst);
                    return CasOutcome::Unknown;
                }
                _ => {}
            }
        }
        result
    }
}
impl BlobStore for Recording {
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }
    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        self.event("get", key);
        if key.ends_with("/head.json") && self.hide_head_once.swap(false, Ordering::SeqCst) {
            return Err(StoreError::Transport("lost confirmation".into()));
        }
        self.inner.read_current(key).await
    }
    async fn read_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        self.event("range", key);
        self.inner.read_range(key, range).await
    }
    async fn create_if_absent(&self, key: &str, bytes: Vec<u8>) -> CasOutcome {
        self.event("put", key);
        if key.contains("/segments/") {
            self.segment.enter().await;
            if self.fail_segment.swap(false, Ordering::SeqCst) {
                return CasOutcome::NotApplied {
                    reason: StoreError::Transport("injected upload failure".into()),
                    retryable: false,
                };
            }
        }
        if key.ends_with("/head.json") {
            self.head.enter().await;
            return self.head_result(self.inner.create_if_absent(key, bytes).await);
        }
        self.inner.create_if_absent(key, bytes).await
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        bytes: Vec<u8>,
    ) -> CasOutcome {
        self.event("cas", key);
        if key.ends_with("/head.json") {
            self.head.enter().await;
            return self.head_result(self.inner.compare_and_swap(key, expected, bytes).await);
        }
        self.inner.compare_and_swap(key, expected, bytes).await
    }
    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        self.event("list", &request.prefix);
        self.inner.list(request).await
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.event("delete", key);
        self.inner.delete(key).await
    }
}
fn service() -> Service<Recording> {
    Service::new(
        Arc::new(Recording::default()),
        "restore-test".into(),
        ServiceOptions {
            segment_size: 65536,
            max_read_ms: 1000,
            ..Default::default()
        },
    )
    .unwrap()
}
fn backup(page_size: usize, payload: usize) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let c = rusqlite::Connection::open(file.path()).unwrap();
    c.execute_batch(&format!("PRAGMA page_size={page_size}; CREATE TABLE t(x BLOB); INSERT INTO t VALUES(zeroblob({payload}));")).unwrap();
    file
}
fn progress() -> Progress {
    Arc::new(|_| {})
}
async fn restore(
    s: &Service<Recording>,
    file: &tempfile::NamedTempFile,
) -> database_service::Result<Value> {
    s.restore_file(
        &Principal::direct(),
        "notes",
        file.path().into(),
        RequestContext::unbounded(),
        progress(),
    )
    .await
}
async fn call(s: &Service<Recording>, name: &str, input: Value) -> Value {
    let op: Operation = serde_json::from_value(json!({"operation":name,"input":input})).unwrap();
    s.call(
        &Principal::direct(),
        op,
        RequestContext::new(Duration::from_secs(30)),
    )
    .await
    .unwrap()
}
async fn head(s: &Service<Recording>) -> versioned_store::HeadView {
    DatabaseStore::new(s.store.clone(), "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
}

#[tokio::test]
async fn multiple_page_sizes_short_final_segment_and_no_staging_or_downloads() {
    for page_size in [512, 4096, 65536] {
        let s = service();
        let file = backup(page_size, 180000);
        let source = std::fs::read(file.path()).unwrap();
        let stages = Arc::new(Mutex::new(Vec::new()));
        let observed = stages.clone();
        let result = s
            .restore_file(
                &Principal::direct(),
                "notes",
                file.path().into(),
                RequestContext::unbounded(),
                Arc::new(move |event| {
                    observed.lock().unwrap().push(event);
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["sha256"], storage_format::digest(&source));
        assert_eq!(result["page_size"], page_size);
        assert_eq!(result["total_bytes"], source.len().to_string());
        let events = s.store.events.lock().unwrap().clone();
        assert!(!events.iter().any(|(_, key)| key.contains("/restore/")));
        assert!(!events
            .iter()
            .any(|(kind, key)| (kind == "get" || kind == "range") && key.contains("/segments/")));
        assert_eq!(
            events
                .iter()
                .filter(|(kind, key)| kind == "put" && key.contains("/segments/"))
                .count(),
            source.len().div_ceil(65536)
        );
        assert_eq!(
            events
                .iter()
                .filter(|(kind, key)| kind == "put" && key.ends_with("/head.json"))
                .count(),
            1
        );
        {
            let observed = stages.lock().unwrap();
            let complete = observed
                .iter()
                .position(|p| p.stage == RestoreStage::Complete)
                .unwrap();
            let publish = observed
                .iter()
                .position(|p| p.stage == RestoreStage::Publishing)
                .unwrap();
            assert!(complete > publish);
            assert_eq!(
                observed
                    .iter()
                    .rfind(|p| p.stage == RestoreStage::Uploading)
                    .unwrap()
                    .completed_bytes,
                source.len() as u64
            );
        }
        let query = call(
            &s,
            "sql_execute",
            json!({"database":"notes","sql":"SELECT length(x) FROM t"}),
        )
        .await;
        assert_eq!(query["results"][0]["rows"][0][0]["value"], "180000");
    }
}

#[tokio::test]
async fn invalid_sources_and_cancelled_local_work_never_touch_storage() {
    for bytes in [vec![], vec![0; 512], b"SQLite format 3\0".to_vec()] {
        let s = service();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        assert_eq!(
            restore(&s, &file).await.unwrap_err().code,
            "INVALID_SQLITE_BACKUP"
        );
        assert!(s.store.events.lock().unwrap().is_empty());
    }
    for sql in [
        "CREATE VIRTUAL TABLE t USING fts5(x)",
        "CREATE TABLE p(x PRIMARY KEY); CREATE TABLE t(x REFERENCES p(x)); INSERT INTO t VALUES(1)",
    ] {
        let s = service();
        let file = tempfile::NamedTempFile::new().unwrap();
        let c = rusqlite::Connection::open(file.path()).unwrap();
        c.execute_batch(&format!("PRAGMA foreign_keys=OFF; {sql}"))
            .unwrap();
        drop(c);
        assert!(restore(&s, &file).await.is_err());
        assert!(s.store.events.lock().unwrap().is_empty());
    }
    for stage in [
        RestoreStage::Copying,
        RestoreStage::Validating,
        RestoreStage::Hashing,
    ] {
        let s = service();
        let file = backup(4096, 500000);
        let ctx = RequestContext::unbounded();
        let cancel = ctx.clone();
        let result = s
            .restore_file(
                &Principal::direct(),
                "notes",
                file.path().into(),
                ctx,
                Arc::new(move |event| {
                    if event.stage == stage {
                        cancel.cancel();
                    }
                }),
            )
            .await;
        assert_eq!(result.unwrap_err().code, "DEADLINE_EXCEEDED");
        assert!(s.store.events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn wal_backup_is_rejected_before_storage_even_after_checkpoint() {
    let s = service();
    let file = backup(4096, 10000);
    let connection = rusqlite::Connection::open(file.path()).unwrap();
    connection.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO t VALUES(zeroblob(100)); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(connection);
    let bytes = std::fs::read(file.path()).unwrap();
    assert_eq!(&bytes[18..20], &[2, 2]);
    let err = restore(&s, &file).await.unwrap_err();
    assert_eq!(err.code, "INVALID_SQLITE_BACKUP");
    assert!(err.message.contains("DELETE journal mode"));
    assert!(s.store.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn backup_is_copied_before_validation_and_upload() {
    let s = service();
    let file = backup(4096, 10000);
    let expected = storage_format::digest(&std::fs::read(file.path()).unwrap());
    let path = file.path().to_owned();
    let result = s
        .restore_file(
            &Principal::direct(),
            "notes",
            file.path().into(),
            RequestContext::unbounded(),
            Arc::new(move |event| {
                if event.stage == RestoreStage::Validating {
                    std::fs::write(&path, b"changed source").unwrap();
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["sha256"], expected);
}

#[tokio::test]
async fn upload_failure_and_cancellation_leave_head_unchanged_and_allow_retry() {
    for existing in [false, true] {
        let s = service();
        let file = backup(4096, 180000);
        if existing {
            call(&s, "database_create", json!({"name":"notes"})).await;
            call(&s, "database_delete", json!({"database":"notes"})).await;
        }
        let before = if existing {
            Some(head(&s).await.head.head_nonce)
        } else {
            None
        };
        s.store.fail_segment.store(true, Ordering::SeqCst);
        assert_eq!(
            restore(&s, &file).await.unwrap_err().code,
            "STORAGE_UNAVAILABLE"
        );
        if let Some(before) = &before {
            assert_eq!(&head(&s).await.head.head_nonce, before);
        } else {
            assert!(s
                .store
                .inner
                .read_current("databases/notes/head.json")
                .await
                .is_err());
        }
        s.store.segment.arm();
        let ctx = RequestContext::unbounded();
        let principal = Principal::direct();
        let import = s.restore_file(
            &principal,
            "notes",
            file.path().into(),
            ctx.clone(),
            progress(),
        );
        let cancel = async {
            s.store.segment.wait().await;
            ctx.cancel();
        };
        let (result, ()) = tokio::join!(import, cancel);
        assert_eq!(result.unwrap_err().code, "DEADLINE_EXCEEDED");
        assert!(restore(&s, &file).await.unwrap()["committed"]
            .as_bool()
            .unwrap());
    }
}

#[tokio::test]
async fn concurrent_restore_and_create_publish_exactly_one_candidate() {
    for other_restore in [false, true] {
        for deleted_target in [false, true] {
            let s = service();
            let file = backup(4096, 10000);
            if deleted_target {
                call(&s, "database_create", json!({"name":"notes"})).await;
                call(&s, "database_delete", json!({"database":"notes"})).await;
            }
            s.store.head.arm();
            let import = restore(&s, &file);
            let competitor = async {
                s.store.head.wait().await;
                if other_restore {
                    restore(&s, &file).await.unwrap();
                } else {
                    call(&s, "database_create", json!({"name":"notes"})).await;
                }
                let nonce = head(&s).await.head.head_nonce;
                s.store.head.resume();
                nonce
            };
            let (result, nonce) = tokio::join!(import, competitor);
            assert_eq!(result.unwrap_err().code, "TX_CONFLICT");
            assert_eq!(head(&s).await.head.head_nonce, nonce);
        }
    }
}

#[tokio::test]
async fn deleted_database_restore_resets_history_and_fences_old_writes_and_tokens() {
    let s = service();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let written = call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"CREATE TABLE old_table(x)"}),
    )
    .await;
    // Populate the shared reader before replacing this lifecycle.
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"SELECT * FROM old_table"}),
    )
    .await;
    let old = head(&s).await;
    let snapshot = format!("{}:{}", old.head.incarnation_id, old.head.data_version);
    call(&s, "database_delete", json!({"database":"notes"})).await;
    let deleted = head(&s).await;
    call(
        &s,
        "snapshot_get",
        json!({"database":"notes","snapshot_id":snapshot}),
    )
    .await;
    let file = backup(4096, 10000);
    restore(&s, &file).await.unwrap();
    let current = head(&s).await;
    assert_ne!(current.head.incarnation_id, old.head.incarnation_id);
    assert!(current.head.revision > deleted.head.revision);
    assert!(current.head.epoch > deleted.head.epoch);
    assert_eq!(current.head.data_version, 1);
    assert!(current.head.safety_policy.drain_ms >= old.head.safety_policy.drain_ms);
    assert_eq!(current.head.snapshot_count, 1);
    let snapshots = call(&s, "snapshot_list", json!({"database":"notes"})).await;
    assert_eq!(snapshots["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(
        snapshots["snapshots"][0]["snapshot_id"],
        format!("{}:1", current.head.incarnation_id)
    );
    let db = DatabaseStore::new(s.store.clone(), "notes").unwrap();
    assert!(matches!(
        db.publish_head(Some(&old), old.head.clone(), &RequestContext::unbounded())
            .await,
        Err(versioned_store::Error::Conflict)
    ));
    for (operation, input, code) in [
        (
            "snapshot_get",
            json!({"database":"notes","snapshot_id":snapshot}),
            "SNAPSHOT_EXPIRED",
        ),
        (
            "sql_execute",
            json!({"database":"notes","sql":"SELECT 1","consistency":"at_least","commit_token":written["commit_token"]}),
            "INVALID_REQUEST",
        ),
        (
            "database_update",
            json!({"database":"notes","if_match":old.etag,"description":"stale"}),
            "METADATA_PRECONDITION_FAILED",
        ),
        (
            "sql_execute",
            json!({"database":"notes","sql":"SELECT * FROM old_table"}),
            "SQL_ERROR",
        ),
    ] {
        let op = serde_json::from_value(json!({"operation":operation,"input":input})).unwrap();
        assert_eq!(
            s.call(&Principal::direct(), op, RequestContext::unbounded())
                .await
                .unwrap_err()
                .code,
            code
        );
    }
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"SELECT * FROM t"}),
    )
    .await;
    assert_eq!(
        restore(&s, &file).await.unwrap_err().code,
        "DATABASE_EXISTS"
    );
}

#[tokio::test]
async fn gc_epoch_change_prevents_stale_restore_publication() {
    let s = service();
    let file = backup(4096, 180000);
    call(&s, "database_create", json!({"name":"notes"})).await;
    call(&s, "database_delete", json!({"database":"notes"})).await;
    s.store.segment.arm();
    let import = restore(&s, &file);
    let gc = async {
        s.store.segment.wait().await;
        call(
            &s,
            "database_gc",
            json!({"database":"notes","budget_ms":5000,"max_objects":1}),
        )
        .await;
        let nonce = head(&s).await.head.head_nonce;
        s.store.segment.resume();
        nonce
    };
    let (result, nonce) = tokio::join!(import, gc);
    assert_eq!(result.unwrap_err().code, "TX_CONFLICT");
    assert_eq!(head(&s).await.head.head_nonce, nonce);
    restore(&s, &file).await.unwrap();
}

#[tokio::test]
async fn lost_publication_response_and_cancellation_preserve_outcome_semantics() {
    for mode in [1, 2] {
        let s = service();
        let file = backup(4096, 10000);
        s.store.head_response.store(mode, Ordering::SeqCst);
        let ctx = RequestContext::unbounded();
        *s.store.cancel_at_head.lock().unwrap() = Some(ctx.clone());
        let result = s
            .restore_file(
                &Principal::direct(),
                "notes",
                file.path().into(),
                ctx,
                progress(),
            )
            .await;
        if mode == 1 {
            assert_eq!(result.unwrap()["committed"], true);
        } else {
            let err = result.unwrap_err();
            assert_eq!(err.code, "OUTCOME_UNKNOWN");
            assert!(!err.retryable);
        }
        assert_eq!(head(&s).await.head.status, "active");
        assert_eq!(
            restore(&s, &file).await.unwrap_err().code,
            "DATABASE_EXISTS"
        );
    }
}

#[tokio::test]
async fn restore_has_no_request_deadline_or_snapshot_reader_lease() {
    let mut s = service();
    let file = backup(4096, 10000);
    call(&s, "database_create", json!({"name":"notes"})).await;
    call(&s, "database_delete", json!({"database":"notes"})).await;
    let previous = head(&s).await;
    let mut old_head = previous.head;
    old_head.safety_policy.max_read_ms = 10;
    assert!(matches!(
        s.store
            .inner
            .compare_and_swap(
                "databases/notes/head.json",
                &previous.revision,
                serde_json::to_vec(&old_head).unwrap()
            )
            .await,
        CasOutcome::Applied { .. }
    ));
    s.options.max_request_ms = 1;
    s.store.segment.arm();
    let import = restore(&s, &file);
    let release = async {
        s.store.segment.wait().await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(head(&s).await.head.status, "deleted");
        s.store.segment.resume();
    };
    let (result, ()) = tokio::join!(import, release);
    assert_eq!(result.unwrap()["committed"], true);
}
