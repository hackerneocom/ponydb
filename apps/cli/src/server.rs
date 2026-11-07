use axum::{
    body::{to_bytes, Body},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use database_service::{error, Service};
use object_store_api::BlobStore;
use protocol::{ApiError, Operation, Principal};
use request_context::RequestContext;
pub use runtime_config::{AuthIdentity as Identity, ServerAuth as AuthFile};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
pub struct AppState<S: BlobStore> {
    pub service: Arc<Service<S>>,
    pub auth: AuthFile,
    pub timeout: Duration,
    pub mcp_enabled: bool,
}
fn authenticate(auth: &AuthFile, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| {
            tracing::debug!(target: "ponydb", event = "authentication_completed", authenticated = false);
            error("UNAUTHENTICATED", "Bearer authentication required")
        })?;
    let found = auth.principals.iter().find(|p| {
        let a = p.token.as_bytes();
        let b = token.as_bytes();
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
    });
    tracing::debug!(target: "ponydb", event = "authentication_completed", authenticated = found.is_some());
    found
        .map(|p| Principal {
            id: p.id.clone(),
            databases: p.databases.clone(),
            scopes: p.scopes.clone(),
        })
        .ok_or_else(|| error("UNAUTHENTICATED", "Invalid bearer token"))
}
fn status(e: &ApiError) -> StatusCode {
    match e.code.as_str() {
        "UNAUTHENTICATED" => StatusCode::UNAUTHORIZED,
        "FORBIDDEN" => StatusCode::FORBIDDEN,
        "DATABASE_NOT_FOUND" | "TABLE_NOT_FOUND" => StatusCode::NOT_FOUND,
        "DATABASE_DELETED" | "SNAPSHOT_EXPIRED" => StatusCode::GONE,
        "METADATA_PRECONDITION_FAILED" => StatusCode::PRECONDITION_FAILED,
        "TX_CONFLICT" | "DATABASE_EXISTS" => StatusCode::CONFLICT,
        "RESOURCE_LIMIT" => StatusCode::PAYLOAD_TOO_LARGE,
        "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "STORAGE_UNAVAILABLE" | "OUTCOME_UNKNOWN" | "MAINTENANCE" | "MAINTENANCE_REQUIRED" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::BAD_REQUEST,
    }
}
fn fail(e: ApiError) -> Response {
    observability::record_result(observability::error_outcome(&e.code, &e.outcome), &e.code);
    (status(&e), Json(json!({"ok":false,"error":e}))).into_response()
}
async fn logged_response(
    operation: &'static str,
    future: impl std::future::Future<Output = Response>,
) -> Response {
    let mut logs = observability::Request::new(operation, None);
    let response = logs.scope(future).await;
    logs.finish(
        if response.status().is_success() {
            "success"
        } else {
            "failed"
        },
        "",
    );
    response
}
pub fn router<S: BlobStore + 'static>(state: AppState<S>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/databases", any(api::<S>))
        .route("/v1/databases/{database}", any(api::<S>))
        .route("/v1/databases/{database}/{action}", any(api::<S>))
        .route("/v1/databases/{database}/requests/lookup", any(api::<S>))
        .route(
            "/v1/databases/{database}/snapshots/{snapshot_id}",
            any(api::<S>),
        )
        .route(
            "/v1/databases/{database}/snapshots/{snapshot_id}/export",
            get(export_snapshot::<S>),
        )
        .route("/mcp", post(mcp::<S>))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(Arc::new(state))
}

