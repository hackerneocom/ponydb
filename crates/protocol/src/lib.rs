//! Shared transport-independent business inputs. Integer values never pass through f64.
use base64::{engine::general_purpose::STANDARD, Engine};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SqlValue {
    Null,
    Integer(String),
    Real(f64),
    Text(String),
    Blob(String),
}
impl SqlValue {
    pub fn to_sql(&self) -> Result<rusqlite::types::Value, String> {
        use rusqlite::types::Value as V;
        Ok(match self {
            Self::Null => V::Null,
            Self::Integer(s) => V::Integer(s.parse().map_err(|_| "invalid 64-bit integer")?),
            Self::Real(n) if n.is_finite() => V::Real(*n),
            Self::Real(_) => return Err("non-finite real".into()),
            Self::Text(s) => V::Text(s.clone()),
            Self::Blob(b) => V::Blob(STANDARD.decode(b).map_err(|_| "invalid base64")?),
        })
    }
    pub fn from_sql(value: rusqlite::types::Value) -> Result<Self, String> {
        use rusqlite::types::Value as V;
        Ok(match value {
            V::Null => Self::Null,
            V::Integer(n) => Self::Integer(n.to_string()),
            V::Real(n) if n.is_finite() => Self::Real(n),
            V::Real(_) => return Err("non-finite SQLite result".into()),
            V::Text(s) => Self::Text(s),
            V::Blob(b) => Self::Blob(STANDARD.encode(b)),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlStatement {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<SqlValue>,
}
macro_rules! dto {($name:ident {$($fields:tt)*})=>{#[derive(Clone,Debug,Serialize,Deserialize,JsonSchema)]#[serde(deny_unknown_fields)]pub struct $name {$($fields)*}}}
dto!(Database {pub database:String});
/// SQLite schema object kinds, shared by all transports.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaObjectType {
    Table,
    Index,
    View,
    Trigger,
}
impl SchemaObjectType {
    pub const VALUES: [&'static str; 4] = ["table", "index", "view", "trigger"];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Index => "index",
            Self::View => "view",
            Self::Trigger => "trigger",
        }
    }
}
fn schema_table<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let table = Option::<String>::deserialize(d)?;
    if table.as_deref() == Some("") {
        return Err(serde::de::Error::custom("table must not be empty"));
    }
    Ok(table)
}
dto!(SchemaGet {
    pub database: String,
    #[serde(default, deserialize_with = "schema_table")]
    #[schemars(length(min = 1))]
    pub table: Option<String>,
    pub r#type: Option<SchemaObjectType>,
});
/// User metadata leaves ample space for HEAD bookkeeping and JSON escaping of descriptions.
pub const MAX_DATABASE_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_DATABASE_DESCRIPTION_BYTES: usize = 16 * 1024;

fn validate_database_metadata(
    description: Option<&str>,
    metadata: Option<&Value>,
) -> Result<(), String> {
    if description.is_some_and(|value| value.len() > MAX_DATABASE_DESCRIPTION_BYTES) {
        return Err("description exceeds 16 KiB of UTF-8 bytes".into());
    }
    if let Some(metadata) = metadata {
        let bytes = serde_json::to_vec(metadata).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_DATABASE_METADATA_BYTES {
            return Err("metadata exceeds 64 KiB of compact JSON UTF-8 bytes".into());
        }
    }
    Ok(())
}

dto!(Create {
    pub name: String,
    /// Database description, at most 16384 UTF-8 bytes.
    #[serde(default)]
    pub description: String,
    /// At most 65536 UTF-8 bytes when serialized as compact JSON, including keys and escaping.
    #[serde(default)]
    pub metadata: Value,
});
impl Create {
    pub fn validate(&self) -> Result<(), String> {
        validate_database_metadata(Some(&self.description), Some(&self.metadata))
    }
}
dto!(List {#[serde(default="default_limit")]pub limit:usize,pub cursor:Option<String>,#[serde(default)]pub include_deleted:bool});
pub fn default_limit() -> usize {
    50
}
dto!(Update {
    pub database: String,
    pub if_match: Option<String>,
    /// Database description, at most 16384 UTF-8 bytes.
    pub description: Option<String>,
    /// At most 65536 UTF-8 bytes when serialized as compact JSON, including keys and escaping.
    pub metadata: Option<Value>,
    pub snapshot_retention_days: Option<u32>,
});
impl Update {
    pub fn validate(&self) -> Result<(), String> {
        validate_database_metadata(self.description.as_deref(), self.metadata.as_ref())
    }
}
dto!(Delete {pub database:String,pub if_match:Option<String>,});
fn strong() -> String {
    "strong".into()
}
macro_rules! execute_fields {
    ($name:ident, $($attrs:tt)*) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
        $($attrs)*
        pub struct $name {
            pub database: String,
            pub sql: Option<String>,
            pub params: Option<Vec<SqlValue>>,
            pub statements: Option<Vec<SqlStatement>>,
            #[serde(default = "strong")]
            pub consistency: String,
            pub snapshot_id: Option<String>,
            pub commit_token: Option<String>,
            pub max_rows: Option<usize>,
        }
    }
}
execute_fields!(Execute, #[serde(try_from = "ExecuteInput")]);
execute_fields!(ExecuteInput, #[serde(deny_unknown_fields)]);
impl TryFrom<ExecuteInput> for Execute {
    type Error = String;
    fn try_from(i: ExecuteInput) -> Result<Self, String> {
        let input = Self {
            database: i.database,
            sql: i.sql,
            params: i.params,
            statements: i.statements,
            consistency: i.consistency,
            snapshot_id: i.snapshot_id,
            commit_token: i.commit_token,
            max_rows: i.max_rows,
        };
        input.validate()?;
        Ok(input)
    }
}
impl Execute {
    pub fn validate(&self) -> Result<(), String> {
        match (&self.sql, &self.statements) {
            (Some(sql), None) if !sql.trim().is_empty() => {}
            (None, Some(statements)) if !statements.is_empty() && self.params.is_none() => {
                if self.consistency != "strong"
                    || self.snapshot_id.is_some()
                    || self.commit_token.is_some()
                    || self.max_rows.is_some()
                {
                    return Err("Read options are not valid for statement batches".into());
                }
                if statements.iter().any(|s| s.sql.trim().is_empty()) {
                    return Err("SQL statements must not be empty".into());
                }
            }
            _ => {
                return Err(
                    "Provide either sql with optional params, or a nonempty statements array"
                        .into(),
                )
            }
        }
        for p in self
            .params
            .iter()
            .flatten()
            .chain(self.statements.iter().flatten().flat_map(|s| &s.params))
        {
            p.to_sql()?;
        }
        Ok(())
    }
}
dto!(Compact {
    pub database: String,
    #[serde(default = "default_compact_budget_ms")]
    pub budget_ms: u64,
    #[serde(default = "default_compact_max_segments")]
    pub max_segments: usize,
});
fn default_compact_budget_ms() -> u64 {
    10000
}
fn default_compact_max_segments() -> usize {
    32
}
dto!(Gc {pub database:String,pub budget_ms:Option<u64>,pub max_objects:Option<usize>});
dto!(SnapshotList {pub database:String,#[serde(default="default_limit")]pub limit:usize,pub cursor:Option<String>,});
dto!(SnapshotGet {pub database:String,pub snapshot_id:String});
// Binary export is intentionally separate from the JSON operation dispatcher.
dto!(SnapshotExport {pub database:String,pub snapshot_id:String});
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", content = "input", rename_all = "snake_case")]
pub enum Operation {
    DatabaseCreate(Create),
    DatabaseList(List),
    DatabaseGet(Database),
    DatabaseUpdate(Update),
    DatabaseDelete(Delete),
    SqlExecute(Execute),
    SchemaGet(SchemaGet),
    DatabaseCompact(Compact),
    DatabaseGc(Gc),
    SnapshotList(SnapshotList),
    SnapshotGet(SnapshotGet),
}
impl Operation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DatabaseCreate(_) => "database_create",
            Self::DatabaseList(_) => "database_list",
            Self::DatabaseGet(_) => "database_get",
            Self::DatabaseUpdate(_) => "database_update",
            Self::DatabaseDelete(_) => "database_delete",
            Self::SqlExecute(_) => "sql_execute",
            Self::SchemaGet(_) => "schema_get",
            Self::DatabaseCompact(_) => "database_compact",
            Self::DatabaseGc(_) => "database_gc",
            Self::SnapshotList(_) => "snapshot_list",
            Self::SnapshotGet(_) => "snapshot_get",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub id: String,
    pub databases: Vec<String>,
    pub scopes: Vec<String>,
}
impl Principal {
    pub fn allows(&self, database: &str, scope: &str) -> bool {
        self.databases.iter().any(|d| d == "*" || d == database)
            && self.scopes.iter().any(|s| s == "*" || s == scope)
    }
    pub fn direct() -> Self {
        Self {
            id: "direct-s3-credential-holder".into(),
            databases: vec!["*".into()],
            scopes: vec!["*".into()],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub request_id: String,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub outcome: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

fn tool_input_schema(name: &str, mut schema: Value) -> Value {
    if name == "sql_execute" {
        schema["oneOf"] = json!([
            {"required":["sql"],"properties":{"sql":{"type":"string","minLength":1},"statements":{"type":"null"}}},
            {"required":["statements"],"properties":{
                "statements":{"type":"array","minItems":1},"sql":{"type":"null"},"params":{"type":"null"},
                "consistency":{"const":"strong"},"snapshot_id":{"type":"null"},
                "commit_token":{"type":"null"},"max_rows":{"type":"null"}
            }}
        ]);
    }
    schema
}

pub fn tools() -> Vec<Value> {
    macro_rules! tool {($name:literal,$ty:ty,$description:literal,$read:expr)=>{serde_json::json!({"name":$name,"description":$description,"inputSchema":tool_input_schema($name, serde_json::to_value(schemars::schema_for!($ty)).unwrap()),"outputSchema":output_schema($name),"annotations":{"readOnlyHint":$read,"destructiveHint":!$read,"idempotentHint":$read,"openWorldHint":false}})}}
    vec![
        tool!(
            "database_create",
            Create,
            "Create a new database.",
            false
        ),
        tool!(
            "database_list",
            List,
            "List authorized databases with a bounded cursor.",
            true
        ),
        tool!(
            "database_get",
            Database,
            "Read database identity, metadata and management ETag.",
            true
        ),
        tool!(
            "database_update",
            Update,
            "Update metadata, optionally requiring a matching management ETag via if_match.",
            false
        ),
        tool!(
            "database_delete",
            Delete,
            "Logically delete a database; retain its tombstone and history. Optionally require a matching management ETag via if_match.",
            false
        ),
        tool!(
            "sql_execute",
            Execute,
            "Execute sql with optional params, or a nonempty statements array atomically. Batches use write semantics and reject read options. Writes commit through remote HEAD CAS; repeating a write executes it again.",
            false
        ),
        tool!(
            "schema_get",
            SchemaGet,
            "Inspect SQLite schema definitions, optionally filtering by exact table name (including its indexes and triggers) and object type. A specified table must exist or TABLE_NOT_FOUND is returned.",
            true
        ),
        tool!(
            "database_compact",
            Compact,
            "Synchronously merge a bounded set of segments; partial requires another call.",
            false
        ),
        tool!(
            "database_gc",
            Gc,
            "Mark and reclaim old unreachable objects online within an optional work budget.",
            false
        ),
        tool!(
            "snapshot_list",
            SnapshotList,
            "List retained logical history.",
            true
        ),
        tool!(
            "snapshot_get",
            SnapshotGet,
            "Read a retained snapshot descriptor.",
            true
        ),
    ]
}

/// A concrete business-output envelope per tool. Arbitrary JSON is confined to
/// user metadata, not the tool's entire result.
pub fn output_schema(name: &str) -> Value {
    let text = json!({"type":"string"});
    let integer = json!({"type":"integer","minimum":0});
    let boolean = json!({"type":"boolean"});
    let typed = json!({"oneOf":[{"type":"object","properties":{"type":{"const":"null"}},"required":["type"],"additionalProperties":false},{"type":"object","properties":{"type":{"enum":["integer","text","blob"]},"value":{"type":"string"}},"required":["type","value"],"additionalProperties":false},{"type":"object","properties":{"type":{"const":"real"},"value":{"type":"number"}},"required":["type","value"],"additionalProperties":false}]});
    let rows = json!({"type":"array","items":{"type":"object","properties":{"columns":{"type":"array","items":{"type":"string"}},"rows":{"type":"array","items":{"type":"array","items":typed}},"rows_affected":{"type":"string"}},"required":["columns","rows","rows_affected"],"additionalProperties":false}});
    let mut properties = serde_json::Map::new();
    for key in ["request_id", "outcome"] {
        properties.insert(key.into(), text.clone());
    }
    let fields: &[(&str, &str)] = match name {
        "database_create" => &[("name", "s"), ("data_version", "s"), ("committed", "b")],
        "database_get" => &[
            ("name", "s"),
            ("description", "s"),
            ("metadata", "any"),
            ("status", "s"),
            ("engine", "s"),
            ("sql_dialect", "s"),
            ("revision", "s"),
            ("metadata_version", "s"),
            ("data_version", "s"),
            ("logical_size_bytes", "s"),
            ("page_size", "i"),
            ("snapshot_retention_days", "i"),
            ("etag", "s"),
            ("commit_token", "s"),
            ("created_at_ms", "i"),
            ("updated_at_ms", "i"),
        ],
        "database_list" => &[("databases", "array"), ("next_cursor", "nullable_string")],
        "database_update" => &[
            ("committed", "b"),
            ("database", "s"),
            ("metadata_version", "s"),
        ],
        "database_delete" => &[("committed", "b"), ("database", "s"), ("status", "s")],
        "schema_get" => &[
            ("database", "s"),
            ("data_version", "s"),
            ("results", "rows"),
            ("truncated", "b"),
        ],
        "sql_execute" => &[
            ("database", "s"),
            ("data_version", "s"),
            ("commit_token", "s"),
            ("committed", "b"),
            ("results", "rows"),
        ],
        "database_compact" => &[
            ("complete", "b"),
            ("database_available", "b"),
            ("segments_processed", "i"),
            ("data_version", "s"),
        ],
        "database_gc" => &[
            ("outcome", "s"),
            ("complete", "b"),
            ("database_available", "b"),
            ("phase", "s"),
            ("deleted_objects", "i"),
        ],
        "snapshot_list" => &[
            ("snapshots", "array"),
            ("next_cursor", "nullable_string"),
            ("retention_days", "i"),
            ("time_authority", "s"),
            ("history_gaps", "array"),
        ],
        "snapshot_get" => &[
            ("snapshot_id", "s"),
            ("data_version", "s"),
            ("manifest", "any"),
            ("retired_at_upper_ms", "nullable_integer"),
            ("observed_at_ms", "i"),
        ],
        _ => &[],
    };
    for (key, kind) in fields {
        properties.insert(
            (*key).into(),
            match *kind {
                "s" => text.clone(),
                "i" => integer.clone(),
                "b" => boolean.clone(),
                "rows" => rows.clone(),
                "array" => json!({"type":"array"}),
                "nullable_string" => json!({"type":["string","null"]}),
                "nullable_integer" => json!({"type":["integer","null"]}),
                _ => json!({}),
            },
        );
    }
    let mut schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","oneOf":[{"properties":{"ok":{"const":true},"result":{"type":"object","properties":properties,"additionalProperties":false}},"required":["ok","result"],"additionalProperties":false},{"properties":{"ok":{"const":false},"error":{"type":"object","properties":{"request_id":text,"code":{"type":"string"},"message":{"type":"string"},"retryable":boolean,"outcome":{"type":"string"}},"required":["request_id","code","message","retryable","outcome"],"additionalProperties":false}},"required":["ok","error"],"additionalProperties":false}]});
    if name == "sql_execute" {
        let mut read = output_schema("schema_get")["oneOf"][0]["properties"]["result"].clone();
        let mut write = schema["oneOf"][0]["properties"]["result"].clone();
        read["properties"]["outcome"] = json!({"const":"read_only"});
        write["properties"]["outcome"] = json!({"const":"committed"});
        read["required"] = json!([
            "outcome",
            "database",
            "data_version",
            "results",
            "truncated"
        ]);
        write["required"] = json!([
            "outcome",
            "database",
            "data_version",
            "results",
            "committed",
            "commit_token"
        ]);
        schema["oneOf"][0]["properties"]["result"] = json!({"oneOf":[read,write]});
    }
    schema
}
/// Parses business JSON for every entry: optional UTF-8 BOM, no duplicate object keys.
pub fn strict_json(bytes: &[u8]) -> Result<Value, String> {
    use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};
    struct Strict;
    impl<'de> DeserializeSeed<'de> for Strict {
        type Value = Value;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Strict {
        type Value = Value;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("JSON value")
        }
        fn visit_unit<E>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
            Ok(Value::Bool(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
            Ok(v.into())
        }
        fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
            Ok(v.into())
        }
        fn visit_f64<E: Error>(self, v: f64) -> Result<Value, E> {
            serde_json::Number::from_f64(v)
                .map(Value::Number)
                .ok_or_else(|| E::custom("non-finite number"))
        }
        fn visit_str<E>(self, v: &str) -> Result<Value, E> {
            Ok(Value::String(v.into()))
        }
        fn visit_string<E>(self, v: String) -> Result<Value, E> {
            Ok(Value::String(v))
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
            let mut out = Vec::new();
            while let Some(v) = seq.next_element_seed(Strict)? {
                out.push(v);
            }
            Ok(Value::Array(out))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
            let mut out = serde_json::Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if out.contains_key(&key) {
                    return Err(A::Error::custom(format!("duplicate JSON key: {key}")));
                }
                let value = map.next_value_seed(Strict)?;
                out.insert(key, value);
            }
            Ok(Value::Object(out))
        }
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| "input is not valid UTF-8".to_string())?;
    let mut d = serde_json::Deserializer::from_str(text);
    let value = Strict.deserialize(&mut d).map_err(|e| e.to_string())?;
    d.end().map_err(|e| e.to_string())?;
    Ok(value)
}
#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn omitted_limits_use_cli_defaults_independently() {
        for (input, budget, segments) in [
            (json!({"database":"notes"}), 10000, 32),
            (json!({"database":"notes","budget_ms":5000}), 5000, 32),
            (json!({"database":"notes","max_segments":16}), 10000, 16),
            (
                json!({"database":"notes","budget_ms":5000,"max_segments":16}),
                5000,
                16,
            ),
        ] {
            let compact: Compact = serde_json::from_value(input).unwrap();
            assert_eq!(compact.budget_ms, budget);
            assert_eq!(compact.max_segments, segments);
        }
        for field in ["budget_ms", "max_segments"] {
            for value in [Value::Null, json!("1"), json!(-1), json!(1.5)] {
                let mut input = json!({"database":"notes"});
                input[field] = value;
                assert!(serde_json::from_value::<Compact>(input).is_err());
            }
        }
    }

    #[test]
    fn mcp_schema_exposes_optional_limits_and_defaults() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "database_compact")
            .unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["required"], json!(["database"]));
        assert_eq!(schema["properties"]["budget_ms"]["default"], 10000);
        assert_eq!(schema["properties"]["max_segments"]["default"], 32);
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn validate(description: &str, metadata: Value) -> (Result<(), String>, Result<(), String>) {
        (
            Create {
                name: "notes".into(),
                description: description.into(),
                metadata: metadata.clone(),
            }
            .validate(),
            Update {
                database: "notes".into(),
                if_match: None,
                description: Some(description.into()),
                metadata: Some(metadata),
                snapshot_retention_days: None,
            }
            .validate(),
        )
    }

    #[test]
    fn metadata_limits_count_compact_utf8_json_including_escaping() {
        let payload_bytes = MAX_DATABASE_METADATA_BYTES - 2; // JSON string quotes.
        for content in [
            "a".repeat(payload_bytes),
            "中".repeat(payload_bytes / 3) + &"a".repeat(payload_bytes % 3),
            "\"".repeat(payload_bytes / 2),
            "\0".repeat(payload_bytes / 6) + &"a".repeat(payload_bytes % 6),
        ] {
            let metadata = json!(content);
            assert_eq!(
                serde_json::to_vec(&metadata).unwrap().len(),
                MAX_DATABASE_METADATA_BYTES
            );
            let (create, update) = validate("", metadata);
            assert!(create.is_ok());
            assert!(update.is_ok());
            let (create, update) = validate("", json!(content + "a"));
            for error in [create.unwrap_err(), update.unwrap_err()] {
                assert!(error.contains("metadata exceeds 64 KiB"));
            }
        }
        // Object keys and delimiters count, while input whitespace does not.
        let metadata = json!({"x": "a".repeat(MAX_DATABASE_METADATA_BYTES - 8)});
        assert!(validate("", metadata.clone()).0.is_ok());
        assert!(validate("", json!({"x": metadata})).0.is_err());
        let pretty = serde_json::to_string_pretty(&metadata).unwrap();
        assert!(pretty.len() > MAX_DATABASE_METADATA_BYTES);
        assert!(validate("", strict_json(pretty.as_bytes()).unwrap())
            .0
            .is_ok());
    }

    #[test]
    fn description_limit_counts_utf8_bytes_and_defaults_remain_valid() {
        let description = "中".repeat(MAX_DATABASE_DESCRIPTION_BYTES / 3) + "a";
        assert_eq!(description.len(), MAX_DATABASE_DESCRIPTION_BYTES);
        let (create, update) = validate(&description, Value::Null);
        assert!(create.is_ok());
        assert!(update.is_ok());
        let (create, update) = validate(&(description + "a"), Value::Null);
        for error in [create.unwrap_err(), update.unwrap_err()] {
            assert!(error.contains("description exceeds 16 KiB"));
        }
        assert!(serde_json::from_value::<Create>(json!({"name":"notes"}))
            .unwrap()
            .validate()
            .is_ok());
        assert!(
            serde_json::from_value::<Update>(json!({"database":"notes"}))
                .unwrap()
                .validate()
                .is_ok()
        );
        for tool in tools().iter().filter(|tool| {
            matches!(
                tool["name"].as_str(),
                Some("database_create" | "database_update")
            )
        }) {
            assert!(tool["inputSchema"]["properties"]["metadata"]["description"]
                .as_str()
                .unwrap()
                .contains("65536"));
        }
    }
}

#[cfg(test)]
mod strict_json_tests {
    #[test]
    fn bom_accepted_duplicates_rejected() {
        assert_eq!(
            super::strict_json(b"\xEF\xBB\xBF{\"a\":[1,{\"b\":null}]}").unwrap()["a"][1]["b"],
            serde_json::Value::Null
        );
        assert!(super::strict_json(br#"{"a":1,"a":1}"#).is_err());
        assert!(super::strict_json(br#"{"x":{"a":1,"a":2}}"#).is_err());
        assert!(super::strict_json(br#"{} {}"#).is_err());
        assert!(super::strict_json(b"\"\xFF\"").is_err());
    }
}

#[cfg(test)]
mod execute_tests {
    use super::*;
    #[test]
    fn schema_filters_validate_and_are_advertised() {
        for kind in SchemaObjectType::VALUES {
            let dto: SchemaGet =
                serde_json::from_value(json!({"database":"notes","table":"items","type":kind}))
                    .unwrap();
            assert_eq!(dto.r#type.unwrap().as_str(), kind);
        }
        for invalid in [
            json!({"database":"notes","table":""}),
            json!({"database":"notes","table":1}),
            json!({"database":"notes","type":"invalid"}),
            json!({"database":"notes","type":["table"]}),
            json!({"database":"notes","unknown":true}),
            json!({"table":"items"}),
        ] {
            assert!(serde_json::from_value::<SchemaGet>(invalid).is_err());
        }
        assert!(serde_json::from_value::<SchemaGet>(json!({"database":"notes"})).is_ok());
        assert!(strict_json(br#"{"database":"notes","table":"a","table":"b"}"#).is_err());
        let tools = tools();
        let schema = &tools.iter().find(|t| t["name"] == "schema_get").unwrap()["inputSchema"];
        assert_eq!(schema["required"], json!(["database"]));
        assert_eq!(schema["properties"]["table"]["minLength"], 1);
        assert!(schema["properties"].get("type").is_some());
        assert_eq!(
            schema["$defs"]["SchemaObjectType"]["enum"],
            json!(SchemaObjectType::VALUES)
        );
    }

    #[test]
    fn management_etags_are_optional_in_dtos_and_tool_schemas() {
        let tools = tools();
        for name in ["database_update", "database_delete"] {
            for input in [
                json!({"database":"notes"}),
                json!({"database":"notes","if_match":"\"v1-etag\""}),
            ] {
                assert!(serde_json::from_value::<Operation>(
                    json!({"operation":name,"input":input})
                )
                .is_ok());
            }
            for input in [
                json!({"if_match":"etag"}),
                json!({"database":"notes","if_match":123}),
                json!({"database":"notes","if_match":false}),
                json!({"database":"notes","unknown":true}),
            ] {
                assert!(serde_json::from_value::<Operation>(
                    json!({"operation":name,"input":input})
                )
                .is_err());
            }
            let schema = &tools.iter().find(|t| t["name"] == name).unwrap()["inputSchema"];
            assert_eq!(schema["required"], json!(["database"]));
            assert!(schema["properties"].get("if_match").is_some());
        }
    }

    #[test]
    fn execute_requires_exactly_one_valid_input_form() {
        for fields in [
            json!({}),
            json!({"statements":[]}),
            json!({"sql":" "}),
            json!({"sql":"SELECT 1","statements":[{"sql":"SELECT 2"}]}),
            json!({"statements":[{"sql":"SELECT 1"}],"params":[]}),
            json!({"statements":[{"sql":""}]}),
            json!({"statements":[{"sql":"SELECT 1","unknown":true}]}),
            json!({"statements":[{"sql":"SELECT ?","params":[1]}]}),
            json!({"statements":[{"sql":"SELECT ?","params":[{"type":"integer","value":"bad"}]}]}),
            json!({"statements":[{"sql":"SELECT 1"}],"max_rows":1}),
            json!({"statements":[{"sql":"SELECT 1"}],"consistency":"snapshot"}),
            json!({"sql":"SELECT 1","unknown":true}),
        ] {
            let mut input = fields.clone();
            input["database"] = json!("notes");
            assert!(
                serde_json::from_value::<Execute>(input).is_err(),
                "{fields}"
            );
        }
        for fields in [
            json!({"sql":"SELECT 1"}),
            json!({"statements":[{"sql":"SELECT 1"}]}),
        ] {
            let mut input = fields;
            input["database"] = json!("notes");
            let request: Execute = serde_json::from_value(input).unwrap();
            assert_eq!(request.consistency, "strong");
            assert!(
                serde_json::from_value::<Execute>(serde_json::to_value(request).unwrap()).is_ok()
            );
        }
        assert!(serde_json::from_value::<Operation>(json!({"operation":"sql_transaction","input":{"database":"notes","statements":[{"sql":"SELECT 1"}]}})).is_err());
        let tools = tools();
        assert!(!tools.iter().any(|t| t["name"] == "sql_transaction"));
        let schema = &tools.iter().find(|t| t["name"] == "sql_execute").unwrap()["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["oneOf"][1]["properties"]["statements"]["minItems"],
            1
        );
    }
}
