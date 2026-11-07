//! One strict configuration parser for CLI and server, with explicit precedence.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use storage_format::LAYOUT_VERSION;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
macro_rules! section {($name:ident{$($field:ident:$ty:ty=$default:expr),*$(,)?})=>{
    #[derive(Clone,Debug,Serialize,Deserialize)]#[serde(default,deny_unknown_fields)]pub struct $name{$(pub $field:$ty,)*}
    impl Default for $name{fn default()->Self{Self{$($field:$default,)*}}}
}}
section!(Storage {
    backend: String = "s3".into(),
    endpoint: String = String::new(),
    region: String = "us-east-1".into(),
    force_path_style: bool = true,
    bucket: String = String::new(),
    prefix: String = String::new(),
    layout_version: u32 = LAYOUT_VERSION,
    require_atomic_conditional_writes: bool = true,
    credentials: Credentials = Credentials::default()
});
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &"[REDACTED]")
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}
section!(Engine {
    kind: String = "sqlite".into(),
    vfs: String = "s3_versioned".into(),
    journal_mode: String = "DELETE".into(),
    synchronous: String = "FULL".into(),
    foreign_keys: bool = true,
    mmap_size: u64 = 0,
    worker_threads: usize = 4,
    max_queued_requests: usize = 64,
    max_active_writers_per_database_per_instance: usize = 1
});
section!(Cache{memory_budget_mib:usize=256,disk_budget_mib:u64=2048,segment_size_kib:usize=256,directory:Option<PathBuf>=None});
section!(Runtime {
    work_disk_budget_mib: u64 = 2048
});
section!(Transactions {
    max_duration_ms: u64 = 10000,
    max_dirty_bytes: usize = 16777216,
    max_statements: usize = 100
});
section!(Query {
    default_max_rows: usize = 1000,
    max_rows: usize = 1000,
    max_result_bytes: usize = 1048576
});
section!(Requests {
    default_timeout_ms: u64 = 90000,
    max_timeout_ms: u64 = 1800000
});
section!(Maintenance {
    execution: String = "synchronous".into(),
    auto_before_sql: bool = true,
    auto_budget_ms: u64 = 2000,
    gc_default_budget_ms: u64 = 60000,
    max_read_duration_ms: u64 = 10000,
    checkpoint_overlay_bytes: u64 = 8388608,
    checkpoint_transaction_count: u64 = 64,
    overlay_hard_limit_bytes: u64 = 67108864,
    compact_max_segments_per_batch: usize = 32,
    gc_max_objects_per_call: usize = 1000
});
section!(Snapshots {
    retention_days: u32 = 7,
    max_retained_snapshots: u64 = 1000000
});
section!(Exports {
    max_file_bytes: u64 = 10737418240
});
section!(Logging {
    level: String = "warn".into(),
    format: String = "json".into()
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthIdentity {
    pub id: String,
    pub token: String,
    pub databases: Vec<String>,
    pub scopes: Vec<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuth {
    pub principals: Vec<AuthIdentity>,
}
impl ServerAuth {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        let mut tokens = std::collections::HashSet::new();
        for p in &self.principals {
            if p.id.is_empty()
                || p.token.len() < 16
                || p.databases.is_empty()
                || p.scopes.is_empty()
                || !ids.insert(&p.id)
                || !tokens.insert(&p.token)
            {
                return Err("Invalid or duplicate authentication identity".into());
            }
        }
        if self.principals.is_empty() {
            return Err("At least one authentication identity required".into());
        }
        Ok(())
    }
}
impl std::fmt::Debug for ServerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAuth")
            .field("principals", &"[REDACTED]")
            .finish()
    }
}
section!(Server {
    listen: String = "127.0.0.1:8080".into(),
    auth: ServerAuth = ServerAuth::default()
});
section!(Mcp {
    enabled: bool = true,
    path: String = "/mcp".into(),
    protocol_version: String = "2026-07-28".into()
});
section!(Cli {
    output: String = "json".into()
});
section!(AppConfig {
    config_version: u32 = 1,
    storage: Storage = Storage::default(),
    engine: Engine = Engine::default(),
    cache: Cache = Cache::default(),
    runtime: Runtime = Runtime::default(),
    transactions: Transactions = Transactions::default(),
    query: Query = Query::default(),
    requests: Requests = Requests::default(),
    maintenance: Maintenance = Maintenance::default(),
    snapshots: Snapshots = Snapshots::default(),
    exports: Exports = Exports::default(),
    logging: Logging = Logging::default(),
    server: Server = Server::default(),
    mcp: Mcp = Mcp::default(),
    cli: Cli = Cli::default()
});
fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
impl AppConfig {
    pub fn parse(text: &str) -> Result<Self, Error> {
        // TOML errors can include source lines or invalid values containing secrets.
        let config: Self = toml::from_str(text).map_err(|e: toml::de::Error| {
            invalid(match e.span() {
                Some(span) => format!(
                    "Invalid TOML configuration at bytes {}..{}",
                    span.start, span.end
                ),
                None => "Invalid TOML configuration".into(),
            })
        })?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.storage.backend != "s3" {
            return Err(invalid("UNSUPPORTED_BACKEND: only s3 is implemented"));
        }
        if self.config_version != 1
            || self.storage.layout_version != LAYOUT_VERSION
            || !self.storage.require_atomic_conditional_writes
            || self.engine.kind != "sqlite"
            || self.engine.vfs != "s3_versioned"
            || self.engine.journal_mode != "DELETE"
            || self.engine.synchronous != "FULL"
            || !self.engine.foreign_keys
            || self.engine.mmap_size != 0
            || self.engine.max_active_writers_per_database_per_instance != 1
        {
            return Err(invalid("Unsupported protocol or SQLite settings"));
        }
        if self.engine.worker_threads == 0
            || self.engine.worker_threads > 64
            || self.engine.max_queued_requests == 0
            || self.engine.max_queued_requests > 100000
            || self.cache.memory_budget_mib == 0
            || self.cache.memory_budget_mib > 65536
            || self.cache.disk_budget_mib > 1048576
            || self.runtime.work_disk_budget_mib > 1048576
            || !matches!(self.cache.segment_size_kib, 64 | 128 | 256 | 512 | 1024)
        {
            return Err(invalid("Invalid engine/cache budget"));
        }
        if self.transactions.max_duration_ms == 0
            || self.transactions.max_duration_ms > 60000
            || self.transactions.max_dirty_bytes < 65536
            || self.transactions.max_dirty_bytes > 1024 * 1024 * 1024
            || self.transactions.max_statements == 0
            || self.transactions.max_statements > 10000
        {
            return Err(invalid("Invalid transaction budget"));
        }
        if self.query.default_max_rows == 0
            || self.query.default_max_rows > self.query.max_rows
            || self.query.max_rows > 1000000
            || self.query.max_result_bytes == 0
            || self.query.max_result_bytes > 64 * 1024 * 1024
        {
            return Err(invalid("Invalid query budget"));
        }
        if self.requests.default_timeout_ms == 0
            || self.requests.default_timeout_ms > self.requests.max_timeout_ms
            || self.requests.max_timeout_ms > 1800000
        {
            return Err(invalid("Invalid request timeout"));
        }
        if self.maintenance.execution != "synchronous"
            || self.maintenance.max_read_duration_ms == 0
            || self.maintenance.max_read_duration_ms > 60000
            || self.maintenance.gc_default_budget_ms == 0
            || self.maintenance.gc_default_budget_ms > self.requests.max_timeout_ms
            || self.maintenance.gc_max_objects_per_call == 0
            || self.maintenance.gc_max_objects_per_call > 1000
            || self.maintenance.compact_max_segments_per_batch == 0
            || self.maintenance.compact_max_segments_per_batch > 32
        {
            return Err(invalid("Invalid maintenance settings"));
        }
        if self.snapshots.retention_days > 36500
            || self.exports.max_file_bytes == 0
            || self.snapshots.max_retained_snapshots == 0
        {
            return Err(invalid("Invalid snapshot/export budget"));
        }
        if self.server.listen.parse::<std::net::SocketAddr>().is_err()
            || self.mcp.path != "/mcp"
            || self.mcp.protocol_version != "2026-07-28"
            || !matches!(self.cli.output.as_str(), "json" | "table")
            || !matches!(
                self.logging.level.as_str(),
                "trace" | "debug" | "info" | "warn" | "error"
            )
            || !matches!(self.logging.format.as_str(), "json" | "text")
        {
            return Err(invalid("Invalid entrypoint/logging settings"));
        }
        Ok(())
    }
    pub fn load(path: Option<&Path>, overrides: BTreeMap<String, String>) -> Result<Self, Error> {
        let explicit = path
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("PONYDB_CONFIG").map(PathBuf::from));
        let selected = explicit.clone().unwrap_or_else(default_path);
        let mut base = match std::fs::read_to_string(&selected) {
            Ok(text) => {
                let mut c = Self::parse(&text)?;
                c.resolve_paths(selected.parent().unwrap_or(Path::new(".")));
                serde_json::to_value(c).unwrap()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
                serde_json::to_value(Self::default()).unwrap()
            }
            Err(e) => return Err(e.into()),
        };
        apply_env(&mut base, std::env::vars())?;
        for (key, value) in overrides {
            set(&mut base, &key, &value)?;
        }
        let c: Self = serde_json::from_value(base)
            .map_err(|_| invalid("Invalid configuration field type"))?;
        c.validate()?;
        Ok(c)
    }
    fn resolve_paths(&mut self, base: &Path) {
        if let Some(path) = &mut self.cache.directory {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
    }
    pub fn cache_directory(&self) -> Option<PathBuf> {
        self.cache
            .directory
            .clone()
            .or_else(|| default_cache_directory(|key| std::env::var_os(key).map(PathBuf::from)))
    }
    pub fn service_options(&self) -> database_service::ServiceOptions {
        database_service::ServiceOptions {
            disk_cache: if self.cache.disk_budget_mib == 0 {
                None
            } else {
                let directory = self.cache_directory();
                if directory.is_none() {
                    tracing::warn!(target: "ponydb::cache", event = "disk_cache_degraded", reason = "user_cache_directory_unavailable");
                }
                directory.map(|directory| versioned_store::DiskCacheOptions {
                    directory,
                    max_bytes: self.cache.disk_budget_mib * 1024 * 1024,
                })
            },
            workers: self.engine.worker_threads,
            queue: self.engine.max_queued_requests,
            execution: engine_sqlite::ExecutionLimits {
                max_dirty_bytes: self.transactions.max_dirty_bytes,
                max_local_bytes: self.runtime.work_disk_budget_mib * 1024 * 1024,
                max_result_bytes: self.query.max_result_bytes,
                max_result_rows: self.query.max_rows,
                max_statements: self.transactions.max_statements,
                cache_bytes: self.cache.memory_budget_mib * 1024 * 1024
                    / self.engine.worker_threads,
                ..Default::default()
            },
            segment_size: self.cache.segment_size_kib * 1024,
            retention_days: self.snapshots.retention_days,
            max_history: self.snapshots.max_retained_snapshots,
            export_max_bytes: self.exports.max_file_bytes,
            max_read_ms: self.maintenance.max_read_duration_ms,
            gc_default_budget_ms: self.maintenance.gc_default_budget_ms,
            gc_max_objects: self.maintenance.gc_max_objects_per_call,
            max_request_ms: self.requests.max_timeout_ms,
            overlay_hard_limit: self.maintenance.overlay_hard_limit_bytes,
        }
    }
    pub fn s3(&self) -> Result<object_store_s3::S3Store, Error> {
        if self.storage.endpoint.is_empty() || self.storage.bucket.is_empty() {
            return Err(invalid("storage.endpoint and storage.bucket required"));
        }
        let c = &self.storage.credentials;
        if c.access_key_id.trim().is_empty() || c.secret_access_key.trim().is_empty() {
            return Err(invalid(
                "storage.credentials.access_key_id and storage.credentials.secret_access_key required",
            ));
        }
        let credentials = object_store_s3::Credentials::new(
            c.access_key_id.clone(),
            c.secret_access_key.clone(),
            c.session_token.clone(),
            None,
            "config",
        );
        object_store_s3::S3Store::new(
            object_store_s3::S3Config {
                endpoint: self.storage.endpoint.clone(),
                region: self.storage.region.clone(),
                force_path_style: self.storage.force_path_style,
                bucket: self.storage.bucket.clone(),
                prefix: self.storage.prefix.clone(),
                max_object_bytes: 8 * 1024 * 1024,
                timeout: Duration::from_secs(10),
            },
            credentials,
        )
        .map_err(|_| invalid("Invalid S3 connection configuration"))
    }
    pub fn namespace(&self) -> String {
        format!(
            "s3:{}:{}:{}",
            self.storage.endpoint.trim_end_matches('/'),
            self.storage.bucket,
            self.storage.prefix.trim_matches('/')
        )
    }
    pub fn effective(&self, entry: &str) -> serde_json::Value {
        let mut config = serde_json::to_value(self).unwrap();
        config["cache"]["directory"] = serde_json::json!(self.cache_directory());
        let credentials = &mut config["storage"]["credentials"];
        credentials["access_key_id"] = serde_json::json!("[REDACTED]");
        credentials["secret_access_key"] = serde_json::json!("[REDACTED]");
        if self.storage.credentials.session_token.is_some() {
            credentials["session_token"] = serde_json::json!("[REDACTED]");
        }
        for principal in config["server"]["auth"]["principals"]
            .as_array_mut()
            .unwrap()
        {
            principal["token"] = serde_json::json!("[REDACTED]");
        }
        serde_json::json!({"config":config,"entry":entry,"inactive":match entry{"cli"=>vec!["server","server.auth","mcp"],"server"=>vec!["cli"],_=>vec![]}})
    }
}
fn apply_env(
    root: &mut serde_json::Value,
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<(), Error> {
    let mut overrides = BTreeMap::new();
    for (name, value) in vars {
        let Some(key) = name.strip_prefix("PONYDB_") else {
            continue;
        };
        let key = key.to_ascii_lowercase().replace("__", ".");
        if key == "config" {
            continue;
        }
        if overrides.insert(key.clone(), value).is_some() {
            return Err(invalid(format!("Duplicate configuration override: {key}")));
        }
    }
    // Sorting paths makes parent overrides precede their more specific children.
    for (key, value) in overrides {
        set(root, &key, &value)?;
    }
    Ok(())
}

fn set(root: &mut serde_json::Value, key: &str, value: &str) -> Result<(), Error> {
    let mut target = root;
    for part in key.split('.') {
        target = match target {
            serde_json::Value::Array(items) => part
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get_mut(index)),
            _ => target.get_mut(part),
        }
        .ok_or_else(|| invalid(format!("Unknown configuration key: {key}")))?;
    }
    let replacement = match target {
        serde_json::Value::String(_) | serde_json::Value::Null => {
            serde_json::Value::String(value.into())
        }
        serde_json::Value::Bool(_) => serde_json::Value::Bool(
            value
                .parse()
                .map_err(|_| invalid(format!("Invalid bool: {key}")))?,
        ),
        serde_json::Value::Number(_) => serde_json::json!(value
            .parse::<u64>()
            .map_err(|_| invalid(format!("Invalid unsigned integer: {key}")))?),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            let parsed: serde_json::Value =
                serde_json::from_str(value).map_err(|_| invalid(format!("Invalid JSON: {key}")))?;
            if target.is_object() != parsed.is_object() || target.is_array() != parsed.is_array() {
                return Err(invalid(format!("Invalid configuration field type: {key}")));
            }
            parsed
        }
    };
    merge_override(target, replacement);
    Ok(())
}

