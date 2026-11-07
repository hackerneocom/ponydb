use database_service::{Service, ServiceOptions};
use object_store_test_support::MemoryStore;
use protocol::{Principal, SnapshotExport};
use request_context::RequestContext;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(30))
}
async fn call(s: &Service<MemoryStore>, name: &str, input: Value) -> Value {
    s.call(
        &Principal::direct(),
        serde_json::from_value(json!({"operation":name,"input":input})).unwrap(),
        ctx(),
    )
    .await
    .unwrap()
}
fn request(id: &str) -> SnapshotExport {
    SnapshotExport {
        database: "notes".into(),
        snapshot_id: id.into(),
    }
}
async fn snapshot(s: &Service<MemoryStore>) -> String {
    let list = call(s, "snapshot_list", json!({"database":"notes"})).await;
    list["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .max_by_key(|v| v["data_version"].as_str().unwrap().parse::<u64>().unwrap())
        .unwrap()["snapshot_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn open(bytes: &[u8]) -> (tempfile::NamedTempFile, rusqlite::Connection) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    let db = rusqlite::Connection::open(file.path()).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    (file, db)
}

#[tokio::test]
async fn current_export_matches_head_before_and_after_compaction() {
    let s = Service::new(
        Arc::new(MemoryStore::default()),
        "test".into(),
        ServiceOptions::default(),
    )
    .unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let mut bytes = Vec::new();
    let empty = s
        .export_database(&Principal::direct(), "notes", ctx(), &mut bytes)
        .await
        .unwrap();
    assert_eq!(empty.data_version, "1");
    let (_file, db) = open(&bytes);
    assert_eq!(
        db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );

    call(
        &s,
        "sql_execute",
        json!({"database":"notes","statements":[
            {"sql":"CREATE TABLE t(x)"}, {"sql":"INSERT INTO t VALUES(42)"}
        ]}),
    )
    .await;
    for compact in [false, true] {
        if compact {
            call(
                &s,
                "database_compact",
                json!({"database":"notes","budget_ms":5000,"max_segments":32}),
            )
            .await;
        }
        let before = call(&s, "database_get", json!({"database":"notes"})).await;
        bytes.clear();
        let result = s
            .export_database(&Principal::direct(), "notes", ctx(), &mut bytes)
            .await
            .unwrap();
        assert_eq!(
            result.data_version,
            before["data_version"].as_str().unwrap()
        );
        assert_eq!(result.snapshot_id, snapshot(&s).await);
        assert_eq!(result.total_bytes, bytes.len().to_string());
        assert_eq!(result.sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(
            before,
            call(&s, "database_get", json!({"database":"notes"})).await
        );
        let (_file, db) = open(&bytes);
        assert_eq!(
            db.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }
}

#[tokio::test]
async fn historical_export_is_independent_and_restorable_after_compaction() {
    let s = Service::new(
        Arc::new(MemoryStore::default()),
        "test".into(),
        ServiceOptions::default(),
    )
    .unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let empty = snapshot(&s).await;
    let mut bytes = Vec::new();
    s.export_snapshot(&Principal::direct(), request(&empty), ctx(), &mut bytes)
        .await
        .unwrap();
    let (_file, db) = open(&bytes);
    assert_eq!(
        db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"CREATE TABLE t(id INTEGER PRIMARY KEY, payload BLOB)"}),
    )
    .await;
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(1, zeroblob(1100000))"}),
    )
    .await;
    let id = snapshot(&s).await;
    call(
        &s,
        "database_compact",
        json!({"database":"notes","budget_ms":5000,"max_segments":32}),
    )
    .await;
    call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"ALTER TABLE t ADD COLUMN newer TEXT"}),
    )
    .await;
    let before = call(&s, "database_get", json!({"database":"notes"})).await;
    bytes.clear();
    let result = s
        .export_snapshot(&Principal::direct(), request(&id), ctx(), &mut bytes)
        .await
        .unwrap();
    assert_eq!(result.total_bytes, bytes.len().to_string());
    assert_eq!(result.sha256, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(
        before,
        call(&s, "database_get", json!({"database":"notes"})).await
    );
    let (_file, db) = open(&bytes);
    assert_eq!(
        db.query_row("SELECT length(payload) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1100000
    );
    assert!(db.prepare("SELECT newer FROM t").is_err());

    s.restore_file(
        &Principal::direct(),
        "restored",
        _file.path().into(),
        RequestContext::unbounded(),
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let query = call(
        &s,
        "sql_execute",
        json!({"database":"restored","sql":"SELECT length(payload) FROM t"}),
    )
    .await;
    assert_eq!(query["results"][0]["rows"][0][0]["value"], "1100000");
}

#[tokio::test]
async fn export_rejects_invalid_access_limits_cancellation_and_old_lifecycles() {
    let mut s = Service::new(
        Arc::new(MemoryStore::default()),
        "test".into(),
        ServiceOptions::default(),
    )
    .unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let id = snapshot(&s).await;
    let mut bytes = Vec::new();
    let mut denied = Principal::direct();
    denied.scopes = vec!["data:read".into()];
    assert_eq!(
        s.export_database(&denied, "notes", ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "FORBIDDEN"
    );
    assert_eq!(
        s.export_database(&Principal::direct(), "missing", ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "DATABASE_NOT_FOUND"
    );
    assert_eq!(
        s.export_snapshot(&denied, request(&id), ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "FORBIDDEN"
    );
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request("missing"), ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "SNAPSHOT_EXPIRED"
    );
    let cancelled = ctx();
    cancelled.cancel();
    assert_eq!(
        s.export_database(&Principal::direct(), "notes", cancelled.clone(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "DEADLINE_EXCEEDED"
    );
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request(&id), cancelled, &mut bytes)
            .await
            .unwrap_err()
            .code,
        "DEADLINE_EXCEEDED"
    );
    s.options.export_max_bytes = 1;
    assert_eq!(
        s.export_database(&Principal::direct(), "notes", ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "RESOURCE_LIMIT"
    );
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut bytes)
            .await
            .unwrap_err()
            .code,
        "RESOURCE_LIMIT"
    );
    assert!(bytes.is_empty());
    s.options.export_max_bytes = 10 * 1024 * 1024;
    let (mut broken, receiver) = tokio::io::duplex(1);
    drop(receiver);
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut broken)
            .await
            .unwrap_err()
            .code,
        "RESOURCE_LIMIT"
    );
    call(&s, "database_delete", json!({"database":"notes"})).await;
    assert_eq!(
        s.export_database(&Principal::direct(), "notes", ctx(), &mut Vec::new())
            .await
            .unwrap_err()
            .code,
        "DATABASE_DELETED"
    );
    s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut bytes)
        .await
        .unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut Vec::new())
            .await
            .unwrap_err()
            .code,
        "SNAPSHOT_EXPIRED"
    );
}

