//! Stateful, newline-delimited MCP transport for local agent clients.
use super::{dispatch_operation, rpc_error, validate_metadata};
use database_service::Service;
use object_store_api::BlobStore;
use protocol::Principal;
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{collections::HashMap, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinSet,
};

pub const VERSION: &str = super::VERSION;
const LEGACY_VERSION: &str = "2025-11-25";
const LEGACY_VERSIONS: &[&str] = &[LEGACY_VERSION, "2025-06-18", "2025-03-26", "2024-11-05"];
pub const VERSIONS: &[&str] = &[
    VERSION,
    LEGACY_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 32;

#[derive(Default)]
struct Session {
    version: Option<String>,
    ready: bool,
}

enum Action {
    Reply(Value),
    Dispatch(Value, String),
    Cancel(Value),
    Ignore,
}

impl Session {
    fn receive(&mut self, message: Value) -> Action {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        if !message.is_object()
            || message["jsonrpc"] != "2.0"
            || !message["method"].is_string()
            || (message.get("id").is_some() && !(id.is_string() || id.is_i64() || id.is_u64()))
            || message.get("params").is_some_and(|p| !p.is_object())
        {
            return Action::Reply(rpc_error(Value::Null, -32600, "Invalid JSON-RPC request"));
        }
        let method = message["method"].as_str().unwrap();
        if message.get("id").is_none() {
            return match method {
                "notifications/initialized" if self.version.is_some() => {
                    self.ready = true;
                    Action::Ignore
                }
                "notifications/cancelled" => Action::Cancel(message["params"]["requestId"].clone()),
                _ => Action::Ignore,
            };
        }
        let meta = &message["params"]["_meta"];
        let modern = meta
            .get("io.modelcontextprotocol/protocolVersion")
            .is_some()
            || meta
                .get("io.modelcontextprotocol/clientCapabilities")
                .is_some()
            || method == "server/discover"
            || (self.version.is_none() && method != "initialize");
        if modern {
            if let Err(response) = validate_metadata(&message, VERSIONS) {
                return Action::Reply(response);
            }
            return match method {
                "server/discover" | "tools/list" | "tools/call" => {
                    Action::Dispatch(message, VERSION.into())
                }
                "ping" => Action::Reply(
                    json!({"jsonrpc":"2.0","id":id,"result":{"resultType":"complete"}}),
                ),
                _ => Action::Reply(rpc_error(id, -32601, "Method not found")),
            };
        }
        if method == "ping" {
            return Action::Reply(json!({"jsonrpc":"2.0","id":id,"result":{}}));
        }
        if method == "initialize" {
            let params = &message["params"];
            if self.version.is_some() {
                return Action::Reply(rpc_error(id, -32600, "Already initialized"));
            }
            if !params["protocolVersion"].is_string()
                || !params["capabilities"].is_object()
                || !params["clientInfo"]["name"].is_string()
                || !params["clientInfo"]["version"].is_string()
            {
                return Action::Reply(rpc_error(id, -32602, "Invalid initialization parameters"));
            }
            let requested = params["protocolVersion"].as_str().unwrap();
            let version = if LEGACY_VERSIONS.contains(&requested) {
                requested
            } else {
                LEGACY_VERSION
            };
            self.version = Some(version.to_owned());
            return Action::Reply(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":version,"capabilities":{"tools":{}},
                "serverInfo":{"name":"ponydb","version":env!("CARGO_PKG_VERSION")},
                "instructions":"Repeated writes execute again. Unknown commit outcomes must be reconciled using business data. Partial maintenance requires a new synchronous call. Supply database IDs explicitly in tool arguments."
            }}));
        }
        if !self.ready {
            return Action::Reply(rpc_error(id, -32600, "Initialization required"));
        }
        match method {
            "tools/list" | "tools/call" => Action::Dispatch(message, self.version.clone().unwrap()),
            _ => Action::Reply(rpc_error(id, -32601, "Method not found")),
        }
    }
}

async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

// Retain the buffer across select iterations to make reads cancellation safe.
// fill_buf/consume bounds allocation even for input without a newline.
async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
) -> io::Result<bool> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!bytes.is_empty());
        }
        let end = available.iter().position(|b| *b == b'\n');
        let count = end.map_or(available.len(), |n| n + 1);
        if bytes.len() + count > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds 8 MiB",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(true);
        }
    }
}

