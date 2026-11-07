//! A bounded cache of verified immutable objects, scoped to one backend instance.
use super::*;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    mem::size_of,
    sync::Mutex,
};

const MAX_OBJECT_BYTES: usize = 16 * 1024 * 1024;
const MAX_OBJECT_ENTRIES: usize = 8192;
const MAX_NODE_WEIGHT: usize = 8 * 1024 * 1024;
const MAX_NODE_ENTRIES: usize = 4096;
const FILL_STRIPES: usize = 64;

type Key = (String, u64, String);
struct Entry<T> {
    value: Arc<T>,
    weight: usize,
    stamp: u64,
}
struct Cache<T> {
    layer: &'static str,
    entries: BTreeMap<Key, Entry<T>>,
    order: BTreeMap<u64, Key>,
    weight: usize,
    clock: u64,
    max_weight: usize,
    max_entries: usize,
}
impl<T> Cache<T> {
    fn new(max_weight: usize, max_entries: usize) -> Self {
        Self::named("memory", max_weight, max_entries)
    }
    fn named(layer: &'static str, max_weight: usize, max_entries: usize) -> Self {
        Self {
            layer,
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            weight: 0,
            clock: 0,
            max_weight,
            max_entries,
        }
    }
    fn get(&mut self, key: &Key) -> Option<Arc<T>> {
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.stamp);
        self.clock += 1;
        entry.stamp = self.clock;
        self.order.insert(self.clock, key.clone());
        Some(Arc::clone(&entry.value))
    }
    fn insert(&mut self, key: Key, value: Arc<T>, weight: usize) {
        if weight > self.max_weight || self.max_entries == 0 {
            observability::cache(self.layer, "skipped", Some(&key.0));
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.order.remove(&old.stamp);
            self.weight -= old.weight;
        }
        while weight > self.max_weight - self.weight || self.entries.len() >= self.max_entries {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.weight -= entry.weight;
                observability::cache(self.layer, "evicted", Some(&oldest.0));
            }
        }
        self.clock += 1;
        self.weight += weight;
        self.order.insert(self.clock, key.clone());
        observability::cache(self.layer, "filled", Some(&key.0));
        self.entries.insert(
            key,
            Entry {
                value,
                weight,
                stamp: self.clock,
            },
        );
    }
}

fn cache_key(database: &DatabasePath, reference: &ObjectRef) -> Result<Key> {
    if !reference.validate() {
        return Err(Error::Corrupt);
    }
    let relative = reference.relative_key().map_err(|_| Error::Corrupt)?;
    Ok((
        database.resolve(&relative),
        reference.size,
        reference.sha256.clone(),
    ))
}

fn stripe(key: &Key) -> usize {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    hash.finish() as usize % FILL_STRIPES
}