async fn export_snapshot<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    Path((database, snapshot_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    logged_response(
        "http_snapshot_export",
        export_snapshot_inner(State(state), Path((database, snapshot_id)), headers),
    )
    .await
}
async fn export_snapshot_inner<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    Path((database, snapshot_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    use tokio::io::AsyncSeekExt;
    let deadline = tokio::time::Instant::now() + state.timeout;
    let principal = match authenticate(&state.auth, &headers) {
        Ok(principal) => principal,
        Err(e) => return fail(e),
    };
    let temporary = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(_) => return fail(error("RESOURCE_LIMIT", "Cannot create export workspace")),
    };
    let file = match temporary.as_file().try_clone() {
        Ok(file) => file,
        Err(_) => return fail(error("RESOURCE_LIMIT", "Cannot open export workspace")),
    };
    let mut file = tokio::fs::File::from_std(file);
    let result = match tokio::time::timeout_at(
        deadline,
        state.service.export_snapshot(
            &principal,
            protocol::SnapshotExport {
                database,
                snapshot_id,
            },
            RequestContext::new(state.timeout),
            &mut file,
        ),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return fail(e),
        Err(_) => return fail(error("DEADLINE_EXCEEDED", "Export deadline exceeded")),
    };
    if file.rewind().await.is_err() {
        return fail(error("RESOURCE_LIMIT", "Cannot rewind export file"));
    }
    export_response(file, temporary, result.total_bytes, deadline)
}

fn export_response(
    file: tokio::fs::File,
    temporary: tempfile::NamedTempFile,
    total_bytes: String,
    deadline: tokio::time::Instant,
) -> Response {
    use tokio::io::AsyncReadExt;
    // Own the temporary file until the body completes or the client drops it.
    let stream = futures_util::stream::try_unfold(
        (file, temporary),
        move |(mut file, temporary)| async move {
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Download deadline exceeded",
                ));
            }
            let mut bytes = vec![0; 65536];
            let n = tokio::time::timeout_at(deadline, file.read(&mut bytes))
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "Download deadline exceeded")
                })??;
            if n == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            bytes.truncate(n);
            Ok(Some((bytes, (file, temporary))))
        },
    );
    Response::builder()
        .header("content-type", "application/vnd.sqlite3")
        .header("content-length", total_bytes)
        .header(
            "content-disposition",
            "attachment; filename=\"snapshot.sqlite\"",
        )
        .body(Body::from_stream(stream))
        .unwrap()
}
fn insert(input: &mut Value, key: &str, value: Value) -> Result<(), ApiError> {
    if input.get(key).is_some_and(|old| *old != value) {
        return Err(error(
            "INVALID_REQUEST",
            "Conflicting path/header and body parameters",
        ));
    }
    input
        .as_object_mut()
        .ok_or_else(|| error("INVALID_REQUEST", "Object body required"))?
        .insert(key.into(), value);
    Ok(())
}
async fn api<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    request: Request,
) -> Response {
    logged_response("http", api_inner(State(state), request)).await
}
async fn api_inner<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    request: Request,
) -> Response {
    let ctx = RequestContext::new(state.timeout);
    let principal = match authenticate(&state.auth, request.headers()) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let (method, uri, headers, body) = {
        let (p, b) = request.into_parts();
        (p.method, p.uri, p.headers, b)
    };
    let bytes = match to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return fail(error("RESOURCE_LIMIT", "Request body too large")),
    };
    let parts: Vec<_> = uri.path().trim_matches('/').split('/').collect();
    let tail = &parts[2..];
    let mut input = if bytes.is_empty() {
        json!({})
    } else {
        match protocol::strict_json(&bytes) {
            Ok(v) if v.is_object() => v,
            _ => return fail(error("INVALID_REQUEST", "JSON object required")),
        }
    };
    if let Some(query) = uri.query() {
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            let value = match k.as_ref() {
                "limit" | "max_rows" => v.parse::<u64>().map(|v| json!(v)).unwrap_or(Value::Null),
                "include_deleted" => v.parse::<bool>().map(|v| json!(v)).unwrap_or(Value::Null),
                _ => json!(v),
            };
            if let Err(e) = insert(&mut input, &k, value) {
                return fail(e);
            }
        }
    }
    for (header, field) in [("if-match", "if_match")] {
        if let Some(value) = headers.get(header).and_then(|v| v.to_str().ok()) {
            if let Err(e) = insert(&mut input, field, json!(value)) {
                return fail(e);
            }
        }
    }
    if let Some(id) = tail.first() {
        if let Err(e) = insert(&mut input, "database", json!(id)) {
            return fail(e);
        }
    }
    let operation = match (method.as_str(), tail) {
        ("POST", []) => "database_create",
        ("GET", []) => "database_list",
        ("GET", [_]) => "database_get",
        ("PATCH", [_]) => "database_update",
        ("DELETE", [_]) => "database_delete",
        ("POST", [_, "execute"]) => "sql_execute",
        ("GET", [_, "schema"]) => "schema_get",
        ("POST", [_, "compact"]) => "database_compact",
        ("POST", [_, "gc"]) => "database_gc",
        ("GET", [_, "snapshots"]) => "snapshot_list",
        ("GET", [_, "snapshots", snapshot]) => {
            if let Err(e) = insert(&mut input, "snapshot_id", json!(snapshot)) {
                return fail(e);
            }
            "snapshot_get"
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    observability::phase("http_input_validated");
    let op: Operation = match serde_json::from_value(json!({"operation":operation,"input":input})) {
        Ok(v) => v,
        Err(_) => {
            return fail(error(
                "INVALID_REQUEST",
                "Unknown fields, missing fields or invalid parameter types",
            ))
        }
    };
    match state.service.call(&principal, op, ctx).await {
        Ok(value) => {
            let etag = value
                .get("etag")
                .and_then(Value::as_str)
                .and_then(|v| HeaderValue::from_str(v).ok());
            let mut response = if headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("application/x-ndjson"))
                && operation == "sql_execute"
                && value["outcome"] == "read_only"
            {
                let mut lines = String::new();
                lines.push_str(
                    &json!({"type":"result_start","data_version":value["data_version"]})
                        .to_string(),
                );
                lines.push('\n');
                for result in value["results"].as_array().into_iter().flatten() {
                    lines.push_str(
                        &json!({"type":"rows","columns":result["columns"],"rows":result["rows"]})
                            .to_string(),
                    );
                    lines.push('\n');
                }
                lines.push_str(&json!({"type":"done","outcome":"read_only"}).to_string());
                lines.push('\n');
                (
                    [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
                    lines,
                )
                    .into_response()
            } else {
                (
                    if operation == "database_create" {
                        StatusCode::CREATED
                    } else {
                        StatusCode::OK
                    },
                    Json(value),
                )
                    .into_response()
            };
            if let Some(etag) = etag {
                response
                    .headers_mut()
                    .insert(axum::http::header::ETAG, etag);
            }
            response
        }
        Err(e) => fail(e),
    }
}
async fn mcp<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    logged_response("mcp_http", mcp_inner(State(state), headers, body)).await
}
async fn mcp_inner<S: BlobStore + 'static>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !state.mcp_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(message) = protocol::strict_json(&body) else {
        return Json(
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
        )
        .into_response();
    };
    // No browser origins are configured in this first transport. Fail closed for
    // Origin-bearing requests, including localhost DNS-rebinding attempts.
    if headers.contains_key("origin") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let principal = match authenticate(&state.auth, &headers) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let version = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok());
    if version != Some(mcp_server::VERSION) {
        return(StatusCode::BAD_REQUEST,Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"UnsupportedProtocolVersion","data":{"supportedVersions":[mcp_server::VERSION]}}}))).into_response();
    }
    if headers.get("mcp-method").and_then(|v| v.to_str().ok()) != message["method"].as_str() {
        return (
            StatusCode::BAD_REQUEST,
            Json(mcp_server::rpc_error(id, -32600, "Mcp-Method mismatch")),
        )
            .into_response();
    }
    if message["method"] == "tools/call"
        && headers.get("mcp-name").and_then(|v| v.to_str().ok())
            != message["params"]["name"].as_str()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(mcp_server::rpc_error(id, -32600, "Mcp-Name mismatch")),
        )
            .into_response();
    }
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("application/json") || !accept.contains("text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    Json(
        mcp_server::dispatch(
            &state.service,
            &principal,
            message,
            RequestContext::new(state.timeout),
        )
        .await,
    )
    .into_response()
}
type BoxError = Box<dyn std::error::Error>;
/// The packaged SQLite must be the pinned bundled build with our protocol requirements.
fn verify_engine() -> Result<engine_sqlite::EngineDiagnostics, BoxError> {
    let d = engine_sqlite::diagnostics()?;
    if d.version != "3.54.1" {
        return Err(format!("unexpected SQLite version {}", d.version).into());
    }
    if d.compile_options.iter().any(|o| o == "OMIT_AUTHORIZATION") {
        return Err("SQLite built without authorizer support".into());
    }
    if !d
        .compile_options
        .iter()
        .any(|o| o == "ENABLE_MATH_FUNCTIONS")
    {
        return Err("SQLite built without math functions".into());
    }
    Ok(d)
}
fn load_auth(config: &runtime_config::AppConfig) -> Result<AuthFile, BoxError> {
    config.server.auth.validate()?;
    Ok(config.server.auth.clone())
}
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!(target: "ponydb", event = "server_draining");
}

