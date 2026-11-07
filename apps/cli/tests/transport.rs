use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use database_service::{Service, ServiceOptions};
use object_store_test_support::MemoryStore;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
const TOKEN: &str = "test-token-at-least-sixteen-bytes";
fn app() -> Router {
    app_with_service().0
}
fn app_with_service() -> (Router, Arc<Service<MemoryStore>>) {
    let service = Arc::new(
        Service::new(
            Arc::new(MemoryStore::default()),
            "transport-test".into(),
            ServiceOptions::default(),
        )
        .unwrap(),
    );
    let router = ponydb::server::router(ponydb::server::AppState {
        service: service.clone(),
        auth: ponydb::server::AuthFile {
            principals: vec![ponydb::server::Identity {
                id: "test".into(),
                token: TOKEN.into(),
                databases: vec!["*".into()],
                scopes: vec!["*".into()],
            }],
        },
        timeout: Duration::from_secs(10),
        mcp_enabled: true,
    });
    (router, service)
}
async fn send(
    app: &Router,
    method: &str,
    path: &str,
    value: Value,
    extra: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn meta() -> Value {
    json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"contract-test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}})
}

#[tokio::test]
async fn http_and_mcp_reject_oversized_user_metadata() {
    let app = app();
    for fields in [
        json!({"metadata": "a".repeat(protocol::MAX_DATABASE_METADATA_BYTES)}),
        json!({"description": "a".repeat(protocol::MAX_DATABASE_DESCRIPTION_BYTES + 1)}),
    ] {
        for (method, path, name, identity) in [
            ("POST", "/v1/databases", "database_create", "name"),
            (
                "PATCH",
                "/v1/databases/missing",
                "database_update",
                "database",
            ),
        ] {
            let mut input = fields.clone();
            input[identity] = json!("missing");
            let (status, value) = send(&app, method, path, input.clone(), &[]).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(value["error"]["code"], "INVALID_REQUEST");
            assert!(value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("exceeds"));
            let (_, mcp) = send(&app, "POST", "/mcp", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":input,"_meta":meta()}}), &[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name",name),("accept","application/json, text/event-stream")]).await;
            assert_eq!(mcp["result"]["isError"], true, "{mcp}");
            assert_eq!(
                mcp["result"]["structuredContent"]["error"]["code"],
                "INVALID_REQUEST"
            );
            assert_eq!(
                mcp["result"]["structuredContent"]["error"]["outcome"],
                "not_committed"
            );
        }
    }
}

#[tokio::test]
async fn snapshot_download_is_sqlite_and_errors_are_json() {
    let (app, _) = app_with_service();
    send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    let (_, list) = send(&app, "GET", "/v1/databases/notes/snapshots", json!({}), &[]).await;
    let id = list["snapshots"][0]["snapshot_id"].as_str().unwrap();
    let path = format!("/v1/databases/notes/snapshots/{id}/export");
    let unauthorized = app
        .clone()
        .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/vnd.sqlite3"
    );
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"snapshot.sqlite\""
    );
    let len: usize = response.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(bytes.len(), len);
    assert!(bytes.starts_with(b"SQLite format 3\0"));
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &bytes).unwrap();
    let db = rusqlite::Connection::open(file.path()).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let (status, error) = send(
        &app,
        "GET",
        "/v1/databases/notes/snapshots/missing/export",
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(error["error"]["code"], "SNAPSHOT_EXPIRED");
}
#[tokio::test]
async fn http_etag_header_matches_the_stored_head_and_json() {
    use object_store_api::BlobStore;
    let (app, service) = app_with_service();
    let (status, _) = send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    assert_eq!(status, StatusCode::CREATED);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/databases/notes")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["etag"], etag);
    assert_eq!(
        service
            .store
            .read_current("databases/notes/head.json")
            .await
            .unwrap()
            .etag,
        etag
    );
}