// Estimates owned node memory, not allocator usage or total process memory.
// Inline values are accounted by their parent; this walks only extra allocations.
const MAP_ENTRY_OVERHEAD: usize = 64;
fn value_heap_weight(value: &Value) -> usize {
    match value {
        Value::String(s) => s.capacity(),
        Value::Array(values) => values.iter().fold(
            values.capacity().saturating_mul(size_of::<Value>()),
            |weight, value| weight.saturating_add(value_heap_weight(value)),
        ),
        Value::Object(values) => values.iter().fold(0usize, |weight, (key, value)| {
            weight
                .saturating_add(size_of::<(String, Value)>())
                .saturating_add(MAP_ENTRY_OVERHEAD)
                .saturating_add(key.capacity())
                .saturating_add(value_heap_weight(value))
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn node_weight(node: &IndexNode) -> usize {
    let weight = size_of::<IndexNode>()
        .saturating_add(node.prefix.capacity())
        .saturating_add(node.value.as_ref().map_or(0, value_heap_weight));
    node.children.values().fold(weight, |weight, reference| {
        weight
            .saturating_add(size_of::<(char, ObjectRef)>())
            .saturating_add(MAP_ENTRY_OVERHEAD)
            .saturating_add(reference.key.capacity())
            .saturating_add(reference.sha256.capacity())
    })
}
/// Share this reader between databases on the same backend. HEAD bypasses it.
/// Cancellation releases the memory fill slot. Already-started disk operations
/// may finish independently while retaining their bounded execution permit.
pub struct ObjectReader<S: BlobStore> {
    pub(crate) backend: Arc<S>,
    cache: Mutex<Cache<Vec<u8>>>,
    disk: Option<Arc<crate::disk_cache::DiskCache>>,
    fills: [tokio::sync::Mutex<()>; FILL_STRIPES],
    nodes: Mutex<Cache<IndexNode>>,
    node_fills: [tokio::sync::Mutex<()>; FILL_STRIPES],
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
}
impl<S: BlobStore> ObjectReader<S> {
    pub fn new(backend: Arc<S>) -> Self {
        Self::with_disk_cache(backend, None)
    }
    pub fn with_disk_cache(backend: Arc<S>, options: Option<crate::DiskCacheOptions>) -> Self {
        let disk = options.and_then(|options| {
            crate::disk_cache::DiskCache::new(options, backend.cache_namespace())
        });
        Self {
            disk,
            backend,
            cache: Mutex::new(Cache::new(MAX_OBJECT_BYTES, MAX_OBJECT_ENTRIES)),
            fills: std::array::from_fn(|_| tokio::sync::Mutex::new(())),
            nodes: Mutex::new(Cache::named("node", MAX_NODE_WEIGHT, MAX_NODE_ENTRIES)),
            node_fills: std::array::from_fn(|_| tokio::sync::Mutex::new(())),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    /// Best-effort write admission never evicts existing bodies or parsed nodes.
    pub(crate) async fn admit_written(
        &self,
        database: &DatabasePath,
        reference: &ObjectRef,
        bytes: Vec<u8>,
        ctx: &RequestContext,
    ) {
        if ctx.check().is_err() {
            return;
        }
        let Ok(key) = cache_key(database, reference) else {
            return;
        };
        let bytes = Arc::new(bytes);
        {
            let mut cache = self.cache.lock().expect("object cache poisoned");
            if !cache.entries.contains_key(&key)
                && bytes.len() <= cache.max_weight.saturating_sub(cache.weight)
                && cache.entries.len() < cache.max_entries
            {
                cache.insert(key.clone(), bytes.clone(), bytes.len());
            }
        }
        if let Some(disk) = &self.disk {
            let fill = disk.write_if_room(
                &key,
                bytes,
                tokio::time::Instant::now() + std::time::Duration::from_millis(50),
            );
            tokio::pin!(fill);
            loop {
                if ctx.check().is_err() {
                    break;
                }
                tokio::select! {
                    _ = &mut fill => break,
                    _ = ctx.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
                }
            }
        }
    }
    pub(crate) async fn read(
        &self,
        database: &DatabasePath,
        reference: &ObjectRef,
    ) -> Result<Arc<Vec<u8>>> {
        let key = cache_key(database, reference)?;
        if let Some(bytes) = self.cache.lock().expect("object cache poisoned").get(&key) {
            observability::cache("memory", "hit", Some(&key.0));
            return Ok(bytes);
        }
        // Striped fill locks bound coordination memory and coalesce identical misses.
        let fill = &self.fills[stripe(&key)];
        let _fill = match fill.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                observability::cache("memory", "waiting", Some(&key.0));
                fill.lock().await
            }
        };
        if let Some(bytes) = self.cache.lock().expect("object cache poisoned").get(&key) {
            observability::cache("memory", "coalesced", Some(&key.0));
            return Ok(bytes);
        }
        observability::cache("memory", "miss", Some(&key.0));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        if let Some(disk) = &self.disk {
            if let Some(bytes) = disk.read(&key, deadline).await {
                self.cache.lock().expect("object cache poisoned").insert(
                    key,
                    bytes.clone(),
                    bytes.len(),
                );
                return Ok(bytes);
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let bytes = self.backend.read_current(&key.0).await?.body;
        if !reference.matches(&bytes) {
            return Err(Error::Corrupt);
        }
        let bytes = Arc::new(bytes);
        if let Some(disk) = &self.disk {
            disk.write(&key, bytes.clone(), tokio::time::Instant::now() + remaining)
                .await;
        }
        self.cache.lock().expect("object cache poisoned").insert(
            key,
            Arc::clone(&bytes),
            bytes.len(),
        );
        Ok(bytes)
    }

    pub(crate) async fn read_index_node(
        &self,
        database: &DatabasePath,
        reference: &ObjectRef,
    ) -> Result<Arc<IndexNode>> {
        if reference.size > MAX_METADATA_BYTES as u64 {
            return Err(Error::Corrupt);
        }
        let key = cache_key(database, reference)?;
        if let Some(node) = self.nodes.lock().expect("node cache poisoned").get(&key) {
            observability::cache("node", "hit", Some(&key.0));
            return Ok(node);
        }
        // Separate locks: node fills may wait for body fills, never the reverse.
        let fill = &self.node_fills[stripe(&key)];
        let _fill = match fill.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                observability::cache("node", "waiting", Some(&key.0));
                fill.lock().await
            }
        };
        if let Some(node) = self.nodes.lock().expect("node cache poisoned").get(&key) {
            observability::cache("node", "coalesced", Some(&key.0));
            return Ok(node);
        }
        observability::cache("node", "miss", Some(&key.0));
        let bytes = self.read(database, reference).await?;
        #[cfg(test)]
        self.parses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let node: IndexNode = decode(bytes.as_slice())?;
        if !node.validate() {
            return Err(Error::Corrupt);
        }
        let weight = node_weight(&node);
        let node = Arc::new(node);
        self.nodes
            .lock()
            .expect("node cache poisoned")
            .insert(key, Arc::clone(&node), weight);
        Ok(node)
    }
}

#[cfg(test)]
mod tests;