#[tokio::test]
async fn export_stops_stalled_writes_and_allows_gc_but_rejects_missing_objects() {
    use object_store_api::BlobStore;
    let store = Arc::new(MemoryStore::default());
    let s = Service::new(store.clone(), "test".into(), ServiceOptions::default()).unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let id = snapshot(&s).await;
    let (mut writer, _receiver) = tokio::io::duplex(1);
    assert_eq!(
        s.export_database(
            &Principal::direct(),
            "notes",
            RequestContext::new(Duration::from_millis(20)),
            &mut writer
        )
        .await
        .unwrap_err()
        .code,
        "DEADLINE_EXCEEDED"
    );
    let (mut writer, _receiver) = tokio::io::duplex(1);
    let result = s
        .export_snapshot(
            &Principal::direct(),
            request(&id),
            RequestContext::new(Duration::from_millis(20)),
            &mut writer,
        )
        .await;
    assert_eq!(result.unwrap_err().code, "DEADLINE_EXCEEDED");
    call(
        &s,
        "database_gc",
        json!({"database":"notes","budget_ms":5000,"max_objects":1}),
    )
    .await;
    s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut Vec::new())
        .await
        .unwrap();
    s.export_database(&Principal::direct(), "notes", ctx(), &mut Vec::new())
        .await
        .unwrap();
    // Use an uncached reader so the deleted object must be fetched from storage.
    let fresh = Arc::new(MemoryStore::default());
    let s = Service::new(fresh.clone(), "test".into(), ServiceOptions::default()).unwrap();
    call(&s, "database_create", json!({"name":"notes"})).await;
    let id = snapshot(&s).await;
    let db = versioned_store::DatabaseStore::new(fresh.clone(), "notes").unwrap();
    let head = db.read_head().await.unwrap();
    fresh
        .delete(&db.physical(&head.head.manifest_ref.relative_key().unwrap()))
        .await;
    let s = Service::new(fresh, "test".into(), ServiceOptions::default()).unwrap();
    assert_eq!(
        s.export_database(&Principal::direct(), "notes", ctx(), &mut Vec::new())
            .await
            .unwrap_err()
            .code,
        "STORAGE_CORRUPT"
    );
    assert_eq!(
        s.export_snapshot(&Principal::direct(), request(&id), ctx(), &mut Vec::new())
            .await
            .unwrap_err()
            .code,
        "STORAGE_CORRUPT"
    );
}

#[tokio::test]
async fn export_remains_pinned_while_sql_writes_publish_new_versions() {
    use tokio::io::AsyncReadExt;
    for current in [false, true] {
        let s = Service::new(
            Arc::new(MemoryStore::default()),
            "test".into(),
            ServiceOptions::default(),
        )
        .unwrap();
        call(&s, "database_create", json!({"name":"notes"})).await;
        call(
            &s,
            "sql_execute",
            json!({"database":"notes","sql":"CREATE TABLE t(x)"}),
        )
        .await;
        let id = snapshot(&s).await;
        let before = call(&s, "database_get", json!({"database":"notes"})).await;
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let principal = Principal::direct();
        let export = async {
            let result = if current {
                s.export_database(&principal, "notes", ctx(), &mut writer)
                    .await
            } else {
                s.export_snapshot(&principal, request(&id), ctx(), &mut writer)
                    .await
            };
            drop(writer);
            result.unwrap()
        };
        let mutate_and_collect = async {
            // Wait for the first byte: the Manifest has been selected, and export is blocked on writing.
            let mut bytes = vec![reader.read_u8().await.unwrap()];
            call(
                &s,
                "sql_execute",
                json!({"database":"notes","sql":"INSERT INTO t VALUES(42)"}),
            )
            .await;
            reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        };
        let (result, bytes) = tokio::join!(export, mutate_and_collect);
        assert_eq!(result.snapshot_id, id);
        assert_eq!(
            result.data_version,
            before["data_version"].as_str().unwrap()
        );
        assert_ne!(
            result.data_version,
            call(&s, "database_get", json!({"database":"notes"})).await["data_version"]
                .as_str()
                .unwrap()
        );
        let (_file, db) = open(&bytes);
        assert_eq!(
            db.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