#[tokio::test]
async fn http_mcp_cli_repeated_calls_each_execute_sql() {
    let (app, service) = app_with_service();
    let (status, create) = send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    assert_eq!(status, StatusCode::CREATED, "{create}");
    for (_key, sql) in [
        ("ddl", "CREATE TABLE t(x)"),
        ("insert", "INSERT INTO t VALUES(0)"),
    ] {
        let (s, r) = send(
            &app,
            "POST",
            "/v1/databases/notes/execute",
            json!({"sql":sql}),
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{r}");
    }
    let input = json!({"database":"notes","sql":"UPDATE t SET x=x+1 RETURNING x"});
    let mut body = input.clone();
    body.as_object_mut().unwrap().remove("database");
    let (status, http) = send(&app, "POST", "/v1/databases/notes/execute", body, &[]).await;
    assert_eq!(status, StatusCode::OK, "{http}");
    let rpc = json!({"jsonrpc":"2.0","id":97,"method":"tools/call","params":{"name":"sql_execute","arguments":input,"_meta":meta()}});
    let (status, mcp) = send(
        &app,
        "POST",
        "/mcp",
        rpc,
        &[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "sql_execute"),
            ("accept", "application/json, text/event-stream"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mcp}");
    assert_eq!(http["results"][0]["rows"][0][0]["value"], "1");
    assert_eq!(
        mcp["result"]["structuredContent"]["result"]["results"][0]["rows"][0][0]["value"],
        "2"
    );
    // The native CLI calls the service directly with the S3 credential holder principal.
    let cli = |input: Value| {
        let service = service.clone();
        async move {
            let op: protocol::Operation =
                serde_json::from_value(json!({"operation":"sql_execute","input":input})).unwrap();
            service
                .call(
                    &protocol::Principal::direct(),
                    op,
                    request_context::RequestContext::new(Duration::from_secs(10)),
                )
                .await
        }
    };
    assert_eq!(
        cli(input.clone()).await.unwrap()["results"][0]["rows"][0][0]["value"],
        "3"
    );
    let mut changed = input.clone();
    changed["sql"] = json!("UPDATE t SET x=x+2 RETURNING x");
    assert_eq!(
        cli(changed).await.unwrap()["results"][0]["rows"][0][0]["value"],
        "5"
    );
    let (_, read) = send(
        &app,
        "POST",
        "/v1/databases/notes/execute",
        json!({"sql":"SELECT x FROM t"}),
        &[],
    )
    .await;
    assert_eq!(read["results"][0]["rows"][0][0]["value"], "5");
}
#[tokio::test]
async fn mcp_metadata_discovery_and_origin_checks() {
    let app = app();
    let message = json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":meta()}});
    let headers = [
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "server/discover"),
        ("accept", "application/json, text/event-stream"),
    ];
    let (status, response) = send(&app, "POST", "/mcp", message.clone(), &headers).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
    let mut bad = headers.to_vec();
    bad.push(("origin", "https://attacker.invalid"));
    assert_eq!(
        send(&app, "POST", "/mcp", message, &bad).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(protocol::tools().len(), 11);
}

#[tokio::test]
async fn unified_execute_query_transports_and_removed_route() {
    let app = app();
    send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    let input = json!({"database":"notes", "sql":"SELECT 42 AS answer"});
    let (_, read) = send(
        &app,
        "POST",
        "/v1/databases/notes/execute",
        json!({"sql":"SELECT 42 AS answer"}),
        &[],
    )
    .await;
    assert_eq!(read["outcome"], "read_only");
    let (_, rpc) = send(&app, "POST", "/mcp", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sql_execute","arguments":input,"_meta":meta()}}), &[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name","sql_execute"),("accept","application/json, text/event-stream")]).await;
    assert_eq!(rpc["result"]["structuredContent"]["result"], read);
    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/databases/notes/query",
            json!({"sql":"SELECT 1"}),
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(!protocol::tools()
        .iter()
        .any(|tool| tool["name"] == "sql_query"));
    let tool = protocol::tools()
        .into_iter()
        .find(|tool| tool["name"] == "sql_execute")
        .unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    assert_eq!(tool["annotations"]["destructiveHint"], true);
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(!required.contains(&json!("idempotency_key")));
    assert!(!required.contains(&json!("expected_incarnation_id")));

    let req = Request::builder()
        .method("POST")
        .uri("/v1/databases/notes/execute")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("accept", "application/x-ndjson")
        .body(Body::from(json!({"sql":"SELECT 42 AS answer"}).to_string()))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/x-ndjson");
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let lines: Vec<Value> = std::str::from_utf8(&body)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "result_start");
    assert_eq!(lines[1]["rows"], read["results"][0]["rows"]);
    assert_eq!(lines[2]["outcome"], "read_only");
}

