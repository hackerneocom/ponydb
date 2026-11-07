//! Payload-free request diagnostics. Libraries never install a subscriber.
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    task::{Context as TaskContext, Poll},
    time::Instant,
};
use tracing::{dispatcher, Dispatch, Span};

// A private process salt prevents offline guessing of database identifiers.
// Object keys are explicitly permitted and are logged separately, without hashing.
pub fn anonymous_id(value: &str) -> String {
    static SALT: OnceLock<[u8; 32]> = OnceLock::new();
    let salt = SALT.get_or_init(|| {
        let mut bytes = [0; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes
    });
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(value.as_bytes());
    format!("{:x}", hash.finalize())[..24].to_owned()
}

/// Counters count logical operations, not distinct pages or objects.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Counter {
    PageReads,
    PageWrites,
    PageReadBytes,
    PageWriteBytes,
    S3Requests,
    S3ReadBytes,
    S3WriteBytes,
    BridgeHits,
    BridgeMisses,
    MemoryHits,
    MemoryMisses,
    NodeHits,
    NodeMisses,
    DiskHits,
    DiskMisses,
    DiskBypasses,
    RemotePageReads,
    RemotePageReadBytes,
}
const COUNTERS: usize = 18;
struct Data {
    id: String,
    counters: [AtomicU64; COUNTERS],
    data_version: AtomicU64,
    result: Mutex<Option<(&'static str, String)>>,
    phase: Mutex<Option<(&'static str, Instant)>>,
}
thread_local! {
    static CURRENT: RefCell<Option<Arc<Data>>> = const { RefCell::new(None) };
}
pub fn request_id() -> Option<String> {
    CURRENT.with(|current| current.borrow().as_ref().map(|data| data.id.clone()))
}
pub fn count(counter: Counter, amount: u64) {
    CURRENT.with(|current| {
        if let Some(data) = current.borrow().as_ref() {
            data.counters[counter as usize].fetch_add(amount, Ordering::Relaxed);
        }
    });
}

/// Capture before spawning; enter only while polling/executing the captured work.
#[derive(Clone)]
pub struct Captured {
    data: Option<Arc<Data>>,
    span: Span,
    dispatch: Dispatch,
}
pub fn capture() -> Captured {
    Captured {
        data: CURRENT.with(|current| current.borrow().clone()),
        span: Span::current(),
        dispatch: dispatcher::get_default(Clone::clone),
    }
}
impl Captured {
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        struct Reset(Option<Arc<Data>>);
        impl Drop for Reset {
            fn drop(&mut self) {
                CURRENT.with(|current| *current.borrow_mut() = self.0.take());
            }
        }
        dispatcher::with_default(&self.dispatch, || {
            let previous = CURRENT.with(|current| current.replace(self.data.clone()));
            let _reset = Reset(previous);
            let _span = self.span.enter();
            f()
        })
    }
    pub fn scope<F: Future>(&self, future: F) -> Scoped<F> {
        Scoped {
            captured: self.clone(),
            future: Some(Box::pin(future)),
        }
    }
}
pub struct Scoped<F> {
    captured: Captured,
    future: Option<Pin<Box<F>>>,
}
impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.captured.run(|| {
            this.future
                .as_mut()
                .expect("scoped future")
                .as_mut()
                .poll(cx)
        })
    }
}

impl<F> Drop for Scoped<F> {
    fn drop(&mut self) {
        // Cancellation drops I/O guards too; their final events need the same context.
        if let Some(future) = self.future.take() {
            self.captured.run(|| drop(future));
        }
    }
}

