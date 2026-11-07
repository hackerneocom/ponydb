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
