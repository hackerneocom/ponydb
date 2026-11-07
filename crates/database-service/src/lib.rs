//! Business semantics shared by HTTP, MCP and the native CLI.
use engine_sqlite::{
    worker::WorkerPool, ExecutionLimits, PreparedTransaction, SqlPermissions, Statement,
};
use object_store_api::*;
use protocol::*;
use remote_pager::bridge::AsyncSnapshot;
use request_context::RequestContext;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use storage_format::*;
use versioned_store::IndexWriteTarget;
use versioned_store::{now_ms, unique, DatabaseStore, Error as StoreFailure};
pub mod export;
pub mod maintenance;
pub mod restore;
struct CallGuard(RequestContext);
impl Drop for CallGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Clone)]
pub struct ServiceOptions {
    pub disk_cache: Option<versioned_store::DiskCacheOptions>,
    pub workers: usize,
    pub queue: usize,
    pub execution: ExecutionLimits,
    pub segment_size: usize,
    pub retention_days: u32,
    pub max_history: u64,
    pub export_max_bytes: u64,
    pub max_read_ms: u64,
    pub gc_default_budget_ms: u64,
    pub gc_max_objects: usize,
    pub max_request_ms: u64,
    pub overlay_hard_limit: u64,
}
impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            disk_cache: None,
            workers: 4,
            queue: 64,
            execution: ExecutionLimits {
                max_statements: 100,
                ..Default::default()
            },
            segment_size: 256 * 1024,
            retention_days: 7,
            max_history: 1000000,
            export_max_bytes: 10 * 1024 * 1024 * 1024,
            max_read_ms: 10000,
            gc_default_budget_ms: 60000,
            gc_max_objects: 1000,
            max_request_ms: 1800000,
            overlay_hard_limit: 64 * 1024 * 1024,
        }
    }
}
pub struct Service<S: BlobStore> {
    pub store: Arc<S>,
    pool: WorkerPool,
    pub options: ServiceOptions,
    namespace: String,
    object_reader: Arc<versioned_store::ObjectReader<S>>,
    gc_tasks: tokio_util::task::TaskTracker,
    gc_stop: tokio_util::sync::CancellationToken,
}
struct Empty;
impl AsyncSnapshot for Empty {
    fn len(&self) -> u64 {
        0
    }
    fn read_at(
        &self,
        _: u64,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = remote_pager::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async { Err(remote_pager::Error::Invalid) })
    }
}
pub fn error(code: &str, message: &str) -> ApiError {
    ApiError {
        request_id: observability::request_id().unwrap_or_else(unique),
        code: code.into(),
        message: message.into(),
        retryable: matches!(code, "TX_CONFLICT" | "STORAGE_UNAVAILABLE" | "MAINTENANCE"),
        outcome: if code == "OUTCOME_UNKNOWN" {
            "unknown"
        } else {
            "not_committed"
        }
        .into(),
    }
}
pub fn store_error(e: StoreFailure) -> ApiError {
    match e {
        StoreFailure::UnsupportedLayout => error("UNSUPPORTED_LAYOUT", &format!("Storage layout version {LAYOUT_VERSION} is required")),
        StoreFailure::Conflict => error("TX_CONFLICT", "HEAD changed; retry the whole operation"),
        StoreFailure::Unknown => error(
            "OUTCOME_UNKNOWN",
            "Commit may have succeeded; inspect business data before deciding whether to submit again",
        ),
        StoreFailure::Deleted => error("DATABASE_DELETED", "Database is deleted"),
        StoreFailure::Maintenance => {
            error("MAINTENANCE", "Database is unavailable during maintenance")
        }
        StoreFailure::Limit => error("RESOURCE_LIMIT", "Storage operation exceeded its budget"),
        StoreFailure::Stopped(_) => error("DEADLINE_EXCEEDED", "Request expired or was cancelled"),
        StoreFailure::DatabaseNotFound(database) => {
            error("DATABASE_NOT_FOUND", &format!("Database '{database}' not found"))
        }
        StoreFailure::MissingObject(database) => error(
            "STORAGE_CORRUPT",
            &format!("Database '{database}' storage is incomplete: a referenced object is missing"),
        ),
        StoreFailure::InvalidHead { database, reason } => error(
            "STORAGE_CORRUPT",
            &format!("Database '{database}' HEAD validation failed: {reason}"),
        ),
        StoreFailure::Store(StoreError::NotFound) => {
            let mut result = error("STORAGE_UNAVAILABLE", "Object store resource not found");
            result.retryable = false;
            result
        }
        StoreFailure::Store(StoreError::Remote { operation, status, code, message }) => {
            let mut text = format!("S3 {operation} failed (HTTP {status}");
            if let Some(code) = code.filter(|s| !s.is_empty()) {
                text.push_str(&format!(", {code}"));
            }
            text.push(')');
            if let Some(message) = message.filter(|s| !s.is_empty()) {
                text.push_str(": ");
                text.push_str(&message);
            }
            let mut result = error("STORAGE_UNAVAILABLE", &text);
            result.retryable = matches!(status, 408 | 429 | 500..=599);
            result
        }
        StoreFailure::Corrupt => error("STORAGE_CORRUPT", "Stored data failed validation"),
        _ => error("STORAGE_UNAVAILABLE", "Object store request failed"),
    }
}
fn engine_error(e: engine_sqlite::Error) -> ApiError {
    match e.into_cause() {
        engine_sqlite::Cause::Busy => {
            error("RATE_LIMITED", "Database worker admission limit reached")
        }
        engine_sqlite::Cause::Stopped(_) => {
            error("DEADLINE_EXCEEDED", "SQL request expired or was cancelled")
        }
        engine_sqlite::Cause::Budget => error("RESOURCE_LIMIT", "SQL resource budget exceeded"),
        engine_sqlite::Cause::Sql(err) => sql_error(err),
        engine_sqlite::Cause::Snapshot => error("SQL_ERROR", "Snapshot read failed"),
        engine_sqlite::Cause::InvalidPages => {
            error("SQL_ERROR", "Invalid page size, range, or snapshot length")
        }
        engine_sqlite::Cause::Frozen => error("SQL_ERROR", "Database candidate is frozen"),
        engine_sqlite::Cause::Workspace => error("SQL_ERROR", "Database workspace failed"),
        engine_sqlite::Cause::Configuration => {
            error("SQL_ERROR", "Engine configuration was not accepted")
        }
        engine_sqlite::Cause::Worker => error("SQL_ERROR", "Database worker stopped"),
    }
}
/// Caller-facing SQL diagnostics. The progress handler is the only interrupt source,
/// so SQLITE_INTERRUPT is the in-statement form of cancellation.
fn sql_error(err: rusqlite::Error) -> ApiError {
    if err.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
        return error("DEADLINE_EXCEEDED", "SQL request expired or was cancelled");
    }
    error("SQL_ERROR", &sql_diagnostic(&err))
}
fn sql_diagnostic(err: &rusqlite::Error) -> String {
    let raw = match err {
        rusqlite::Error::InvalidPath(_) => return "Invalid database path".into(),
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        other => other.to_string(),
    };
    let mut text = String::new();
    for ch in raw.chars() {
        if ch.is_control() {
            if !text.ends_with(' ') {
                text.push(' ');
            }
        } else {
            text.push(ch);
        }
    }
    let text = text.trim();
    if text.is_empty() {
        return "SQL failed".into();
    }
    const MAX: usize = 1024;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let mut clipped: String = text.chars().take(MAX).collect();
        clipped.push_str("...");
        clipped
    }
}
fn log_result(logs: &mut observability::Request, result: &Result<Value>) {
    if let Ok(value) = result {
        if let Some(version) = value["data_version"]
            .as_str()
            .and_then(|version| version.parse().ok())
        {
            logs.set_data_version(version);
        }
    }
    match result {
        Ok(value) => logs.finish(
            observability::outcome(value["outcome"].as_str().unwrap_or("")),
            "",
        ),
        Err(e) => logs.finish(observability::error_outcome(&e.code, &e.outcome), &e.code),
    }
}
fn check(ctx: &RequestContext) -> Result<()> {
    ctx.check()
        .map_err(|_| error("DEADLINE_EXCEEDED", "Request expired or was cancelled"))
}
fn require(principal: &Principal, id: &str, scope: &str) -> Result<()> {
    let allowed = principal.allows(id, scope);
    tracing::debug!(target: "ponydb", event = "authorization_completed", allowed, scope);
    if allowed {
        Ok(())
    } else {
        Err(error(
            "FORBIDDEN",
            "Required database action is not authorized",
        ))
    }
}
fn statements(input: &[SqlStatement]) -> Result<Vec<Statement>> {
    input
        .iter()
        .map(|s| {
            Ok(Statement {
                sql: s.sql.clone(),
                parameters: s
                    .params
                    .iter()
                    .map(SqlValue::to_sql)
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| error("INVALID_REQUEST", "Invalid SQL parameter"))?,
            })
        })
        .collect()
}
fn permissions(p: &Principal, id: &str, read_only: bool) -> SqlPermissions {
    SqlPermissions {
        read: p.allows(id, "data:read"),
        write: p.allows(id, "data:write"),
        schema: p.allows(id, "schema:write"),
        read_only,
    }
}
fn typed_results(tx: PreparedTransaction) -> Result<(remote_pager::FrozenPages, Value)> {
    let rows = tx
        .results
        .into_iter()
        .map(|s| {
            let rows = s
                .rows
                .into_iter()
                .map(|r| {
                    r.into_iter()
                        .map(SqlValue::from_sql)
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| error("SQL_ERROR", "Non-finite SQL result"))?;
            Ok(json!({"columns":s.columns,"rows":rows,"rows_affected":s.rows_affected.to_string()}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((tx.pages, Value::Array(rows)))
}
impl<S: BlobStore + 'static> Service<S> {
    pub fn new(store: Arc<S>, namespace: String, options: ServiceOptions) -> Result<Self> {
        store.capabilities().validate().map_err(|_| {
            error(
                "UNSUPPORTED_BACKEND",
                "Strong atomic object operations required",
            )
        })?;
        if options.retention_days > 36500
            || options.export_max_bytes == 0
            || options.max_read_ms == 0
            || options.max_read_ms > 60000
            || options.gc_default_budget_ms == 0
            || options.gc_default_budget_ms > options.max_request_ms
            || options.max_request_ms > 1800000
            || options.gc_max_objects == 0
            || options.gc_max_objects > 1000
            || options.segment_size < 65536
            || options.segment_size > 1024 * 1024
            || !options.segment_size.is_power_of_two()
        {
            return Err(error("INVALID_CONFIG", "Invalid storage or safety limits"));
        }
        Ok(Self {
            gc_tasks: tokio_util::task::TaskTracker::new(),
            gc_stop: tokio_util::sync::CancellationToken::new(),
            object_reader: Arc::new(versioned_store::ObjectReader::with_disk_cache(
                store.clone(),
                options.disk_cache.clone(),
            )),
            store,
            pool: WorkerPool::new(options.workers, options.queue, options.workers)
                .map_err(engine_error)?,
            options,
            namespace,
        })
    }
    pub fn cancel_gc(&self) {
        self.gc_stop.cancel();
    }
    /// Wait for admitted GC operations and independent ownership cleanup.
    pub async fn wait_for_gc(&self) {
        self.gc_tasks.close();
        self.gc_tasks.wait().await;
    }
    fn db(&self, name: &str) -> Result<DatabaseStore<S>> {
        storage_namespace::DatabaseName::new(name).map_err(|_| error("INVALID_REQUEST",
            "Database name must contain 1–64 BMP characters, no NUL, /, backslash or dot, and no trailing space"))?;
        DatabaseStore::with_reader(self.object_reader.clone(), name)
            .map(DatabaseStore::with_read_lease)
            .map_err(store_error)
    }
    fn token(&self, head: &Head) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"namespace":digest(self.namespace.as_bytes()),"database":head.name,"incarnation_id":head.incarnation_id,"data_version":head.data_version.to_string()})).expect("json"))
    }
    fn metadata(&self, view: &versioned_store::HeadView, manifest: &Manifest) -> Value {
        let head = &view.head;
        json!({"name":head.name,"description":head.description,"metadata":head.metadata,"status":head.status,"engine":"sqlite","sql_dialect":"sqlite","revision":head.revision.to_string(),"metadata_version":head.metadata_version.to_string(),"data_version":head.data_version.to_string(),"logical_size_bytes":manifest.logical_size.to_string(),"page_size":manifest.page_size,"snapshot_retention_days":head.snapshot_retention_days,"etag":view.etag,"commit_token":self.token(head),"created_at_ms":head.created_at_ms,"updated_at_ms":head.updated_at_ms})
    }
    fn active(&self, head: &Head) -> Result<()> {
        if head.status != "active" {
            return Err(error("DATABASE_DELETED", "Database is not active"));
        }
        Ok(())
    }
    async fn record_snapshot(
        &self,
        db: &DatabaseStore<S>,
        head: &mut Head,
        ctx: &RequestContext,
    ) -> Result<()> {
        if head.snapshot_count >= self.options.max_history {
            return Err(error(
                "RESOURCE_LIMIT",
                "Retained snapshot capacity reached",
            ));
        }
        let id = format!("{}:{}", head.incarnation_id, head.data_version);
        let history = History {
            snapshot_id: id.clone(),
            incarnation_id: head.incarnation_id.clone(),
            data_version: head.data_version,
            manifest: head.manifest_ref.clone(),
            retired_at_upper_ms: None,
            observed_at_ms: now_ms(),
        };
        head.snapshot_index_root = Some(
            db.index_insert(
                head.snapshot_index_root.clone(),
                &digest(id.as_bytes()),
                serde_json::to_value(history).unwrap(),
                IndexWriteTarget {
                    incarnation: &head.incarnation_id,
                    epoch: head.epoch,
                    category: "snapshots",
                },
                ctx,
            )
            .await
            .map_err(store_error)?,
        );
        head.snapshot_count += 1;
        Ok(())
    }
    pub async fn call(
        &self,
        principal: &Principal,
        operation: Operation,
        ctx: RequestContext,
    ) -> Result<Value> {
        let database = match &operation {
            Operation::DatabaseCreate(i) => Some(i.name.as_str()),
            Operation::DatabaseList(_) => None,
            Operation::DatabaseGet(i) => Some(i.database.as_str()),
            Operation::DatabaseUpdate(i) => Some(i.database.as_str()),
            Operation::DatabaseDelete(i) => Some(i.database.as_str()),
            Operation::SqlExecute(i) => Some(i.database.as_str()),
            Operation::SchemaGet(i) => Some(i.database.as_str()),
            Operation::DatabaseCompact(i) => Some(i.database.as_str()),
            Operation::DatabaseGc(i) => Some(i.database.as_str()),
            Operation::SnapshotList(i) => Some(i.database.as_str()),
            Operation::SnapshotGet(i) => Some(i.database.as_str()),
        };
        let mut logs = observability::Request::new(operation.name(), database);
        let result = logs.scope(async {
            tracing::debug!(target: "ponydb", event = "operation_dispatched", operation = operation.name());
            self.call_inner(principal, operation, ctx).await
        }).await;
        log_result(&mut logs, &result);
        result
    }
    async fn call_inner(
        &self,
        principal: &Principal,
        operation: Operation,
        ctx: RequestContext,
    ) -> Result<Value> {
        let _guard = CallGuard(ctx.clone());
        check(&ctx)?;
        match operation {
            Operation::DatabaseCreate(i) => self.create(principal, i, &ctx).await,
            Operation::DatabaseList(i) => self.list(principal, i, &ctx).await,
            Operation::DatabaseGet(i) => {
                require(principal, &i.database, "db:read")?;
                let db = self.db(&i.database)?;
                let h = db.read_head().await.map_err(store_error)?;
                let m = db.manifest(&h.head).await.map_err(store_error)?;
                Ok(self.metadata(&h, &m))
            }
            Operation::DatabaseUpdate(i) => self.update(principal, i, &ctx).await,
            Operation::DatabaseDelete(i) => self.delete(principal, i, &ctx).await,
            Operation::SqlExecute(i) => self.execute(principal, i, &ctx).await,
            Operation::SchemaGet(i) => {
                require(principal, &i.database, "schema:read")?;
                if i.table.as_deref() == Some("") {
                    return Err(error("INVALID_REQUEST", "table must not be empty"));
                }
                let mut sql = "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'".to_owned();
                let mut params = Vec::new();
                if let Some(table) = &i.table {
                    sql.push_str(" AND tbl_name = ?");
                    params.push(SqlValue::Text(table.clone()));
                    sql =
                        format!("WITH target AS ({sql}) SELECT type,name,tbl_name,sql FROM target WHERE EXISTS (SELECT 1 FROM target WHERE type = 'table')");
                }
                if let Some(kind) = i.r#type {
                    sql.push_str(" AND type = ?");
                    params.push(SqlValue::Text(kind.as_str().into()));
                }
                if i.table.is_some() {
                    // A NULL sentinel signals a missing table in the same snapshot as the
                    // definitions. Existing tables add no rows or bytes to the result budget.
                    sql.push_str(" UNION ALL SELECT NULL,NULL,NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM target WHERE type = 'table')");
                }
                sql.push_str(" ORDER BY type,name");
                let result = self
                    .execute(
                        principal,
                        Execute {
                            database: i.database.clone(),
                            sql: Some(sql),
                            params: Some(params),
                            statements: None,
                            consistency: "strong".into(),
                            snapshot_id: None,
                            commit_token: None,
                            max_rows: None,
                        },
                        &ctx,
                    )
                    .await?;
                if let Some(table) = &i.table {
                    // NULL sorts before all object types and cannot occur in sqlite_schema.type.
                    if result["results"][0]["rows"][0][0] == json!({"type":"null"}) {
                        return Err(error(
                            "TABLE_NOT_FOUND",
                            &format!("Table '{table}' not found in database '{}'", i.database),
                        ));
                    }
                }
                Ok(result)
            }
            Operation::DatabaseCompact(i) => self.compact(principal, i, &ctx).await,
            Operation::DatabaseGc(i) => self.gc(principal, i, &ctx).await,
            Operation::SnapshotList(i) => self.snapshots(principal, i, &ctx).await,
            Operation::SnapshotGet(i) => {
                require(principal, &i.database, "db:backup")?;
                let db = self.db(&i.database)?;
                let h = db.read_head().await.map_err(store_error)?.head;
                Self::history_value(self.history(&db, &h, &i.snapshot_id).await?)
            }
        }
    }
    async fn creation_base(
        &self,
        db: &DatabaseStore<S>,
    ) -> Result<Option<versioned_store::HeadView>> {
        match db.read_head().await {
            Ok(h) => {
                if h.head.status != "deleted" {
                    return Err(error("DATABASE_EXISTS", "Database already exists"));
                }
                Ok(Some(h))
            }
            Err(StoreFailure::DatabaseNotFound(_)) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }
    fn history_value(h: History) -> Result<Value> {
        let mut value =
            serde_json::to_value(h).map_err(|_| error("STORAGE_CORRUPT", "Invalid history"))?;
        value.as_object_mut().unwrap().remove("incarnation_id");
        Ok(value)
    }
    async fn create(&self, p: &Principal, i: Create, ctx: &RequestContext) -> Result<Value> {
        let name = i.name.clone();
        require(p, &name, "db:create")?;
        i.validate().map_err(|e| error("INVALID_REQUEST", &e))?;
        let db = self.db(&name)?;
        let old = self.creation_base(&db).await?;
        observability::phase("executing_sql");
        let tx = self
            .pool
            .prepare(
                name,
                p.id.clone(),
                Arc::new(Empty),
                4096,
                vec![
                    Statement {
                        sql: "CREATE TABLE _ponydb_init(x)".into(),
                        parameters: vec![],
                    },
                    Statement {
                        sql: "DROP TABLE _ponydb_init".into(),
                        parameters: vec![],
                    },
                ],
                self.options.execution.clone(),
                ctx.clone(),
            )
            .await
            .map_err(engine_error)?;
        let epoch = old.as_ref().map_or(Ok(1), |h| {
            h.head
                .epoch
                .checked_add(1)
                .ok_or_else(|| error("RESOURCE_LIMIT", "Epoch exhausted"))
        })?;
        let inc = unique();
        let manifest = Manifest {
            version: 1,
            page_size: 4096,
            logical_size: 0,
            segment_size: self.options.segment_size,
            generation_id: unique(),
            base_root: None,
            delta_root: None,
            overlay_bytes: 0,
            overlay_transactions: 0,
        };
        let manifest_ref = db
            .apply_pages(&inc, epoch, manifest, tx.pages, ctx)
            .await
            .map_err(store_error)?;
        let mut head = Head {
            protocol_version: 1,
            storage_layout_version: LAYOUT_VERSION,
            incarnation_id: inc,
            revision: 1,
            data_version: 1,
            metadata_version: 1,
            epoch,
            head_nonce: unique(),
            name: i.name,
            description: i.description,
            metadata: i.metadata,
            status: "active".into(),
            manifest_ref,
            snapshot_index_root: None,
            snapshot_retention_days: self.options.retention_days,
            snapshot_count: 0,
            safety_policy: SafetyPolicy {
                max_read_ms: self.options.max_read_ms,
                // Readers of a deleted lifecycle may still hold its longer lease.
                drain_ms: old.as_ref().map_or(self.options.max_read_ms + 1000, |h| {
                    h.head
                        .safety_policy
                        .drain_ms
                        .max(self.options.max_read_ms + 1000)
                }),
            },
            gc: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        self.record_snapshot(&db, &mut head, ctx).await?;
        let response =
            json!({"committed":true,"outcome":"committed","name":head.name,"data_version":"1"});
        db.publish_head(old.as_ref(), head, ctx)
            .await
            .map_err(store_error)?;
        Ok(response)
    }
    async fn execute(&self, p: &Principal, i: Execute, ctx: &RequestContext) -> Result<Value> {
        i.validate()
            .map_err(|message| error("INVALID_REQUEST", &message))?;
        if !["data:read", "data:write", "schema:write"]
            .iter()
            .any(|scope| p.allows(&i.database, scope))
        {
            return Err(error("FORBIDDEN", "SQL action required"));
        }
        let db = self.db(&i.database)?;
        let base = db.read_head().await.map_err(store_error)?;
        let head = &base.head;
        let write_options = i.consistency == "strong"
            && i.snapshot_id.is_none()
            && i.commit_token.is_none()
            && i.max_rows.is_none();
        let mut selected_version = head.data_version;
        let manifest = match i.consistency.as_str() {
            "strong" => {
                self.active(head)?;
                db.manifest(head).await.map_err(store_error)?
            }
            "snapshot" => {
                require(p, &i.database, "db:backup")?;
                let h = self
                    .history(
                        &db,
                        head,
                        i.snapshot_id
                            .as_deref()
                            .ok_or_else(|| error("INVALID_REQUEST", "snapshot_id required"))?,
                    )
                    .await?;
                selected_version = h.data_version;
                db.get(&h.manifest).await.map_err(store_error)?
            }
            "at_least" => {
                self.active(head)?;
                use base64::Engine;
                let token = i
                    .commit_token
                    .clone()
                    .ok_or_else(|| error("INVALID_REQUEST", "commit_token required"))?;
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(token)
                    .map_err(|_| error("INVALID_REQUEST", "Bad commit token"))?;
                let v: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| error("INVALID_REQUEST", "Bad commit token"))?;
                if v["namespace"] != digest(self.namespace.as_bytes())
                    || v["database"] != i.database
                    || v["incarnation_id"] != head.incarnation_id
                    || v["data_version"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .is_none_or(|n| n > head.data_version)
                {
                    return Err(error(
                        "INVALID_REQUEST",
                        "Commit token is incompatible or ahead of HEAD",
                    ));
                }
                db.manifest(head).await.map_err(store_error)?
            }
            _ => return Err(error("INVALID_REQUEST", "Unsupported consistency")),
        };
        if !manifest.validate() {
            return Err(error("STORAGE_CORRUPT", "Invalid manifest"));
        }

        observability::phase("preparing_sql");
        let sql_ctx = ctx.with_max_duration(Duration::from_millis(head.safety_policy.max_read_ms));
        let batch = i.statements.is_some();
        let inputs = match &i.statements {
            Some(batch) => statements(batch)?,
            None => statements(&[SqlStatement {
                sql: i
                    .sql
                    .clone()
                    .ok_or_else(|| error("INVALID_REQUEST", "SQL required"))?,
                params: i.params.clone().unwrap_or_default(),
            }])?,
        };
        // Batches retain transaction write semantics. Inspecting all statements before
        // execution would reject dependencies such as CREATE TABLE followed by INSERT.
        let read_only = if batch {
            false
        } else {
            self.pool
                .inspect(
                    i.database.clone(),
                    p.id.clone(),
                    db.snapshot(manifest.clone(), sql_ctx.clone()),
                    manifest.page_size,
                    inputs.clone(),
                    self.options.execution.clone(),
                    sql_ctx.clone(),
                )
                .await
                .map_err(engine_error)?
                .read_only
        };
        tracing::debug!(target: "ponydb", event = "sql_classified", read_only, batch);
        let mut limits = self.options.execution.clone();
        limits.permissions = permissions(p, &i.database, read_only);
        if read_only {
            require(p, &i.database, "data:read")?;
            if let Some(max) = i.max_rows {
                if max == 0 || max > limits.max_result_rows {
                    return Err(error("RESOURCE_LIMIT", "Row limit exceeds server policy"));
                }
                limits.max_result_rows = max;
            }
        } else {
            if !p.allows(&i.database, "data:write") && !p.allows(&i.database, "schema:write") {
                return Err(error("FORBIDDEN", "Write action required"));
            }
            if !write_options {
                return Err(error(
                    "INVALID_REQUEST",
                    "Read options are not valid for writes",
                ));
            }
            if manifest.overlay_bytes >= self.options.overlay_hard_limit {
                return Err(error(
                    "MAINTENANCE_REQUIRED",
                    "Compact current overlay before further writes",
                ));
            }
        }
        observability::phase("executing_sql");
        let tx = self
            .pool
            .prepare(
                i.database.clone(),
                p.id.clone(),
                db.snapshot(manifest.clone(), sql_ctx.clone()),
                manifest.page_size,
                inputs,
                limits,
                sql_ctx,
            )
            .await
            .map_err(engine_error)?;
        if read_only {
            let (_, results) = typed_results(tx)?;
            return Ok(json!({"outcome":"read_only", "database":i.database,
                "data_version":selected_version.to_string(),
                "results":results, "truncated":false}));
        }
        let (pages, results) = typed_results(tx)?;
        let mut next = base.head.clone();
        observability::phase("building_data");
        next.manifest_ref = db
            .apply_pages(&next.incarnation_id, next.epoch, manifest, pages, ctx)
            .await
            .map_err(store_error)?;
        next.data_version = next
            .data_version
            .checked_add(1)
            .ok_or_else(|| error("RESOURCE_LIMIT", "Version exhausted"))?;
        self.record_snapshot(&db, &mut next, ctx).await?;
        let response = json!({"database":i.database,"committed":true,"outcome":"committed","data_version":next.data_version.to_string(),"commit_token":self.token(&next),"results":results});
        db.publish_head(Some(&base), next, ctx)
            .await
            .map_err(store_error)?;
        Ok(response)
    }
    async fn update(&self, p: &Principal, i: Update, ctx: &RequestContext) -> Result<Value> {
        require(p, &i.database, "db:manage")?;
        i.validate().map_err(|e| error("INVALID_REQUEST", &e))?;
        if i.snapshot_retention_days.is_some() {
            require(p, &i.database, "db:backup")?;
        }
        let db = self.db(&i.database)?;
        let base = db.read_head().await.map_err(store_error)?;
        self.active(&base.head)?;
        if i.if_match
            .as_ref()
            .is_some_and(|expected| *expected != base.etag)
        {
            return Err(error(
                "METADATA_PRECONDITION_FAILED",
                "Management ETag does not match",
            ));
        }
        let mut next = base.head.clone();
        if let Some(v) = i.description {
            next.description = v;
        }
        if let Some(v) = i.metadata {
            next.metadata = v;
        }
        if let Some(v) = i.snapshot_retention_days {
            if next.gc.is_some() {
                return Err(error("MAINTENANCE", "GC prevents retention policy changes"));
            }
            next.snapshot_retention_days = v;
        }
        next.metadata_version += 1;
        if !next.validate(&i.database) {
            return Err(error("INVALID_REQUEST", "Invalid metadata"));
        }
        let response = json!({"outcome":"committed","committed":true,"database":i.database,"metadata_version":next.metadata_version.to_string()});
        db.publish_head(Some(&base), next, ctx)
            .await
            .map_err(store_error)?;
        Ok(response)
    }
    async fn delete(&self, p: &Principal, i: Delete, ctx: &RequestContext) -> Result<Value> {
        require(p, &i.database, "db:manage")?;
        let db = self.db(&i.database)?;
        let base = db.read_head().await.map_err(store_error)?;
        self.active(&base.head)?;
        if i.if_match
            .as_ref()
            .is_some_and(|expected| *expected != base.etag)
        {
            return Err(error(
                "METADATA_PRECONDITION_FAILED",
                "Management ETag does not match",
            ));
        }
        let mut next = base.head.clone();
        next.status = "deleted".into();
        next.epoch = next
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("RESOURCE_LIMIT", "Epoch exhausted"))?;
        next.gc = None;
        db.publish_head(Some(&base), next, ctx)
            .await
            .map_err(store_error)?;
        Ok(json!({"outcome":"committed","committed":true,"database":i.database,"status":"deleted"}))
    }
    async fn list(&self, p: &Principal, i: List, ctx: &RequestContext) -> Result<Value> {
        if !p.scopes.iter().any(|s| s == "db:list" || s == "*") {
            return Err(error("FORBIDDEN", "Database list scope required"));
        }
        if i.limit == 0 || i.limit > 1000 {
            return Err(error("INVALID_REQUEST", "Invalid page size"));
        }
        let binding =
            digest(&serde_json::to_vec(&(&self.namespace, p, i.include_deleted)).unwrap());
        let mut cursor = match i.cursor {
            None => None,
            Some(c) => {
                let v: Value =
                    serde_json::from_str(&c).map_err(|_| error("INVALID_REQUEST", "Bad cursor"))?;
                if v["binding"] != binding {
                    return Err(error("INVALID_REQUEST", "Cursor scope mismatch"));
                }
                v["storage"].as_str().map(str::to_owned)
            }
        };
        let mut items = vec![];
        let namespace = storage_namespace::StorageNamespace::default();
        // One storage candidate at a time prevents losing an unconsumed filtered page.
        for _ in 0..1000 {
            check(ctx)?;
            let page = self
                .store
                .list(ListRequest {
                    prefix: namespace.databases_prefix(),
                    delimiter: Some("/".into()),
                    cursor,
                    limit: 1,
                })
                .await
                .map_err(|e| store_error(StoreFailure::Store(e)))?;
            for prefix in page.common_prefixes {
                let name = namespace
                    .database_name(&prefix)
                    .map_err(|_| error("STORAGE_CORRUPT", "Invalid database prefix"))?;
                if !p.allows(name.as_str(), "db:read") {
                    continue;
                }
                let db = self.db(name.as_str())?;
                match db.read_head().await {
                    Ok(view) => {
                        if i.include_deleted || view.head.status != "deleted" {
                            items.push(self.metadata(
                                &view,
                                &db.manifest(&view.head).await.map_err(store_error)?,
                            ));
                        }
                    }
                    Err(StoreFailure::DatabaseNotFound(_)) => {}
                    Err(e) => return Err(store_error(e)),
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() || items.len() >= i.limit {
                break;
            }
        }
        Ok(
            json!({"outcome":"read_only","databases":items,"next_cursor":cursor.map(|c|json!({"binding":binding,"storage":c}).to_string())}),
        )
    }
    async fn history(&self, db: &DatabaseStore<S>, head: &Head, id: &str) -> Result<History> {
        let v = db
            .index_get(head.snapshot_index_root.clone(), &digest(id.as_bytes()))
            .await
            .map_err(store_error)?
            .ok_or_else(|| error("SNAPSHOT_EXPIRED", "Snapshot is unavailable"))?;
        let h: History = serde_json::from_value(v)
            .map_err(|_| error("STORAGE_CORRUPT", "Invalid history entry"))?;
        if h.snapshot_id != id {
            return Err(error("STORAGE_CORRUPT", "Snapshot identity mismatch"));
        }
        Ok(h)
    }
    async fn snapshots(
        &self,
        p: &Principal,
        i: SnapshotList,
        ctx: &RequestContext,
    ) -> Result<Value> {
        require(p, &i.database, "db:backup")?;
        check(ctx)?;
        let db = self.db(&i.database)?;
        let head = db.read_head().await.map_err(store_error)?.head;
        let entries = db
            .index_list(
                head.snapshot_index_root.clone(),
                i.cursor.as_deref(),
                i.limit,
            )
            .await
            .map_err(store_error)?;
        let next = entries.last().map(|(k, _)| k.clone());
        let mut items = vec![];
        for (_, v) in entries {
            let h: History = serde_json::from_value(v)
                .map_err(|_| error("STORAGE_CORRUPT", "Invalid snapshot"))?;
            items.push(Self::history_value(h)?);
        }
        Ok(
            json!({"outcome":"read_only","snapshots":items,"next_cursor":next,"retention_days":head.snapshot_retention_days,"time_authority":"system_clock","history_gaps":[]}),
        )
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    fn sqlite(code: i32, message: &str) -> engine_sqlite::Error {
        engine_sqlite::Error::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            Some(message.into()),
        ))
    }

    #[test]
    fn sql_diagnostic_keeps_the_sqlite_reason() {
        let err = engine_error(sqlite(
            rusqlite::ffi::SQLITE_ERROR,
            "no such table: ordersr",
        ));
        assert_eq!(err.code, "SQL_ERROR");
        assert_eq!(err.message, "no such table: ordersr");
        assert!(!err.retryable);
        assert_eq!(err.outcome, "not_committed");
    }

    #[test]
    fn sql_interrupt_is_a_deadline() {
        let err = engine_error(sqlite(rusqlite::ffi::SQLITE_INTERRUPT, "interrupted"));
        assert_eq!(err.code, "DEADLINE_EXCEEDED");
        assert_eq!(err.message, "SQL request expired or was cancelled");
    }

    #[test]
    fn sql_diagnostic_drops_controls_paths_and_limits_length() {
        let messy = engine_error(sqlite(
            rusqlite::ffi::SQLITE_ERROR,
            "no such table: a\nb\u{0000}c",
        ));
        assert_eq!(messy.message, "no such table: a b c");
        let long = "x".repeat(1100);
        let clipped = sql_diagnostic(&rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(long),
        ));
        assert_eq!(clipped.chars().count(), 1027);
        assert!(clipped.ends_with("..."));
        let path = engine_error(engine_sqlite::Error::Sql(rusqlite::Error::InvalidPath(
            "/tmp/ponydb-secret/main.db".into(),
        )));
        assert_eq!(path.message, "Invalid database path");
        let snapshot = engine_error(engine_sqlite::Error::Pager(remote_pager::Error::Io(
            "transport error: https://secret.example/key".into(),
        )));
        assert_eq!(snapshot.code, "SQL_ERROR");
        assert_eq!(snapshot.message, "Snapshot read failed");
        assert!(!snapshot.message.contains("secret"));
    }
}

