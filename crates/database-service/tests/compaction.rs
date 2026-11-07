use database_service::{Service, ServiceOptions};
use object_store_api::*;
use object_store_test_support::MemoryStore;
use protocol::Principal;
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
    time::Duration,
};

enum Fault {
    Cancel,
    Deadline,
    Store(StoreError),
    Corrupt,
}

#[derive(Default)]
struct ReadFaultStore {
    inner: MemoryStore,
    fault: Mutex<Option<(Fault, RequestContext)>>,
}

impl BlobStore for ReadFaultStore {
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }

    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        // Inject only while reading source data, after HEAD and indices were read.
        let fault = if key.contains("/deltas/") {
            self.fault.lock().unwrap().take()
        } else {
            None
        };
        if let Some((fault, ctx)) = fault {
            match fault {
                Fault::Cancel => ctx.cancel(),
                Fault::Deadline => {
                    while ctx.check().is_ok() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                Fault::Store(error) => return Err(error),
                Fault::Corrupt => {
                    let mut object = self.inner.read_current(key).await?;
                    object.body[0] ^= 1;
                    return Ok(object);
                }
            }
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
        self.inner.compare_and_swap(key, expected, body).await
    }

    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        self.inner.list(request).await
    }

    async fn delete(&self, key: &str) -> DeleteOutcome {
        self.inner.delete(key).await
    }
}

async fn call(service: &Service<ReadFaultStore>, operation: &str, input: Value) -> Value {
    service
        .call(
            &Principal::direct(),
            serde_json::from_value(json!({"operation":operation,"input":input})).unwrap(),
            RequestContext::new(Duration::from_secs(30)),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn compaction_preserves_source_read_errors_without_publishing() {
    let store = Arc::new(ReadFaultStore::default());
    let service = Service::new(store.clone(), "test".into(), ServiceOptions::default()).unwrap();
    call(&service, "database_create", json!({"name":"notes"})).await;
    call(
        &service,
        "sql_execute",
        json!({"database":"notes","sql":"CREATE TABLE t(x)"}),
    )
    .await;
    call(
        &service,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(42)"}),
    )
    .await;
    let before = store
        .read_current("databases/notes/head.json")
        .await
        .unwrap();
    let query = json!({"database":"notes","sql":"SELECT x FROM t"});
    let expected_rows = call(&service, "sql_execute", query.clone()).await["results"].clone();

    for (fault, code, message, retryable) in [
        (
            Fault::Deadline,
            "DEADLINE_EXCEEDED",
            "Request expired or was cancelled",
            false,
        ),
        (
            Fault::Cancel,
            "DEADLINE_EXCEEDED",
            "Request expired or was cancelled",
            false,
        ),
        (
            Fault::Store(StoreError::Remote {
                operation: "GetObject",
                status: 403,
                code: Some("AccessDenied".into()),
                message: Some("Access denied".into()),
            }),
            "STORAGE_UNAVAILABLE",
            "S3 GetObject failed (HTTP 403, AccessDenied): Access denied",
            false,
        ),
        (
            Fault::Store(StoreError::Remote {
                operation: "GetObject",
                status: 503,
                code: Some("SlowDown".into()),
                message: Some("Please retry".into()),
            }),
            "STORAGE_UNAVAILABLE",
            "S3 GetObject failed (HTTP 503, SlowDown): Please retry",
            true,
        ),
        (
            Fault::Store(StoreError::Transport("connection lost".into())),
            "STORAGE_UNAVAILABLE",
            "Object store request failed",
            true,
        ),
        (
            Fault::Store(StoreError::NotFound),
            "STORAGE_CORRUPT",
            "Database 'notes' storage is incomplete: a referenced object is missing",
            false,
        ),
        (
            Fault::Corrupt,
            "STORAGE_CORRUPT",
            "Stored data failed validation",
            false,
        ),
    ] {
        // A fresh reader forces source data through the fault instead of the cache.
        let cold = Service::new(store.clone(), "test".into(), ServiceOptions::default()).unwrap();
        let ctx = RequestContext::new(Duration::from_secs(if matches!(fault, Fault::Deadline) {
            1
        } else {
            30
        }));
        *store.fault.lock().unwrap() = Some((fault, ctx.clone()));
        let error = cold.call(
            &Principal::direct(),
            serde_json::from_value(json!({"operation":"database_compact","input":{"database":"notes","budget_ms":5000,"max_segments":32}})).unwrap(),
            ctx,
        ).await.unwrap_err();
        assert!(
            store.fault.lock().unwrap().is_none(),
            "source read was not reached"
        );
        assert_eq!(error.code, code);
        assert_eq!(error.message, message);
        assert_eq!(error.retryable, retryable);
        assert_eq!(error.outcome, "not_committed");
        let after = store
            .read_current("databases/notes/head.json")
            .await
            .unwrap();
        assert_eq!(after.etag, before.etag);
        assert_eq!(after.body, before.body);
        assert_eq!(
            call(&cold, "sql_execute", query.clone()).await["results"],
            expected_rows
        );
    }
}
