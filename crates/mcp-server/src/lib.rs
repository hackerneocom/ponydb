//! Stateless MCP 2026-07-28 message adapter; HTTP validates request metadata headers.
use database_service::Service;
use object_store_api::BlobStore;
use protocol::*;
use request_context::RequestContext;
use serde_json::{json, Value};
pub mod stdio;
pub const VERSION: &str = "2026-07-28";
pub fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    observability::record_result("failed", "INVALID_REQUEST");
    tracing::debug!(target: "ponydb::mcp", event = "mcp_request_rejected", rpc_code = code);
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub async fn dispatch<S: BlobStore + 'static>(
    service: &Service<S>,
    principal: &Principal,
    message: Value,
    ctx: RequestContext,
) -> Value {
    let mut logs = observability::Request::new("mcp_http_dispatch", None);
    let response = logs
        .scope(dispatch_inner(service, principal, message, ctx))
        .await;
    logs.finish("success", "");
    response
}
async fn dispatch_inner<S: BlobStore + 'static>(
    service: &Service<S>,
    principal: &Principal,
    message: Value,
    ctx: RequestContext,
) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if message["jsonrpc"] != "2.0" || !matches!(id, Value::String(_) | Value::Number(_)) {
        return rpc_error(id, -32600, "Invalid JSON-RPC request");
    }
    if let Err(response) = validate_metadata(&message, &[VERSION]) {
        observability::record_result("failed", "INVALID_REQUEST");
        return response;
    }
    dispatch_operation(service, principal, message, ctx).await
}

// Modern metadata is per request, never inherited from a previous message.
fn validate_metadata(message: &Value, supported: &[&str]) -> Result<(), Value> {
    let id = message["id"].clone();
    let meta = &message["params"]["_meta"];
    let Some(version) = meta["io.modelcontextprotocol/protocolVersion"].as_str() else {
        return Err(rpc_error(id, -32602, "Protocol version required"));
    };
    if version != VERSION {
        return Err(json!({"jsonrpc":"2.0","id":id,"error":{
            "code":-32022,"message":"Unsupported protocol version",
            "data":{"supported":supported,"requested":version}
        }}));
    }
    if !meta["io.modelcontextprotocol/clientCapabilities"].is_object()
        || meta
            .get("io.modelcontextprotocol/clientInfo")
            .is_some_and(|info| !info["name"].is_string() || !info["version"].is_string())
    {
        return Err(rpc_error(id, -32602, "Invalid or missing request metadata"));
    }
    Ok(())
}

// Shared tool semantics; each transport validates its own envelope and lifecycle.
async fn dispatch_operation<S: BlobStore + 'static>(
    service: &Service<S>,
    principal: &Principal,
    message: Value,
    ctx: RequestContext,
) -> Value {
    let mut logs = observability::Request::new("mcp", None);
    let response = logs
        .scope(dispatch_operation_inner(service, principal, message, ctx))
        .await;
    logs.finish("success", "");
    response
}
async fn dispatch_operation_inner<S: BlobStore + 'static>(
    service: &Service<S>,
    principal: &Principal,
    message: Value,
    ctx: RequestContext,
) -> Value {
    let id = message["id"].clone();
    let params = &message["params"];
    let result = match message["method"].as_str() {
        Some("server/discover") => {
            json!({"resultType":"complete","supportedVersions":[VERSION],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"ponydb","version":env!("CARGO_PKG_VERSION")}},"instructions":"Repeated writes execute again. Unknown commit outcomes must be reconciled using business data. Partial maintenance requires a new synchronous call."})
        }
        Some("tools/list") => {
            if params.get("cursor").is_some() {
                return rpc_error(id, -32602, "Unknown tool cursor");
            }
            json!({"resultType":"complete","tools":protocol::tools()})
        }
        Some("tools/call") => {
            let Some(name) = params["name"].as_str() else {
                return rpc_error(id, -32602, "Tool name required");
            };
            if !protocol::tools().iter().any(|t| t["name"] == name) {
                return rpc_error(id, -32602, "Unknown tool");
            }
            let op: Operation =
                match serde_json::from_value(json!({"operation":name,"input":params["arguments"]}))
                {
                    Ok(op) => op,
                    Err(_) => return rpc_error(id, -32602, "Invalid tool arguments"),
                };
            let (result, is_error) = match service.call(principal, op, ctx).await {
                Ok(value) => (json!({"ok":true,"result":value}), false),
                Err(e) => (json!({"ok":false,"error":e}), true),
            };
            json!({"resultType":"complete","content":[{"type":"text","text":result.to_string()}],"structuredContent":result,"isError":is_error})
        }
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