/// Owns exactly one request summary. Nested service calls reuse its correlation.
pub struct Request {
    captured: Captured,
    root: bool,
    finished: bool,
    started: Instant,
}
impl Request {
    pub fn new(operation: &'static str, database: Option<&str>) -> Self {
        let mut captured = capture();
        let root = captured.data.is_none();
        if root {
            let data = Arc::new(Data {
                id: uuid::Uuid::new_v4().to_string(),
                counters: std::array::from_fn(|_| AtomicU64::new(0)),
                data_version: AtomicU64::new(0),
                result: Mutex::new(None),
                phase: Mutex::new(None),
            });
            let database_id = tracing::enabled!(target: "ponydb", tracing::Level::DEBUG)
                .then(|| database.map(anonymous_id))
                .flatten();
            captured.span = tracing::debug_span!(target: "ponydb", "request",
                request_id = %data.id, operation, database_id = database_id.as_deref());
            captured.data = Some(data);
            captured.run(|| tracing::debug!(target: "ponydb", event = "request_started"));
        }
        if !root {
            let data = captured.data.as_ref().unwrap();
            let database_id = tracing::enabled!(target: "ponydb", tracing::Level::DEBUG)
                .then(|| database.map(anonymous_id))
                .flatten();
            captured.span = tracing::debug_span!(target: "ponydb", "operation",
                request_id = %data.id, operation, database_id = database_id.as_deref());
        }
        Self {
            captured,
            root,
            finished: false,
            started: Instant::now(),
        }
    }
    pub fn scope<F: Future>(&self, future: F) -> Scoped<F> {
        self.captured.scope(future)
    }
    pub fn set_data_version(&self, version: u64) {
        self.captured
            .data
            .as_ref()
            .unwrap()
            .data_version
            .store(version, Ordering::Relaxed);
    }
    /// Only call with application-owned outcome and error codes, never error text.
    pub fn finish(&mut self, outcome: &'static str, error_code: &str) {
        self.finished = true;
        let data = self.captured.data.as_ref().expect("request context");
        {
            let mut result = data.result.lock().unwrap();
            // A transport-level success retains the more precise business outcome.
            if !matches!(outcome, "success" | "failed")
                || !error_code.is_empty()
                || result.is_none()
            {
                *result = Some((outcome, error_code.to_owned()));
            }
        }
        if self.root {
            self.emit();
        }
    }
    fn emit(&self) {
        self.captured.run(|| {
            let data = self.captured.data.as_ref().unwrap();
            let result = data.result.lock().unwrap();
            let (outcome, code) = result.as_ref().map(|(o, c)| (*o, c.as_str())).unwrap_or(("aborted", ""));
            let version = data.data_version.load(Ordering::Relaxed);
            let data_version = (version != 0).then_some(version);
            let last_phase = *data.phase.lock().unwrap();
            let n = |counter: Counter| data.counters[counter as usize].load(Ordering::Relaxed);
            tracing::debug!(target: "ponydb", event = "request_completed", outcome,
                error_code = code, elapsed_ms = self.started.elapsed().as_millis() as u64, data_version,
                last_stage = last_phase.map(|(name, _)| name), last_stage_elapsed_ms = last_phase.map(|(_, started)| started.elapsed().as_millis() as u64),
                page_reads = n(Counter::PageReads), page_writes = n(Counter::PageWrites),
                page_read_bytes = n(Counter::PageReadBytes), page_write_bytes = n(Counter::PageWriteBytes),
                s3_requests = n(Counter::S3Requests), s3_read_bytes = n(Counter::S3ReadBytes), s3_write_bytes = n(Counter::S3WriteBytes),
                bridge_hits = n(Counter::BridgeHits), bridge_misses = n(Counter::BridgeMisses),
                memory_hits = n(Counter::MemoryHits), memory_misses = n(Counter::MemoryMisses),
                remote_page_reads = n(Counter::RemotePageReads), remote_page_read_bytes = n(Counter::RemotePageReadBytes),
                node_hits = n(Counter::NodeHits), node_misses = n(Counter::NodeMisses),
                disk_hits = n(Counter::DiskHits), disk_misses = n(Counter::DiskMisses), disk_bypasses = n(Counter::DiskBypasses));
        });
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        if self.root && !self.finished {
            // A dropped future does not establish whether a write committed.
            let data = self.captured.data.as_ref().unwrap();
            *data.result.lock().unwrap() = Some(("aborted", String::new()));
            self.emit();
        }
    }
}