#[tokio::test]
async fn execute_batches_over_http_and_mcp_and_remove_transaction_route() {
    let app = app();
    send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    let (status, result) = send(
        &app,
        "POST",
        "/v1/databases/notes/execute",
        json!({"statements":[
            {"sql":"CREATE TABLE t(x)"},{"sql":"INSERT INTO t VALUES(42)"},{"sql":"SELECT x FROM t"}
        ]}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["results"][2]["rows"][0][0]["value"], "42");
    let (status, rpc) = send(&app,"POST","/mcp",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sql_execute","arguments":{"database":"notes","statements":[{"sql":"UPDATE t SET x=x+1"},{"sql":"SELECT x FROM t"}]},"_meta":meta()}}),&[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name","sql_execute"),("accept","application/json, text/event-stream")]).await;
    assert_eq!(status, StatusCode::OK, "{rpc}");
    assert_eq!(
        rpc["result"]["structuredContent"]["result"]["results"][1]["rows"][0][0]["value"],
        "43"
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/databases/notes/transactions",
            json!({"statements":[{"sql":"SELECT 1"}]}),
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/databases/notes/execute",
            json!({"sql":"SELECT 1","statements":[{"sql":"SELECT 2"}]}),
            &[]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_diagnostics_reach_cli_http_and_mcp() {
    use object_store_s3::{Credentials, S3Config, S3Store};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mock = Router::new().fallback(|| async {
        (StatusCode::FORBIDDEN, [("content-type", "application/xml"), ("x-private", "header-secret")],
         "<Error><Code>SecondLevelDomainForbidden</Code><Message>Please use virtual hosted style to access. test-access-secret test-access test-session&#10;</Message><Resource>body-secret</Resource></Error>")
    });
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let service = Arc::new(
        Service::new(
            Arc::new(
                S3Store::new(
                    S3Config {
                        endpoint: endpoint.clone(),
                        region: "us-east-1".into(),
                        force_path_style: true,
                        bucket: "test".into(),
                        prefix: String::new(),
                        max_object_bytes: 1024,
                        timeout: Duration::from_secs(2),
                    },
                    Credentials::new(
                        "test-access",
                        "test-access-secret",
                        Some("test-session".into()),
                        None,
                        "test",
                    ),
                )
                .unwrap(),
            ),
            "diagnostic-test".into(),
            ServiceOptions::default(),
        )
        .unwrap(),
    );
    let app = ponydb::server::router(ponydb::server::AppState {
        service,
        auth: ponydb::server::AuthFile {
            principals: vec![ponydb::server::Identity {
                id: "test".into(),
                token: TOKEN.into(),
                databases: vec!["*".into()],
                scopes: vec!["*".into()],
            }],
        },
        timeout: Duration::from_secs(5),
        mcp_enabled: true,
    });
    let expected = "S3 ListObjectsV2 failed (HTTP 403, SecondLevelDomainForbidden): Please use virtual hosted style to access. [REDACTED] [REDACTED] [REDACTED] ";
    let assert_error = |error: &Value| {
        assert_eq!(error["message"], expected);
        assert_eq!(error["code"], "STORAGE_UNAVAILABLE");
        assert_eq!(error["retryable"], false);
        assert_eq!(error["outcome"], "not_committed");
        for secret in [
            "test-access",
            "test-session",
            "header-secret",
            "body-secret",
        ] {
            assert!(!error.to_string().contains(secret));
        }
    };
    let (status, http) = send(&app, "GET", "/v1/databases", json!({}), &[]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_error(&http["error"]);
    let (_, mcp) = send(&app, "POST", "/mcp", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"database_list","arguments":{},"_meta":meta()}}), &[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name","database_list"),("accept","application/json, text/event-stream")]).await;
    assert_eq!(mcp["result"]["isError"], true, "{mcp}");
    assert_error(&mcp["result"]["structuredContent"]["error"]);
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ponydb.toml");
    std::fs::write(&config, format!("[storage]\nendpoint = {endpoint:?}\nbucket = \"test\"\n[storage.credentials]\naccess_key_id = \"test-access\"\nsecret_access_key = \"test-access-secret\"\nsession_token = \"test-session\"\n")).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ponydb"));
        for (name, _) in std::env::vars().filter(|(k, _)| k.starts_with("PONYDB_")) {
            command.env_remove(name);
        }
        command
            .arg("--config")
            .arg(config)
            .args(["databases", "list"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_error(&serde_json::from_slice::<Value>(&output.stderr).unwrap());
    server.abort();
}

#[tokio::test]
async fn schema_filters_are_shared_by_http_and_mcp() {
    let app = app();
    assert_eq!(
        send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[])
            .await
            .0,
        StatusCode::CREATED
    );
    for sql in [
        "CREATE TABLE items(id INTEGER)",
        "CREATE INDEX items_id ON items(id)",
        "CREATE TABLE other(id INTEGER)",
    ] {
        assert_eq!(
            send(
                &app,
                "POST",
                "/v1/databases/notes/execute",
                json!({"sql":sql}),
                &[]
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let path = "/v1/databases/notes/schema?table=items&type=index";
    let (status, filtered) = send(&app, "GET", path, json!({}), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["results"][0]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["results"][0]["rows"][0][1]["value"], "items_id");
    for (path, body) in [
        ("/v1/databases/missing/schema?table=", json!({})),
        ("/v1/databases/missing/schema?type=invalid", json!({})),
        ("/v1/databases/notes/schema?limit=1", json!({})),
        (
            "/v1/databases/notes/schema?table=items",
            json!({"table":"other"}),
        ),
    ] {
        let (status, value) = send(&app, "GET", path, body, &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"]["code"], "INVALID_REQUEST");
    }
    for suffix in ["?table=missing", "?table=missing&type=index"] {
        let (status, value) = send(
            &app,
            "GET",
            &format!("/v1/databases/notes/schema{suffix}"),
            json!({}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["error"]["code"], "TABLE_NOT_FOUND");
    }
    for suffix in ["?table=items&type=trigger", "?type=trigger"] {
        let (status, value) = send(
            &app,
            "GET",
            &format!("/v1/databases/notes/schema{suffix}"),
            json!({}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["results"][0]["rows"], json!([]));
    }
    let (_, rpc) = send(&app, "POST", "/mcp", json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"schema_get","arguments":{"database":"notes","table":"items","type":"index"},"_meta":meta()}
    }), &[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name","schema_get"),("accept","application/json, text/event-stream")]).await;
    assert_eq!(
        rpc["result"]["structuredContent"]["result"]["results"],
        filtered["results"]
    );
    let (_, rpc) = send(&app, "POST", "/mcp", json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"schema_get","arguments":{"database":"notes","table":"missing"},"_meta":meta()}
    }), &[("mcp-protocol-version","2026-07-28"),("mcp-method","tools/call"),("mcp-name","schema_get"),("accept","application/json, text/event-stream")]).await;
    assert_eq!(rpc["result"]["isError"], true);
    assert_eq!(
        rpc["result"]["structuredContent"]["error"]["code"],
        "TABLE_NOT_FOUND"
    );
}

#[tokio::test]
async fn restore_is_cli_only_while_remote_maintenance_remains_available() {
    let app = app();
    for (method, path) in [
        ("POST", "/v1/databases/notes/restore"),
        ("POST", "/v1/databases/notes/restores"),
        ("GET", "/v1/databases/notes/restores"),
        ("GET", "/v1/databases/notes/restores/session"),
        ("POST", "/v1/databases/notes/restores/session/commit"),
        ("POST", "/v1/databases/notes/restores/session/abort"),
        ("PUT", "/v1/databases/notes/restores/session/chunks/0"),
    ] {
        let (status, _) = send(&app, method, path, json!({}), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":meta()}});
    let (status, result) = send(
        &app,
        "POST",
        "/mcp",
        rpc,
        &[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/list"),
            ("accept", "application/json, text/event-stream"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 11);
    assert!(names.contains(&"database_compact"));
    assert!(names.contains(&"database_gc"));
    assert!(!names.iter().any(|name| name.starts_with("restore")));
    for name in [
        "restore_begin",
        "restore_upload_chunk",
        "restore_status",
        "restore_commit",
        "restore_abort",
    ] {
        let rpc = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":{},"_meta":meta()}});
        let (_, result) = send(
            &app,
            "POST",
            "/mcp",
            rpc,
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", name),
                ("accept", "application/json, text/event-stream"),
            ],
        )
        .await;
        assert!(
            result.get("error").is_some() || result["result"]["isError"] == true,
            "{result}"
        );
    }
    send(&app, "POST", "/v1/databases", json!({"name":"notes"}), &[]).await;
    for action in ["compact", "gc"] {
        let (status, result) = send(
            &app,
            "POST",
            &format!("/v1/databases/notes/{action}"),
            if action == "compact" {
                json!({"budget_ms":1000,"max_segments":1})
            } else {
                json!({"budget_ms":1,"max_objects":1})
            },
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{action}: {result}");
    }
}

#[tokio::test]
async fn http_and_mcp_logs_correlate_errors_without_request_content_or_tokens() {
    use std::io::{self, Write};
    use std::sync::Mutex;
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);
    impl Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }
    // Other concurrent tests intentionally have no subscriber. Register their
    // dispatcher too: tracing's single-dispatch fast path otherwise caches a
    // callsite against the unsubscribed thread that happens to use it first.
    let _unsubscribed_threads =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let buffer = Logs::default();
    let dispatch = tracing::Dispatch::new(ponydb::logging::subscriber(
        tracing::level_filters::LevelFilter::DEBUG,
        "json",
        buffer.clone(),
    ));
    let context = tracing::dispatcher::with_default(&dispatch, observability::capture);
    let (sql_error, auth_error, mcp_error) = context.scope(async {
        let app = app();
        assert_eq!(send(&app, "POST", "/v1/databases", json!({"name":"private_database", "metadata":{"secret":"SECRET_METADATA"}}), &[]).await.0, StatusCode::CREATED);
        let (_, sql_error) = send(&app, "POST", "/v1/databases/private_database/execute", json!({"sql":"SELECT SECRET_SQL_DIAGNOSTIC"}), &[]).await;
        let response = app.clone().oneshot(Request::builder().uri("/v1/databases/private_database")
            .header("authorization", "Bearer SECRET_BAD_TOKEN").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let auth_error: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let (_, mcp_error) = send(&app, "POST", "/mcp", json!({"jsonrpc":"2.0", "id":"SECRET_CLIENT_ID", "method":"tools/call", "params":{"name":"sql_execute", "arguments":{"database":"private_database","sql":"SELECT SECRET_MCP_SQL"},"_meta":meta()}}), &[
            ("mcp-protocol-version", mcp_server::VERSION), ("mcp-method", "tools/call"), ("mcp-name", "sql_execute"), ("accept", "application/json, text/event-stream"),
        ]).await;
        let before_health = buffer.0.lock().unwrap().len();
        let response = app.oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(buffer.0.lock().unwrap().len(), before_health);
        (sql_error, auth_error, mcp_error)
    }).await;
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    for value in [
        "private_database",
        "SECRET_",
        TOKEN,
        "Authorization",
        "/v1/",
    ] {
        assert!(!text.contains(value), "diagnostics leaked {value}");
    }
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summaries: Vec<_> = events
        .iter()
        .filter(|event| event["fields"]["event"] == "request_completed")
        .collect();
    assert_eq!(summaries.len(), 4, "events: {events:?}");
    for (error, code) in [
        (&sql_error["error"], "SQL_ERROR"),
        (&auth_error["error"], "UNAUTHENTICATED"),
        (
            &mcp_error["result"]["structuredContent"]["error"],
            "SQL_ERROR",
        ),
    ] {
        let id = error["request_id"].as_str().unwrap();
        let summary = summaries
            .iter()
            .find(|event| event["span"]["request_id"] == id)
            .unwrap();
        assert_eq!(summary["fields"]["error_code"], code);
        assert_eq!(summary["fields"]["outcome"], "failed");
    }
}