/// Serve until stdin closes. Calls share the service and have independent deadlines.
/// Dropping the transport cancels outstanding contexts, including on I/O failure.
pub async fn serve<S, R, W>(
    service: Arc<Service<S>>,
    principal: Principal,
    reader: R,
    writer: W,
    deadline: Duration,
) -> io::Result<()>
where
    S: BlobStore + 'static,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let result = serve_inner(service.clone(), principal, reader, writer, deadline).await;
    // serve_inner drops all request guards before waiting for GC cleanup.
    service.wait_for_gc().await;
    result
}

async fn serve_inner<S, R, W>(
    service: Arc<Service<S>>,
    principal: Principal,
    mut reader: R,
    mut writer: W,
    deadline: Duration,
) -> io::Result<()>
where
    S: BlobStore + 'static,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    tracing::debug!(target: "ponydb::mcp", event = "stdio_started");
    struct InFlight {
        ctx: RequestContext,
        modern: bool,
        cancelled: bool,
    }
    struct Pending(HashMap<String, InFlight>);
    impl Drop for Pending {
        fn drop(&mut self) {
            for call in self.0.values() {
                call.ctx.cancel();
            }
        }
    }
    let mut session = Session::default();
    let mut pending = Pending(HashMap::new());
    let mut tasks: JoinSet<(String, String, Value)> = JoinSet::new();
    let mut bytes = Vec::new();
    loop {
        tokio::select! {
            completed = tasks.join_next(), if !tasks.is_empty() => {
                let (key, version, mut response) = completed.unwrap().map_err(io::Error::other)?;
                if pending.0.remove(&key).is_some_and(|call| call.modern && call.cancelled) {
                    continue;
                }
                if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
                    if version != VERSION {
                        result.remove("resultType");
                    } else {
                        result.insert("_meta".into(), json!({"io.modelcontextprotocol/serverInfo":{"name":"ponydb","version":env!("CARGO_PKG_VERSION")}}));
                        if result.contains_key("supportedVersions") {
                            result.insert("supportedVersions".into(), json!(VERSIONS));
                        }
                    }
                    if matches!(version.as_str(), "2024-11-05" | "2025-03-26") {
                        result.remove("structuredContent");
                        if let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) {
                            for tool in tools {
                                tool.as_object_mut().unwrap().remove("outputSchema");
                                if version == "2024-11-05" {
                                    tool.as_object_mut().unwrap().remove("annotations");
                                }
                            }
                        }
                    }
                }
                write_message(&mut writer, &response).await?;
            }
            read = read_message(&mut reader, &mut bytes) => {
                if !read? { break; }
                let message = serde_json::from_slice::<Value>(&bytes);
                bytes.clear();
                let action = match message {
                    Ok(value) => session.receive(value),
                    Err(_) => Action::Reply(rpc_error(Value::Null, -32700, "Parse error")),
                };
                match action {
                    Action::Reply(value) => write_message(&mut writer, &value).await?,
                    Action::Ignore => {},
                    Action::Cancel(id) => {
                        if let Some(call) = pending.0.get_mut(&id.to_string()) {
                            call.cancelled = true;
                            call.ctx.cancel();
                        }
                    }
                    Action::Dispatch(mut message, version) => {
                        if message["method"] == "tools/call" && message["params"].get("arguments").is_none() {
                            if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
                                params.insert("arguments".into(), json!({}));
                            }
                        }
                        let key = message["id"].to_string();
                        if pending.0.contains_key(&key) || pending.0.len() >= MAX_IN_FLIGHT {
                            write_message(&mut writer, &rpc_error(message["id"].clone(), -32600, "Duplicate request ID or too many in-flight requests")).await?;
                            continue;
                        }
                        let ctx = RequestContext::new(deadline);
                        pending.0.insert(key.clone(), InFlight { ctx: ctx.clone(), modern: version == VERSION, cancelled: false });
                        let service = service.clone();
                        let principal = principal.clone();
                        tasks.spawn(async move {
                            let response = dispatch_operation(&service, &principal, message, ctx).await;
                            (key, version, response)
                        });
                    }
                }
            }
        }
    }
    tracing::debug!(target: "ponydb::mcp", event = "stdio_stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store_test_support::MemoryStore;
    use tokio::io::{duplex, BufReader};

    fn initialize(version: &str) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}
        }})
    }

    #[test]
    fn lifecycle_and_version_negotiation() {
        for version in LEGACY_VERSIONS.iter().copied().chain(["unknown"]) {
            let mut session = Session::default();
            assert!(
                matches!(session.receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})), Action::Reply(v) if v["error"]["code"] == -32602)
            );
            assert!(
                matches!(session.receive(initialize(version)), Action::Reply(v) if v["result"]["protocolVersion"] == if version == "unknown" { LEGACY_VERSION } else { version })
            );
            assert!(
                matches!(session.receive(initialize(version)), Action::Reply(v) if v["error"].is_object())
            );
            assert!(matches!(
                session.receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
                Action::Ignore
            ));
            assert!(matches!(
                session.receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})),
                Action::Dispatch(_, _)
            ));
            assert!(matches!(
                session.receive(json!({"jsonrpc":"2.0","method":"unknown"})),
                Action::Ignore
            ));
            assert!(
                matches!(session.receive(json!([])), Action::Reply(v) if v["error"]["code"] == -32600)
            );
        }
    }

    async fn exchange<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
        write: &mut W,
        read: &mut R,
        value: Value,
    ) -> Value {
        write_message(write, &value).await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), read.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn transport_discovery_calls_errors_and_eof() {
        let service = Arc::new(
            Service::new(
                Arc::new(MemoryStore::default()),
                "test".into(),
                Default::default(),
            )
            .unwrap(),
        );
        let (client, server) = duplex(65536);
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            service,
            Principal::direct(),
            BufReader::new(sr),
            sw,
            Duration::from_secs(5),
        ));
        let (cr, mut cw) = tokio::io::split(client);
        let mut cr = BufReader::new(cr);
        assert_eq!(
            exchange(&mut cw, &mut cr, initialize(LEGACY_VERSION)).await["result"]
                ["protocolVersion"],
            LEGACY_VERSION
        );
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await
        .unwrap();
        let list = exchange(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}),
        )
        .await;
        assert_eq!(list["id"], "list");
        assert_eq!(
            list["result"]["tools"].as_array().unwrap().len(),
            protocol::tools().len()
        );
        assert!(list["result"].get("resultType").is_none());
        let result = exchange(&mut cw, &mut cr, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"database_list","arguments":{}}})).await;
        assert_eq!(result["result"]["isError"], false);
        assert_eq!(result["result"]["structuredContent"]["ok"], true);
        let invalid = exchange(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"no_such_tool"}}),
        )
        .await;
        assert_eq!(invalid["error"]["code"], -32602);
        let failure = exchange(&mut cw, &mut cr, json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"database_get","arguments":{"database":"missing"}}})).await;
        assert_eq!(failure["result"]["isError"], true);
        cw.write_all(b"not json\n").await.unwrap();
        let mut line = String::new();
        cr.read_line(&mut line).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["error"]["code"],
            -32700
        );
        assert_eq!(
            exchange(
                &mut cw,
                &mut cr,
                json!({"jsonrpc":"2.0","id":6,"method":"ping"})
            )
            .await["result"],
            json!({})
        );
        cw.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        line.clear();
        assert_eq!(cr.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cancellation_does_not_block_ping() {
        let service = Arc::new(
            Service::new(
                Arc::new(MemoryStore::default()),
                "cancel".into(),
                Default::default(),
            )
            .unwrap(),
        );
        let (client, server) = duplex(65536);
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            service,
            Principal::direct(),
            BufReader::new(sr),
            sw,
            Duration::from_secs(5),
        ));
        let (cr, mut cw) = tokio::io::split(client);
        let mut cr = BufReader::new(cr);
        exchange(&mut cw, &mut cr, initialize(LEGACY_VERSION)).await;
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await
        .unwrap();
        let created = exchange(&mut cw, &mut cr, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"database_create","arguments":{"name":"canceltest"}}})).await;
        assert_eq!(created["result"]["isError"], false);
        write_message(&mut cw, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sql_execute","arguments":{"database":"canceltest","sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n"}}})).await.unwrap();
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}),
        )
        .await
        .unwrap();
        write_message(&mut cw, &json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
            .await
            .unwrap();
        let mut replies = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), cr.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        assert!(replies
            .iter()
            .any(|v| v["id"] == 4 && v["result"] == json!({})));
        assert!(replies
            .iter()
            .any(|v| v["id"] == 3 && v["result"]["isError"] == true));
        cw.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    }

    fn modern(mut message: Value) -> Value {
        if !message["params"].is_object() {
            message["params"] = json!({});
        }
        message["params"]["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": VERSION,
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        message
    }

    #[test]
    fn modern_metadata_is_required_on_every_request() {
        let mut session = Session::default();
        let request = modern(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
        assert!(matches!(session.receive(request.clone()), Action::Dispatch(_, v) if v == VERSION));
        for field in [
            "io.modelcontextprotocol/protocolVersion",
            "io.modelcontextprotocol/clientCapabilities",
        ] {
            let mut invalid = request.clone();
            invalid["params"]["_meta"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                matches!(session.receive(invalid), Action::Reply(v) if v["error"]["code"] == -32602)
            );
        }
        let mut unknown = request.clone();
        unknown["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
        assert!(
            matches!(session.receive(unknown), Action::Reply(v) if v["error"]["code"] == -32022 && v["error"]["data"]["supported"] == json!(VERSIONS) && v["error"]["data"]["requested"] == "2099-01-01")
        );
        let mut invalid = request.clone();
        invalid["params"]["_meta"]["io.modelcontextprotocol/clientInfo"] = json!({});
        assert!(
            matches!(session.receive(invalid), Action::Reply(v) if v["error"]["code"] == -32602)
        );
        assert!(matches!(session.receive(request), Action::Dispatch(_, _)));
        // initialize remains a legacy handshake even if it asks for a modern version.
        assert!(
            matches!(session.receive(initialize(VERSION)), Action::Reply(v) if v["result"]["protocolVersion"] == LEGACY_VERSION)
        );
    }

    #[tokio::test]
    async fn modern_direct_calls_discovery_mixed_versions_and_cancellation() {
        let service = Arc::new(
            Service::new(
                Arc::new(MemoryStore::default()),
                "modern".into(),
                Default::default(),
            )
            .unwrap(),
        );
        let (client, server) = duplex(65536);
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            service,
            Principal::direct(),
            BufReader::new(sr),
            sw,
            Duration::from_secs(5),
        ));
        let (cr, mut cw) = tokio::io::split(client);
        let mut cr = BufReader::new(cr);
        // No initialize and no discovery prerequisite. clientInfo is optional.
        let created = exchange(&mut cw, &mut cr, modern(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"database_create","arguments":{"name":"modern"}}}))).await;
        assert_eq!(created["result"]["resultType"], "complete");
        assert_eq!(created["result"]["isError"], false);
        let discover = exchange(
            &mut cw,
            &mut cr,
            modern(json!({"jsonrpc":"2.0","id":2,"method":"server/discover"})),
        )
        .await;
        assert_eq!(discover["result"]["supportedVersions"], json!(VERSIONS));
        assert_eq!(
            discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "ponydb"
        );
        let missing = exchange(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        )
        .await;
        assert_eq!(missing["error"]["code"], -32602);
        // Interleaved legacy and modern calls must each retain their response format.
        exchange(&mut cw, &mut cr, initialize("2024-11-05")).await;
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await
        .unwrap();
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
        )
        .await
        .unwrap();
        write_message(
            &mut cw,
            &modern(json!({"jsonrpc":"2.0","id":5,"method":"tools/list"})),
        )
        .await
        .unwrap();
        for _ in 0..2 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), cr.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            let is_modern = value["id"] == 5;
            assert_eq!(value["result"].get("resultType").is_some(), is_modern);
            assert_eq!(
                value["result"]["tools"][0].get("outputSchema").is_some(),
                is_modern
            );
        }
        write_message(&mut cw, &modern(json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"sql_execute","arguments":{"database":"modern","sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n"}}}))).await.unwrap();
        write_message(
            &mut cw,
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":6}}),
        )
        .await
        .unwrap();
        let ping = exchange(
            &mut cw,
            &mut cr,
            modern(json!({"jsonrpc":"2.0","id":7,"method":"ping"})),
        )
        .await;
        assert_eq!(ping["id"], 7);
        assert_eq!(ping["result"]["resultType"], "complete");
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), cr.read_line(&mut line))
                .await
                .is_err(),
            "cancelled request emitted a response: {line}"
        );
        cw.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn oversized_message_is_bounded() {
        let input = vec![b'x'; MAX_MESSAGE + 1];
        let mut reader = BufReader::new(input.as_slice());
        let mut bytes = Vec::new();
        assert_eq!(
            read_message(&mut reader, &mut bytes)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(bytes.len() <= MAX_MESSAGE);
    }
}
