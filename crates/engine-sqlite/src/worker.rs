//! Fixed database threads, bounded admission, and request-scoped asynchronous I/O.
use super::*;
use remote_pager::bridge::{bridge, AsyncSnapshot};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    thread,
};
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send + 'static>;
#[derive(Default)]
struct Admission {
    databases: HashSet<String>,
    subjects: HashMap<String, usize>,
}
struct Lease {
    state: Arc<Mutex<Admission>>,
    database: String,
    subject: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.databases.remove(&self.database);
            if let Some(n) = s.subjects.get_mut(&self.subject) {
                *n -= 1;
                if *n == 0 {
                    s.subjects.remove(&self.subject);
                }
            }
        }
    }
}
struct CancelOnDrop(Option<RequestContext>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(ctx) = &self.0 {
            ctx.cancel();
        }
    }
}
pub struct WorkerPool {
    tx: Option<crossbeam_channel::Sender<Job>>,
    threads: Vec<thread::JoinHandle<()>>,
    admission: Arc<Mutex<Admission>>,
    per_subject: usize,
}
impl WorkerPool {
    pub fn new(threads: usize, queue: usize, per_subject: usize) -> Result<Self, Error> {
        if threads == 0 || queue == 0 || per_subject == 0 {
            return Err(Error::Configuration);
        }
        let (tx, rx) = crossbeam_channel::bounded::<Job>(queue);
        let mut pool = Self {
            tx: Some(tx),
            threads: Vec::new(),
            admission: Arc::default(),
            per_subject,
        };
        for n in 0..threads {
            let rx = rx.clone();
            pool.threads.push(
                thread::Builder::new()
                    .name(format!("sqlite-{n}"))
                    .spawn(move || {
                        while let Ok(job) = rx.recv() {
                            job()
                        }
                    })
                    .map_err(|_| Error::Worker)?,
            );
        }
        Ok(pool)
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare(
        &self,
        database: String,
        subject: String,
        source: Arc<dyn AsyncSnapshot>,
        page_size: usize,
        statements: Vec<Statement>,
        limits: ExecutionLimits,
        ctx: RequestContext,
    ) -> Result<PreparedTransaction, Error> {
        self.prepare_mode(
            database, subject, source, page_size, statements, limits, ctx, false,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn inspect(
        &self,
        database: String,
        subject: String,
        source: Arc<dyn AsyncSnapshot>,
        page_size: usize,
        statements: Vec<Statement>,
        limits: ExecutionLimits,
        ctx: RequestContext,
    ) -> Result<PreparedTransaction, Error> {
        self.prepare_mode(
            database, subject, source, page_size, statements, limits, ctx, true,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn prepare_mode(
        &self,
        database: String,
        subject: String,
        source: Arc<dyn AsyncSnapshot>,
        page_size: usize,
        statements: Vec<Statement>,
        limits: ExecutionLimits,
        ctx: RequestContext,
        inspect_only: bool,
    ) -> Result<PreparedTransaction, Error> {
        ctx.check()?;
        let lease = {
            let mut state = self.admission.lock().map_err(|_| Error::Worker)?;
            if state.databases.contains(&database)
                || state.subjects.get(&subject).copied().unwrap_or(0) >= self.per_subject
            {
                tracing::debug!(target: "ponydb::engine", event = "worker_rejected", reason = "admission_limit");
                return Err(Error::Busy);
            }
            state.databases.insert(database.clone());
            *state.subjects.entry(subject.clone()).or_default() += 1;
            Lease {
                state: self.admission.clone(),
                database,
                subject,
            }
        };
        let mut cancel_guard = CancelOnDrop(Some(ctx.clone()));
        let (snapshot, server) = bridge(source, ctx.clone(), limits.cache_bytes);
        let (reply, result) = oneshot::channel();
        let job_ctx = ctx.clone();
        let captured = observability::capture();
        let queued_at = std::time::Instant::now();
        tracing::debug!(target: "ponydb::engine", event = "worker_queued", inspect_only);
        self.tx
            .as_ref()
            .ok_or(Error::Worker)?
            .try_send(Box::new(move || captured.run(|| {
                tracing::debug!(target: "ponydb::engine", event = "worker_started", inspect_only, queue_ms = queued_at.elapsed().as_millis() as u64);
                let started = std::time::Instant::now();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepare_mode(
                        snapshot,
                        page_size,
                        &statements,
                        &limits,
                        job_ctx.clone(),
                        inspect_only,
                    )
                }))
                .unwrap_or(Err(Error::Worker));
                let result = match job_ctx.check() {
                    Ok(()) => result,
                    Err(e) => Err(Error::Stopped(e)),
                };
                tracing::debug!(target: "ponydb::engine", event = "worker_completed", inspect_only, success = result.is_ok(), elapsed_ms = started.elapsed().as_millis() as u64);
                // A returned result must allow the next inspect/execute call to
                // acquire this database and subject, including on SQL errors.
                drop(lease);
                let _ = reply.send(result);
            })))
            .map_err(|_| {
                tracing::debug!(target: "ponydb::engine", event = "worker_rejected", reason = "queue_full");
                Error::Busy
            })?;
        let serve = server.serve();
        tokio::pin!(serve);
        tokio::pin!(result);
        // A completed bridge means its sender was dropped; still join the worker result.
        let answer = tokio::select! {
            r=&mut result=>r.map_err(|_|Error::Worker)?,
            _=&mut serve=>result.await.map_err(|_|Error::Worker)?,
        };
        cancel_guard.0 = None;
        answer
    }
}
impl Drop for WorkerPool {
    fn drop(&mut self) {
        drop(self.tx.take());
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
