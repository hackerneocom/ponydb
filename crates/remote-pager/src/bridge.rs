//! A bounded synchronous VFS ↔ asynchronous storage bridge. Run `serve` on Tokio,
//! never on the database worker that waits for its responses.
use crate::{Error, Result, Snapshot};
use request_context::RequestContext;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc as async_mpsc;

pub trait AsyncSnapshot: Send + Sync + 'static {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Fixed immutable version, exact bytes or an error. Dropping the future cancels I/O.
    fn read_at(
        &self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>>;
}
struct ReadRequest {
    offset: u64,
    len: usize,
    reply: mpsc::SyncSender<Result<Vec<u8>>>,
}
struct Cache {
    bytes: usize,
    entries: HashMap<(u64, usize), Vec<u8>>,
    order: VecDeque<(u64, usize)>,
}
pub struct BridgeSnapshot {
    len: u64,
    tx: async_mpsc::Sender<ReadRequest>,
    ctx: RequestContext,
    cache: Mutex<Cache>,
    max_cache_bytes: usize,
}
pub struct BridgeServer {
    source: Arc<dyn AsyncSnapshot>,
    rx: async_mpsc::Receiver<ReadRequest>,
    ctx: RequestContext,
}
pub fn bridge(
    source: Arc<dyn AsyncSnapshot>,
    ctx: RequestContext,
    max_cache_bytes: usize,
) -> (Arc<BridgeSnapshot>, BridgeServer) {
    let (tx, rx) = async_mpsc::channel(1);
    (
        Arc::new(BridgeSnapshot {
            len: source.len(),
            tx,
            ctx: ctx.clone(),
            cache: Mutex::new(Cache {
                bytes: 0,
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
            max_cache_bytes,
        }),
        BridgeServer { source, rx, ctx },
    )
}
fn stopped() -> Error {
    Error::Io("request cancelled or deadline exceeded".into())
}
impl Snapshot for BridgeSnapshot {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<()> {
        self.ctx.check().map_err(|_| stopped())?;
        let key = (offset, out.len());
        if let Some(bytes) = self.cache.lock().map_err(|_| stopped())?.entries.get(&key) {
            observability::count(observability::Counter::BridgeHits, 1);
            tracing::trace!(target: "ponydb::cache", event = "cache_access", layer = "bridge", outcome = "hit", offset, bytes = out.len());
            out.copy_from_slice(bytes);
            return Ok(());
        }
        observability::count(observability::Counter::BridgeMisses, 1);
        tracing::trace!(target: "ponydb::cache", event = "cache_access", layer = "bridge", outcome = "miss", offset, bytes = out.len());
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .try_send(ReadRequest {
                offset,
                len: out.len(),
                reply,
            })
            .map_err(|_| Error::Io("I/O bridge unavailable or saturated".into()))?;
        let bytes = loop {
            self.ctx.check().map_err(|_| stopped())?;
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(value) => break value?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(stopped()),
            }
        };
        self.ctx.check().map_err(|_| stopped())?;
        if bytes.len() != out.len() {
            return Err(Error::Io("truncated snapshot range".into()));
        }
        out.copy_from_slice(&bytes);
        if bytes.len() <= self.max_cache_bytes {
            let mut cache = self.cache.lock().map_err(|_| stopped())?;
            while cache.bytes + bytes.len() > self.max_cache_bytes {
                if let Some(old) = cache.order.pop_front() {
                    if let Some(v) = cache.entries.remove(&old) {
                        cache.bytes -= v.len();
                        tracing::trace!(target: "ponydb::cache", event = "cache_evicted", layer = "bridge", offset = old.0, bytes = v.len());
                    }
                } else {
                    break;
                }
            }
            // Each bridge has a single candidate worker; no concurrent cache fills.
            if !cache.entries.contains_key(&key) {
                cache.bytes += bytes.len();
                cache.order.push_back(key);
                cache.entries.insert(key, bytes);
                tracing::trace!(target: "ponydb::cache", event = "cache_filled", layer = "bridge", offset, bytes = out.len());
            }
        }
        Ok(())
    }
}
impl BridgeServer {
    /// Request-scoped future, polled alongside the worker result, never detached.
    pub async fn serve(mut self) {
        while let Some(req) = self.rx.recv().await {
            let read = self.source.read_at(req.offset, req.len);
            tokio::pin!(read);
            let result = loop {
                if self.ctx.check().is_err() {
                    break Err(stopped());
                }
                tokio::select! {
                    result=&mut read=>break result,
                    _=self.ctx.cancelled()=>break Err(stopped()),
                    _=tokio::time::sleep(Duration::from_millis(10))=>{},
                }
            };
            let _ = req.reply.send(result);
        }
    }
}
