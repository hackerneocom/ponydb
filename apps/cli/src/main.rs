mod restore_progress;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use database_service::{error, Service};
use protocol::{Operation, Principal};
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read, path::PathBuf, sync::Arc, time::Duration};
#[derive(Parser)]
#[command(
    name = "ponydb",
    about = "SQLite over S3 with direct CLI access and an optional server"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true, value_name = "CONFIG_FILE")]
    config: Option<PathBuf>,
    /// Output format; defaults to cli.output in the configuration.
    #[arg(long, global = true, value_parser = ["json", "table"], value_name = "FORMAT")]
    output: Option<String>,
    /// Request timeout in milliseconds; defaults to requests.default_timeout_ms.
    #[arg(long, global = true)]
    deadline_ms: Option<u64>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Serve HTTP and MCP.
    Server {
        /// Server bind address, such as 127.0.0.1:8080; overrides server.listen.
        #[arg(long, value_name = "ADDRESS")]
        listen: Option<String>,
    },
    /// Serve MCP over stdin/stdout for agent clients.
    Mcp,
    /// Create and list databases.
    #[command(subcommand)]
    Databases(Databases),
    /// Inspect or operate on an explicitly named database.
    Db(Database),
    /// Show and validate the effective configuration.
    #[command(subcommand)]
    Config(Config),
    /// Show program, SQLite and protocol versions.
    Version,
}
#[derive(Subcommand)]
enum Databases {
    /// Create a new database.
    Create(CreateArgs),
    /// List databases.
    List(ListArgs),
}
#[derive(Args)]
struct Database {
    /// Unique database name.
    #[arg(value_name = "DATABASE_NAME", required = true)]
    name: String,
    /// Strict request DTO for database details.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    #[command(subcommand)]
    action: Option<Db>,
}
#[derive(Subcommand)]
enum Db {
    /// Update metadata, optionally requiring a matching object ETag.
    Update(UpdateArgs),
    /// Logically delete the database, optionally requiring a matching object ETag.
    Delete(DeleteArgs),
    /// Merge storage segments within a work budget.
    Compact(CompactArgs),
    /// Reclaim unused storage objects.
    Gc(GcArgs),
    /// Execute SQL or an atomic statements array.
    Execute(ExecuteArgs),
    /// Show SQLite schema definitions.
    Schema(SchemaArgs),
    /// Export the current database version as a standalone SQLite file (never overwrites).
    #[command(
        override_usage = "ponydb db <DATABASE_NAME> export [OPTIONS] <SQLITE_FILE>\n       ponydb db <DATABASE_NAME> export [OPTIONS] --input <REQUEST_FILE>"
    )]
    Export(DatabaseExportArgs),
    /// Inspect retained snapshots.
    #[command(subcommand)]
    Snapshot(Snapshot),
    /// Restore a local SQLite backup directly to object storage.
    #[command(subcommand)]
    Restore(Restore),
}
#[derive(Subcommand)]
enum Snapshot {
    /// List retained database snapshots with cursor-based pagination.
    List(SnapshotListArgs),
    /// Show a retained snapshot's descriptor.
    Get(SnapshotGetArgs),
    /// Export a retained snapshot as a standalone SQLite file (never overwrites).
    #[command(
        override_usage = "ponydb db <DATABASE_NAME> snapshot export [OPTIONS] <SNAPSHOT_ID> <SQLITE_FILE>\n       ponydb db <DATABASE_NAME> snapshot export [OPTIONS] --input <REQUEST_FILE>"
    )]
    Export(SnapshotExportArgs),
}