/// Record a terminal lookup outcome or a cache lifecycle event.
pub fn cache(layer: &'static str, outcome: &'static str, key: Option<&str>) {
    use Counter::*;
    let counter = match (layer, outcome) {
        ("bridge", "hit") => Some(BridgeHits),
        ("bridge", "miss") => Some(BridgeMisses),
        ("memory", "hit") => Some(MemoryHits),
        ("memory", "miss" | "coalesced") => Some(MemoryMisses),
        ("node", "hit") => Some(NodeHits),
        ("node", "miss" | "coalesced") => Some(NodeMisses),
        ("disk", "hit") => Some(DiskHits),
        ("disk", "miss") => Some(DiskMisses),
        ("disk", "bypass") => Some(DiskBypasses),
        _ => None,
    };
    if let Some(counter) = counter {
        count(counter, 1);
    }
    tracing::trace!(target: "ponydb::cache", event = "cache_access", layer, outcome, key);
}

/// A stage boundary, not a polling/progress event.
pub fn phase(stage: &'static str) {
    CURRENT.with(|current| {
        let data = current.borrow();
        let previous = data
            .as_ref()
            .and_then(|data| data.phase.lock().unwrap().replace((stage, Instant::now())));
        tracing::debug!(target: "ponydb", event = "phase", stage,
            previous_stage = previous.map(|(name, _)| name),
            previous_elapsed_ms = previous.map(|(_, start)| start.elapsed().as_millis() as u64));
    });
}

/// Normalize response fields before using them as diagnostic labels.
pub fn outcome(value: &str) -> &'static str {
    match value {
        "committed" => "committed",
        "read_only" => "read_only",
        "partial" => "partial",
        "applied" => "applied",
        "unknown" => "unknown",
        "not_committed" => "failed",
        _ => "success",
    }
}
pub fn error_outcome(code: &str, value: &str) -> &'static str {
    if value == "unknown" {
        "unknown"
    } else if value == "partial" {
        "partial"
    } else if matches!(
        code,
        "TX_CONFLICT" | "METADATA_PRECONDITION_FAILED" | "DATABASE_EXISTS"
    ) {
        "conflict"
    } else if code == "DEADLINE_EXCEEDED" {
        "stopped"
    } else {
        "failed"
    }
}

/// A single page action. No page contents are inspected or formatted.
pub struct PageIo {
    layer: &'static str,
    direction: &'static str,
    page: u64,
    bytes: u64,
    started: Instant,
    outcome: &'static str,
}
impl PageIo {
    pub fn new(layer: &'static str, direction: &'static str, page: u64) -> Self {
        count(
            if layer == "snapshot" {
                Counter::RemotePageReads
            } else if direction == "read" {
                Counter::PageReads
            } else {
                Counter::PageWrites
            },
            1,
        );
        Self {
            layer,
            direction,
            page,
            bytes: 0,
            started: Instant::now(),
            outcome: "aborted",
        }
    }
    pub fn finish(&mut self, outcome: &'static str, bytes: u64) {
        self.outcome = outcome;
        self.bytes = bytes;
        count(
            if self.layer == "snapshot" {
                Counter::RemotePageReadBytes
            } else if self.direction == "read" {
                Counter::PageReadBytes
            } else {
                Counter::PageWriteBytes
            },
            bytes,
        );
    }
}
impl Drop for PageIo {
    fn drop(&mut self) {
        tracing::trace!(target: "ponydb::pager", event = "page_io_completed", layer = self.layer,
            direction = self.direction, page = self.page, bytes = self.bytes, outcome = self.outcome,
            elapsed_us = self.started.elapsed().as_micros() as u64);
    }
}

