use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
    sync::{Arc, Mutex},
};

fn cache_diagnostic(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|value| {
        value["level"] == "WARN"
            && value["target"] == "ponydb::cache"
            && value["fields"]["event"] == "disk_cache_degraded"
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    None,
    UnsupportedCreate,
    UnsupportedCas,
    IgnoreCreate,
    IgnoreCas,
    RacyCreate,
    RacyCas,
    BadRange,
    MissingMetadata,
    SizeLimit,
    NoPagination,
    NoDelimiter,
    DenyDelete,
    IgnoreDelete,
    UnknownWrite,
    UnknownHead,
    ConflictHead,
    SlowWrite,
}
#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    etag: String,
}
struct Mock {
    fault: Fault,
    objects: Mutex<BTreeMap<String, Object>>,
    requests: Mutex<Vec<(Method, String)>>,
}
fn xml(status: StatusCode, body: String) -> Response {
    (status, [("content-type", "application/xml")], body).into_response()
}
fn rejected(status: StatusCode, code: &str, message: &str) -> Response {
    xml(
        status,
        format!("<Error><Code>{code}</Code><Message>{message}</Message></Error>"),
    )
}
async fn handler(
    State(state): State<Arc<Mock>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let key = uri.path().strip_prefix("/test/").unwrap().to_owned();
    state
        .requests
        .lock()
        .unwrap()
        .push((method.clone(), key.clone()));
    let query: BTreeMap<_, _> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect();
    if method == Method::PUT && state.fault == Fault::SlowWrite {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let mut objects = state.objects.lock().unwrap();
    match method {
        Method::PUT => {
            if state.fault == Fault::SizeLimit && body.len() > 1024 {
                return rejected(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "EntityTooLarge",
                    "PUT exceeds provider size limit",
                );
            }
            let cas = headers.contains_key("if-match");
            if (state.fault == Fault::UnsupportedCreate && !cas)
                || (state.fault == Fault::UnsupportedCas && cas)
            {
                return rejected(StatusCode::BAD_REQUEST, "NotImplemented", "A header you provided implies functionality that is not implemented. test-access-secret test-access test-session&#10;unsupported");
            }
            let racy = key.contains("/race/");
            if !cas
                && headers.get("if-none-match").is_some_and(|v| v == "*")
                && objects.contains_key(&key)
                && state.fault != Fault::IgnoreCreate
                && !(racy && state.fault == Fault::RacyCreate)
            {
                return rejected(
                    StatusCode::PRECONDITION_FAILED,
                    "PreconditionFailed",
                    "already exists",
                );
            }
            if let Some(expected) = headers.get("if-match") {
                if objects
                    .get(&key)
                    .is_none_or(|o| expected != o.etag.as_str())
                    && state.fault != Fault::IgnoreCas
                    && !(racy && state.fault == Fault::RacyCas)
                {
                    return rejected(
                        StatusCode::PRECONDITION_FAILED,
                        "PreconditionFailed",
                        "ETag differs",
                    );
                }
            }
            if key.ends_with("/head.json") {
                if state.fault == Fault::UnknownHead {
                    return rejected(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "InternalError",
                        "provider-head-failure test-access-secret test-session Authorization: unknown-authorization",
                    );
                }
                if state.fault == Fault::ConflictHead {
                    return rejected(
                        StatusCode::PRECONDITION_FAILED,
                        "PreconditionFailed",
                        "provider-head-failure test-access-secret test-session Authorization: unknown-authorization",
                    );
                }
            }
            let etag = format!("\"{:x}\"", Sha256::digest(&body));
            objects.insert(
                key,
                Object {
                    bytes: body.to_vec(),
                    etag: etag.clone(),
                },
            );
            if state.fault == Fault::UnknownWrite {
                return rejected(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "InternalError",
                    "response lost after write",
                );
            }
            (StatusCode::OK, [("etag", etag)]).into_response()
        }
        Method::DELETE => {
            if state.fault == Fault::DenyDelete {
                return rejected(
                    StatusCode::FORBIDDEN,
                    "AccessDenied",
                    "DeleteObject permission required",
                );
            }
            if state.fault != Fault::IgnoreDelete {
                objects.remove(&key);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Method::GET if query.contains_key("list-type") => {
            let prefix = query.get("prefix").unwrap();
            let mut entries = BTreeSet::new();
            for key in objects.keys().filter(|k| k.starts_with(prefix)) {
                if query.contains_key("delimiter") && state.fault != Fault::NoDelimiter {
                    if let Some(pos) = key[prefix.len()..].find('/') {
                        entries.insert((key[..prefix.len() + pos + 1].to_owned(), true));
                        continue;
                    }
                }
                entries.insert((key.clone(), false));
            }
            let entries: Vec<_> = entries.into_iter().collect();
            let offset: usize = query
                .get("continuation-token")
                .map_or(0, |s| s.parse().unwrap());
            let limit: usize = if state.fault == Fault::NoPagination {
                1000
            } else {
                query["max-keys"].parse().unwrap()
            };
            let end = (offset + limit).min(entries.len());
            let mut content = String::new();
            for (key, common) in &entries[offset..end] {
                content.push_str(&if *common {
                    format!("<CommonPrefixes><Prefix>{key}</Prefix></CommonPrefixes>")
                } else {
                    format!("<Contents><Key>{key}</Key></Contents>")
                });
            }
            let token = if end < entries.len() {
                format!("<NextContinuationToken>{end}</NextContinuationToken>")
            } else {
                String::new()
            };
            xml(StatusCode::OK, format!("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>{}</IsTruncated>{content}{token}</ListBucketResult>", end < entries.len()))
        }
        Method::GET => {
            let Some(object) = objects.get(&key) else {
                return rejected(StatusCode::NOT_FOUND, "NoSuchKey", "missing");
            };
            let mut response_headers = HeaderMap::new();
            response_headers.insert("etag", object.etag.parse().unwrap());
            if state.fault != Fault::MissingMetadata {
                response_headers.insert(
                    "x-amz-meta-sha256",
                    format!("{:x}", Sha256::digest(&object.bytes))
                        .parse()
                        .unwrap(),
                );
            }
            if let Some(range) = headers
                .get("range")
                .filter(|_| state.fault != Fault::BadRange)
            {
                let (start, end) = range
                    .to_str()
                    .unwrap()
                    .strip_prefix("bytes=")
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                let (start, end): (usize, usize) = (start.parse().unwrap(), end.parse().unwrap());
                response_headers.insert(
                    "content-range",
                    format!("bytes {start}-{end}/{}", object.bytes.len())
                        .parse()
                        .unwrap(),
                );
                return (
                    StatusCode::PARTIAL_CONTENT,
                    response_headers,
                    object.bytes[start..=end].to_vec(),
                )
                    .into_response();
            }
            (StatusCode::OK, response_headers, object.bytes.clone()).into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}
async fn start(fault: Fault) -> (String, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Mock {
        fault,
        objects: Mutex::new(BTreeMap::new()),
        requests: Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(handler)
        .layer(DefaultBodyLimit::disable())
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, state, server)
}
fn command(endpoint: &str, directory: &std::path::Path) -> Command {
    configured_command(
        Command::new(env!("CARGO_BIN_EXE_ponydb")),
        endpoint,
        directory,
    )
}
fn configured_command(mut cmd: Command, endpoint: &str, directory: &std::path::Path) -> Command {
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("PONYDB_")) {
        cmd.env_remove(key);
    }
    cmd.env("XDG_CONFIG_HOME", directory);
    cmd.env("PONYDB_CACHE__DIRECTORY", directory.join("cache"));
    let file = directory.join("config.toml");
    std::fs::write(
        &file,
        format!(
            r#"
[storage]
endpoint = "{endpoint}"
bucket = "test"
prefix = "configured/"
[storage.credentials]
access_key_id = "test-access"
secret_access_key = "test-access-secret"
session_token = "test-session"
[[server.auth.principals]]
id = "test"
token = "server-secret-token"
databases = ["*"]
scopes = ["*"]
"#
        ),
    )
    .unwrap();
    cmd.arg("--config").arg(file);
    cmd
}

// Linux can reproduce the Windows main-thread stack budget without a Windows
// runner. Use a child shell so the limit never changes the test runner's stack.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_sql_runs_with_a_one_mib_main_stack() {
    let (endpoint, _, server) = start(Fault::None).await;
    let directory = tempfile::tempdir().unwrap();
    let batch = directory.path().join("batch.json");
    std::fs::write(
        &batch,
        r#"{"statements":[{"sql":"CREATE TABLE t(x)"},{"sql":"INSERT INTO t VALUES(42)"}]}"#,
    )
    .unwrap();
    for args in [
        vec!["databases", "create", "notes"],
        vec!["db", "notes", "execute", "--input", batch.to_str().unwrap()],
        vec!["db", "notes", "execute", "SELECT x FROM t"],
        vec!["db", "notes", "schema"],
    ] {
        let mut shell = Command::new("sh");
        shell
            .args(["-c", "ulimit -s 1024 && exec \"$@\"", "ponydb-small-stack"])
            .arg(env!("CARGO_BIN_EXE_ponydb"));
        let mut cmd = configured_command(shell, &endpoint, directory.path());
        cmd.env("RUST_MIN_STACK", "2097152").args(&args);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        if args.last() == Some(&"SELECT x FROM t") {
            assert_eq!(result["results"][0]["rows"][0][0]["value"], "42");
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_etag_is_the_s3_etag_from_one_head_read() {
    let (endpoint, state, server) = start(Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(&endpoint, dir.path());
    cmd.args(["databases", "create", "notes"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    state.requests.lock().unwrap().clear();
    let mut cmd = command(&endpoint, dir.path());
    cmd.args(["db", "notes"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["etag"],
        state.objects.lock().unwrap()["configured/databases/notes/head.json"].etag
    );
    {
        let requests = state.requests.lock().unwrap();
        let head_requests: Vec<_> = requests
            .iter()
            .filter(|(_, key)| key == "configured/databases/notes/head.json")
            .collect();
        assert_eq!(head_requests.len(), 1);
        assert_eq!(head_requests[0].0, Method::GET);
        assert!(!requests.iter().any(|(method, _)| *method == Method::HEAD));
    }
    // Simulate an external S3 write with valid checksum metadata but unchanged
    // application revision and nonce. The mock computes metadata from these bytes.
    {
        let mut objects = state.objects.lock().unwrap();
        let object = objects
            .get_mut("configured/databases/notes/head.json")
            .unwrap();
        let mut head: Value = serde_json::from_slice(&object.bytes).unwrap();
        head["description"] = serde_json::json!("external edit");
        object.bytes = serde_json::to_vec(&head).unwrap();
        object.etag = format!("\"{:x}\"", Sha256::digest(&object.bytes));
    }
    for action in ["update", "delete"] {
        let mut cmd = command(&endpoint, dir.path());
        cmd.args([
            "db",
            "notes",
            action,
            "--if-match",
            result["etag"].as_str().unwrap(),
        ]);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["code"], "METADATA_PRECONDITION_FAILED");
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validates_wire_contract_for_every_target_and_preserves_existing_objects() {
    let (endpoint, state, server) = start(Fault::None).await;
    state.objects.lock().unwrap().insert(
        "configured/databases/notes/head.json".into(),
        Object {
            bytes: b"real database".to_vec(),
            etag: "protected".into(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    for target in ["cli", "server", "all"] {
        let mut cmd = command(&endpoint, dir.path());
        cmd.args(["config", "validate", "--target", target]);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(result["storage_capabilities_verified"], true);
        assert_eq!(result["target"], target);
    }
    let objects = state.objects.lock().unwrap();
    assert_eq!(objects.len(), 1);
    assert_eq!(
        objects["configured/databases/notes/head.json"].bytes,
        b"real database"
    );
    for (method, key) in state.requests.lock().unwrap().iter() {
        if *method == Method::PUT || *method == Method::DELETE {
            assert!(key.starts_with("configured/_ponydb-validation/"));
        }
    }
    server.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_incompatible_storage_with_actionable_sanitized_errors_and_cleanup() {
    for (fault, diagnostic) in [
        (Fault::UnsupportedCreate, "If-None-Match"),
        (Fault::UnsupportedCas, "If-Match"),
        (Fault::IgnoreCreate, "duplicate create"),
        (Fault::IgnoreCas, "stale CAS"),
        (Fault::RacyCreate, "concurrent conditional create"),
        (Fault::RacyCas, "concurrent CAS"),
        (Fault::BadRange, "Range GET"),
        (Fault::MissingMetadata, "integrity"),
        (Fault::SizeLimit, "probe PUT/GET"),
        (Fault::NoPagination, "max-keys"),
        (Fault::NoDelimiter, "delimiter"),
        (Fault::DenyDelete, "DeleteObject permission required"),
        (Fault::IgnoreDelete, "immediately absent"),
        (Fault::UnknownWrite, "outcome is unknown"),
    ] {
        let (endpoint, state, server) = start(fault).await;
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = command(&endpoint, dir.path());
        cmd.args(["config", "validate"]);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{fault:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let result: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(result["code"], "INVALID_CONFIG");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains(diagnostic), "{fault:?}: {message}");
        assert_eq!(result["retryable"], false);
        assert_eq!(result["outcome"], "not_committed");
        for secret in [
            "test-access",
            "test-access-secret",
            "test-session",
            "server-secret-token",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
        if matches!(fault, Fault::UnsupportedCreate | Fault::UnsupportedCas) {
            assert!(message.contains("HTTP 400, NotImplemented"));
        }
        if matches!(
            fault,
            Fault::DenyDelete | Fault::IgnoreDelete | Fault::UnknownWrite
        ) {
            assert!(message.contains("_ponydb-validation/"));
        }
        if !matches!(fault, Fault::DenyDelete | Fault::IgnoreDelete) {
            assert!(state.objects.lock().unwrap().is_empty(), "{fault:?}");
        }
        server.abort();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_current_export_validates_locally_and_reads_head_once() {
    let (endpoint, state, server) = start(Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.sqlite");
    std::fs::write(&existing, b"keep").unwrap();
    let request = dir.path().join("export.json");
    for dto in [
        serde_json::json!({"file": existing}),
        serde_json::json!({"file": dir.path().join("missing/backup.sqlite")}),
        serde_json::json!({"file": "-"}),
        serde_json::json!({"file": ""}),
        serde_json::json!({}),
        serde_json::json!({"file": false}),
        serde_json::json!({"database": "other", "file": dir.path().join("backup.sqlite")}),
        serde_json::json!({"snapshot_id": "snap", "file": dir.path().join("backup.sqlite")}),
    ] {
        std::fs::write(&request, dto.to_string()).unwrap();
        let mut cmd = command(&endpoint, dir.path());
        cmd.args(["db", "notes", "export", "--input"]).arg(&request);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{dto}: {output:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["code"], "INVALID_REQUEST");
        assert!(state.requests.lock().unwrap().is_empty());
    }
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
    let mut cmd = command(&endpoint, dir.path());
    cmd.args(["databases", "create", "notes"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let head_key = "configured/databases/notes/head.json";
    let (head_before, count_before) = {
        let objects = state.objects.lock().unwrap();
        (objects[head_key].bytes.clone(), objects.len())
    };
    let head: Value = serde_json::from_slice(&head_before).unwrap();
    state.requests.lock().unwrap().clear();
    let path = dir.path().join("current.sqlite");
    let mut cmd = command(&endpoint, dir.path());
    cmd.args(["db", "notes", "export"]).arg(&path);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // Cache contention/timeout diagnostics may accompany successful JSON output.
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .all(cache_diagnostic),
        "{output:?}"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data_version"], head["data_version"]);
    assert_eq!(
        result["snapshot_id"],
        format!(
            "{}:{}",
            head["incarnation_id"].as_str().unwrap(),
            head["data_version"].as_str().unwrap()
        )
    );
    let connection = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|(_, key)| key == head_key).count(),
        1
    );
    assert!(requests.iter().all(|(method, _)| *method == Method::GET));
    assert!(!requests
        .iter()
        .any(|(_, key)| key.contains("/indexes/") || key.contains("/snapshots/")));
    let objects = state.objects.lock().unwrap();
    assert_eq!(objects[head_key].bytes, head_before);
    assert_eq!(objects.len(), count_before);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_show_and_local_validation_errors_never_contact_storage() {
    let (endpoint, state, server) = start(Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["config", "show"],
        vec!["--database", "notes", "config", "validate"],
        vec!["config", "validate", "--database", "notes"],
        vec!["--deadline-ms", "100", "config", "validate"],
        vec!["config", "validate", "--deadline-ms", "100"],
        vec!["config", "validate", "--limit", "1"],
    ] {
        let success = args == ["config", "show"];
        let mut cmd = command(&endpoint, dir.path());
        cmd.args(args);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output.status.success(), success);
    }
    let mut cmd = command(&endpoint, dir.path());
    cmd.env("PONYDB_SERVER__AUTH", r#"{"principals":[]}"#)
        .args(["config", "validate", "--target", "server"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("authentication identity"));
    assert!(state.requests.lock().unwrap().is_empty());
    server.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_timeout_is_not_success_and_reports_possible_late_writes() {
    let (endpoint, _state, server) = start(Fault::SlowWrite).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(&endpoint, dir.path());
    cmd.env("PONYDB_REQUESTS__DEFAULT_TIMEOUT_MS", "250")
        .args(["config", "validate"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stderr).unwrap();
    let message = result["message"].as_str().unwrap();
    assert!(message.contains("timed out after 250 ms"), "{message}");
    assert!(message.contains("_ponydb-validation/"), "{message}");
    assert!(message.contains("may still finish later"), "{message}");
    server.abort();
}

fn sqlite_backup(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("backup.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE restored(x); INSERT INTO restored VALUES(zeroblob(700000));")
        .unwrap();
    path
}
fn parse_restore_error(bytes: &[u8]) -> Value {
    let text = std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .filter(|line| {
            !serde_json::from_str::<Value>(line).is_ok_and(|value| {
                value.get("level").is_some()
                    && value["target"]
                        .as_str()
                        .is_some_and(|target| target == "ponydb" || target.starts_with("ponydb::"))
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    let start = text.find('{').unwrap();
    assert!(text[..start]
        .lines()
        .all(|line| line.starts_with("Restore: ")));
    serde_json::from_str(&text[start..]).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_restore_streams_formal_segments_and_keeps_stdout_machine_readable() {
    let (endpoint, state, server) = start(Fault::None).await;
    let directory = tempfile::tempdir().unwrap();
    let path = sqlite_backup(directory.path());
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let mut cmd = command(&endpoint, directory.path());
    cmd.env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("PONYDB_REQUESTS__DEFAULT_TIMEOUT_MS", "1")
        .args(["db", "notes", "restore", "file"])
        .arg(&path);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["committed"], true);
    assert_eq!(
        result["sha256"],
        format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()))
    );
    let logs = String::from_utf8(output.stderr).unwrap();
    assert!(
        logs.lines()
            .all(|line| line.starts_with("Restore: ") || cache_diagnostic(line)),
        "{logs}"
    );
    for stage in [
        "copying backup",
        "validating SQLite",
        "computing SHA-256",
        "uploading data segments",
        "building index and snapshot",
        "publishing database HEAD",
        "complete",
    ] {
        assert!(logs.contains(stage), "{logs}");
    }
    assert!(!logs.contains('\r'));
    assert!(std::fs::read_dir(&temporary).unwrap().next().is_none());
    let requests = state.requests.lock().unwrap().clone();
    assert!(!requests.iter().any(|(_, key)| key.contains("/restore/")));
    assert!(!requests
        .iter()
        .any(|(method, key)| *method == Method::GET && key.contains("/segments/")));
    assert_eq!(
        requests
            .iter()
            .filter(|(method, key)| *method == Method::PUT && key.contains("/segments/"))
            .count(),
        std::fs::metadata(&path).unwrap().len().div_ceil(256 * 1024) as usize
    );
    let stored: Value = serde_json::from_slice(
        &state.objects.lock().unwrap()["configured/databases/notes/head.json"].bytes,
    )
    .unwrap();
    assert!(stored.get("restore").is_none());
    assert!(stored.get("restore_result").is_none());
    let mut cmd = command(&endpoint, directory.path());
    cmd.args(["db", "notes", "execute", "SELECT length(x) FROM restored"]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["results"][0]["rows"][0][0]["value"], "700000");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_restore_local_failures_clean_temporary_files_without_s3_requests() {
    let (endpoint, state, server) = start(Fault::None).await;
    let directory = tempfile::tempdir().unwrap();
    let path = sqlite_backup(directory.path());
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    for invalid_workspace in [false, true] {
        let workspace = if invalid_workspace {
            directory.path().join("does-not-exist")
        } else {
            temporary.clone()
        };
        let source = if invalid_workspace {
            path.clone()
        } else {
            let invalid = directory.path().join("invalid.sqlite");
            std::fs::write(&invalid, b"not SQLite").unwrap();
            invalid
        };
        let mut cmd = command(&endpoint, directory.path());
        cmd.env("TMPDIR", &workspace)
            .env("TMP", &workspace)
            .env("TEMP", &workspace)
            .args(["db", "notes", "restore", "file"])
            .arg(source);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = parse_restore_error(&output.stderr);
        assert_eq!(
            error["code"],
            if invalid_workspace {
                "RESOURCE_LIMIT"
            } else {
                "INVALID_SQLITE_BACKUP"
            }
        );
        assert!(state.requests.lock().unwrap().is_empty());
        assert!(std::fs::read_dir(&temporary).unwrap().next().is_none());
    }
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_restore_ctrl_c_stops_uploads_without_publishing_or_leaking_local_files() {
    use std::{process::Stdio, time::Duration};
    let (endpoint, state, server) = start(Fault::SlowWrite).await;
    let directory = tempfile::tempdir().unwrap();
    let path = sqlite_backup(directory.path());
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let mut cmd = command(&endpoint, directory.path());
    cmd.env("PONYDB_LOGGING__LEVEL", "trace")
        .env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .args(["db", "notes", "restore", "file"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(method, key)| *method == Method::PUT && key.contains("/segments/"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        parse_restore_error(&output.stderr)["code"],
        "DEADLINE_EXCEEDED"
    );
    let events: Vec<Value> = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("level").is_some())
        .collect();
    let summary = events
        .iter()
        .find(|event| event["fields"]["event"] == "request_completed")
        .unwrap();
    assert_eq!(summary["fields"]["outcome"], "stopped");
    assert_eq!(
        summary["span"]["request_id"],
        parse_restore_error(&output.stderr)["request_id"]
    );
    assert!(events
        .iter()
        .any(|event| event["fields"]["event"] == "s3_request_completed"
            && event["fields"]["outcome"] == "aborted"));
    assert!(!state
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|(method, key)| *method == Method::PUT && key.ends_with("/head.json")));
    assert!(std::fs::read_dir(&temporary).unwrap().next().is_none());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_restore_rejects_wal_format_before_storage_and_cleans_workspace() {
    let (endpoint, state, server) = start(Fault::None).await;
    let directory = tempfile::tempdir().unwrap();
    let path = sqlite_backup(directory.path());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(connection);
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let mut cmd = command(&endpoint, directory.path());
    cmd.env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .args(["db", "notes", "restore", "file"])
        .arg(&path);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = parse_restore_error(&output.stderr);
    assert_eq!(error["code"], "INVALID_SQLITE_BACKUP");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("DELETE journal mode"));
    assert!(state.requests.lock().unwrap().is_empty());
    assert!(std::fs::read_dir(&temporary).unwrap().next().is_none());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_cli_queries_reuse_disk_bodies_but_fetch_head() {
    let (endpoint, state, server) = start(Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    let mut create = command(&endpoint, dir.path());
    create.args(["databases", "create", "notes"]);
    let output = tokio::task::spawn_blocking(move || create.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut samples = Vec::new();
    let mut cache_version: Option<(rusqlite::Connection, i64)> = None;
    let mut warm_reads = 0;
    // Disable, cold, then retry across new processes until two healthy cache hits.
    // Keep the production 50 ms cache budget; allow transient suite load to subside.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut iteration = 0;
    while tokio::time::Instant::now() < deadline && warm_reads < 2 {
        state.requests.lock().unwrap().clear();
        let mut cmd = command(&endpoint, dir.path());
        cmd.args(["db", "notes", "execute", "SELECT 1"]);
        if iteration == 0 {
            cmd.env("PONYDB_CACHE__DISK_BUDGET_MIB", "0");
        }
        let start = std::time::Instant::now();
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        let elapsed = start.elapsed().as_micros();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap();
        let (heads, bodies) = {
            let requests = state.requests.lock().unwrap();
            let heads = requests
                .iter()
                .filter(|(method, key)| *method == Method::GET && key.ends_with("/head.json"))
                .count();
            let bodies = requests
                .iter()
                .filter(|(method, key)| *method == Method::GET && !key.ends_with("/head.json"))
                .count();
            (heads, bodies)
        };
        assert!(heads > 0);
        if iteration == 0 {
            assert!(bodies > 0);
        }
        let diagnostics: Vec<_> = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| cache_diagnostic(line))
            .map(str::to_owned)
            .collect();
        let degraded = !diagnostics.is_empty();
        // Under concurrent suite load the 50 ms budget may deliberately fall back.
        // Require actual cross-process hits, and enforce zero GETs on subsequent
        // healthy reads without treating a documented timeout as a cache bug.
        if warm_reads > 0 && !degraded {
            assert_eq!(
                bodies, 0,
                "healthy warm process fetched immutable objects: {output:?}"
            );
        }
        if let Some((db, version)) = cache_version.as_mut() {
            let current: i64 = db
                .query_row("PRAGMA data_version", [], |r| r.get(0))
                .unwrap();
            if bodies == 0 && !degraded {
                assert_eq!(
                    *version, current,
                    "warm reads wrote persistent LRU on each process"
                );
            }
            *version = current;
        }
        if iteration > 0 && bodies == 0 && !degraded {
            warm_reads += 1;
            if cache_version.is_none() {
                let db = rusqlite::Connection::open(dir.path().join("cache/objects-v1.sqlite3"))
                    .unwrap();
                let version: i64 = db
                    .query_row("PRAGMA data_version", [], |r| r.get(0))
                    .unwrap();
                cache_version = Some((db, version));
            }
        }
        samples.push(serde_json::json!({"iteration":iteration,"microseconds":elapsed,"head_gets":heads,"body_gets":bodies,"degraded":degraded,"diagnostics":diagnostics}));
        iteration += 1;
        if warm_reads < 2 {
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(100)),
            )
            .await;
        }
    }
    assert!(
        warm_reads >= 2,
        "no repeated healthy cross-process cache reuse within the 20 s retry budget: {samples:?}"
    );
    eprintln!(
        "CLI disk-cache mock-S3 samples: {}",
        serde_json::to_string(&samples).unwrap()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_summarizes_and_trace_records_s3_pages_and_caches_without_payloads() {
    let (endpoint, state, server) = start(Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    let metadata = dir.path().join("SECRET_METADATA_PATH.json");
    std::fs::write(&metadata, r#"{"private":"SECRET_METADATA_VALUE"}"#).unwrap();
    let mut create = command(&endpoint, dir.path());
    create.env("PONYDB_LOGGING__LEVEL", "debug");
    create.args([
        "databases",
        "create",
        "logs",
        "--description",
        "SECRET_DESCRIPTION",
        "--metadata-file",
    ]);
    create.arg(&metadata);
    let output = tokio::task::spawn_blocking(move || create.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<Value>(&output.stdout).unwrap();
    let events: Vec<Value> = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = events
        .iter()
        .find(|event| event["fields"]["event"] == "request_completed")
        .unwrap();
    assert_eq!(summary["fields"]["outcome"], "committed");
    assert!(summary["fields"]["page_writes"].as_u64().unwrap() > 0);
    assert_eq!(
        summary["fields"]["s3_requests"].as_u64().unwrap() as usize,
        state.requests.lock().unwrap().len()
    );
    let debug_output = std::str::from_utf8(&output.stderr).unwrap();
    for value in [
        "SECRET_METADATA",
        "SECRET_DESCRIPTION",
        "test-access",
        "test-session",
        "server-secret-token",
        "configured/databases/",
    ] {
        assert!(
            !debug_output.contains(value),
            "unexpected diagnostic field: {value}"
        );
    }
    for event in ["s3_request_completed", "page_io_completed", "cache_access"] {
        assert!(!debug_output.contains(event));
    }
    let publication = events
        .iter()
        .position(|event| event["fields"]["event"] == "head_publication_completed")
        .unwrap();
    let completed = events
        .iter()
        .position(|event| event["fields"]["event"] == "request_completed")
        .unwrap();
    assert!(publication < completed);

    for level in ["debug", "trace"] {
        state.requests.lock().unwrap().clear();
        let mut cmd = command(&endpoint, dir.path());
        cmd.env("PONYDB_LOGGING__LEVEL", level)
            .env("PONYDB_CACHE__DISK_BUDGET_MIB", "0");
        cmd.args([
            "db",
            "logs",
            "execute",
            "SELECT ? AS SECRET_COLUMN",
            "--param",
            r#"{"type":"text","value":"SECRET_RESULT"}"#,
        ]);
        let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["results"][0]["rows"][0][0]["value"], "SECRET_RESULT");
        let text = std::str::from_utf8(&output.stderr).unwrap();
        for secret in [
            "SECRET_RESULT",
            "SECRET_COLUMN",
            "SELECT ?",
            "test-access",
            "test-session",
            "server-secret-token",
            endpoint.as_str(),
            dir.path().to_str().unwrap(),
        ] {
            assert!(!text.contains(secret), "diagnostics leaked {secret}");
        }
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let summary = events
            .iter()
            .find(|event| event["fields"]["event"] == "request_completed")
            .unwrap();
        let id = summary["span"]["request_id"].as_str().unwrap();
        assert_eq!(summary["fields"]["outcome"], "read_only");
        assert!(summary["fields"]["page_reads"].as_u64().unwrap() > 0);
        assert!(summary["fields"]["memory_hits"].as_u64().unwrap() > 0);
        assert_eq!(
            summary["fields"]["s3_requests"].as_u64().unwrap() as usize,
            state.requests.lock().unwrap().len()
        );
        for event in &events {
            assert_eq!(
                event["span"]["request_id"], id,
                "unassociated event: {event}"
            );
        }
        let s3: Vec<_> = events
            .iter()
            .filter(|event| event["fields"]["event"] == "s3_request_completed")
            .collect();
        if level == "trace" {
            assert_eq!(s3.len(), state.requests.lock().unwrap().len());
            assert!(text.contains("configured/databases/logs/head.json"));
            assert!(text.contains("page_io_completed"));
            assert!(text.contains("cache_access"));
            assert!(text.contains("worker_started"));
        } else {
            assert!(s3.is_empty());
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_logs_keep_conflicts_and_unknown_outcomes_distinct() {
    for (fault, outcome, code, exit, status) in [
        (Fault::UnknownHead, "unknown", "OUTCOME_UNKNOWN", 5, 500),
        (Fault::ConflictHead, "conflict", "TX_CONFLICT", 4, 412),
    ] {
        let (endpoint, _, server) = start(fault).await;
        let directory = tempfile::tempdir().unwrap();
        let mut command = command(&endpoint, directory.path());
        command
            .env("PONYDB_LOGGING__LEVEL", "trace")
            .args(["databases", "create", "notes"]);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(exit), "{output:?}");
        assert!(output.stdout.is_empty());
        let events: Vec<Value> = std::str::from_utf8(&output.stderr)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|value| value.get("level").is_some())
            .collect();
        let summary = events
            .iter()
            .find(|event| event["fields"]["event"] == "request_completed")
            .unwrap();
        assert_eq!(summary["fields"]["outcome"], outcome);
        assert_eq!(summary["fields"]["error_code"], code);
        let publication = events
            .iter()
            .find(|event| event["fields"]["event"] == "head_publication_completed")
            .unwrap();
        assert_eq!(publication["fields"]["outcome"], outcome);
        assert!(events
            .iter()
            .any(|event| event["fields"]["event"] == "s3_request_completed"
                && event["fields"]["operation"] == "PutObject"
                && event["fields"]["status"] == status));
        let failed_put = events
            .iter()
            .find(|event| {
                event["fields"]["event"] == "s3_request_failed"
                    && event["fields"]["operation"] == "PutObject"
                    && event["fields"]["status"] == status
            })
            .expect("SDK failure diagnostic");
        assert_eq!(failed_put["fields"]["sdk_error_type"], "ServiceError");
        assert!(failed_put["fields"]["error_chain"]
            .as_str()
            .unwrap()
            .contains("provider-head-failure"));
        for event in &events {
            let text = serde_json::to_string(event).unwrap();
            for secret in ["test-access", "test-session", "unknown-authorization"] {
                assert!(!text.contains(secret), "diagnostics leaked {secret}");
            }
            if event["fields"]["event"] != "s3_request_failed" {
                assert!(!text.contains("provider-head-failure"));
            }
        }
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_maintenance_and_disk_fallback_keep_correlated_summaries() {
    let (endpoint, _, server) = start(Fault::None).await;
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("seed.json");
    std::fs::write(&input, r#"{"statements":[{"sql":"CREATE TABLE t(x)"},{"sql":"INSERT INTO t VALUES(zeroblob(700000))"}]}"#).unwrap();
    let mut create = command(&endpoint, directory.path());
    create.args(["databases", "create", "notes"]);
    assert!(
        tokio::task::spawn_blocking(move || create.output().unwrap())
            .await
            .unwrap()
            .status
            .success()
    );
    let mut seed = command(&endpoint, directory.path());
    seed.args(["db", "notes", "execute", "--input"]).arg(&input);
    assert!(tokio::task::spawn_blocking(move || seed.output().unwrap())
        .await
        .unwrap()
        .status
        .success());
    for args in [
        vec![
            "db",
            "notes",
            "compact",
            "--budget-ms",
            "5000",
            "--max-segments",
            "1",
        ],
        vec![
            "db",
            "notes",
            "gc",
            "--budget-ms",
            "1",
            "--max-objects",
            "1",
        ],
    ] {
        let is_gc = args[2] == "gc";
        let mut command = command(&endpoint, directory.path());
        command.env("PONYDB_LOGGING__LEVEL", "debug").args(args);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(6), "{output:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["outcome"],
            "partial"
        );
        let events: Vec<Value> = std::str::from_utf8(&output.stderr)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let summary = events
            .iter()
            .find(|event| event["fields"]["event"] == "request_completed")
            .unwrap();
        assert_eq!(summary["fields"]["outcome"], "partial");
        assert!(summary["fields"]["s3_requests"].as_u64().unwrap() > 0);
        if is_gc {
            let stage = events
                .iter()
                .find(|event| event["fields"]["stage"] == "gc_claiming")
                .unwrap();
            assert_eq!(stage["span"]["request_id"], summary["span"]["request_id"]);
        }
    }
    let blocked_cache = directory.path().join("SECRET_LOCAL_CACHE_PATH");
    std::fs::write(&blocked_cache, b"not a directory").unwrap();
    let mut command = command(&endpoint, directory.path());
    command
        .env("PONYDB_LOGGING__LEVEL", "trace")
        .env("PONYDB_CACHE__DIRECTORY", &blocked_cache)
        .args(["db", "notes", "execute", "SELECT count(*) FROM t"]);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["results"][0]["rows"][0][0]
            ["value"],
        "1"
    );
    let text = std::str::from_utf8(&output.stderr).unwrap();
    assert!(!text.contains("SECRET_LOCAL_CACHE_PATH"));
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = events
        .iter()
        .find(|event| event["fields"]["event"] == "request_completed")
        .unwrap();
    assert!(summary["fields"]["disk_bypasses"].as_u64().unwrap() > 0);
    let warnings: Vec<_> = events
        .iter()
        .filter(|event| event["fields"]["event"] == "disk_cache_degraded")
        .collect();
    assert!(!warnings.is_empty());
    let mut categories = BTreeSet::new();
    for event in warnings {
        assert!(categories.insert(event["fields"]["reason"].as_str().unwrap()));
        assert_eq!(event["span"]["request_id"], summary["span"]["request_id"]);
    }
    server.abort();
}