#[cfg(test)]
mod storage_diagnostic_tests {
    use super::*;
    #[test]
    fn remote_errors_format_optional_metadata_and_retry_flags() {
        for status in [301, 400, 403, 408, 429, 500, 503] {
            let e = store_error(StoreFailure::Store(StoreError::Remote {
                operation: "ListObjectsV2",
                status,
                code: Some("SecondLevelDomainForbidden".into()),
                message: Some("Please use virtual hosted style to access.".into()),
            }));
            assert_eq!(e.code, "STORAGE_UNAVAILABLE");
            assert_eq!(e.outcome, "not_committed");
            assert_eq!(e.retryable, matches!(status, 408 | 429 | 500..=599));
            assert_eq!(e.message, format!("S3 ListObjectsV2 failed (HTTP {status}, SecondLevelDomainForbidden): Please use virtual hosted style to access."));
        }
        for field in [None, Some(String::new())] {
            let e = store_error(StoreFailure::Store(StoreError::Remote {
                operation: "GetObject",
                status: 500,
                code: field.clone(),
                message: field,
            }));
            assert_eq!(e.message, "S3 GetObject failed (HTTP 500)");
        }
        let e = store_error(StoreFailure::Store(StoreError::Transport(
            "secret URL".into(),
        )));
        assert_eq!(e.message, "Object store request failed");
        assert_eq!(
            store_error(StoreFailure::Store(StoreError::NotFound)).code,
            "STORAGE_UNAVAILABLE"
        );
        assert!(!store_error(StoreFailure::Store(StoreError::NotFound)).retryable);
        assert_eq!(store_error(StoreFailure::Unknown).outcome, "unknown");
    }
}
