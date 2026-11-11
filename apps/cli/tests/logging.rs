use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn stdio_debug_logs_do_not_pollute_protocol_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
[storage]
endpoint = "http://127.0.0.1:1"
bucket = "test"
[storage.credentials]
access_key_id = "SECRET_ACCESS"
secret_access_key = "SECRET_CREDENTIAL"
[cache]
disk_budget_mib = 0
[logging]
level = "debug"
format = "json"
"#,
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ponydb"));
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("PONYDB_")) {
        command.env_remove(key);
    }
    let mut child = command
        .arg("--config")
        .arg(&config)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"SECRET_CLIENT","version":"1"}}})).unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"ping"})).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let responses: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    for response in &responses {
        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response.get("level").is_none());
    }
    let logs = std::str::from_utf8(&output.stderr).unwrap();
    assert!(logs.contains("stdio_started"));
    assert!(logs.contains("stdio_stopped"));
    assert!(!logs.contains("SECRET_"));
    assert!(!logs.contains("ping"));
    for line in logs.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[test]
fn cli_s3_failure_includes_correlated_sanitized_sdk_diagnostics() {
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        use std::io::Read;
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut request = [0; 1];
        stream.read_exact(&mut request).unwrap();
        // Drop the connection without returning HTTP headers.
    });
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[storage]
endpoint = "{endpoint}"
bucket = "test"
[storage.credentials]
access_key_id = "SECRET_ACCESS"
secret_access_key = "SECRET_CREDENTIAL"
session_token = "SECRET_SESSION"
[cache]
disk_budget_mib = 0
[logging]
level = "debug"
format = "json"
"#
        ),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ponydb"));
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("PONYDB_")) {
        command.env_remove(key);
    }
    let output = command
        .env("HTTP_PROXY", "http://proxy-user:proxy-secret@127.0.0.1:1")
        .env("http_proxy", "http://proxy-user:proxy-secret@127.0.0.1:1")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .arg("--config")
        .arg(&config)
        .args(["db", "notes", "schema", "--output", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let logs = String::from_utf8(output.stderr).unwrap();
    assert!(!logs.contains("SECRET_"), "{logs}");
    assert!(!logs.contains("proxy-user"), "{logs}");
    assert!(!logs.contains("proxy-secret"), "{logs}");
    let events: Vec<Value> = serde_json::Deserializer::from_str(&logs)
        .into_iter::<Value>()
        .map(Result::unwrap)
        .collect();
    let failure = events
        .iter()
        .find(|event| event["fields"]["event"] == "s3_request_failed")
        .expect("S3 failure event");
    let fields = &failure["fields"];
    assert_eq!(fields["operation"], "GetObject");
    assert_eq!(fields["sdk_error_type"], "DispatchFailure");
    assert_eq!(fields["attempts"], 1);
    assert_eq!(fields["endpoint"], format!("{endpoint}/"));
    assert_eq!(fields["hostname"], "127.0.0.1");
    assert_eq!(fields["proxy_used"], false);
    assert_eq!(fields["connect_timeout_ms"], 3100);
    assert_eq!(fields["operation_timeout_ms"], 10000);
    assert_eq!(fields["operation_attempt_timeout_ms"], 10000);
    let chain: Value = serde_json::from_str(fields["error_chain"].as_str().unwrap()).unwrap();
    assert!(chain.as_array().unwrap().len() >= 3);
    let result = events
        .iter()
        .find(|event| event["code"] == "STORAGE_UNAVAILABLE")
        .expect("business error");
    assert_eq!(failure["span"]["request_id"], result["request_id"]);
    server.join().unwrap();
}
