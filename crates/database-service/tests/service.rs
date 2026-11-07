use database_service::*;
use object_store_test_support::MemoryStore;
use protocol::*;
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
fn context() -> RequestContext {
    RequestContext::new(Duration::from_secs(30))
}
fn service() -> Service<MemoryStore> {
    Service::new(
        Arc::new(MemoryStore::default()),
        "test".into(),
        ServiceOptions {
            max_read_ms: 2000,
            ..Default::default()
        },
    )
    .unwrap()
}
async fn call(s: &Service<MemoryStore>, name: &str, input: Value) -> Result<Value> {
    s.call(
        &Principal::direct(),
        serde_json::from_value(json!({"operation":name,"input":input})).unwrap(),
        context(),
    )
    .await
}
async fn create(s: &Service<MemoryStore>) -> String {
    call(s, "database_create", json!({"name":"notes"}))
        .await
        .unwrap();
    versioned_store::DatabaseStore::new(s.store.clone(), "notes")
        .unwrap()
        .read_head()
        .await
        .unwrap()
        .head
        .incarnation_id
}
async fn execute(s: &Service<MemoryStore>, _inc: &str, _key: &str, sql: &str) -> Result<Value> {
    call(s, "sql_execute", json!({"database":"notes","sql":sql})).await
}
async fn query(s: &Service<MemoryStore>, sql: &str) -> Value {
    call(s, "sql_execute", json!({"database":"notes","sql":sql}))
        .await
        .unwrap()
}
#[tokio::test]
async fn oversized_metadata_is_rejected_before_database_lookup() {
    let s = service();
    for fields in [
        json!({"metadata": "x".repeat(MAX_DATABASE_METADATA_BYTES)}),
        json!({"description": "x".repeat(MAX_DATABASE_DESCRIPTION_BYTES + 1)}),
    ] {
        for operation in ["database_create", "database_update"] {
            let mut input = fields.clone();
            input[if operation == "database_create" {
                "name"
            } else {
                "database"
            }] = json!("missing");
            let error = call(&s, operation, input).await.unwrap_err();
            assert_eq!(error.code, "INVALID_REQUEST");
            assert!(error.message.contains("exceeds"));
            assert_eq!(error.outcome, "not_committed");
        }
    }
    assert!(
        call(&s, "database_list", json!({})).await.unwrap()["databases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn maximum_user_metadata_leaves_head_space_for_writes_compaction_and_gc() {
    let s = service();
    let metadata = json!({"x": "a".repeat(MAX_DATABASE_METADATA_BYTES - 8)});
    let description = "\0".repeat(MAX_DATABASE_DESCRIPTION_BYTES);
    call(
        &s,
        "database_create",
        json!({"name":"notes", "metadata":metadata, "description":description}),
    )
    .await
    .unwrap();
    call(
        &s,
        "sql_execute",
        json!({"database":"notes", "sql":"CREATE TABLE t(x)"}),
    )
    .await
    .unwrap();
    call(
        &s,
        "database_compact",
        json!({"database":"notes", "budget_ms":5000, "max_segments":32}),
    )
    .await
    .unwrap();
    call(
        &s,
        "database_gc",
        json!({"database":"notes", "budget_ms":10000, "max_objects":1000}),
    )
    .await
    .unwrap();
    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(before["metadata"], metadata);
    assert_eq!(before["description"], description);
    let error = call(
        &s,
        "database_update",
        json!({"database":"notes", "metadata": "x".repeat(MAX_DATABASE_METADATA_BYTES)}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "INVALID_REQUEST");
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(after, before);
    call(
        &s,
        "database_update",
        json!({"database":"notes", "metadata":metadata}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn legacy_metadata_above_request_limit_remains_readable_and_can_be_reduced() {
    let s = service();
    create(&s).await;
    let db = versioned_store::DatabaseStore::new(s.store.clone(), "notes").unwrap();
    let base = db.read_head().await.unwrap();
    let mut legacy = base.head.clone();
    legacy.metadata = json!("a".repeat(MAX_DATABASE_METADATA_BYTES));
    db.publish_head(Some(&base), legacy.clone(), &context())
        .await
        .unwrap();
    let value = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(value["metadata"], legacy.metadata);
    call(
        &s,
        "database_update",
        json!({"database":"notes", "metadata":{}}),
    )
    .await
    .unwrap();
    let value = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(value["metadata"], json!({}));
}

#[tokio::test]
async fn math_results_preserve_protocol_types_and_reject_overflow() {
    let s = service();
    create(&s).await;
    let result = query(&s, "SELECT sqrt(9), pow(2, 3), sqrt(-1)").await;
    assert_eq!(
        result["results"][0]["rows"][0],
        json!([
            {"type":"real", "value":3.0},
            {"type":"real", "value":8.0},
            {"type":"null"}
        ])
    );
    let err = call(
        &s,
        "sql_execute",
        json!({"database":"notes", "sql":"SELECT exp(1000)"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SQL_ERROR");
    assert_eq!(err.message, "Non-finite SQL result");
    assert_eq!(err.outcome, "not_committed");
}
#[tokio::test]
async fn json_and_jsonb_persist_update_and_roundtrip_through_typed_parameters() {
    let s = service();
    create(&s).await;
    let document = r#"{"name":"你好 🌍","count":1,"items":[10,20]}"#;
    call(
        &s,
        "sql_execute",
        json!({"database":"notes", "statements":[
            {"sql":"CREATE TABLE docs(id INTEGER PRIMARY KEY, text_doc TEXT, binary_doc BLOB)"},
            {"sql":"INSERT INTO docs VALUES(1, json(?1), jsonb(?1))",
             "params":[{"type":"text", "value":document}]}
        ]}),
    )
    .await
    .unwrap();
    let stored = query(
        &s,
        "SELECT text_doc, binary_doc, json(binary_doc), json_valid(binary_doc, 8) FROM docs",
    )
    .await;
    let row = &stored["results"][0]["rows"][0];
    assert_eq!(row[0], json!({"type":"text", "value":document}));
    assert_eq!(row[1]["type"], "blob");
    assert!(!row[1]["value"].as_str().unwrap().is_empty());
    assert_eq!(row[2], row[0]);
    assert_eq!(row[3], json!({"type":"integer", "value":"1"}));
    let roundtrip = call(
        &s,
        "sql_execute",
        json!({"database":"notes", "sql":"SELECT json(?1), jsonb_extract(?1, '$.name')",
               "params":[row[1].clone()]}),
    )
    .await
    .unwrap();
    assert_eq!(
        roundtrip["results"][0]["rows"][0],
        json!([{"type":"text", "value":document}, {"type":"text", "value":"你好 🌍"}])
    );
    query(&s, "UPDATE docs SET text_doc=json_set(text_doc, '$.count', 2), binary_doc=jsonb_set(binary_doc, '$.count', 2)").await;
    let expected = json!([
        {"type":"integer", "value":"2"},
        {"type":"integer", "value":"2"}
    ]);
    let read_counts =
        "SELECT text_doc ->> '$.count', jsonb_extract(binary_doc, '$.count') FROM docs";
    assert_eq!(
        query(&s, read_counts).await["results"][0]["rows"][0],
        expected
    );

    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    let err = call(
        &s,
        "sql_execute",
        json!({"database":"notes", "statements":[
            {"sql":"UPDATE docs SET text_doc=json_set(text_doc, '$.count', 99), binary_doc=jsonb_set(binary_doc, '$.count', 99)"},
            {"sql":"SELECT jsonb('{invalid')"}
        ]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SQL_ERROR");
    assert!(err.message.contains("malformed JSON"), "{}", err.message);
    assert_eq!(err.outcome, "not_committed");
    assert_eq!(
        query(&s, read_counts).await["results"][0]["rows"][0],
        expected
    );
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(after["data_version"], before["data_version"]);
}

#[tokio::test]
async fn json_queries_work_with_read_only_permissions_and_reject_invalid_inputs() {
    let s = service();
    create(&s).await;
    let reader = Principal {
        id: "reader".into(),
        databases: vec!["notes".into()],
        scopes: vec!["data:read".into()],
    };
    for function in ["json_each", "jsonb_each", "json_tree", "jsonb_tree"] {
        let op = serde_json::from_value(json!({"operation":"sql_execute", "input":{
            "database":"notes", "sql":format!(
                "SELECT atom FROM {function}(jsonb('[10,20]')) WHERE type='integer' ORDER BY atom"
            )
        }}))
        .unwrap();
        let result = s.call(&reader, op, context()).await.unwrap();
        assert_eq!(
            result["results"][0]["rows"],
            json!([
                [{"type":"integer", "value":"10"}],
                [{"type":"integer", "value":"20"}]
            ]),
            "{function}"
        );
    }
    for sql in [
        "SELECT json('{invalid')",
        "SELECT jsonb('{invalid')",
        "SELECT json_extract('{}', 'invalid path')",
        "SELECT jsonb_extract(x'FF', '$')",
    ] {
        let err = call(&s, "sql_execute", json!({"database":"notes", "sql":sql}))
            .await
            .unwrap_err();
        assert_eq!(err.code, "SQL_ERROR", "{sql}");
        assert_eq!(err.outcome, "not_committed", "{sql}");
    }
    let valid = query(&s, "SELECT json_valid('{invalid'), json_valid(x'FF', 8)").await;
    assert_eq!(
        valid["results"][0]["rows"][0],
        json!([
            {"type":"integer", "value":"0"}, {"type":"integer", "value":"0"}
        ])
    );
}

#[tokio::test]
async fn missing_database_errors_identify_the_database_across_operations() {
    let s = service();
    for database in ["test3", "笔记"] {
        for (operation, mut input) in [
            ("database_get", json!({})),
            ("database_update", json!({"description":"updated"})),
            ("database_delete", json!({})),
            ("sql_execute", json!({"sql":"SELECT 1"})),
            ("snapshot_list", json!({})),
        ] {
            input["database"] = json!(database);
            let err = call(&s, operation, input).await.unwrap_err();
            assert_eq!(err.code, "DATABASE_NOT_FOUND", "{operation}");
            assert_eq!(err.message, format!("Database '{database}' not found"));
            assert!(!err.retryable);
            assert_eq!(err.outcome, "not_committed");
        }
    }
    let mut principal = Principal::direct();
    principal.scopes.clear();
    let err = s
        .call(
            &principal,
            Operation::DatabaseGet(Database {
                database: "test3".into(),
            }),
            context(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "FORBIDDEN");
}

#[tokio::test]
async fn invalid_head_diagnostics_identify_schema_failures_without_stored_values() {
    use object_store_api::{BlobStore, CasOutcome};
    let s = service();
    create(&s).await;
    let key = "databases/notes/head.json";
    let original = s.store.read_current(key).await.unwrap();
    let original: Value = serde_json::from_slice(&original.body).unwrap();
    let secret = "stored-secret-value";
    let mut cases = Vec::new();
    for field in [
        "restore",
        "restore_result",
        "unsafe field\nname",
        "restore`injected",
        "restore`, expected injected",
    ] {
        let mut head = original.clone();
        head[field] = json!({"token": secret});
        let field = if field.contains([' ', '`']) {
            "<redacted>"
        } else {
            field
        };
        cases.push((
            serde_json::to_vec(&head).unwrap(),
            format!("unknown field '{field}'; stored HEAD may be incompatible with this version"),
        ));
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("manifest_ref");
    cases.push((
        serde_json::to_vec(&missing).unwrap(),
        "missing field 'manifest_ref'; stored HEAD may be incompatible with this version".into(),
    ));
    for field in ["revision", "snapshot_count"] {
        let mut head = original.clone();
        head[field] = json!(secret);
        cases.push((
            serde_json::to_vec(&head).unwrap(),
            "schema mismatch: invalid field type or value".into(),
        ));
    }
    let mut invalid = original.clone();
    invalid["status"] = json!(secret);
    cases.push((
        serde_json::to_vec(&invalid).unwrap(),
        "metadata values violate HEAD constraints".into(),
    ));
    cases.push((
        b"{invalid JSON stored-secret-value".to_vec(),
        "invalid JSON".into(),
    ));
    cases.push((
        vec![b' '; storage_format::MAX_METADATA_BYTES + 1],
        "metadata exceeds the size limit".into(),
    ));

    for (body, reason) in cases {
        let current = s.store.read_current(key).await.unwrap();
        assert!(matches!(
            s.store.compare_and_swap(key, &current.revision, body).await,
            CasOutcome::Applied { .. }
        ));
        for operation in ["database_get", "database_list", "snapshot_list"] {
            let input = if operation == "database_list" {
                json!({})
            } else {
                json!({"database":"notes"})
            };
            let err = call(&s, operation, input).await.unwrap_err();
            assert_eq!(err.code, "STORAGE_CORRUPT", "{operation}");
            assert_eq!(
                err.message,
                format!("Database 'notes' HEAD validation failed: {reason}")
            );
            assert!(!err.retryable);
            assert_eq!(err.outcome, "not_committed");
            assert!(!err.message.contains(secret));
            assert!(!err.message.contains(key));
            assert!(!err.message.contains('\n'));
        }
    }
    let current = s.store.read_current(key).await.unwrap();
    let mut unsupported = original;
    unsupported["storage_layout_version"] = json!(2);
    assert!(matches!(
        s.store
            .compare_and_swap(
                key,
                &current.revision,
                serde_json::to_vec(&unsupported).unwrap()
            )
            .await,
        CasOutcome::Applied { .. }
    ));
    let err = call(&s, "snapshot_list", json!({"database":"notes"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "UNSUPPORTED_LAYOUT");
}

#[tokio::test]
async fn missing_manifest_is_corruption_instead_of_a_missing_database() {
    use object_store_api::{BlobStore, DeleteOutcome};
    let s = service();
    create(&s).await;
    let db = versioned_store::DatabaseStore::new(s.store.clone(), "notes").unwrap();
    let head = db.read_head().await.unwrap().head;
    let key = db.physical(&head.manifest_ref.relative_key().unwrap());
    assert!(matches!(s.store.delete(&key).await, DeleteOutcome::Deleted));
    // A fresh service has no metadata retained from the confirmed creation PUT.
    let s = Service::new(s.store.clone(), "test".into(), s.options.clone()).unwrap();
    for operation in ["database_get", "database_list", "sql_execute"] {
        let input = match operation {
            "database_list" => json!({}),
            "sql_execute" => json!({"database":"notes","sql":"SELECT 1"}),
            _ => json!({"database":"notes"}),
        };
        let err = call(&s, operation, input).await.unwrap_err();
        assert_eq!(err.code, "STORAGE_CORRUPT", "{operation}");
        assert_eq!(
            err.message,
            "Database 'notes' storage is incomplete: a referenced object is missing"
        );
        assert!(!err.retryable);
        assert_eq!(err.outcome, "not_committed");
        assert!(!err.message.contains(&key));
    }
}

#[tokio::test]
async fn list_skips_orphaned_objects_and_create_accepts_a_missing_head() {
    use object_store_api::{BlobStore, DeleteOutcome};
    let s = service();
    create(&s).await;
    assert!(matches!(
        s.store.delete("databases/notes/head.json").await,
        DeleteOutcome::Deleted
    ));
    let listed = call(&s, "database_list", json!({})).await.unwrap();
    assert_eq!(listed["databases"], json!([]));
    create(&s).await;
    let meta = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(meta["name"], "notes");
}

#[tokio::test]
async fn object_etag_detects_head_changes_without_revision_or_nonce_changes() {
    use object_store_api::{BlobStore, CasOutcome};
    let s = service();
    create(&s).await;
    let key = "databases/notes/head.json";
    let stored = s.store.read_current(key).await.unwrap();
    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(before["etag"], stored.etag);
    let listed = call(&s, "database_list", json!({})).await.unwrap();
    assert_eq!(listed["databases"][0]["etag"], stored.etag);
    let mut head: Value = serde_json::from_slice(&stored.body).unwrap();
    head["description"] = json!("external change");
    assert!(matches!(
        s.store
            .compare_and_swap(key, &stored.revision, serde_json::to_vec(&head).unwrap())
            .await,
        CasOutcome::Applied { .. }
    ));
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(before["revision"], after["revision"]);
    assert_ne!(before["etag"], after["etag"]);
    for operation in ["database_update", "database_delete"] {
        assert_eq!(
            call(
                &s,
                operation,
                json!({"database":"notes", "if_match":before["etag"]})
            )
            .await
            .unwrap_err()
            .code,
            "METADATA_PRECONDITION_FAILED"
        );
    }
    call(
        &s,
        "database_update",
        json!({"database":"notes", "description":"accepted", "if_match":after["etag"]}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn management_updates_and_deletes_accept_optional_etags() {
    for conditional in [false, true] {
        let s = service();
        let inc = create(&s).await;
        execute(&s, &inc, "", "CREATE TABLE t(x)").await.unwrap();
        let before = call(&s, "database_get", json!({"database":"notes"}))
            .await
            .unwrap();
        let mut update = json!({"database":"notes","description":"updated","metadata":{"key":"value"},"snapshot_retention_days":7});
        if conditional {
            update["if_match"] = before["etag"].clone();
        }
        let result = call(&s, "database_update", update).await.unwrap();
        assert_eq!(result["committed"], true);
        let after = call(&s, "database_get", json!({"database":"notes"}))
            .await
            .unwrap();
        assert_eq!(after["description"], "updated");
        assert_eq!(after["metadata"], json!({"key":"value"}));
        assert_eq!(after["snapshot_retention_days"], 7);
        assert_eq!(after["data_version"], before["data_version"]);
        assert_ne!(after["etag"], before["etag"]);
        assert!(query(&s, "SELECT * FROM t").await["results"][0]["rows"]
            .as_array()
            .unwrap()
            .is_empty());
        for name in ["database_update", "database_delete"] {
            assert_eq!(
                call(
                    &s,
                    name,
                    json!({"database":"notes","if_match":before["etag"]})
                )
                .await
                .unwrap_err()
                .code,
                "METADATA_PRECONDITION_FAILED"
            );
        }
        assert_eq!(
            call(&s, "database_get", json!({"database":"notes"}))
                .await
                .unwrap(),
            after
        );
        let mut delete = json!({"database":"notes"});
        if conditional {
            delete["if_match"] = after["etag"].clone();
        }
        let result = call(&s, "database_delete", delete).await.unwrap();
        assert_eq!(result["status"], "deleted");
        assert_eq!(
            call(
                &s,
                "sql_execute",
                json!({"database":"notes","sql":"SELECT 1"})
            )
            .await
            .unwrap_err()
            .code,
            "DATABASE_DELETED"
        );
    }
}

#[tokio::test]
async fn repeated_writes_execute_again_and_history_survives_deletion() {
    let s = service();
    let inc = create(&s).await;
    execute(
        &s,
        &inc,
        "ddl",
        "CREATE TABLE counter(id INTEGER PRIMARY KEY, value INTEGER)",
    )
    .await
    .unwrap();
    execute(&s, &inc, "insert", "INSERT INTO counter VALUES(1,0)")
        .await
        .unwrap();
    let a = execute(
        &s,
        &inc,
        "increment",
        "UPDATE counter SET value=value+1 RETURNING value",
    )
    .await
    .unwrap();
    let b = execute(
        &s,
        &inc,
        "increment",
        "UPDATE counter SET value=value+1 RETURNING value",
    )
    .await
    .unwrap();
    assert_ne!(a["data_version"], b["data_version"]);
    assert_eq!(
        query(&s, "SELECT value FROM counter").await["results"][0]["rows"][0][0]["value"],
        "2"
    );
    let snapshot=call(&s,"sql_execute",json!({"database":"notes","sql":"SELECT value FROM counter","consistency":"snapshot","snapshot_id":format!("{inc}:3")})).await.unwrap();
    assert_eq!(snapshot["results"][0]["rows"][0][0]["value"], "0");
    assert_eq!(snapshot["data_version"], "3");
    let m = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    call(
        &s,
        "database_delete",
        json!({"database":"notes","if_match":m["etag"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &s,
            "sql_execute",
            json!({"database":"notes","sql":"SELECT 1"})
        )
        .await
        .unwrap_err()
        .code,
        "DATABASE_DELETED"
    );
    assert_eq!(
        execute(&s, &inc, "late", "UPDATE counter SET value=value+1")
            .await
            .unwrap_err()
            .code,
        "DATABASE_DELETED"
    );
}
#[tokio::test]
async fn compact_and_online_gc_preserve_live_and_historical_data() {
    let s = service();
    let inc = create(&s).await;
    execute(&s, &inc, "ddl", "CREATE TABLE t(x)").await.unwrap();
    execute(&s, &inc, "insert", "INSERT INTO t VALUES(1)")
        .await
        .unwrap();
    let before = query(&s, "SELECT x FROM t").await;
    let compact = call(
        &s,
        "database_compact",
        json!({"database":"notes","budget_ms":5000,"max_segments":32}),
    )
    .await
    .unwrap();
    assert_eq!(compact["complete"], true);
    assert!(compact.get("resume_required").is_none());
    for n in 0..100 {
        let r = call(
            &s,
            "database_gc",
            json!({"database":"notes","budget_ms":5000,"max_objects":1000}),
        )
        .await
        .unwrap();
        if r["complete"] == true {
            break;
        }
        assert!(n < 99, "GC did not converge");
    }
    assert_eq!(
        query(&s, "SELECT x FROM t").await["results"],
        before["results"]
    );
    call(&s,"sql_execute",json!({"database":"notes","sql":"SELECT x FROM t","consistency":"snapshot","snapshot_id":format!("{inc}:3")})).await.unwrap();
}
#[tokio::test]
async fn readonly_and_sql_action_permissions_are_enforced() {
    let s = service();
    let inc = create(&s).await;
    execute(&s, &inc, "ddl", "CREATE TABLE t(x)").await.unwrap();
    assert!(call(
        &s,
        "sql_execute",
        json!({"database":"notes","sql":"INSERT INTO t VALUES(1)"})
    )
    .await
    .is_ok());
    let p = Principal {
        id: "reader".into(),
        databases: vec!["notes".into()],
        scopes: vec!["data:read".into()],
    };
    let op: Operation = serde_json::from_value(
        json!({"operation":"sql_execute","input":{"database":"notes","sql":"DROP TABLE t"}}),
    )
    .unwrap();
    assert_eq!(
        s.call(&p, op, context()).await.unwrap_err().code,
        "FORBIDDEN"
    );
}
#[tokio::test]
async fn file_restore_publishes_a_queryable_database() {
    let s = service();
    let file = tempfile::NamedTempFile::new().unwrap();
    let c = rusqlite::Connection::open(file.path()).unwrap();
    c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(42)")
        .unwrap();
    drop(c);
    let result = s
        .restore_file(
            &Principal::direct(),
            "notes",
            file.path().into(),
            RequestContext::unbounded(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    assert_eq!(result["committed"], true);
    assert_eq!(
        result["sha256"],
        storage_format::digest(&std::fs::read(file.path()).unwrap())
    );
    assert_eq!(
        query(&s, "SELECT x FROM t").await["results"][0]["rows"][0][0]["value"],
        "42"
    );
    assert_eq!(
        s.restore_file(
            &Principal::direct(),
            "notes",
            file.path().into(),
            RequestContext::unbounded(),
            Arc::new(|_| {})
        )
        .await
        .unwrap_err()
        .code,
        "DATABASE_EXISTS"
    );
}

#[tokio::test]
async fn unified_execute_reads_writes_and_rejects_duplicate_ddl() {
    let s = service();
    let inc = create(&s).await;
    execute(&s, &inc, "ddl", "CREATE TABLE t(x UNIQUE)")
        .await
        .unwrap();
    let duplicate = execute(&s, &inc, "ddl", "CREATE TABLE t(x UNIQUE)")
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, "SQL_ERROR");
    assert!(
        duplicate.message.contains("already exists"),
        "{}",
        duplicate.message
    );
    let inserted = execute(
        &s,
        &inc,
        "insert",
        "WITH input(x) AS (VALUES(7)) INSERT INTO t SELECT x FROM input RETURNING x",
    )
    .await
    .unwrap();
    assert_eq!(inserted["outcome"], "committed");
    assert_eq!(inserted["results"][0]["rows"][0][0]["value"], "7");
    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    for options in [
        json!({}),
        json!({"consistency":"at_least", "commit_token":inserted["commit_token"]}),
        json!({"consistency":"snapshot", "snapshot_id":format!("{inc}:3")}),
    ] {
        let mut input = json!({"database":"notes", "sql":"WITH c AS (SELECT x FROM t WHERE x=?) SELECT x FROM c", "params":[{"type":"integer","value":"7"}]});
        input
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let result = call(&s, "sql_execute", input).await.unwrap();
        assert_eq!(result["outcome"], "read_only");
        assert_eq!(result["data_version"], "3");
        assert_eq!(result["results"][0]["rows"][0][0]["value"], "7");
        assert!(result.get("committed").is_none());
    }
    let empty = query(&s, "SELECT x FROM t WHERE 0").await;
    assert_eq!(empty["results"][0]["rows"], json!([]));
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(before["etag"], after["etag"]);
    let read = call(
        &s,
        "sql_execute",
        json!({"database":"notes", "sql":"SELECT 1"}),
    )
    .await
    .unwrap();
    assert_eq!(read["outcome"], "read_only");
}

#[tokio::test]
async fn sql_errors_include_the_sqlite_diagnostic() {
    let s = service();
    let inc = create(&s).await;
    let missing = execute(
        &s,
        &inc,
        "missing",
        "SELECT * FROM ordersr ORDER BY id LIMIT 1",
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, "SQL_ERROR");
    assert_eq!(missing.message, "no such table: ordersr");
    assert!(!missing.retryable);
    assert_eq!(missing.outcome, "not_committed");
    execute(&s, &inc, "ddl", "CREATE TABLE t(x UNIQUE)")
        .await
        .unwrap();
    execute(&s, &inc, "seed", "INSERT INTO t VALUES(1)")
        .await
        .unwrap();
    let duplicate = execute(&s, &inc, "dup", "INSERT INTO t VALUES(1)")
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, "SQL_ERROR");
    assert_eq!(duplicate.message, "UNIQUE constraint failed: t.x");
    let syntax = execute(&s, &inc, "syntax", "SELEC 1").await.unwrap_err();
    assert_eq!(syntax.code, "SQL_ERROR");
    assert!(
        syntax.message.contains("syntax error"),
        "{}",
        syntax.message
    );
    let denied = execute(&s, &inc, "pragma", "PRAGMA user_version=1")
        .await
        .unwrap_err();
    assert_eq!(denied.code, "SQL_ERROR");
    assert_eq!(denied.message, "not authorized");
    let batch = execute(&s, &inc, "batch", "SELECT 1; SELECT 2")
        .await
        .unwrap_err();
    assert_eq!(batch.code, "SQL_ERROR");
    assert_eq!(batch.message, "Multiple statements provided");
}

#[tokio::test]
async fn unified_execute_rejects_invalid_writes_without_publishing() {
    let s = service();
    let inc = create(&s).await;
    execute(&s, &inc, "ddl", "CREATE TABLE t(x UNIQUE)")
        .await
        .unwrap();
    execute(&s, &inc, "seed", "INSERT INTO t VALUES(1)")
        .await
        .unwrap();
    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    for options in [
        json!({"max_rows":1}),
        json!({"snapshot_id":format!("{inc}:3")}),
        json!({"consistency":"snapshot", "snapshot_id":format!("{inc}:3")}),
        json!({"commit_token":before["commit_token"]}),
        json!({"consistency":"at_least", "commit_token":before["commit_token"]}),
    ] {
        let mut input = json!({"database":"notes", "sql":"INSERT INTO t VALUES(2)"});
        input
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        assert_eq!(
            call(&s, "sql_execute", input).await.unwrap_err().code,
            "INVALID_REQUEST"
        );
    }
    for sql in [
        "SELECT 1; INSERT INTO t VALUES(2)",
        "INSERT INTO t VALUES(2); SELECT 1",
        "PRAGMA user_version=1",
        "BEGIN",
        "INSERT INTO t VALUES(1)",
    ] {
        assert!(
            execute(&s, &inc, "invalid-sql", sql).await.is_err(),
            "{sql}"
        );
    }
    assert_eq!(
        call(
            &s,
            "sql_execute",
            json!({"database":"notes", "sql":"SELECT 1 UNION ALL SELECT 2", "max_rows":1})
        )
        .await
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
    let reader = Principal {
        id: "reader".into(),
        databases: vec!["notes".into()],
        scopes: vec!["data:read".into()],
    };
    for (sql, allowed) in [
        ("SELECT x FROM t", true),
        (
            "WITH c(x) AS (VALUES(2)) INSERT INTO t SELECT x FROM c RETURNING x",
            false,
        ),
    ] {
        let op = serde_json::from_value(
            json!({"operation":"sql_execute", "input":{"database":"notes", "sql":sql}}),
        )
        .unwrap();
        let result = s.call(&reader, op, context()).await;
        if allowed {
            assert_eq!(result.unwrap()["outcome"], "read_only");
        } else {
            assert_eq!(result.unwrap_err().code, "FORBIDDEN");
        }
    }
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(before["etag"], after["etag"]);
    assert!(serde_json::from_value::<Operation>(
        json!({"operation":"sql_query", "input":{"database":"notes", "sql":"SELECT 1"}})
    )
    .is_err());
}

#[tokio::test]
async fn returning_budget_failure_and_mixed_transactions_are_atomic() {
    let mut s = service();
    let inc = create(&s).await;
    let tx = call(&s, "sql_execute", json!({"database":"notes", "statements":[{"sql":"CREATE TABLE t(x)"},{"sql":"INSERT INTO t VALUES(1)"},{"sql":"SELECT x FROM t"}]})).await.unwrap();
    assert_eq!(tx["outcome"], "committed");
    assert_eq!(tx["results"][2]["rows"][0][0]["value"], "1");
    s.options.execution.max_result_rows = 1;
    assert_eq!(
        execute(
            &s,
            &inc,
            "too-many",
            "INSERT INTO t VALUES(2),(3) RETURNING x"
        )
        .await
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
    let read = query(&s, "SELECT x FROM t").await;
    assert_eq!(read["data_version"], tx["data_version"]);
    assert_eq!(read["results"][0]["rows"], tx["results"][2]["rows"]);
}

async fn delete_notes(s: &Service<MemoryStore>) {
    let m = call(s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    call(
        s,
        "database_delete",
        json!({"database":"notes","if_match":m["etag"]}),
    )
    .await
    .unwrap();
}
async fn gc_notes(s: &Service<MemoryStore>) {
    for _ in 0..100 {
        let r = call(
            s,
            "database_gc",
            json!({"database":"notes","budget_ms":5000,"max_objects":1000}),
        )
        .await
        .unwrap();
        if r["complete"] == true {
            return;
        }
    }
    panic!("GC did not converge");
}
#[tokio::test]
async fn recreation_resets_history_rejects_old_tokens_and_reclaims_old_objects() {
    use object_store_api::{BlobStore, ListRequest};
    let s = service();
    let inc = create(&s).await;
    let old = execute(&s, &inc, "", "CREATE TABLE old_table(x)")
        .await
        .unwrap();
    let old_meta = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    let db = versioned_store::DatabaseStore::new(s.store.clone(), "notes").unwrap();
    let old_head = db.read_head().await.unwrap();
    delete_notes(&s).await;
    let new_inc = create(&s).await;
    assert_ne!(inc, new_inc);
    assert!(db.read_head().await.unwrap().head.revision > old_head.head.revision);
    assert!(matches!(
        db.publish_head(Some(&old_head), old_head.head.clone(), &context())
            .await,
        Err(versioned_store::Error::Conflict)
    ));
    assert_eq!(
        call(
            &s,
            "database_update",
            json!({"database":"notes","if_match":old_meta["etag"],"description":"stale"})
        )
        .await
        .unwrap_err()
        .code,
        "METADATA_PRECONDITION_FAILED"
    );
    assert_eq!(call(&s, "sql_execute", json!({"database":"notes","sql":"SELECT 1","consistency":"at_least","commit_token":old["commit_token"]})).await.unwrap_err().code, "INVALID_REQUEST");
    assert_eq!(
        call(
            &s,
            "snapshot_get",
            json!({"database":"notes","snapshot_id":format!("{inc}:2")})
        )
        .await
        .unwrap_err()
        .code,
        "SNAPSHOT_EXPIRED"
    );
    let snapshots = call(&s, "snapshot_list", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(snapshots["snapshots"].as_array().unwrap().len(), 1);
    // The old name now resolves to the new database, without an identity parameter.
    execute(&s, &inc, "", "CREATE TABLE current_table(x)")
        .await
        .unwrap();
    gc_notes(&s).await;
    let old_objects = s
        .store
        .list(ListRequest {
            prefix: format!("databases/notes/incarnations/{inc}/"),
            delimiter: None,
            cursor: None,
            limit: 1000,
        })
        .await
        .unwrap();
    assert!(old_objects.objects.is_empty());
    assert!(query(&s, "SELECT * FROM current_table").await["results"].is_array());
}
#[test]
fn unknown_outcome_does_not_advise_repeating_a_write() {
    let e = store_error(versioned_store::Error::Unknown);
    assert_eq!(e.outcome, "unknown");
    assert!(!e.retryable);
}

#[tokio::test]
async fn recreation_preserves_old_reader_drain_window() {
    let mut s = service();
    s.options.max_read_ms = 10000;
    create(&s).await;
    delete_notes(&s).await;
    s.options.max_read_ms = 20;
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch("CREATE TABLE restored(x)").unwrap();
    drop(conn);
    s.restore_file(
        &Principal::direct(),
        "notes",
        file.path().into(),
        RequestContext::unbounded(),
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let db = versioned_store::DatabaseStore::new(s.store.clone(), "notes").unwrap();
    assert!(db.read_head().await.unwrap().head.safety_policy.drain_ms >= 11000);
}

#[tokio::test]
async fn database_name_is_the_only_immutable_identity() {
    let s = service();
    let name = "中文 $Notes-1";
    let created = call(&s, "database_create", json!({"name":name}))
        .await
        .unwrap();
    assert_eq!(created["name"], name);
    assert!(created.get("database").is_none());
    let meta = call(&s, "database_get", json!({"database":name}))
        .await
        .unwrap();
    assert_eq!(meta["name"], name);
    assert!(meta.get("database").is_none());
    let listed = call(&s, "database_list", json!({})).await.unwrap();
    assert_eq!(listed["databases"][0]["name"], name);
    assert!(listed["databases"][0].get("database").is_none());
    assert!(call(&s, "database_create", json!({"name":name}))
        .await
        .is_err());
    assert!(serde_json::from_value::<Create>(json!({"database":"old","name":name})).is_err());
    assert!(serde_json::from_value::<Update>(
        json!({"database":name,"if_match":meta["etag"],"name":"renamed"})
    )
    .is_err());
    for invalid in [
        "",
        "bad/name",
        "bad.name",
        "bad\\name",
        "trailing ",
        "😀",
        "\0",
    ] {
        assert_eq!(
            call(&s, "database_create", json!({"name":invalid}))
                .await
                .unwrap_err()
                .code,
            "INVALID_REQUEST"
        );
    }
    assert_eq!(
        call(&s, "database_create", json!({"name":"中".repeat(65)}))
            .await
            .unwrap_err()
            .code,
        "INVALID_REQUEST"
    );
    call(&s, "database_create", json!({"name":"中".repeat(64)}))
        .await
        .unwrap();
}

#[tokio::test]
async fn execute_batches_bind_parameters_and_rollback_all_changes() {
    let mut s = service();
    create(&s).await;
    let result = call(
        &s,
        "sql_execute",
        json!({"database":"notes","statements":[
            {"sql":"CREATE TABLE batch_values(x INTEGER UNIQUE)"},
            {"sql":"INSERT INTO batch_values VALUES(?)","params":[{"type":"integer","value":"42"}]},
            {"sql":"SELECT x FROM batch_values"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(result["data_version"], "2");
    assert_eq!(result["results"].as_array().unwrap().len(), 3);
    assert_eq!(result["results"][2]["rows"][0][0]["value"], "42");
    let before = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    for batch in [
        json!([{"sql":"INSERT INTO batch_values VALUES(43)"},{"sql":"INSERT INTO batch_values VALUES(42)"}]),
        json!([{"sql":"CREATE TABLE rolled_back(x)"},{"sql":"SELECT * FROM missing_table"}]),
        json!([{"sql":"INSERT INTO batch_values VALUES(43)"},{"sql":"COMMIT"}]),
    ] {
        assert!(call(
            &s,
            "sql_execute",
            json!({"database":"notes","statements":batch})
        )
        .await
        .is_err());
        let after = call(&s, "database_get", json!({"database":"notes"}))
            .await
            .unwrap();
        assert_eq!(after["etag"], before["etag"]);
    }
    assert_eq!(
        query(&s, "SELECT count(*) FROM batch_values").await["results"][0]["rows"][0][0]["value"],
        "1"
    );
    assert_eq!(
        query(
            &s,
            "SELECT count(*) FROM sqlite_schema WHERE name='rolled_back'"
        )
        .await["results"][0]["rows"][0][0]["value"],
        "0"
    );
    s.options.execution.max_result_rows = 1;
    assert_eq!(
        call(
            &s,
            "sql_execute",
            json!({"database":"notes","statements":[
                {"sql":"INSERT INTO batch_values VALUES(43)"},{"sql":"SELECT x FROM batch_values"}
            ]})
        )
        .await
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
    let after = call(&s, "database_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(after["etag"], before["etag"]);
    let op = serde_json::from_value(json!({"operation":"sql_execute","input":{"database":"notes","statements":[{"sql":"SELECT 1"}]}})).unwrap();
    let reader = Principal {
        id: "reader".into(),
        databases: vec!["notes".into()],
        scopes: vec!["data:read".into()],
    };
    assert_eq!(
        s.call(&reader, op, context()).await.unwrap_err().code,
        "FORBIDDEN"
    );
}

#[tokio::test]
async fn schema_filters_match_tables_and_object_types() {
    let s = service();
    let inc = create(&s).await;
    for sql in [
        "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT UNIQUE)",
        "CREATE TABLE other(id INTEGER)",
        "CREATE INDEX notes_body ON notes(body)",
        "CREATE VIEW notes_view AS SELECT * FROM notes",
        "CREATE TRIGGER notes_insert AFTER INSERT ON notes BEGIN SELECT 1; END",
        "CREATE TABLE \"odd'%_name\"(id INTEGER)",
    ] {
        execute(&s, &inc, "", sql).await.unwrap();
    }
    let names = |v: &Value| -> Vec<String> {
        v["results"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[1]["value"].as_str().unwrap().to_owned())
            .collect()
    };
    let all = call(&s, "schema_get", json!({"database":"notes"}))
        .await
        .unwrap();
    assert_eq!(
        names(&all),
        [
            "notes_body",
            "notes",
            "odd'%_name",
            "other",
            "notes_insert",
            "notes_view"
        ]
    );
    assert_eq!(all["truncated"], false);
    let filtered = call(
        &s,
        "schema_get",
        json!({"database":"notes","table":"notes"}),
    )
    .await
    .unwrap();
    assert_eq!(names(&filtered), ["notes_body", "notes", "notes_insert"]);
    assert_eq!(filtered["data_version"], all["data_version"]);
    for (kind, expected) in [
        ("table", vec!["notes", "odd'%_name", "other"]),
        ("index", vec!["notes_body"]),
        ("view", vec!["notes_view"]),
        ("trigger", vec!["notes_insert"]),
    ] {
        let v = call(&s, "schema_get", json!({"database":"notes","type":kind}))
            .await
            .unwrap();
        assert_eq!(names(&v), expected);
    }
    for (table, kind, expected) in [
        ("notes", "index", vec!["notes_body"]),
        ("notes", "view", vec![]),
        ("odd'%_name", "table", vec!["odd'%_name"]),
    ] {
        let v = call(
            &s,
            "schema_get",
            json!({"database":"notes","table":table,"type":kind}),
        )
        .await
        .unwrap();
        assert_eq!(names(&v), expected);
    }
    for table in [
        "missing",
        "Notes",
        "%",
        "notes' OR 1=1 --",
        "notes_view",
        "sqlite_schema",
        "sqlite_autoindex_notes_1",
    ] {
        for kind in [
            None,
            Some("table"),
            Some("index"),
            Some("view"),
            Some("trigger"),
        ] {
            let mut input = json!({"database":"notes","table":table});
            if let Some(kind) = kind {
                input["type"] = json!(kind);
            }
            let e = call(&s, "schema_get", input).await.unwrap_err();
            assert_eq!(e.code, "TABLE_NOT_FOUND");
            assert_eq!(
                e.message,
                format!("Table '{table}' not found in database 'notes'")
            );
            assert!(!e.retryable);
            assert_eq!(e.outcome, "not_committed");
        }
    }
    let e = call(
        &s,
        "schema_get",
        json!({"database":"missing","table":"missing"}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "DATABASE_NOT_FOUND");
    // Direct Rust callers receive the same validation before a database lookup.
    let e = s
        .call(
            &Principal::direct(),
            Operation::SchemaGet(SchemaGet {
                database: "missing".into(),
                table: Some(String::new()),
                r#type: None,
            }),
            context(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "INVALID_REQUEST");
}

#[tokio::test]
async fn schema_existence_check_preserves_successful_result_budgets() {
    let s = Service::new(
        Arc::new(MemoryStore::default()),
        "test".into(),
        ServiceOptions {
            execution: engine_sqlite::ExecutionLimits {
                max_result_rows: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let inc = create(&s).await;
    execute(&s, &inc, "", "CREATE TABLE notes(id INTEGER)")
        .await
        .unwrap();
    execute(&s, &inc, "", "CREATE INDEX notes_id ON notes(id)")
        .await
        .unwrap();
    let v = call(
        &s,
        "schema_get",
        json!({"database":"notes","table":"notes","type":"index"}),
    )
    .await
    .unwrap();
    assert_eq!(v["results"][0]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(v["results"][0]["rows"][0][1]["value"], "notes_id");
    let v = call(
        &s,
        "schema_get",
        json!({"database":"notes","table":"notes","type":"trigger"}),
    )
    .await
    .unwrap();
    assert_eq!(v["results"][0]["rows"], json!([]));
    let v = call(
        &s,
        "schema_get",
        json!({"database":"notes","type":"trigger"}),
    )
    .await
    .unwrap();
    assert_eq!(v["results"][0]["rows"], json!([]));
    execute(
        &s,
        &inc,
        "",
        "CREATE VIEW notes_view AS SELECT * FROM notes",
    )
    .await
    .unwrap();
    let e = call(
        &s,
        "schema_get",
        json!({"database":"notes","table":"notes_view"}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "TABLE_NOT_FOUND");
}
