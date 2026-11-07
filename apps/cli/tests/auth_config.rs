use std::process::Command;

const AUTH: &str = r#"{"principals":[{"id":"reader","token":"test-secret-token-12345","databases":["notes"],"scopes":["data:read"]}]}"#;

fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ponydb"));
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("PONYDB_")) {
        cmd.env_remove(key);
    }
    cmd.env(
        "XDG_CONFIG_HOME",
        std::env::temp_dir().join(format!("ponydb-auth-test-{}", std::process::id())),
    );
    cmd
}

#[test]
fn environment_auth_validation_requires_storage_and_redacts_secrets() {
    let output = command()
        .env("PONYDB_SERVER__AUTH", AUTH)
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("storage.endpoint and storage.bucket required"));
    let output = command()
        .env("PONYDB_SERVER__AUTH", AUTH)
        .args(["config", "show", "--target", "server"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["config"]["server"]["auth"]["principals"][0]["token"],
        "[REDACTED]"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test-secret-token"));
}

#[test]
fn rejects_missing_removed_and_invalid_auth() {
    let output = command()
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = command()
        .env("PONYDB_SERVER__AUTH", AUTH)
        .env("PONYDB_SERVER__AUTH__FILE", "missing.json")
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown configuration key"));
    let output = command()
        .env("PONYDB_SERVER__AUTH__JSON", AUTH)
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    for invalid in [
        "",
        "not-json",
        r#"{"principals":[]}"#,
        r#"{"json":"removed"}"#,
        r#"{"principals":[{"id":"a","token":"valid-long-token","databases":["*"],"scopes":["*"]},{"id":"a","token":"another-long-token","databases":["*"],"scopes":["*"]}]}"#,
        r#"{"principals":[{"id":"a","token":"short","databases":["*"],"scopes":["*"]}]}"#,
    ] {
        let output = command()
            .env("PONYDB_SERVER__AUTH", invalid)
            .args(["config", "validate", "--target", "server"])
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let output = command()
        .env("PONYDB_SERVER__AUTH", r#"{"test-secret-token-12345":true}"#)
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("test-secret-token"));
}

#[test]
fn inline_config_auth_and_environment_override() {
    let path = std::env::temp_dir().join(format!("ponydb-auth-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        r#"[[server.auth.principals]]
id = "reader"
token = "test-secret-token-12345"
databases = ["notes"]
scopes = ["data:read"]
"#,
    )
    .unwrap();
    let output = command()
        .arg("--config")
        .arg(&path)
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("storage.endpoint and storage.bucket required"));
    let output = command()
        .arg("--config")
        .arg(&path)
        .env("PONYDB_SERVER__AUTH", "invalid")
        .args(["config", "validate", "--target", "server"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn generic_environment_objects_arrays_and_dotted_paths() {
    let auth: serde_json::Value = serde_json::from_str(AUTH).unwrap();
    let output = command()
        .env(
            "PONYDB_SERVER__AUTH__PRINCIPALS",
            auth["principals"].to_string(),
        )
        .env("PONYDB_SERVER.AUTH.PRINCIPALS.0.DATABASES", r#"["other"]"#)
        .env(
            "PONYDB_STORAGE",
            r#"{"bucket":"env-bucket","region":"test-region"}"#,
        )
        .env("PONYDB_STORAGE__BUCKET", "specific-bucket")
        .args(["config", "show", "--target", "server"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["config"]["storage"]["bucket"], "specific-bucket");
    assert_eq!(result["config"]["storage"]["region"], "test-region");
    assert_eq!(
        result["config"]["server"]["auth"]["principals"][0]["databases"],
        serde_json::json!(["other"])
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test-secret-token"));
}