pub async fn serve(config: &runtime_config::AppConfig) -> Result<(), BoxError> {
    let engine = verify_engine()?;
    let auth = load_auth(config)?;
    let service = database_service::Service::new(
        Arc::new(config.s3()?),
        config.namespace(),
        config.service_options(),
    )?;
    let service = Arc::new(service);
    let app = router(AppState {
        service: service.clone(),
        auth,
        timeout: Duration::from_millis(config.requests.default_timeout_ms),
        mcp_enabled: config.mcp.enabled,
    });
    let listener = tokio::net::TcpListener::bind(&config.server.listen).await?;
    tracing::info!(target: "ponydb", event = "server_started", address = %listener.local_addr()?, sqlite_version = %engine.version);
    let shutdown_service = service.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_service.cancel_gc();
        })
        .await?;
    service.wait_for_gc().await;
    tracing::info!(target: "ponydb", event = "server_stopped");
    Ok(())
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn bundled_engine_passes_startup_checks() {
        verify_engine().unwrap();
    }

    fn prepared(deadline: tokio::time::Instant) -> (Response, std::path::PathBuf) {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temporary.path(), b"test").unwrap();
        let path = temporary.path().to_owned();
        let file = tokio::fs::File::from_std(temporary.reopen().unwrap());
        (export_response(file, temporary, "4".into(), deadline), path)
    }

    #[tokio::test]
    async fn downloads_release_temporary_files_on_completion_drop_and_deadline() {
        let (response, path) = prepared(tokio::time::Instant::now() + Duration::from_secs(5));
        assert!(path.exists());
        drop(response);
        assert!(!path.exists());
        let (response, path) = prepared(tokio::time::Instant::now() + Duration::from_secs(5));
        assert_eq!(
            &to_bytes(response.into_body(), 100).await.unwrap()[..],
            b"test"
        );
        assert!(!path.exists());
        let (response, path) = prepared(tokio::time::Instant::now());
        assert!(to_bytes(response.into_body(), 100).await.is_err());
        assert!(!path.exists());
    }
}
