//! Best-effort persistent storage. Only verified immutable bodies belong here.
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

#[derive(Clone, Debug)]
pub struct DiskCacheOptions {
    pub directory: PathBuf,
    /// Sum of object body lengths, excluding SQLite overhead and free pages.
    pub max_bytes: u64,
}
const APP_ID: i64 = 0x504f4e59;
const MAX_ENTRIES: i64 = 65536;
type Key = (String, u64, String);
#[derive(Default)]
struct State {
    disabled: bool,
    retry: Option<Instant>,
    checked: bool,
    reclaim: bool,
    touches: std::collections::BTreeMap<String, i64>,
    maintenance: Option<Instant>,
}
// Dropping the async wait prevents a queued blocking operation from starting.
// An operation already inside SQLite retains the semaphore permit until it exits.
struct Pending(Arc<AtomicBool>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
pub(crate) struct DiskCache {
    options: DiskCacheOptions,
    namespace: String,
    gate: Arc<Semaphore>,
    state: Mutex<State>,
    warnings: Mutex<BTreeSet<&'static str>>,
}
impl DiskCache {
    #[cfg(test)]
    pub(crate) async fn wait_idle(&self) {
        drop(self.gate.acquire().await.unwrap());
    }
    pub fn new(options: DiskCacheOptions, namespace: Option<String>) -> Option<Arc<Self>> {
        if options.max_bytes == 0 {
            return None;
        }
        let Some(namespace) = namespace else {
            tracing::warn!(target: "ponydb::cache", event = "disk_cache_degraded", reason = "backend_identity_unavailable");
            return None;
        };
        Some(Arc::new(Self {
            options,
            namespace,
            gate: Arc::new(Semaphore::new(1)),
            state: Mutex::new(State::default()),
            warnings: Mutex::new(BTreeSet::new()),
        }))
    }
    fn warn(&self, category: &'static str) {
        if self.warnings.lock().unwrap().insert(category) {
            tracing::warn!(target: "ponydb::cache", event = "disk_cache_degraded", reason = category);
        }
    }
    fn key(&self, key: &Key) -> String {
        let encoded = serde_json::to_vec(&(1, &self.namespace, &key.0, key.1, &key.2)).unwrap();
        format!("{:x}", Sha256::digest(encoded))
    }
    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
        f: impl FnOnce(&Self, &mut Connection, &mut State) -> rusqlite::Result<T> + Send + 'static,
    ) -> Option<T> {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        let permit = self.gate.clone().try_acquire_owned().ok()?;
        let pending = Pending(Arc::new(AtomicBool::new(false)));
        let cancelled = pending.0.clone();
        let this = self.clone();
        let captured = observability::capture();
        let job = tokio::task::spawn_blocking(move || {
            captured.run(|| {
                let _permit = permit;
                if cancelled.load(Ordering::Acquire) || tokio::time::Instant::now() >= deadline {
                    return None;
                }
                let mut state = this.state.lock().unwrap();
                if state.disabled || state.retry.is_some_and(|t| Instant::now() < t) {
                    return None;
                }
                let result = this.open().and_then(|mut db| f(&this, &mut db, &mut state));
                match result {
                    Ok(value) => Some(value),
                    Err(error) => {
                        use rusqlite::ErrorCode::*;
                        match error.sqlite_error_code() {
                            Some(DatabaseBusy | DatabaseLocked) => this.warn("busy"),
                            Some(DatabaseCorrupt | NotADatabase | SchemaChanged | Unknown) => {
                                state.disabled = true;
                                this.warn("format invalid or corrupt");
                            }
                            _ => {
                                state.retry = Some(Instant::now() + Duration::from_secs(30));
                                this.warn("I/O unavailable");
                            }
                        }
                        None
                    }
                }
            })
        });
        match tokio::time::timeout_at(deadline, job).await {
            Ok(Ok(value)) => value,
            _ => {
                self.warn("wait timed out");
                None
            }
        }
    }
    fn open(&self) -> rusqlite::Result<Connection> {
        let io = |_| rusqlite::Error::InvalidPath(self.options.directory.clone());
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.options.directory).map_err(io)?;
        let path = self.options.directory.join("objects-v1.sqlite3");
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file.mode(0o600);
        }
        match file.open(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(5))?;
        let app: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app == APP_ID && version == 1 {
            db.execute_batch("PRAGMA synchronous=NORMAL;")?;
            return Ok(db);
        }
        // SQLite requires auto_vacuum before BEGIN and before the first table.
        // Only an unclaimed, empty database may be configured here; recheck under
        // the writer lock below because another process may initialize meanwhile.
        if app == 0
            && version == 0
            && db.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })? == 0
        {
            db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        }
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let tables: i64 = tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        if !((app == 0 && version == 0 && tables == 0) || (app == APP_ID && version == 1)) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(26),
                None,
            ));
        }
        if app == 0 {
            tx.execute_batch("
                CREATE TABLE objects (key TEXT PRIMARY KEY, body BLOB NOT NULL, touched INTEGER NOT NULL);
                CREATE INDEX lru ON objects(touched, key);
                CREATE TABLE usage (bytes INTEGER NOT NULL, entries INTEGER NOT NULL);
                INSERT INTO usage VALUES(0,0);
                CREATE TRIGGER added AFTER INSERT ON objects BEGIN UPDATE usage SET bytes=bytes+length(new.body), entries=entries+1; END;
                CREATE TRIGGER removed AFTER DELETE ON objects BEGIN UPDATE usage SET bytes=bytes-length(old.body), entries=entries-1; END;
                PRAGMA application_id=1347374681; PRAGMA user_version=1;")?;
        }
        tx.commit()?;
        db.execute_batch("PRAGMA synchronous=NORMAL;")?;
        Ok(db)
    }
    fn trim(
        db: &Connection,
        max_bytes: u64,
        reserve: usize,
        limit: usize,
    ) -> rusqlite::Result<(bool, usize)> {
        let mut evicted = 0;
        for _ in 0..limit {
            let (bytes, entries): (i64, i64) =
                db.query_row("SELECT bytes, entries FROM usage", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            if (bytes.max(0) as u64).saturating_add(reserve as u64) <= max_bytes
                && entries < MAX_ENTRIES
            {
                return Ok((true, evicted));
            }
            if db.execute("DELETE FROM objects WHERE key=(SELECT key FROM objects ORDER BY touched,key LIMIT 1)", [])? == 0 { break; }
            evicted += 1;
        }
        Ok((false, evicted))
    }
    pub async fn read(
        self: &Arc<Self>,
        key: &Key,
        deadline: tokio::time::Instant,
    ) -> Option<Arc<Vec<u8>>> {
        let id = self.key(key);
        let size = i64::try_from(key.1).ok()?;
        let hash = key.2.clone();
        let value = self
            .run(deadline, move |this, db, state| {
                if !state.checked {
                    let tx =
                        db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    let (checked, evicted) = Self::trim(&tx, this.options.max_bytes, 0, 128)?;
                    tx.commit()?;
                    for _ in 0..evicted {
                        observability::cache("disk", "evicted", None);
                    }
                    state.checked = checked;
                    state.reclaim =
                        db.query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))? > 0;
                }
                // Filter length before allocating the blob. Hash is always checked, including on warm reads.
                let body: Option<(Vec<u8>, i64)> = db
                    .query_row(
                        "SELECT body,touched FROM objects WHERE key=?1 AND length(body)=?2",
                        params![id, size],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((body, touched)) = body {
                    if format!("{:x}", Sha256::digest(&body)) == hash {
                        if state.touches.len() >= 4096 {
                            state.touches.clear();
                        }
                        let now = super::now_ms() as i64 / 1000;
                        if touched < now - 60 {
                            state.touches.insert(id, now);
                        }
                        return Ok(Some(Arc::new(body)));
                    }
                    this.warn("entry checksum invalid");
                }
                db.execute("DELETE FROM objects WHERE key=?1", [&id])?;
                Ok(None)
            })
            .await;
        observability::cache(
            "disk",
            match &value {
                Some(Some(_)) => "hit",
                Some(None) => "miss",
                None => "bypass",
            },
            Some(&key.0),
        );
        self.maintain();
        value.flatten()
    }
    pub async fn write(
        self: &Arc<Self>,
        key: &Key,
        body: Arc<Vec<u8>>,
        deadline: tokio::time::Instant,
    ) {
        self.write_inner(key, body, deadline, true).await;
    }
    pub(crate) async fn write_if_room(
        self: &Arc<Self>,
        key: &Key,
        body: Arc<Vec<u8>>,
        deadline: tokio::time::Instant,
    ) {
        self.write_inner(key, body, deadline, false).await;
    }
    async fn write_inner(
        self: &Arc<Self>,
        key: &Key,
        body: Arc<Vec<u8>>,
        deadline: tokio::time::Instant,
        evict: bool,
    ) {
        if body.len() as u64 > self.options.max_bytes {
            observability::cache("disk", "oversized", Some(&key.0));
            return;
        }
        let id = self.key(key);
        let stored = self
            .run(deadline, move |this, db, state| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                if !evict {
                    let (bytes, entries): (i64, i64) =
                        tx.query_row("SELECT bytes,entries FROM usage", [], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })?;
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE key=?1)",
                        [&id],
                        |r| r.get(0),
                    )?;
                    if exists
                        || (bytes.max(0) as u64).saturating_add(body.len() as u64)
                            > this.options.max_bytes
                        || entries >= MAX_ENTRIES
                    {
                        return Ok(false);
                    }
                }
                tx.execute("DELETE FROM objects WHERE key=?1", [&id])?;
                let (stored, evicted) = Self::trim(&tx, this.options.max_bytes, body.len(), 128)?;
                if stored {
                    tx.execute(
                        "INSERT INTO objects VALUES(?1,?2,?3)",
                        params![id, body.as_slice(), super::now_ms() as i64 / 1000],
                    )?;
                } else {
                    state.checked = false;
                }
                Self::touch(&tx, state)?;
                tx.commit()?;
                for _ in 0..evicted {
                    observability::cache("disk", "evicted", None);
                }
                state.reclaim =
                    db.query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))? > 0;
                Ok(stored)
            })
            .await;
        observability::cache(
            "disk",
            match stored {
                Some(true) => "filled",
                Some(false) => "skipped",
                None => "bypass",
            },
            Some(&key.0),
        );
        self.maintain();
    }
    fn touch(db: &Connection, state: &mut State) -> rusqlite::Result<()> {
        for (id, stamp) in &state.touches {
            db.execute(
                "UPDATE objects SET touched=?2 WHERE key=?1 AND touched < ?2-60",
                params![id, stamp],
            )?;
        }
        state.touches.clear();
        Ok(())
    }
    fn maintain(self: &Arc<Self>) {
        let Ok(permit) = self.gate.clone().try_acquire_owned() else {
            return;
        };
        {
            let Ok(mut state) = self.state.try_lock() else {
                return;
            };
            if state.retry.is_some_and(|t| Instant::now() < t)
                || state.disabled
                || state
                    .maintenance
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            {
                return;
            }
            if !state.reclaim && state.touches.is_empty() {
                return;
            }
            state.maintenance = Some(Instant::now());
        }
        let this = self.clone();
        let captured = observability::capture();
        tokio::task::spawn_blocking(move || {
            captured.run(|| {
                let _permit = permit;
                let mut state = this.state.lock().unwrap();
                if let Ok(mut db) = this.open() {
                    let result = (|| {
                        let tx = db.transaction()?;
                        Self::touch(&tx, &mut state)?;
                        tx.commit()?;
                        if state.reclaim {
                            db.execute_batch("PRAGMA incremental_vacuum(64)")?;
                            state.reclaim = db
                                .query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))?
                                > 0;
                        }
                        Ok::<_, rusqlite::Error>(())
                    })();
                    if result.is_err() {
                        this.warn("maintenance unavailable");
                    }
                }
            })
        });
    }
}

#[cfg(test)]
mod tests;