/// Transport failures can occur before reaching the business service.
pub fn record_result(outcome: &'static str, error_code: &str) {
    CURRENT.with(|current| {
        if let Some(data) = current.borrow().as_ref() {
            *data.result.lock().unwrap() = Some((outcome, error_code.to_owned()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::io::{self, Write};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }
    impl Buffer {
        fn events(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }
    fn subscriber(buffer: Buffer, level: tracing::Level) -> Dispatch {
        Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .with_span_list(false)
                .with_max_level(level)
                .with_writer(buffer)
                .finish(),
        )
    }
    fn id(event: &Value) -> &str {
        event["span"]["request_id"].as_str().unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_requests_keep_correlation_and_counts_across_threads() {
        let buffer = Buffer::default();
        let dispatch = subscriber(buffer.clone(), tracing::Level::DEBUG);
        let mut requests: Vec<_> = (0..2)
            .map(|_| {
                dispatcher::with_default(&dispatch, || {
                    Request::new("test", Some("private_database"))
                })
            })
            .collect();
        let mut tasks = Vec::new();
        for request in &requests {
            tasks.push(tokio::spawn(request.scope(async {
                let expected = request_id().unwrap();
                tokio::task::yield_now().await;
                assert_eq!(request_id().as_deref(), Some(expected.as_str()));
                let context = capture();
                let worker = std::thread::spawn(move || {
                    context.run(|| {
                        count(Counter::PageReads, 3);
                        cache("memory", "hit", Some("permitted/key"));
                        phase("worker");
                        request_id().unwrap()
                    })
                });
                assert_eq!(worker.join().unwrap(), expected);
                let context = capture();
                let blocking_id = tokio::task::spawn_blocking(move || {
                    context.run(|| {
                        count(Counter::S3Requests, 2);
                        phase("blocking");
                        request_id().unwrap()
                    })
                })
                .await
                .unwrap();
                assert_eq!(blocking_id, expected);
                expected
            })));
        }
        let mut ids = Vec::new();
        for (request, task) in requests.iter_mut().zip(tasks) {
            ids.push(task.await.unwrap());
            request.finish("read_only", "");
        }
        assert_ne!(ids[0], ids[1]);
        assert!(request_id().is_none());
        let events = buffer.events();
        let summaries: Vec<_> = events
            .iter()
            .filter(|e| e["fields"]["event"] == "request_completed")
            .collect();
        assert_eq!(summaries.len(), 2);
        for event in summaries {
            assert!(ids.iter().any(|expected| expected == id(event)));
            assert_eq!(event["fields"]["page_reads"], 3);
            assert_eq!(event["fields"]["s3_requests"], 2);
            assert_eq!(event["fields"]["memory_hits"], 1);
        }
        for event in &events {
            assert!(ids.iter().any(|expected| expected == id(event)));
        }
        let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(!output.contains("private_database"));
        // Counters remain available with trace events filtered out.
        assert!(!output.contains("cache_access"));
    }

    #[tokio::test]
    async fn dropping_future_logs_aborted_io_in_original_context() {
        let buffer = Buffer::default();
        let dispatch = subscriber(buffer.clone(), tracing::Level::TRACE);
        let request = dispatcher::with_default(&dispatch, || Request::new("cancel_test", None));
        let mut future = Box::pin(request.scope(async {
            let _io = PageIo::new("snapshot", "read", 7);
            std::future::pending::<()>().await;
        }));
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(future);
        drop(request);
        let events = buffer.events();
        let started_id = id(&events[0]);
        let aborted = events
            .iter()
            .find(|e| e["fields"]["event"] == "page_io_completed")
            .unwrap();
        assert_eq!(id(aborted), started_id);
        assert_eq!(aborted["fields"]["outcome"], "aborted");
        assert_eq!(events.last().unwrap()["fields"]["outcome"], "aborted");
        assert_eq!(events.last().unwrap()["fields"]["remote_page_reads"], 1);
    }

    #[test]
    fn nested_outcomes_and_error_ids_are_retained() {
        let buffer = Buffer::default();
        let dispatch = subscriber(buffer.clone(), tracing::Level::DEBUG);
        let mut root = dispatcher::with_default(&dispatch, || Request::new("http", None));
        root.captured.run(|| {
            let mut child = Request::new("write", None);
            assert_eq!(
                request_id(),
                child.captured.data.as_ref().map(|d| d.id.clone())
            );
            child.finish("unknown", "OUTCOME_UNKNOWN");
        });
        root.finish("success", "");
        let events = buffer.events();
        assert_eq!(
            events
                .iter()
                .filter(|e| e["fields"]["event"] == "request_completed")
                .count(),
            1
        );
        assert_eq!(events.last().unwrap()["fields"]["outcome"], "unknown");
        assert_eq!(
            events.last().unwrap()["fields"]["error_code"],
            "OUTCOME_UNKNOWN"
        );
        assert_eq!(error_outcome("DEADLINE_EXCEEDED", "partial"), "partial");
        assert_eq!(error_outcome("TX_CONFLICT", "not_committed"), "conflict");
        assert_eq!(anonymous_id("database"), anonymous_id("database"));
        assert_ne!(anonymous_id("database"), anonymous_id("another"));
    }
}