fn merge_override(target: &mut serde_json::Value, replacement: serde_json::Value) {
    if let (Some(current), serde_json::Value::Object(incoming)) =
        (target.as_object_mut(), &replacement)
    {
        for (key, value) in incoming {
            merge_override(
                current
                    .entry(key.clone())
                    .or_insert(serde_json::Value::Null),
                value.clone(),
            );
        }
    } else {
        *target = replacement;
    }
}
pub fn default_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("ponydb/config.toml")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gc_defaults_and_layout_validation() {
        let mut config = AppConfig::default();
        assert_eq!(LAYOUT_VERSION, 1);
        assert_eq!(config.storage.layout_version, LAYOUT_VERSION);
        assert_eq!(config.service_options().gc_default_budget_ms, 60000);
        assert_eq!(config.requests.default_timeout_ms, 90000);
        config.maintenance.gc_default_budget_ms = 300000;
        assert!(config.validate().is_ok());
        config.maintenance.gc_default_budget_ms = config.requests.max_timeout_ms + 1;
        assert!(config.validate().is_err());
        config.maintenance.gc_default_budget_ms = 60000;
        config.storage.layout_version = 2;
        assert!(config.validate().is_err());
    }

    #[test]
    fn database_context_is_not_configuration() {
        let config = AppConfig::default();
        assert!(serde_json::to_value(&config)
            .unwrap()
            .get("context")
            .is_none());
        assert!(toml::from_str::<AppConfig>("[context]\ndatabase = \"notes\"").is_err());
    }

    #[test]
    fn generic_environment_overrides() {
        let vars = vec![
            (
                "PONYDB_SERVER.AUTH.PRINCIPALS.0.TOKEN",
                "overridden-secret-token",
            ),
            (
                "PONYDB_SERVER__AUTH__PRINCIPALS",
                r#"[{"id":"admin","token":"initial-secret-token","databases":["*"],"scopes":["*"]}]"#,
            ),
            (
                "PONYDB_STORAGE",
                r#"{"bucket":"env-bucket","credentials":{"access_key_id":"00123"}}"#,
            ),
            ("PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY", "true"),
            ("PONYDB_STORAGE__FORCE_PATH_STYLE", "false"),
            ("PONYDB_ENGINE__WORKER_THREADS", "8"),
            ("PONYDB_MCP.ENABLED", "false"),
            ("PONYDB_CACHE__DIRECTORY", "./cache"),
            ("PONYDB_CONFIG", "ignored.toml"),
            ("UNRELATED", "ignored"),
        ];
        let mut root = serde_json::to_value(AppConfig::default()).unwrap();
        root["storage"]["prefix"] = serde_json::json!("existing/");
        apply_env(
            &mut root,
            vars.into_iter().map(|(k, v)| (k.into(), v.into())),
        )
        .unwrap();
        let config: AppConfig = serde_json::from_value(root).unwrap();
        config.validate().unwrap();
        config.server.auth.validate().unwrap();
        assert_eq!(config.storage.bucket, "env-bucket");
        assert!(!config.storage.force_path_style);
        assert_eq!(config.storage.prefix, "existing/");
        assert_eq!(config.storage.credentials.access_key_id, "00123");
        assert_eq!(config.storage.credentials.secret_access_key, "true");
        assert_eq!(config.engine.worker_threads, 8);
        assert!(!config.mcp.enabled);
        assert_eq!(config.cache.directory, Some(PathBuf::from("./cache")));
        assert_eq!(
            config.server.auth.principals[0].token,
            "overridden-secret-token"
        );
    }

    #[test]
    fn generic_json_overrides_are_strict_and_redacted() {
        for (key, value) in [
            ("server.auth", r#"{"principals":[]}"#),
            ("server.auth.principals", "[]"),
            ("storage.credentials", r#"{"secret_access_key":"secret"}"#),
        ] {
            let mut root = serde_json::to_value(AppConfig::default()).unwrap();
            set(&mut root, key, value).unwrap();
            serde_json::from_value::<AppConfig>(root).unwrap();
        }
        for (key, value) in [
            ("storage", "secret-invalid-json"),
            ("storage", "[]"),
            ("server.auth.principals", "{}"),
            ("server.auth.principals.0.token", "secret"),
            ("unknown", "secret"),
        ] {
            let mut root = serde_json::to_value(AppConfig::default()).unwrap();
            let error = set(&mut root, key, value).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        for value in [r#"{"unknown":"secret"}"#, r#"{"bucket":123}"#] {
            let mut root = serde_json::to_value(AppConfig::default()).unwrap();
            set(&mut root, "storage", value).unwrap();
            assert!(serde_json::from_value::<AppConfig>(root).is_err());
        }
        let mut root = serde_json::to_value(AppConfig::default()).unwrap();
        let error = apply_env(
            &mut root,
            [
                ("PONYDB_STORAGE__BUCKET".into(), "first-secret".into()),
                ("PONYDB_STORAGE.BUCKET".into(), "second-secret".into()),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Duplicate configuration override"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn inline_auth_debug_is_redacted() {
        let mut c = AppConfig::default();
        c.server.auth.principals.push(AuthIdentity {
            id: "reader".into(),
            token: "sensitive-token".into(),
            databases: vec!["*".into()],
            scopes: vec!["*".into()],
        });
        assert!(!format!("{c:?}").contains("sensitive-token"));
        assert!(!c.effective("all").to_string().contains("sensitive-token"));
    }
    #[test]
    fn strict_config_and_inactive_sections() {
        assert!(AppConfig::parse("tenant_id='old'").is_err());
        assert!(AppConfig::parse("[engine]\nworker_thread=2").is_err());
        assert!(AppConfig::parse("[storage]\nbackend='gcs'").is_err());
        assert!(AppConfig::parse("[query]\ndefault_max_rows=2000\nmax_rows=1000").is_err());
        assert!(AppConfig::parse("[credentials]\nprovider='env'").is_err());
        assert!(AppConfig::parse("[storage.credentials]\nprovider='unknown'").is_err());
        assert!(AppConfig::parse("[storage.credentials]\nprovider='file'").is_err());
        assert!(AppConfig::parse("[storage.credentials]\nfile='c.json'").is_err());
        assert!(AppConfig::parse("[server.auth]\nfile='old.json'").is_err());
        assert!(AppConfig::parse("[server.auth]\nmode='bearer_file'").is_err());
        assert!(AppConfig::parse("[server.auth]\njson='{}'").is_err());
        let c = AppConfig::parse("[server.auth]\nprincipals=[]").unwrap();
        assert!(c.effective("cli")["inactive"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("server.auth")));
    }

    #[test]
    fn export_limit_is_positive_and_maps_to_service_options() {
        let mut config = AppConfig::default();
        assert_eq!(config.service_options().export_max_bytes, 10737418240);
        config.exports.max_file_bytes = 0;
        assert!(config.validate().is_err());
        config.exports.max_file_bytes = 4096;
        config.validate().unwrap();
        assert_eq!(config.service_options().export_max_bytes, 4096);
    }
    #[test]
    fn storage_credentials_config_overrides_and_redaction() {
        let mut c = AppConfig::parse(
            "[storage.credentials]\naccess_key_id='test-access-key'\nsecret_access_key='test-secret-key'\nsession_token='test-session-token'",
        )
        .unwrap();
        let mut value = serde_json::to_value(&c).unwrap();
        set(
            &mut value,
            "storage.credentials.access_key_id",
            "override-access-key",
        )
        .unwrap();
        set(
            &mut value,
            "storage.credentials.secret_access_key",
            "override-secret-key",
        )
        .unwrap();
        set(
            &mut value,
            "storage.credentials.session_token",
            "override-session-token",
        )
        .unwrap();
        c = serde_json::from_value(value.clone()).unwrap();
        c.validate().unwrap();
        assert_eq!(c.storage.credentials.access_key_id, "override-access-key");
        assert_eq!(
            c.storage.credentials.secret_access_key,
            "override-secret-key"
        );
        assert_eq!(
            c.storage.credentials.session_token.as_deref(),
            Some("override-session-token")
        );
        assert!(set(&mut value, "storage.credentials.provider", "env").is_err());
        assert!(set(&mut value, "storage.credentials.file", "credentials.json").is_err());
        let effective = c.effective("all");
        assert!(effective["config"].get("credentials").is_none());
        for key in ["access_key_id", "secret_access_key", "session_token"] {
            assert_eq!(
                effective["config"]["storage"]["credentials"][key],
                "[REDACTED]"
            );
        }
        let debug = format!("{c:?}");
        for secret in [
            "override-access-key",
            "override-secret-key",
            "override-session-token",
        ] {
            assert!(!debug.contains(secret));
            assert!(!effective.to_string().contains(secret));
        }
    }
    #[test]
    fn missing_storage_credentials_fail_when_opening_s3() {
        let mut c = AppConfig::default();
        c.storage.endpoint = "https://s3.example.com".into();
        c.storage.bucket = "test-bucket".into();
        for (access, secret) in [
            ("", ""),
            ("test-access-key", ""),
            ("", "test-secret-key"),
            (" ", "test-secret-key"),
        ] {
            c.storage.credentials.access_key_id = access.into();
            c.storage.credentials.secret_access_key = secret.into();
            let error = c.s3().err().unwrap().to_string();
            assert!(error.contains("storage.credentials.access_key_id"));
            assert!(error.contains("storage.credentials.secret_access_key"));
            assert!(!error.contains("test-secret-key"));
        }
    }
    #[test]
    fn credential_parse_errors_do_not_echo_secrets() {
        for text in [
            "[storage.credentials]\nsecret_access_key=['test-secret-key']",
            "[storage.credentials]\nsecret_access_key='test-secret-key' invalid",
            "[storage.credentials]\nsession_token={unexpected='test-session-token'}",
        ] {
            let error = AppConfig::parse(text).unwrap_err().to_string();
            assert!(!error.contains("test-secret-key"));
            assert!(!error.contains("test-session-token"));
        }
    }
    #[test]
    fn storage_addressing_defaults_and_strict_types() {
        assert!(AppConfig::default().storage.force_path_style);
        assert!(AppConfig::parse("").unwrap().storage.force_path_style);
        for enabled in [true, false] {
            let c = AppConfig::parse(&format!("[storage]\nforce_path_style = {enabled}")).unwrap();
            assert_eq!(c.storage.force_path_style, enabled);
            assert_eq!(
                c.effective("all")["config"]["storage"]["force_path_style"],
                enabled
            );
        }
        for value in ["'false'", "0", "[]"] {
            assert!(AppConfig::parse(&format!("[storage]\nforce_path_style = {value}")).is_err());
        }
        let mut root = serde_json::to_value(AppConfig::default()).unwrap();
        assert!(set(&mut root, "storage.force_path_style", "invalid").is_err());
    }
    #[test]
    fn shared_example_uses_nested_storage_credentials() {
        let c = AppConfig::parse(include_str!("../../../examples/ponydb.toml")).unwrap();
        assert_eq!(c.storage.endpoint, "https://<s3-endpoint>");
        assert_eq!(c.storage.bucket, "serverless-db");
        assert_eq!(c.storage.credentials.access_key_id, "<access-key-id>");
        assert_eq!(
            c.storage.credentials.secret_access_key,
            "<secret-access-key>"
        );
        assert!(c.storage.credentials.session_token.is_none());
        let defaults = AppConfig::default();
        assert_eq!(c.storage.region, defaults.storage.region);
        assert_eq!(c.storage.prefix, defaults.storage.prefix);
        assert_eq!(c.engine.worker_threads, defaults.engine.worker_threads);
        assert_eq!(c.server.listen, defaults.server.listen);
        assert!(c.server.auth.principals.is_empty());
        assert_eq!(c.cli.output, defaults.cli.output);
    }
}

fn default_cache_directory(get: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let absolute = |key| get(key).filter(|path| path.is_absolute());
    if cfg!(target_os = "windows") {
        absolute("LOCALAPPDATA").map(|p| p.join("ponydb"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|p| p.join("Library/Caches/ponydb"))
    } else {
        absolute("XDG_CACHE_HOME")
            .map(|p| p.join("ponydb"))
            .or_else(|| absolute("HOME").map(|p| p.join(".cache/ponydb")))
    }
}

#[cfg(test)]
mod disk_cache_tests {
    use super::*;
    #[test]
    fn budgets_are_independent_and_zero_disables_only_disk_cache() {
        let c = AppConfig::parse("[cache]\ndisk_budget_mib=0\n[runtime]\nwork_disk_budget_mib=17")
            .unwrap();
        assert!(c.service_options().disk_cache.is_none());
        assert_eq!(
            c.service_options().execution.max_local_bytes,
            17 * 1024 * 1024
        );
        let c = AppConfig::parse("[cache]\ndirectory='/tmp/ponydb-test-cache'\ndisk_budget_mib=23")
            .unwrap();
        let options = c.service_options();
        assert_eq!(options.disk_cache.unwrap().max_bytes, 23 * 1024 * 1024);
        assert_eq!(options.execution.max_local_bytes, 2048 * 1024 * 1024);
        assert!(AppConfig::parse("[runtime]\nwork_disk_budget_mib=1048577").is_err());
    }
    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn defaults_require_absolute_environment_paths() {
        assert_eq!(
            default_cache_directory(|key| match key {
                "XDG_CACHE_HOME" => Some("relative".into()),
                "HOME" => Some("/home/test".into()),
                _ => None,
            }),
            Some("/home/test/.cache/ponydb".into())
        );
        assert_eq!(default_cache_directory(|_| None), None);
        assert_eq!(
            default_cache_directory(
                |key| (key == "XDG_CACHE_HOME").then(|| PathBuf::from("/cache"))
            ),
            Some("/cache/ponydb".into())
        );
    }
}
