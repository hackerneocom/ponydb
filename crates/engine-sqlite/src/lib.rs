//! Local P0 transaction preparation, not a remotely committed database service.
use remote_pager::{CowFile, FrozenPages, Snapshot};
use request_context::RequestContext;
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    params_from_iter,
    types::Value,
};
use sqlite_vfs::Workspace;
use std::sync::Arc;
use thiserror::Error;
pub mod worker;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Vfs(#[from] sqlite_vfs::Error),
    #[error(transparent)]
    Pager(#[from] remote_pager::Error),
    #[error("engine configuration was not accepted")]
    Configuration,
    #[error("result budget exceeded")]
    ResultBudget,
    #[error(transparent)]
    Stopped(#[from] request_context::Stop),
    #[error("database, subject or worker queue saturated")]
    Busy,
    #[error("worker stopped or panicked")]
    Worker,
}
/// Engine failures grouped for the service boundary.
/// SQL text stays available; snapshot and workspace failures do not carry storage details.
#[derive(Debug)]
pub enum Cause {
    Busy,
    Stopped(request_context::Stop),
    Budget,
    Sql(rusqlite::Error),
    Snapshot,
    InvalidPages,
    Frozen,
    Workspace,
    Configuration,
    Worker,
}
impl Error {
    pub fn into_cause(self) -> Cause {
        match self {
            Error::Busy => Cause::Busy,
            Error::Stopped(stop) => Cause::Stopped(stop),
            Error::ResultBudget => Cause::Budget,
            Error::Sql(err) => Cause::Sql(err),
            Error::Configuration => Cause::Configuration,
            Error::Worker => Cause::Worker,
            Error::Pager(err) => pager_cause(err),
            Error::Vfs(err) => match err {
                sqlite_vfs::Error::Sql(err) => Cause::Sql(err),
                sqlite_vfs::Error::Pager(err) => pager_cause(err),
                sqlite_vfs::Error::Io(_) | sqlite_vfs::Error::Vfs => Cause::Workspace,
            },
        }
    }
}
fn pager_cause(err: remote_pager::Error) -> Cause {
    match err {
        remote_pager::Error::Budget => Cause::Budget,
        remote_pager::Error::Io(_) => Cause::Snapshot,
        remote_pager::Error::Invalid => Cause::InvalidPages,
        remote_pager::Error::Frozen => Cause::Frozen,
    }
}
#[derive(Debug, Clone)]
pub struct Statement {
    pub sql: String,
    pub parameters: Vec<Value>,
}
#[derive(Debug)]
pub struct PreparedTransaction {
    pub read_only: bool,
    pub pages: FrozenPages,
    pub results: Vec<StatementResult>,
}
#[derive(Debug)]
pub struct StatementResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub rows_affected: u64,
}
impl std::ops::Index<usize> for StatementResult {
    type Output = Vec<Value>;
    fn index(&self, n: usize) -> &Self::Output {
        &self.rows[n]
    }
}
#[derive(Clone, Debug)]
pub struct SqlPermissions {
    pub read: bool,
    pub write: bool,
    pub schema: bool,
    pub read_only: bool,
}
impl Default for SqlPermissions {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            schema: true,
            read_only: false,
        }
    }
}
#[derive(Debug)]
pub struct EngineDiagnostics {
    pub version: String,
    pub source_id: String,
    pub compile_options: Vec<String>,
}
pub fn diagnostics() -> Result<EngineDiagnostics, Error> {
    let conn = rusqlite::Connection::open_in_memory()?;
    let mut stmt = conn.prepare("PRAGMA compile_options")?;
    let compile_options = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(EngineDiagnostics {
        version: rusqlite::version().into(),
        source_id: conn.query_row("SELECT sqlite_source_id()", [], |r| r.get(0))?,
        compile_options,
    })
}
/// Run on a dedicated blocking worker. The caller owns scheduling and deadlines.
/// Consumes one immutable snapshot and returns an unpublished candidate only.
pub fn prepare_transaction(
    base: Arc<dyn Snapshot>,
    page_size: usize,
    max_dirty_bytes: usize,
    statements: &[Statement],
) -> Result<PreparedTransaction, Error> {
    let limits = ExecutionLimits {
        max_dirty_bytes,
        ..ExecutionLimits::default()
    };
    prepare_with_limits(
        base,
        page_size,
        statements,
        &limits,
        RequestContext::new(std::time::Duration::from_secs(30)),
    )
}
#[derive(Clone, Debug)]
pub struct ExecutionLimits {
    pub max_dirty_bytes: usize,
    pub max_local_bytes: u64,
    pub max_result_bytes: usize,
    pub max_result_rows: usize,
    pub max_sql_bytes: usize,
    pub max_statements: usize,
    pub cache_bytes: usize,
    pub permissions: SqlPermissions,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_dirty_bytes: 16 * 1024 * 1024,
            max_local_bytes: 64 * 1024 * 1024,
            max_result_bytes: 1024 * 1024,
            max_result_rows: 10000,
            max_sql_bytes: 1024 * 1024,
            max_statements: 1000,
            cache_bytes: 1024 * 1024,
            permissions: SqlPermissions::default(),
        }
    }
}
pub fn prepare_with_limits(
    base: Arc<dyn Snapshot>,
    page_size: usize,
    statements: &[Statement],
    limits: &ExecutionLimits,
    ctx: RequestContext,
) -> Result<PreparedTransaction, Error> {
    prepare_mode(base, page_size, statements, limits, ctx, false)
}
fn prepare_mode(
    base: Arc<dyn Snapshot>,
    page_size: usize,
    statements: &[Statement],
    limits: &ExecutionLimits,
    ctx: RequestContext,
    inspect_only: bool,
) -> Result<PreparedTransaction, Error> {
    ctx.check()?;
    if statements.len() > limits.max_statements
        || statements
            .iter()
            .try_fold(0usize, |n, s| n.checked_add(s.sql.len()))
            .is_none_or(|n| n > limits.max_sql_bytes)
    {
        return Err(Error::ResultBudget);
    }
    let mut ws = Workspace::open_with_limits(
        CowFile::new(base, page_size, limits.max_dirty_bytes)?,
        sqlite_vfs::WorkspaceLimits {
            max_local_bytes: limits.max_local_bytes,
            context: ctx.clone(),
            ..Default::default()
        },
    )?;
    let c = ws.connection_mut();
    let progress_ctx = ctx.clone();
    c.progress_handler(1000, Some(move || progress_ctx.check().is_err()))?;
    c.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        limits.max_dirty_bytes.min(i32::MAX as usize) as i32,
    )?;
    c.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
        limits.max_sql_bytes.min(i32::MAX as usize) as i32,
    )?;
    c.execute_batch(&format!("PRAGMA page_size={page_size}; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA mmap_size=0; PRAGMA temp_store=FILE; PRAGMA threads=0; PRAGMA cache_size=64;"))?;
    let journal: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let synchronous: i64 = c.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    let foreign_keys: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    let actual_page_size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    if journal != "delete"
        || synchronous != 2
        || foreign_keys != 1
        || usize::try_from(actual_page_size).ok() != Some(page_size)
    {
        return Err(Error::Configuration);
    }
    c.execute_batch("BEGIN IMMEDIATE")?;
    let permissions = limits.permissions.clone();
    if permissions.read_only {
        c.execute_batch("PRAGMA query_only=ON")?;
    }
    c.authorizer(Some(move |ctx: AuthContext<'_>| {
        let allowed = match ctx.action {
            AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::Pragma { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::CreateVtable { .. }
            | AuthAction::DropVtable { .. }
            | AuthAction::Unknown { .. } => false,
            AuthAction::Function { function_name } => {
                !function_name.eq_ignore_ascii_case("load_extension")
            }
            AuthAction::Insert { table_name }
            | AuthAction::Delete { table_name }
            | AuthAction::Update { table_name, .. } => {
                !permissions.read_only
                    && if matches!(
                        table_name,
                        "sqlite_master"
                            | "sqlite_schema"
                            | "sqlite_temp_master"
                            | "sqlite_temp_schema"
                    ) {
                        permissions.schema
                    } else {
                        permissions.write
                    }
            }
            AuthAction::Read { table_name, .. } => {
                permissions.read
                    || matches!(
                        table_name,
                        "sqlite_master"
                            | "sqlite_schema"
                            | "sqlite_temp_master"
                            | "sqlite_temp_schema"
                    )
            }
            AuthAction::CreateIndex { .. }
            | AuthAction::CreateTable { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTrigger { .. }
            | AuthAction::DropView { .. }
            | AuthAction::AlterTable { .. }
            | AuthAction::Reindex { .. }
            | AuthAction::Analyze { .. } => permissions.schema && !permissions.read_only,
            AuthAction::Select | AuthAction::Recursive => true,
            _ => false,
        };
        if allowed {
            Authorization::Allow
        } else {
            Authorization::Deny
        }
    }))?;
    let mut results = Vec::new();
    let mut read_only = true;
    let mut result_bytes = 0usize;
    let mut result_rows = 0usize;
    for input in statements {
        ctx.check()?;
        // execute/query check for a trailing statement via rusqlite's extra_check feature.
        let before = c.total_changes();
        let mut stmt = c.prepare(&input.sql)?;
        if limits.permissions.read_only && !stmt.readonly() {
            return Err(Error::Configuration);
        }
        read_only &= stmt.readonly();
        if inspect_only {
            continue;
        }
        let names = stmt.column_names().map(str::to_owned).collect();
        let columns = stmt.column_count();
        let mut rows = stmt.query(params_from_iter(input.parameters.iter()))?;
        let mut output = Vec::new();
        while let Some(row) = rows.next()? {
            ctx.check()?;
            result_rows += 1;
            if result_rows > limits.max_result_rows {
                return Err(Error::ResultBudget);
            }
            let mut values = Vec::new();
            for i in 0..columns {
                let v: Value = row.get(i)?;
                result_bytes += match &v {
                    Value::Text(s) => s.len(),
                    Value::Blob(b) => b.len(),
                    _ => 8,
                };
                if result_bytes > limits.max_result_bytes {
                    return Err(Error::ResultBudget);
                }
                values.push(v);
            }
            output.push(values);
        }
        drop(rows);
        drop(stmt);
        results.push(StatementResult {
            columns: names,
            rows: output,
            rows_affected: c.total_changes() - before,
        });
    }
    c.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    ctx.check()?;
    c.execute_batch(if inspect_only { "ROLLBACK" } else { "COMMIT" })?;
    ctx.check()?;
    if !c.is_autocommit() {
        return Err(Error::Configuration);
    }
    Ok(PreparedTransaction {
        read_only,
        pages: ws.freeze()?,
        results,
    })
}

#[cfg(test)]
mod cause_tests {
    use super::*;

    #[test]
    fn into_cause_keeps_sql_and_drops_storage_text() {
        let sql = Error::Vfs(sqlite_vfs::Error::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("no such table: ordersr".into()),
        )));
        match sql.into_cause() {
            Cause::Sql(rusqlite::Error::SqliteFailure(_, Some(msg))) => {
                assert_eq!(msg, "no such table: ordersr")
            }
            other => panic!("expected sql, got {other:?}"),
        }
        let budget = Error::Vfs(sqlite_vfs::Error::Pager(remote_pager::Error::Budget));
        assert!(matches!(budget.into_cause(), Cause::Budget));
        let snapshot = Error::Pager(remote_pager::Error::Io(
            "transport error: https://secret.example/key".into(),
        ));
        assert!(matches!(snapshot.into_cause(), Cause::Snapshot));
        let workspace = Error::Vfs(sqlite_vfs::Error::Io(std::io::Error::other(
            "/tmp/ponydb-secret",
        )));
        assert!(matches!(workspace.into_cause(), Cause::Workspace));
    }
}