#[derive(Args)]
struct DatabaseExportArgs {
    /// Strict JSON request containing database and file, or - for stdin.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Destination SQLite file; - is not supported. Parent directory must exist.
    #[arg(value_name = "SQLITE_FILE", required_unless_present = "input")]
    file: Option<PathBuf>,
}
impl From<DatabaseExportArgs> for Input {
    fn from(args: DatabaseExportArgs) -> Self {
        Self {
            input: args.input,
            file: args.file,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct SnapshotExportArgs {
    /// Strict JSON request containing database, snapshot_id and file, or - for stdin.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Retained snapshot to export.
    #[arg(value_name = "SNAPSHOT_ID", required_unless_present = "input")]
    snapshot_id: Option<String>,
    /// Destination SQLite file; - is not supported. Parent directory must exist.
    #[arg(value_name = "SQLITE_FILE", required_unless_present = "input")]
    file: Option<PathBuf>,
}
impl From<SnapshotExportArgs> for Input {
    fn from(args: SnapshotExportArgs) -> Self {
        Self {
            input: args.input,
            snapshot_id: args.snapshot_id,
            file: args.file,
            ..Default::default()
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseExportFile {
    database: String,
    file: PathBuf,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotExportFile {
    database: String,
    snapshot_id: String,
    file: PathBuf,
}
impl SnapshotExportFile {
    fn prepare(&self) -> Result<tempfile::NamedTempFile, protocol::ApiError> {
        if self.snapshot_id.is_empty() {
            return Err(error(
                "INVALID_REQUEST",
                "Export requires a snapshot ID and a destination file; - is not supported",
            ));
        }
        prepare_export(&self.file)
    }
}

fn prepare_export(file: &std::path::Path) -> Result<tempfile::NamedTempFile, protocol::ApiError> {
    let invalid = |message| error("INVALID_REQUEST", message);
    if file.as_os_str().is_empty() || file == std::path::Path::new("-") {
        return Err(invalid(
            "Export requires a destination file; - is not supported",
        ));
    }
    match std::fs::symlink_metadata(file) {
        Ok(_) => return Err(invalid("Export destination already exists")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(invalid("Cannot inspect export destination")),
    }
    tempfile::NamedTempFile::new_in(
        file.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new(".")),
    )
    .map_err(|_| invalid("Cannot create export file in destination directory"))
}

struct ExportFile {
    database: String,
    snapshot_id: Option<String>,
    file: PathBuf,
}
impl From<DatabaseExportFile> for ExportFile {
    fn from(input: DatabaseExportFile) -> Self {
        Self {
            database: input.database,
            snapshot_id: None,
            file: input.file,
        }
    }
}
impl From<SnapshotExportFile> for ExportFile {
    fn from(input: SnapshotExportFile) -> Self {
        Self {
            database: input.database,
            snapshot_id: Some(input.snapshot_id),
            file: input.file,
        }
    }
}

async fn export_file<S: object_store_api::BlobStore + 'static>(
    service: &Service<S>,
    input: impl Into<ExportFile>,
    temporary: tempfile::NamedTempFile,
    deadline: u64,
) -> Result<Value, protocol::ApiError> {
    let input = input.into();
    let finish_by = tokio::time::Instant::now() + Duration::from_millis(deadline);
    let file = temporary
        .as_file()
        .try_clone()
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot open export workspace"))?;
    let mut writer = tokio::fs::File::from_std(file);
    let principal = Principal::direct();
    let ctx = RequestContext::new(Duration::from_millis(deadline));
    let result = match input.snapshot_id {
        Some(snapshot_id) => {
            service
                .export_snapshot(
                    &principal,
                    protocol::SnapshotExport {
                        database: input.database,
                        snapshot_id,
                    },
                    ctx,
                    &mut writer,
                )
                .await?
        }
        None => {
            service
                .export_database(&principal, &input.database, ctx, &mut writer)
                .await?
        }
    };
    tokio::time::timeout_at(finish_by, writer.sync_all())
        .await
        .map_err(|_| error("DEADLINE_EXCEEDED", "Export deadline exceeded"))?
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot sync export file"))?;
    drop(writer);
    if tokio::time::Instant::now() >= finish_by {
        return Err(error("DEADLINE_EXCEEDED", "Export deadline exceeded"));
    }
    temporary.persist_noclobber(&input.file).map_err(|_| {
        error(
            "INVALID_REQUEST",
            "Cannot publish export file; destination may already exist",
        )
    })?;
    let mut value = serde_json::to_value(result).unwrap();
    value["outcome"] = json!("read_only");
    value["file"] = json!(input.file);
    Ok(value)
}
#[derive(Subcommand)]
enum Restore {
    /// Validate and restore a local SQLite backup without an execution deadline.
    #[command(override_usage = "ponydb db <DATABASE_NAME> restore file [OPTIONS] <SQLITE_FILE>")]
    File(FileRestore),
}
#[derive(Subcommand)]
enum Config {
    /// Show the effective configuration with secrets redacted.
    Show {
        /// Configuration scope to show or validate.
        #[arg(long, default_value = "cli", value_parser = ["cli", "server", "all"], value_name = "SCOPE")]
        target: String,
    },
    /// Validate configuration and probe storage with temporary objects (Put/Get/List/Delete).
    ///
    /// Creates and deletes isolated probe objects, including one 2 MiB object.
    Validate {
        /// Configuration scope to show or validate.
        #[arg(long, default_value = "cli", value_parser = ["cli", "server", "all"], value_name = "SCOPE")]
        target: String,
    },
}
#[derive(Default)]
struct Input {
    input: Option<String>,
    database: Option<String>,
    if_match: Option<String>,
    sql: Option<String>,
    sql_file: Option<String>,
    params: Vec<String>,
    params_file: Option<String>,
    metadata_file: Option<String>,
    include_deleted: bool,
    consistency: Option<String>,
    commit_token: Option<String>,
    max_rows: Option<u64>,
    snapshot_retention_days: Option<u64>,
    name: Option<String>,
    description: Option<String>,
    limit: Option<u64>,
    cursor: Option<String>,
    table: Option<String>,
    object_type: Option<String>,
    snapshot_id: Option<String>,
    budget_ms: Option<u64>,
    max_segments: Option<u64>,
    max_objects: Option<u64>,
    file: Option<PathBuf>,
}

#[derive(Args)]
struct CreateArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Unique, immutable database name for creation (1–64 MySQL identifier characters).
    #[arg(value_name = "DATABASE_NAME", required = true)]
    name: Option<String>,
    /// Description of the database to create (at most 16 KiB of UTF-8 bytes).
    #[arg(long)]
    description: Option<String>,
    /// Database metadata JSON, or - for stdin (at most 64 KiB after compact serialization).
    #[arg(long, value_name = "METADATA_FILE")]
    metadata_file: Option<String>,
}
impl From<CreateArgs> for Input {
    fn from(args: CreateArgs) -> Self {
        Self {
            input: args.input,
            name: args.name,
            description: args.description,
            metadata_file: args.metadata_file,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct ListArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Maximum number of databases to list in one page.
    #[arg(long)]
    limit: Option<u64>,
    /// Pagination cursor returned by a previous list operation.
    #[arg(long)]
    cursor: Option<String>,
    /// Include logically deleted databases in the list.
    #[arg(long)]
    include_deleted: bool,
}
impl From<ListArgs> for Input {
    fn from(args: ListArgs) -> Self {
        Self {
            input: args.input,
            limit: args.limit,
            cursor: args.cursor,
            include_deleted: args.include_deleted,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct DatabaseArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
}
impl From<DatabaseArgs> for Input {
    fn from(args: DatabaseArgs) -> Self {
        Self {
            input: args.input,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct SchemaArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Exact, case-sensitive table name; must exist. Includes associated indexes and triggers.
    #[arg(long, value_name = "TABLE_NAME", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    table: Option<String>,
    /// SQLite schema object type; combined with --table using AND.
    #[arg(long = "type", value_name = "TYPE", value_parser = protocol::SchemaObjectType::VALUES)]
    object_type: Option<String>,
}
impl From<SchemaArgs> for Input {
    fn from(args: SchemaArgs) -> Self {
        Self {
            input: args.input,
            table: args.table,
            object_type: args.object_type,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct UpdateArgs {
    #[command(flatten)]
    target: DatabaseArgs,
    /// Optional object ETag from db DATABASE_NAME; update only if it still matches.
    #[arg(long, value_name = "ETAG")]
    if_match: Option<String>,
    /// New database description (at most 16 KiB of UTF-8 bytes).
    #[arg(long)]
    description: Option<String>,
    /// Database metadata JSON, or - for stdin (at most 64 KiB after compact serialization).
    #[arg(long, value_name = "METADATA_FILE")]
    metadata_file: Option<String>,
    /// Number of days to retain database snapshots.
    #[arg(long)]
    snapshot_retention_days: Option<u64>,
}
impl From<UpdateArgs> for Input {
    fn from(args: UpdateArgs) -> Self {
        Self {
            if_match: args.if_match,
            description: args.description,
            metadata_file: args.metadata_file,
            snapshot_retention_days: args.snapshot_retention_days,
            ..args.target.into()
        }
    }
}

#[derive(Args)]
struct DeleteArgs {
    #[command(flatten)]
    target: DatabaseArgs,
    /// Optional object ETag from db DATABASE_NAME; delete only if it still matches.
    #[arg(long, value_name = "ETAG")]
    if_match: Option<String>,
}
impl From<DeleteArgs> for Input {
    fn from(args: DeleteArgs) -> Self {
        Self {
            if_match: args.if_match,
            ..args.target.into()
        }
    }
}

#[derive(Args)]
struct CompactArgs {
    #[command(flatten)]
    target: DatabaseArgs,
    /// Work budget in milliseconds for this compaction call; defaults to 10000.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=60000))]
    budget_ms: Option<u64>,
    /// Maximum number of storage segments to compact in this call; defaults to 32.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=32))]
    max_segments: Option<u64>,
}
impl From<CompactArgs> for Input {
    fn from(args: CompactArgs) -> Self {
        Self {
            budget_ms: args.budget_ms,
            max_segments: args.max_segments,
            ..args.target.into()
        }
    }
}

#[derive(Args)]
struct GcArgs {
    #[command(flatten)]
    target: DatabaseArgs,
    /// Work budget in milliseconds; defaults to maintenance.gc_default_budget_ms (60000).
    #[arg(long)]
    budget_ms: Option<u64>,
    /// Maximum DELETE attempts; defaults to maintenance.gc_max_objects_per_call (1000).
    #[arg(long)]
    max_objects: Option<u64>,
}
impl From<GcArgs> for Input {
    fn from(args: GcArgs) -> Self {
        Self {
            budget_ms: args.budget_ms,
            max_objects: args.max_objects,
            ..args.target.into()
        }
    }
}

#[derive(Args)]
struct ExecuteArgs {
    /// Request DTO with sql/params or an atomic statements array; - reads stdin.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// SQL statement to execute; use --sql-file to read from a file.
    #[arg(value_name = "SQL", conflicts_with = "sql_file", required_unless_present_any = ["input", "sql_file"])]
    sql: Option<String>,
    /// UTF-8 SQL file, or - for stdin.
    #[arg(long, value_name = "SQL_FILE")]
    sql_file: Option<String>,
    /// Typed SQL parameter as a JSON object; repeat for multiple parameters.
    #[arg(
        long = "param",
        conflicts_with = "params_file",
        value_name = "PARAM_JSON"
    )]
    params: Vec<String>,
    /// JSON array of typed params, or - for stdin.
    #[arg(long, value_name = "PARAMS_FILE")]
    params_file: Option<String>,
    /// SQL read consistency: strong, snapshot or at_least.
    #[arg(long, value_parser = ["strong", "snapshot", "at_least"])]
    consistency: Option<String>,
    /// Commit token used by at_least reads to require a minimum data version.
    #[arg(long)]
    commit_token: Option<String>,
    /// Retained snapshot ID to read with --consistency snapshot.
    #[arg(long)]
    snapshot_id: Option<String>,
    /// Maximum number of rows returned by a SQL query.
    #[arg(long)]
    max_rows: Option<u64>,
}
impl From<ExecuteArgs> for Input {
    fn from(args: ExecuteArgs) -> Self {
        Self {
            input: args.input,
            sql: args.sql,
            sql_file: args.sql_file,
            params: args.params,
            params_file: args.params_file,
            consistency: args.consistency,
            commit_token: args.commit_token,
            snapshot_id: args.snapshot_id,
            max_rows: args.max_rows,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct SnapshotListArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Maximum number of retained snapshots to list in one page.
    #[arg(long)]
    limit: Option<u64>,
    /// Pagination cursor returned by a previous list operation.
    #[arg(long)]
    cursor: Option<String>,
}
impl From<SnapshotListArgs> for Input {
    fn from(args: SnapshotListArgs) -> Self {
        Self {
            input: args.input,
            limit: args.limit,
            cursor: args.cursor,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct SnapshotGetArgs {
    /// Strict JSON DTO file, or - for stdin. Conflicting explicit arguments are rejected.
    #[arg(long, value_name = "REQUEST_FILE")]
    input: Option<String>,
    /// Retained snapshot ID to inspect.
    #[arg(value_name = "SNAPSHOT_ID", required_unless_present = "input")]
    snapshot_id: Option<String>,
}
impl From<SnapshotGetArgs> for Input {
    fn from(args: SnapshotGetArgs) -> Self {
        Self {
            input: args.input,
            snapshot_id: args.snapshot_id,
            ..Default::default()
        }
    }
}

#[derive(Args)]
struct FileRestore {
    /// Local SQLite backup file to restore.
    #[arg(value_name = "SQLITE_FILE")]
    file: PathBuf,
}
/// Global context options may appear before or after the subcommand.
fn cli_command() -> clap::Command {
    let mut command = Cli::command();
    command.build();
    scope_context_help(&mut command, &[]);
    command
}

// Clap recognizes subcommands before required positional arguments. Protect the
// database operand so names such as "update" and "help" remain ordinary names.
fn parse_cli(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
) -> Result<Cli, clap::Error> {
    let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    let mut in_db = false;
    let mut original = None;
    let mut index = 1;
    while index < args.len() {
        let value = args[index].to_string_lossy();
        if matches!(value.as_ref(), "--config" | "--output" | "--deadline-ms")
            || (in_db && value == "--input")
        {
            index += 2;
            continue;
        }
        if value.starts_with('-') {
            index += 1;
            continue;
        }
        if !in_db {
            if value != "db" {
                break;
            }
            in_db = true;
        } else {
            original = Some(args[index].clone());
            args[index] = "__ponydb_database_operand__".into();
            break;
        }
        index += 1;
    }
    let matches = cli_command().try_get_matches_from(args)?;
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let (Command::Db(db), Some(name)) = (&mut cli.command, original) {
        db.name = name.into_string().map_err(|_| {
            clap::Error::raw(
                clap::error::ErrorKind::InvalidUtf8,
                "Database name must be UTF-8",
            )
        })?;
    }
    Ok(cli)
}

fn scope_context_help(command: &mut clap::Command, path: &[String]) {
    let top = path.first().map(String::as_str);
    *command = std::mem::take(command).mut_args(|arg| {
        let hidden = match arg.get_id().as_str() {
            "output" => matches!(top, Some("server" | "mcp")),
            "deadline_ms" => {
                matches!(top, Some("server" | "config" | "version"))
                    || path.iter().any(|part| part == "restore")
            }
            _ => false,
        };
        arg.hide(hidden)
    });
    for child in command.get_subcommands_mut() {
        let mut child_path = path.to_vec();
        child_path.push(child.get_name().to_owned());
        scope_context_help(child, &child_path);
    }
}

fn merge(v: &mut Value, key: &str, value: Value) -> Result<(), String> {
    if v.get(key).is_some_and(|old| *old != value) {
        return Err(format!("Conflicting value for {key}"));
    }
    v.as_object_mut()
        .ok_or("Input must be an object")?
        .insert(key.into(), value);
    Ok(())
}
const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
fn read_source(path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    if path == "-" {
        std::io::stdin()
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    } else {
        bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(format!("{path}: input exceeds 8 MiB"));
    }
    Ok(bytes)
}
fn read_json(path: &str) -> Result<Value, String> {
    protocol::strict_json(&read_source(path)?).map_err(|e| format!("{path}: {e}"))
}
fn read_text(path: &str) -> Result<String, String> {
    let bytes = read_source(path)?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("{path}: input is not valid UTF-8"))
}
fn load_input(input: Input, name: &str) -> Result<Value, String> {
    let stdin_sources = [
        &input.input,
        &input.sql_file,
        &input.params_file,
        &input.metadata_file,
    ]
    .iter()
    .filter(|p| p.as_deref() == Some("-"))
    .count();
    if stdin_sources > 1 {
        return Err("At most one input may read from stdin (-)".into());
    }
    let mut v = match &input.input {
        Some(path) => read_json(path)?,
        None => json!({}),
    };
    if !v.is_object() {
        return Err("Request file must contain a JSON object".into());
    }
    let sql = match (input.sql, &input.sql_file) {
        (Some(sql), _) => Some(sql),
        (None, Some(path)) => Some(read_text(path)?),
        (None, None) => None,
    };
    if let Some(path) = &input.metadata_file {
        merge(&mut v, "metadata", read_json(path)?)?;
    }
    if let Some(path) = &input.params_file {
        let params = read_json(path)?;
        if !params.is_array() {
            return Err("--params-file must contain a JSON array".into());
        }
        merge(&mut v, "params", params)?;
    }
    for (key, value) in [
        ("database", input.database),
        ("if_match", input.if_match),
        ("sql", sql),
        ("consistency", input.consistency),
        ("commit_token", input.commit_token),
        ("name", input.name),
        ("description", input.description),
        ("cursor", input.cursor),
        ("table", input.table),
        ("type", input.object_type),
        ("snapshot_id", input.snapshot_id),
    ] {
        if let Some(value) = value {
            merge(&mut v, key, json!(value))?;
        }
    }
    for (key, value) in [
        ("limit", input.limit),
        ("budget_ms", input.budget_ms),
        ("max_segments", input.max_segments),
        ("max_objects", input.max_objects),
        ("max_rows", input.max_rows),
        ("snapshot_retention_days", input.snapshot_retention_days),
    ] {
        if let Some(value) = value {
            merge(&mut v, key, json!(value))?;
        }
    }
    if name == "database_compact" {
        // Apply defaults only after merging explicit values with the request DTO.
        for (key, default, maximum) in [("budget_ms", 10000, 60000), ("max_segments", 32, 32)] {
            if v.get(key).is_none() {
                merge(&mut v, key, json!(default))?;
            }
            if !v[key]
                .as_u64()
                .is_some_and(|value| (1..=maximum).contains(&value))
            {
                return Err(format!("{key} must be an integer between 1 and {maximum}"));
            }
        }
    }
    for (key, set) in [("include_deleted", input.include_deleted)] {
        if set {
            merge(&mut v, key, json!(true))?;
        }
    }
    if !input.params.is_empty() {
        let params = input
            .params
            .into_iter()
            .map(|p| protocol::strict_json(p.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        merge(&mut v, "params", json!(params))?;
    }
    if let Some(file) = input.file {
        merge(&mut v, "file", json!(file))?;
    }
    Ok(v)
}
fn mapped(command: Command) -> (&'static str, Input) {
    let (name, mut input, database) = match command {
        Command::Databases(Databases::Create(i)) => ("database_create", i.into(), None),
        Command::Databases(Databases::List(i)) => ("database_list", i.into(), None),
        Command::Db(db) => {
            let (name, input): (_, Input) = match db.action {
                None => ("database_get", DatabaseArgs { input: db.input }.into()),
                Some(Db::Update(i)) => ("database_update", i.into()),
                Some(Db::Delete(i)) => ("database_delete", i.into()),
                Some(Db::Compact(i)) => ("database_compact", i.into()),
                Some(Db::Gc(i)) => ("database_gc", i.into()),
                Some(Db::Execute(i)) => ("sql_execute", i.into()),
                Some(Db::Schema(i)) => ("schema_get", i.into()),
                Some(Db::Export(i)) => ("database_export", i.into()),
                Some(Db::Snapshot(Snapshot::List(i))) => ("snapshot_list", i.into()),
                Some(Db::Snapshot(Snapshot::Get(i))) => ("snapshot_get", i.into()),
                Some(Db::Snapshot(Snapshot::Export(i))) => ("snapshot_export", i.into()),
                _ => unreachable!(),
            };
            (name, input, Some(db.name))
        }
        _ => unreachable!(),
    };
    input.database = database;
    (name, input)
}
async fn invoke<S: object_store_api::BlobStore + 'static>(
    service: &Service<S>,
    name: &str,
    input: Value,
    deadline: u64,
) -> Result<Value, protocol::ApiError> {
    let op: Operation =
        serde_json::from_value(json!({"operation":name,"input":input})).map_err(|_| {
            error(
                "INVALID_REQUEST",
                "Missing fields, unknown fields or invalid types",
            )
        })?;
    let ctx = RequestContext::new(Duration::from_millis(deadline));
    let principal = Principal::direct();
    let call = service.call(&principal, op, ctx.clone());
    tokio::pin!(call);
    let result = tokio::select! {
        result = &mut call => result,
        _ = termination_signal() => { ctx.cancel(); call.await },
    };
    service.wait_for_gc().await;
    result
}
async fn termination_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = signal.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
async fn restore_file<S: object_store_api::BlobStore + 'static>(
    service: &Service<S>,
    database: &str,
    input: FileRestore,
) -> Result<Value, protocol::ApiError> {
    let ctx = RequestContext::unbounded();
    let (display, progress) = restore_progress::ProgressDisplay::start();
    let principal = Principal::direct();
    let call = service.restore_file(&principal, database, input.file, ctx.clone(), progress);
    tokio::pin!(call);
    let result = tokio::select! {
        result = &mut call => result,
        _ = termination_signal() => { ctx.cancel(); call.await },
    };
    drop(display);
    result
}
fn cell(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        Value::Object(o) if o.contains_key("type") => match o.get("value") {
            None | Some(Value::Null) => "NULL".into(),
            Some(v) => cell(v),
        },
        other => other.to_string(),
    }
}
fn table(header: &[String], rows: &[Vec<String>]) -> String {
    let mut width: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, c) in row.iter().enumerate() {
            if i < width.len() {
                width[i] = width[i].max(c.chars().count());
            }
        }
    }
    let line = |cells: &[String]| {
        cells
            .iter()
            .zip(&width)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join(" | ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![
        line(header),
        width
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-"),
    ];
    out.extend(rows.iter().map(|r| line(r)));
    out.join("\n")
}
/// Human-oriented rendering. JSON remains the stable scripting format.
fn render_table(value: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(results) = value["results"].as_array() {
        for r in results {
            let header: Vec<String> = r["columns"]
                .as_array()
                .map(|c| c.iter().map(cell).collect())
                .unwrap_or_default();
            let rows: Vec<Vec<String>> = r["rows"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            row.as_array()
                                .map(|c| c.iter().map(cell).collect())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            if header.is_empty() {
                parts.push(format!("rows_affected: {}", cell(&r["rows_affected"])));
            } else {
                parts.push(table(&header, &rows));
            }
        }
    }
    for key in ["databases", "snapshots"] {
        if let Some(items) = value[key].as_array().filter(|i| !i.is_empty()) {
            let mut header: Vec<String> = Vec::new();
            for item in items {
                for k in item.as_object().into_iter().flat_map(|o| o.keys()) {
                    if !header.contains(k) {
                        header.push(k.clone());
                    }
                }
            }
            let rows: Vec<Vec<String>> = items
                .iter()
                .map(|i| header.iter().map(|h| cell(&i[h])).collect())
                .collect();
            parts.push(table(&header, &rows));
        }
    }
    if let Some(object) = value.as_object() {
        let rest: Vec<Vec<String>> = object
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "results" | "databases" | "snapshots"))
            .map(|(k, v)| vec![k.clone(), cell(v)])
            .collect();
        if !rest.is_empty() {
            parts.push(table(&["field".into(), "value".into()], &rest));
        }
    } else {
        parts.push(cell(value));
    }
    parts.join("\n\n")
}
fn version() -> Value {
    let engine = engine_sqlite::diagnostics();
    json!({
        "program": env!("CARGO_PKG_VERSION"),
        "sqlite_version": engine.as_ref().map(|d| d.version.clone()).ok(),
        "sqlite_source_id": engine.as_ref().map(|d| d.source_id.clone()).ok(),
        "mcp_protocol_version": "2026-07-28",
        "mcp_stdio_protocol_version": mcp_server::stdio::VERSION,
        "mcp_stdio_supported_versions": mcp_server::stdio::VERSIONS,
        "storage_layout_version": storage_format::LAYOUT_VERSION,
        "operations": protocol::tools().len(),
    })
}
async fn config_command(
    config: &runtime_config::AppConfig,
    command: &Config,
) -> Result<Value, protocol::ApiError> {
    let target = match command {
        Config::Show { target, .. } | Config::Validate { target } => target.as_str(),
    };
    if let Config::Show { .. } = command {
        return Ok(config.effective(target));
    }
    if matches!(target, "server" | "all") {
        config
            .server
            .auth
            .validate()
            .map_err(|message| error("INVALID_CONFIG", &message))?;
    }
    let store = config
        .s3()
        .map_err(|e| error("INVALID_CONFIG", &e.to_string()))?;
    ponydb::storage_validation::validate(
        &store,
        Duration::from_millis(config.requests.default_timeout_ms),
    )
    .await
    .map_err(|message| error("INVALID_CONFIG", &message))?;
    Ok(json!({"valid":true,"target":target,"storage_capabilities_verified":true}))
}
#[tokio::main]
async fn main() {
    let args = parse_cli(std::env::args_os()).unwrap_or_else(|error| error.exit());
    let is_streaming = matches!(args.command, Command::Mcp | Command::Server { .. });
    let mut output = args.output.clone();
    let result = run_with_logging(args, &mut output, ponydb::logging::init).await;
    let code = match result {
        Ok(value) => {
            if is_streaming {
                return;
            }
            if output.as_deref() == Some("table") {
                println!("{}", render_table(&value));
            } else {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            }
            if value["outcome"] == "partial" {
                6
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("{}", serde_json::to_string_pretty(&e).unwrap());
            match e.code.as_str() {
                "INVALID_CONFIG" | "INVALID_REQUEST" => 2,
                "FORBIDDEN" | "UNAUTHENTICATED" => 3,
                "TX_CONFLICT" | "METADATA_PRECONDITION_FAILED" => 4,
                "OUTCOME_UNKNOWN" => 5,
                _ => 1,
            }
        }
    };
    std::process::exit(code);
}
fn validate_context(args: &Cli) -> Result<(), protocol::ApiError> {
    if matches!(&args.command, Command::Db(db) if db.input.is_some() && db.action.is_some()) {
        return Err(error("INVALID_REQUEST", "--input before an action is only valid for database details; place it after the action"));
    }
    if args.deadline_ms.is_some()
        && matches!(&args.command, Command::Db(db) if matches!(db.action, Some(Db::Restore(_))))
    {
        return Err(error(
            "INVALID_REQUEST",
            "--deadline-ms is not valid for restore; restore has no execution deadline",
        ));
    }
    if args.deadline_ms.is_some()
        && matches!(
            args.command,
            Command::Config(_) | Command::Version | Command::Server { .. }
        )
    {
        return Err(error(
            "INVALID_REQUEST",
            "--deadline-ms is only valid for business operations and mcp",
        ));
    }
    if args.output.is_some() && matches!(args.command, Command::Mcp | Command::Server { .. }) {
        return Err(error(
            "INVALID_REQUEST",
            "--output is not valid for server or mcp",
        ));
    }
    Ok(())
}

#[cfg(test)]
async fn run(args: Cli, output: &mut Option<String>) -> Result<Value, protocol::ApiError> {
    run_with_logging(args, output, |_| Ok(())).await
}
async fn run_with_logging(
    args: Cli,
    output: &mut Option<String>,
    initialize: impl FnOnce(&runtime_config::Logging) -> Result<(), &'static str>,
) -> Result<Value, protocol::ApiError> {
    validate_context(&args)?;
    if let Command::Version = args.command {
        return Ok(version());
    }
    let mut overrides = BTreeMap::new();
    if let Command::Server {
        listen: Some(listen),
    } = &args.command
    {
        overrides.insert("server.listen".into(), listen.clone());
    }
    let config = runtime_config::AppConfig::load(args.config.as_deref(), overrides)
        .map_err(|e| error("INVALID_CONFIG", &e.to_string()))?;
    initialize(&config.logging).map_err(|message| error("INVALID_CONFIG", message))?;
    if matches!(args.command, Command::Server { .. } | Command::Mcp) {
        observability::phase("configuration_loaded");
        return run_configured(args, output, config).await;
    }
    let mut logs = observability::Request::new("cli", None);
    let result = logs
        .scope(async {
            observability::phase("configuration_loaded");
            run_configured(args, output, config).await
        })
        .await;
    match &result {
        Ok(value) => logs.finish(
            observability::outcome(value["outcome"].as_str().unwrap_or("")),
            "",
        ),
        Err(e) => logs.finish(observability::error_outcome(&e.code, &e.outcome), &e.code),
    }
    result
}

async fn run_configured(
    args: Cli,
    output: &mut Option<String>,
    config: runtime_config::AppConfig,
) -> Result<Value, protocol::ApiError> {
    output.get_or_insert_with(|| config.cli.output.clone());
    if let Command::Config(command) = &args.command {
        return config_command(&config, command).await;
    }
    if matches!(args.command, Command::Server { .. }) {
        config
            .server
            .auth
            .validate()
            .map_err(|e| error("INVALID_CONFIG", &e))?;
        config
            .s3()
            .map_err(|e| error("INVALID_CONFIG", &e.to_string()))?;
        ponydb::server::serve(&config)
            .await
            .map_err(|e| error("IO_ERROR", &e.to_string()))?;
        return Ok(Value::Null);
    }
    let restoring =
        matches!(&args.command, Command::Db(db) if matches!(db.action, Some(Db::Restore(_))));
    let deadline = if restoring {
        0
    } else {
        args.deadline_ms
            .unwrap_or(config.requests.default_timeout_ms)
    };
    if !restoring && (deadline == 0 || deadline > config.requests.max_timeout_ms) {
        return Err(error(
            "INVALID_REQUEST",
            "Deadline exceeds configured maximum",
        ));
    }
    if matches!(args.command, Command::Mcp) {
        let service = Arc::new(Service::new(
            Arc::new(
                config
                    .s3()
                    .map_err(|e| error("INVALID_CONFIG", &e.to_string()))?,
            ),
            config.namespace(),
            config.service_options(),
        )?);
        let result = {
            let serve = mcp_server::stdio::serve(
                service.clone(),
                Principal::direct(),
                tokio::io::BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
                Duration::from_millis(deadline),
            );
            tokio::pin!(serve);
            tokio::select! { result = &mut serve => result, _ = termination_signal() => Ok(()) }
        };
        service.wait_for_gc().await;
        result.map_err(|e| error("IO_ERROR", &e.to_string()))?;
        return Ok(Value::Null);
    }
    observability::phase("validating_input");
    // Parse and validate every local input before touching object storage.
    enum Request {
        Business(&'static str, Value),
        Restore(String, FileRestore),
        Export(ExportFile, tempfile::NamedTempFile),
    }
    let request = match args.command {
        Command::Db(Database {
            name,
            action: Some(Db::Restore(Restore::File(input))),
            ..
        }) => Request::Restore(name, input),
        command => {
            let (name, input) = mapped(command);
            let input = load_input(input, name).map_err(|e| error("INVALID_REQUEST", &e))?;
            if name == "snapshot_export" {
                let input: SnapshotExportFile = serde_json::from_value(input)
                    .map_err(|e| error("INVALID_REQUEST", &e.to_string()))?;
                let temporary = input.prepare()?;
                Request::Export(input.into(), temporary)
            } else if name == "database_export" {
                let input: DatabaseExportFile = serde_json::from_value(input)
                    .map_err(|e| error("INVALID_REQUEST", &e.to_string()))?;
                let temporary = prepare_export(&input.file)?;
                Request::Export(input.into(), temporary)
            } else {
                let operation = serde_json::from_value::<Operation>(
                    json!({"operation":name,"input":input.clone()}),
                )
                .map_err(|e| error("INVALID_REQUEST", &e.to_string()))?;
                match &operation {
                    Operation::DatabaseCreate(input) => input.validate(),
                    Operation::DatabaseUpdate(input) => input.validate(),
                    _ => Ok(()),
                }
                .map_err(|e| error("INVALID_REQUEST", &e))?;
                if let Operation::DatabaseGc(ref gc) = operation {
                    config.service_options().gc_limits(gc)?;
                }
                Request::Business(name, input)
            }
        }
    };
    observability::phase("initializing_service");
    let service = Service::new(
        Arc::new(
            config
                .s3()
                .map_err(|e| error("INVALID_CONFIG", &e.to_string()))?,
        ),
        config.namespace(),
        config.service_options(),
    )?;
    match request {
        Request::Restore(database, input) => restore_file(&service, &database, input).await,
        Request::Business(name, input) => invoke(&service, name, input, deadline).await,
        Request::Export(input, temporary) => {
            export_file(&service, input, temporary, deadline).await
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        parse_cli(std::iter::once("ponydb").chain(args.iter().copied()))
    }

    #[test]
    fn command_definitions_are_consistent() {
        cli_command().debug_assert();
    }

    #[tokio::test]
    async fn metadata_limits_are_validated_locally_for_files_and_dtos() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let metadata = directory.path().join("metadata.json");
        let request = directory.path().join("request.json");
        let oversized = json!({"x": "a".repeat(protocol::MAX_DATABASE_METADATA_BYTES - 7)});
        std::fs::write(&metadata, serde_json::to_vec(&oversized).unwrap()).unwrap();
        for command in [
            vec!["databases", "create", "notes"],
            vec!["db", "notes", "update"],
        ] {
            for (flag, path) in [("--metadata-file", &metadata), ("--input", &request)] {
                std::fs::write(
                    &request,
                    serde_json::to_vec(&json!({"metadata":oversized})).unwrap(),
                )
                .unwrap();
                for global_before in [true, false] {
                    let mut args = command.clone();
                    args.extend([flag, path.to_str().unwrap()]);
                    let position = if global_before { 0 } else { args.len() };
                    args.splice(position..position, ["--config", config.to_str().unwrap()]);
                    // No usable storage configuration: validation must run before S3 setup.
                    let error = run(parse(&args).unwrap(), &mut None).await.unwrap_err();
                    assert_eq!(error.code, "INVALID_REQUEST");
                    assert!(error.message.contains("metadata exceeds 64 KiB"));
                }
            }
            let description = "a".repeat(protocol::MAX_DATABASE_DESCRIPTION_BYTES + 1);
            let mut args = command;
            args.extend([
                "--config",
                config.to_str().unwrap(),
                "--description",
                &description,
            ]);
            let error = run(parse(&args).unwrap(), &mut None).await.unwrap_err();
            assert_eq!(error.code, "INVALID_REQUEST");
            assert!(error.message.contains("description exceeds 16 KiB"));
        }
    }

    #[tokio::test]
    async fn compact_optional_limits_and_dto_defaults() {
        let service = Service::new(
            Arc::new(object_store_test_support::MemoryStore::default()),
            "test".into(),
            Default::default(),
        )
        .unwrap();
        invoke(&service, "database_create", json!({"name":"notes"}), 30000)
            .await
            .unwrap();
        for (options, budget, segments) in [
            (vec![], 10000, 32),
            (vec!["--budget-ms", "5000"], 5000, 32),
            (vec!["--max-segments", "16"], 10000, 16),
            (
                vec!["--budget-ms", "60000", "--max-segments", "32"],
                60000,
                32,
            ),
        ] {
            let mut args = vec!["db", "notes", "compact"];
            args.extend(options);
            let (name, input) = mapped(parse(&args).unwrap().command);
            assert_eq!(name, "database_compact");
            let value = load_input(input, name).unwrap();
            assert_eq!(
                value,
                json!({"database":"notes","budget_ms":budget,"max_segments":segments})
            );
            invoke(&service, name, value, 30000).await.unwrap();
        }

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("compact.json");
        let path = file.to_str().unwrap();
        for (dto, options, budget, segments) in [
            (json!({}), vec![], 10000, 32),
            (json!({"budget_ms":5000}), vec![], 5000, 32),
            (json!({"max_segments":16}), vec![], 10000, 16),
            (
                json!({"budget_ms":5000}),
                vec!["--max-segments", "16"],
                5000,
                16,
            ),
            (
                json!({"database":"notes","budget_ms":5000,"max_segments":16}),
                vec!["--budget-ms", "5000", "--max-segments", "16"],
                5000,
                16,
            ),
        ] {
            std::fs::write(&file, dto.to_string()).unwrap();
            let mut args = vec!["db", "notes", "compact", "--input", path];
            args.extend(options);
            let (name, input) = mapped(parse(&args).unwrap().command);
            let value = load_input(input, name).unwrap();
            assert_eq!(
                value,
                json!({"database":"notes","budget_ms":budget,"max_segments":segments})
            );
            assert!(
                serde_json::from_value::<Operation>(json!({"operation":name,"input":value}))
                    .is_ok()
            );
        }
        for key in ["budget_ms", "max_segments"] {
            for value in [
                json!(0),
                json!(-1),
                json!(60001),
                json!(1.5),
                json!("1"),
                Value::Null,
            ] {
                std::fs::write(&file, json!({key:value}).to_string()).unwrap();
                let (name, input) = mapped(
                    parse(&["db", "notes", "compact", "--input", path])
                        .unwrap()
                        .command,
                );
                assert!(load_input(input, name).is_err(), "{key}: {value}");
            }
        }
        std::fs::write(&file, r#"{"max_segments":33}"#).unwrap();
        let (name, input) = mapped(
            parse(&["db", "notes", "compact", "--input", path])
                .unwrap()
                .command,
        );
        assert!(load_input(input, name).is_err());
        for (dto, options) in [
            (json!({"budget_ms":5000}), vec!["--budget-ms", "10000"]),
            (json!({"max_segments":16}), vec!["--max-segments", "32"]),
            (json!({"database":"other"}), vec![]),
        ] {
            std::fs::write(&file, dto.to_string()).unwrap();
            let mut args = vec!["db", "notes", "compact", "--input", path];
            args.extend(options);
            let (name, input) = mapped(parse(&args).unwrap().command);
            assert!(load_input(input, name).is_err());
        }
        for contents in [r#"{"budget_ms":1,"budget_ms":2}"#, r#"{"unknown":true}"#] {
            std::fs::write(&file, contents).unwrap();
            let (name, input) = mapped(
                parse(&["db", "notes", "compact", "--input", path])
                    .unwrap()
                    .command,
            );
            assert!(load_input(input, name)
                .and_then(|value| serde_json::from_value::<Operation>(
                    json!({"operation":name,"input":value})
                )
                .map_err(|e| e.to_string()))
                .is_err());
        }
        for (option, values) in [
            ("--budget-ms", ["0", "60001"]),
            ("--max-segments", ["0", "33"]),
        ] {
            for value in values {
                assert!(parse(&["db", "notes", "compact", option, value]).is_err());
            }
        }
        for position in 0..=3 {
            let mut args = vec!["db", "notes", "compact"];
            args.splice(
                position..position,
                ["--output", "json", "--deadline-ms", "30000"],
            );
            let cli = parse(&args).unwrap();
            validate_context(&cli).unwrap();
            let (name, input) = mapped(cli.command);
            assert_eq!(
                load_input(input, name).unwrap(),
                json!({"database":"notes","budget_ms":10000,"max_segments":32})
            );
            let mut args = vec!["db", "notes", "compact"];
            args.splice(position..position, ["--max-objects", "1"]);
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn database_export_arguments_and_strict_dto() {
        for args in [
            vec!["db", "notes", "export", "backup.sqlite"],
            vec![
                "--output",
                "table",
                "--deadline-ms",
                "30000",
                "db",
                "notes",
                "export",
                "backup.sqlite",
            ],
            vec![
                "db",
                "notes",
                "export",
                "backup.sqlite",
                "--output",
                "json",
                "--deadline-ms",
                "30000",
            ],
        ] {
            let cli = parse(&args).unwrap();
            validate_context(&cli).unwrap();
            let (name, input) = mapped(cli.command);
            assert_eq!(name, "database_export");
            assert_eq!(
                load_input(input, name).unwrap(),
                json!({"database":"notes","file":"backup.sqlite"})
            );
        }
        for args in [
            vec!["db", "notes", "export"],
            vec!["db", "export", "backup.sqlite"],
            vec!["db", "notes", "export", "snap", "backup.sqlite"],
            vec![
                "db",
                "notes",
                "export",
                "backup.sqlite",
                "--snapshot-id",
                "snap",
            ],
            vec!["db", "notes", "export", "backup.sqlite", "--limit", "1"],
            vec!["--limit", "1", "db", "notes", "export", "backup.sqlite"],
            vec![
                "--if-match",
                "etag",
                "db",
                "notes",
                "export",
                "backup.sqlite",
            ],
            vec![
                "db",
                "notes",
                "export",
                "backup.sqlite",
                "--budget-ms",
                "1000",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let cli = parse(&[
            "db",
            "notes",
            "--input",
            "request.json",
            "export",
            "backup.sqlite",
        ])
        .unwrap();
        assert!(validate_context(&cli).is_err());

        let directory = tempfile::tempdir().unwrap();
        let dto = directory.path().join("export.json");
        let request = json!({"database":"notes","file":"backup.sqlite"});
        std::fs::write(&dto, request.to_string()).unwrap();
        for positional in [false, true] {
            let mut args = vec!["db", "notes", "export", "--input", dto.to_str().unwrap()];
            if positional {
                args.push("backup.sqlite");
            }
            let (name, input) = mapped(parse(&args).unwrap().command);
            let value = load_input(input, name).unwrap();
            assert_eq!(value, request);
            assert!(serde_json::from_value::<DatabaseExportFile>(value).is_ok());
        }
        for args in [
            vec!["db", "other", "export", "--input", dto.to_str().unwrap()],
            vec![
                "db",
                "notes",
                "export",
                "other.sqlite",
                "--input",
                dto.to_str().unwrap(),
            ],
        ] {
            let (name, input) = mapped(parse(&args).unwrap().command);
            assert!(load_input(input, name).is_err());
        }
        for content in [
            r#"{}"#,
            r#"{"file":null}"#,
            r#"{"file":12}"#,
            r#"{"file":"backup.sqlite","snapshot_id":"snap"}"#,
            r#"{"file":"backup.sqlite","unknown":true}"#,
            r#"{"file":"a","file":"b"}"#,
            r#"[]"#,
        ] {
            std::fs::write(&dto, content).unwrap();
            let (name, input) = mapped(
                parse(&["db", "notes", "export", "--input", dto.to_str().unwrap()])
                    .unwrap()
                    .command,
            );
            let valid = load_input(input, name)
                .ok()
                .and_then(|value| serde_json::from_value::<DatabaseExportFile>(value).ok());
            assert!(valid.is_none(), "{content}");
        }
        for path in [
            PathBuf::new(),
            PathBuf::from("-"),
            directory.path().join("missing/backup.sqlite"),
        ] {
            assert!(prepare_export(&path).is_err());
        }
    }

    #[tokio::test]
    async fn database_export_current_version_and_atomic_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.sqlite");
        let service = Service::new(
            Arc::new(object_store_test_support::MemoryStore::default()),
            "test".into(),
            Default::default(),
        )
        .unwrap();
        invoke(&service, "database_create", json!({"name":"notes"}), 30000)
            .await
            .unwrap();
        let written = invoke(
            &service,
            "sql_execute",
            json!({"database":"notes","statements":[
                {"sql":"CREATE TABLE t(x)"}, {"sql":"INSERT INTO t VALUES(42)"}
            ]}),
            30000,
        )
        .await
        .unwrap();
        let input = DatabaseExportFile {
            database: "notes".into(),
            file: path.clone(),
        };
        let temporary = prepare_export(&input.file).unwrap();
        let result = export_file(&service, input, temporary, 30000)
            .await
            .unwrap();
        assert_eq!(result["data_version"], written["data_version"]);
        assert_eq!(result["outcome"], "read_only");
        assert_eq!(result["file"], json!(path));
        let bytes = std::fs::read(&path).unwrap();
        use sha2::Digest;
        assert_eq!(
            result["sha256"],
            format!("{:x}", sha2::Sha256::digest(&bytes))
        );
        assert_eq!(result["total_bytes"], bytes.len().to_string());
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            db.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            42
        );
        drop(db);
        assert!(prepare_export(&path).is_err());
        for (database, expected) in [
            ("missing", "DATABASE_NOT_FOUND"),
            ("notes", "DATABASE_DELETED"),
        ] {
            if database == "notes" {
                invoke(
                    &service,
                    "database_delete",
                    json!({"database":"notes"}),
                    30000,
                )
                .await
                .unwrap();
            }
            let input = DatabaseExportFile {
                database: database.into(),
                file: directory.path().join("failed.sqlite"),
            };
            let temporary = prepare_export(&input.file).unwrap();
            assert_eq!(
                export_file(&service, input, temporary, 30000)
                    .await
                    .unwrap_err()
                    .code,
                expected
            );
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
        // A destination created after preparation must survive the final publication attempt.
        invoke(&service, "database_create", json!({"name":"notes"}), 30000)
            .await
            .unwrap();
        let race = directory.path().join("race.sqlite");
        let input = DatabaseExportFile {
            database: "notes".into(),
            file: race.clone(),
        };
        let temporary = prepare_export(&input.file).unwrap();
        std::fs::write(&race, b"keep").unwrap();
        assert!(export_file(&service, input, temporary, 30000)
            .await
            .is_err());
        assert_eq!(std::fs::read(&race).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn snapshot_export_cli_inputs_and_atomic_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backup.sqlite");
        let service = Service::new(
            Arc::new(object_store_test_support::MemoryStore::default()),
            "test".into(),
            Default::default(),
        )
        .unwrap();
        invoke(&service, "database_create", json!({"name":"notes"}), 30000)
            .await
            .unwrap();
        let list = invoke(
            &service,
            "snapshot_list",
            json!({"database":"notes"}),
            30000,
        )
        .await
        .unwrap();
        let id = list["snapshots"][0]["snapshot_id"].as_str().unwrap();
        let cli = parse(&[
            "--output",
            "table",
            "db",
            "notes",
            "snapshot",
            "export",
            id,
            path.to_str().unwrap(),
            "--deadline-ms",
            "30000",
        ])
        .unwrap();
        let (name, input) = mapped(cli.command);
        assert_eq!(name, "snapshot_export");
        let value = load_input(input, name).unwrap();
        let input: SnapshotExportFile = serde_json::from_value(value.clone()).unwrap();
        let temporary = input.prepare().unwrap();
        let result = export_file(&service, input, temporary, 30000)
            .await
            .unwrap();
        assert_eq!(result["file"], json!(path));
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let input: SnapshotExportFile = serde_json::from_value(value.clone()).unwrap();
        assert!(input.prepare().is_err());
        let dto = directory.path().join("request.json");
        std::fs::write(&dto, value.to_string()).unwrap();
        let cli = parse(&[
            "db",
            "notes",
            "snapshot",
            "export",
            "--input",
            dto.to_str().unwrap(),
        ])
        .unwrap();
        let (name, input) = mapped(cli.command);
        assert_eq!(load_input(input, name).unwrap(), value);
        let cli = parse(&[
            "db",
            "other",
            "snapshot",
            "export",
            "--input",
            dto.to_str().unwrap(),
        ])
        .unwrap();
        let (name, input) = mapped(cli.command);
        assert!(load_input(input, name).is_err());
        for args in [
            vec!["db", "notes", "snapshot", "export"],
            vec!["db", "notes", "snapshot", "export", id],
            vec![
                "db", "notes", "snapshot", "export", id, "file", "--limit", "1",
            ],
            vec![
                "--limit", "1", "db", "notes", "snapshot", "export", id, "file",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let unknown = json!({"database":"notes","snapshot_id":id,"file":"backup.sqlite","limit":1});
        assert!(serde_json::from_value::<SnapshotExportFile>(unknown).is_err());
        assert!(protocol::strict_json(br#"{"file":"a","file":"b"}"#).is_err());
        let dash = SnapshotExportFile {
            database: "notes".into(),
            snapshot_id: id.into(),
            file: "-".into(),
        };
        assert!(dash.prepare().is_err());
        // If another writer wins after preflight, preserve its destination.
        let race_path = directory.path().join("race.sqlite");
        let input = SnapshotExportFile {
            database: "notes".into(),
            snapshot_id: id.into(),
            file: race_path.clone(),
        };
        let temporary = input.prepare().unwrap();
        std::fs::write(&race_path, b"keep").unwrap();
        assert!(export_file(&service, input, temporary, 30000)
            .await
            .is_err());
        assert_eq!(std::fs::read(&race_path).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
    }

    #[test]
    fn explicit_database_commands() {
        for database in ["notes", "list", "create", "update", "help", "中文"] {
            for (tail, operation) in [
                (vec![], "database_get"),
                (vec!["schema"], "schema_get"),
                (vec!["execute", "SELECT 1"], "sql_execute"),
                (vec!["snapshot", "list"], "snapshot_list"),
                (vec!["snapshot", "get", "snap"], "snapshot_get"),
                (vec!["update"], "database_update"),
                (vec!["update", "--if-match", "etag"], "database_update"),
                (vec!["delete"], "database_delete"),
                (vec!["delete", "--if-match", "etag"], "database_delete"),
            ] {
                let mut args = vec!["db", database];
                args.extend(tail);
                let (name, input) = mapped(parse(&args).unwrap().command);
                assert_eq!(name, operation);
                assert_eq!(load_input(input, name).unwrap()["database"], database);
            }
        }
    }

    #[test]
    fn schema_filter_arguments_and_dto_conflicts() {
        for kind in protocol::SchemaObjectType::VALUES {
            let (name, input) = mapped(
                parse(&["db", "notes", "schema", "--table", "items", "--type", kind])
                    .unwrap()
                    .command,
            );
            assert_eq!(name, "schema_get");
            assert_eq!(
                load_input(input, name).unwrap(),
                json!({"database":"notes","table":"items","type":kind})
            );
        }
        for args in [
            vec!["db", "notes", "schema", "--table", ""],
            vec!["db", "notes", "schema", "--type", "invalid"],
            vec!["db", "notes", "schema", "--type", "TABLE"],
            vec!["db", "notes", "schema", "--type", "table", "--type", "view"],
            vec!["db", "notes", "schema", "--table"],
            vec!["db", "notes", "schema", "--limit", "1"],
            vec!["--table", "items", "db", "notes", "schema"],
            vec!["db", "notes", "execute", "SELECT 1", "--type", "table"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.json");
        let dto = json!({"database":"notes","table":"items","type":"index"});
        std::fs::write(&path, dto.to_string()).unwrap();
        for (extra, valid) in [
            (vec![], true),
            (vec!["--table", "items", "--type", "index"], true),
            (vec!["--table", "other"], false),
            (vec!["--type", "table"], false),
        ] {
            let mut args = vec!["db", "notes", "schema", "--input", path.to_str().unwrap()];
            args.extend(extra);
            let (name, input) = mapped(parse(&args).unwrap().command);
            let loaded = load_input(input, name);
            assert_eq!(loaded.is_ok(), valid);
            if valid {
                assert_eq!(loaded.unwrap(), dto);
            }
        }
        for position in 0..=3 {
            let mut args = vec!["db", "notes", "schema", "--table", "items"];
            args.splice(
                position..position,
                ["--output", "json", "--deadline-ms", "100"],
            );
            let cli = parse(&args).unwrap();
            assert!(validate_context(&cli).is_ok());
        }
    }

    #[test]
    fn invalid_combinations_are_rejected() {
        for args in [
            vec!["db"],
            vec!["databases", "create", "--input", "r.json"],
            vec!["db", "notes", "--limit", "1"],
            vec!["db", "notes", "schema", "--description", "x"],
            vec!["db", "notes", "gc", "--allow-pause"],
            vec!["db", "notes", "gc", "--pass-id", "old"],
            vec!["db", "notes", "execute", "SELECT 1", "--sql-file", "q.sql"],
            vec![
                "db",
                "notes",
                "execute",
                "SELECT 1",
                "--param",
                "{}",
                "--params-file",
                "p.json",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        for position in 0..=3 {
            let mut args = vec!["db", "notes", "schema"];
            args.splice(position..position, ["--database", "other"]);
            assert!(parse(&args).is_err());
        }
        for args in [
            vec!["db", "notes", "--input", "r.json", "schema"],
            vec!["--deadline-ms", "100", "version"],
            vec!["config", "show", "--deadline-ms", "100"],
            vec!["--output", "json", "mcp"],
            vec!["server", "--output", "json"],
        ] {
            assert!(validate_context(&parse(&args).unwrap()).is_err());
        }
        assert!(parse(&["db", "notes", "restore", "file", "begin"]).is_ok());
        assert!(parse(&["db", "notes", "execute", "--", "-SQL"]).is_ok());
    }

    #[test]
    fn dto_fields_merge_but_database_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request.json");
        let path = path.to_str().unwrap();
        for (dto, valid) in [
            (json!({"if_match":"etag"}), true),
            (json!({"database":"notes","if_match":"etag"}), true),
            (json!({"database":"other","if_match":"etag"}), false),
            (json!({"database":"notes"}), true),
            (json!({"if_match":null}), true),
        ] {
            std::fs::write(path, dto.to_string()).unwrap();
            let (name, input) = mapped(
                parse(&["db", "notes", "update", "--input", path])
                    .unwrap()
                    .command,
            );
            assert_eq!(load_input(input, name).is_ok(), valid);
        }
        std::fs::write(path, r#"{"database":"notes","database":"notes"}"#).unwrap();
        let (name, input) = mapped(parse(&["db", "notes", "--input", path]).unwrap().command);
        assert!(load_input(input, name).is_err());
    }

    #[test]
    fn optional_etag_dto_validation_and_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("request.json");
        let path = file.to_str().unwrap();
        for action in ["update", "delete"] {
            for dto in [
                json!({}),
                json!({"if_match":null}),
                json!({"if_match":"etag"}),
            ] {
                std::fs::write(path, dto.to_string()).unwrap();
                let (name, input) = mapped(
                    parse(&["db", "notes", action, "--input", path])
                        .unwrap()
                        .command,
                );
                let value = load_input(input, name).unwrap();
                assert!(serde_json::from_value::<Operation>(
                    json!({"operation":name,"input":value})
                )
                .is_ok());
            }
            std::fs::write(path, r#"{"if_match":123}"#).unwrap();
            let (name, input) = mapped(
                parse(&["db", "notes", action, "--input", path])
                    .unwrap()
                    .command,
            );
            let value = load_input(input, name).unwrap();
            assert!(
                serde_json::from_value::<Operation>(json!({"operation":name,"input":value}))
                    .is_err()
            );
            std::fs::write(path, r#"{"if_match":"dto"}"#).unwrap();
            let (name, input) = mapped(
                parse(&[
                    "db",
                    "notes",
                    action,
                    "--input",
                    path,
                    "--if-match",
                    "explicit",
                ])
                .unwrap()
                .command,
            );
            assert!(load_input(input, name).is_err());
            for position in 0..=3 {
                let mut args = vec!["db", "notes", action];
                args.splice(position..position, ["--output", "json"]);
                let cli = parse(&args).unwrap();
                assert!(validate_context(&cli).is_ok());
                let (name, input) = mapped(cli.command);
                assert!(load_input(input, name).unwrap().get("if_match").is_none());
                let mut args = vec!["db", "notes", action];
                args.splice(position..position, ["--limit", "1"]);
                assert!(parse(&args).is_err());
            }
        }
    }

    #[test]
    fn globals_and_dto_validation_preserve_explicit_targets() {
        for position in 0..=3 {
            let mut args = vec!["db", "update", "schema"];
            args.splice(position..position, ["--config", "db"]);
            let cli = parse(&args).unwrap();
            assert_eq!(cli.config, Some(PathBuf::from("db")));
            let (name, input) = mapped(cli.command);
            assert_eq!(load_input(input, name).unwrap()["database"], "update");
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("request.json");
        let path = file.to_str().unwrap();
        for (dto, valid) in [
            (json!({"name":"notes"}), true),
            (json!({"name":"other"}), false),
        ] {
            std::fs::write(path, dto.to_string()).unwrap();
            let (name, input) = mapped(
                parse(&["databases", "create", "notes", "--input", path])
                    .unwrap()
                    .command,
            );
            assert_eq!(load_input(input, name).is_ok(), valid);
        }
        for dto in [json!({"unknown":true}), json!({"limit":"bad"})] {
            std::fs::write(path, dto.to_string()).unwrap();
            let (name, input) = mapped(
                parse(&["databases", "list", "--input", path])
                    .unwrap()
                    .command,
            );
            let input = load_input(input, name).unwrap();
            assert!(
                serde_json::from_value::<Operation>(json!({"operation":name,"input":input}))
                    .is_err()
            );
        }
        let (name, input) = mapped(
            parse(&[
                "db",
                "notes",
                "execute",
                "--sql-file",
                "-",
                "--params-file",
                "-",
            ])
            .unwrap()
            .command,
        );
        assert!(load_input(input, name).unwrap_err().contains("stdin"));
    }

    #[test]
    fn gc_optional_limits_preserve_absence() {
        let cli = parse(&[
            "db",
            "notes",
            "gc",
            "--budget-ms",
            "1000",
            "--max-objects",
            "10",
        ])
        .unwrap();
        let (name, input) = mapped(cli.command);
        let value = load_input(input, name).unwrap();
        assert!(value.get("allow_pause").is_none());
        assert!(serde_json::from_value::<protocol::Gc>(value).is_ok());
    }

    #[test]
    fn gc_defaults_dto_conflicts_and_global_positions() {
        for args in [
            vec!["db", "notes", "gc"],
            vec![
                "--deadline-ms",
                "330000",
                "db",
                "notes",
                "gc",
                "--budget-ms",
                "300000",
            ],
            vec!["db", "notes", "gc", "--max-objects", "2"],
        ] {
            let (name, input) = mapped(parse(&args).unwrap().command);
            let value = load_input(input, name).unwrap();
            let input: protocol::Gc = serde_json::from_value(value).unwrap();
            let (budget, objects) = database_service::ServiceOptions::default()
                .gc_limits(&input)
                .unwrap();
            assert_eq!(budget, input.budget_ms.unwrap_or(60000));
            assert_eq!(objects, input.max_objects.unwrap_or(1000));
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), r#"{"budget_ms":300000,"max_objects":2}"#).unwrap();
        let path = file.path().to_str().unwrap();
        let (name, input) = mapped(
            parse(&["db", "notes", "gc", "--input", path])
                .unwrap()
                .command,
        );
        let value = load_input(input, name).unwrap();
        assert_eq!(value["budget_ms"], 300000);
        let (name, input) = mapped(
            parse(&["db", "notes", "gc", "--input", path, "--budget-ms", "1"])
                .unwrap()
                .command,
        );
        assert!(load_input(input, name).is_err());
        for args in [
            vec!["--max-segments", "1", "db", "notes", "gc"],
            vec!["--allow-pause", "db", "notes", "gc"],
            vec!["db", "notes", "gc", "--include-deleted"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn server_flags_parse() {
        for args in [
            vec![
                "ponydb",
                "--config",
                "test.toml",
                "server",
                "--listen",
                "127.0.0.1:0",
            ],
            vec![
                "ponydb",
                "server",
                "--config",
                "test.toml",
                "--listen",
                "127.0.0.1:0",
            ],
        ] {
            let cli = parse_cli(args).unwrap();
            assert_eq!(cli.config, Some(PathBuf::from("test.toml")));
            assert!(
                matches!(cli.command, Command::Server { listen: Some(ref value) } if value == "127.0.0.1:0")
            );
        }
    }
    #[tokio::test]
    async fn server_rejects_business_flags() {
        for flags in [["--output", "json"], ["--deadline-ms", "100"]] {
            let cli = parse_cli(["ponydb", "server", flags[0], flags[1]]).unwrap();
            let result = run(cli, &mut None).await.unwrap_err();
            assert_eq!(result.code, "INVALID_REQUEST");
        }
    }
    #[test]
    fn request_file_flags_merge_and_conflict() {
        let dir = std::env::temp_dir().join(format!("ponydb-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let request = dir.join("r.json");
        std::fs::write(
            &request,
            b"\xEF\xBB\xBF{\"sql\":\"SELECT 1\",\"description\":\"k\"}",
        )
        .unwrap();
        let path = request.to_str().unwrap().to_owned();
        let v = load_input(
            Input {
                input: Some(path.clone()),
                database: Some("db".into()),
                description: Some("k".into()),
                ..Default::default()
            },
            "sql_execute",
        )
        .unwrap();
        assert_eq!(v["database"], "db");
        let conflict = load_input(
            Input {
                input: Some(path),
                database: Some("db".into()),
                description: Some("other".into()),
                ..Default::default()
            },
            "sql_execute",
        );
        assert!(conflict.unwrap_err().contains("description"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn create_requires_explicit_name() {
        let cli = parse_cli(["ponydb", "databases", "create", "中文"]);
        let cli = cli.unwrap();
        let (operation, input) = mapped(cli.command);
        let value = load_input(input, operation).unwrap();
        assert_eq!(value, json!({"name":"中文"}));
        assert!(serde_json::from_value::<protocol::Create>(value).is_ok());
        let value = load_input(Input::default(), "database_create").unwrap();
        assert!(serde_json::from_value::<protocol::Create>(value).is_err());
    }

    #[test]
    fn unified_sql_command_accepts_queries_without_write_fields() {
        let cli = parse_cli([
            "ponydb",
            "db",
            "notes",
            "execute",
            "SELECT 1",
            "--max-rows",
            "10",
        ])
        .unwrap();
        let (name, input) = mapped(cli.command);
        assert_eq!(name, "sql_execute");
        let value = load_input(input, name).unwrap();
        let operation: Operation =
            serde_json::from_value(json!({"operation":name,"input":value})).unwrap();
        assert!(matches!(operation, Operation::SqlExecute(_)));
        assert!(parse_cli(["ponydb", "sql", "query", "--sql", "SELECT 1"]).is_err());
    }
    #[test]
    fn execute_batch_dto_and_explicit_inputs_are_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch.json");
        std::fs::write(
            &path,
            r#"{"statements":[{"sql":"SELECT ?","params":[{"type":"integer","value":"42"}]}]}"#,
        )
        .unwrap();
        let path = path.to_str().unwrap();
        for extra in [
            vec![],
            vec!["SELECT 1"],
            vec!["--param", r#"{"type":"null"}"#],
            vec!["--max-rows", "10"],
        ] {
            let mut args = vec!["db", "notes", "execute", "--input", path];
            args.extend(extra.clone());
            let cli = parse(&args).unwrap();
            validate_context(&cli).unwrap();
            let (name, input) = mapped(cli.command);
            let value = load_input(input, name).unwrap();
            let op = serde_json::from_value::<Operation>(json!({"operation":name,"input":value}));
            assert_eq!(op.is_ok(), extra.is_empty(), "{extra:?}");
        }
        assert!(parse(&["sql", "transaction", "--input", path]).is_err());
    }

    #[test]
    fn table_renders_typed_rows() {
        let out = render_table(
            &json!({"results":[{"columns":["id","title"],"rows":[[{"type":"integer","value":"1"},{"type":"null"}]],"rows_affected":"0"}]}),
        );
        assert!(out.contains("id | title"));
        assert!(out.contains("1  | NULL"));
    }
    #[test]
    fn restore_arguments_and_global_deadline_are_scoped() {
        cli_command().debug_assert();
        assert!(parse(&["db", "notes", "restore", "file", "backup.sqlite"]).is_ok());
        for tail in [
            vec!["file"],
            vec!["begin"],
            vec!["upload", "id", "chunk"],
            vec!["status"],
            vec!["commit", "id"],
            vec!["abort", "id"],
            vec!["file", "backup.sqlite", "--input", "request.json"],
            vec!["file", "backup.sqlite", "--resume-file", "resume.json"],
            vec!["file", "backup.sqlite", "--budget-ms", "1"],
        ] {
            let mut args = vec!["db", "notes", "restore"];
            args.extend(tail);
            assert!(parse(&args).is_err(), "{args:?}");
        }
        for position in 0..=5 {
            let mut args = vec!["db", "notes", "restore", "file", "backup.sqlite"];
            args.splice(position..position, ["--deadline-ms", "1"]);
            assert!(
                validate_context(&parse(&args).unwrap()).is_err(),
                "{args:?}"
            );
        }
        let mut command = cli_command();
        let help = command
            .find_subcommand_mut("db")
            .unwrap()
            .find_subcommand_mut("restore")
            .unwrap()
            .find_subcommand_mut("file")
            .unwrap()
            .render_help()
            .to_string();
        assert!(!help.contains("--deadline-ms"));
        assert!(!help.contains("--resume-file"));
    }
}
