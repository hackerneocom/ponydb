//! Immutable objects, compressed COW indices and the sole HEAD publication point.
use object_store_api::*;
use remote_pager::{bridge::AsyncSnapshot, FrozenPages};
use request_context::RequestContext;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use storage_format::*;
use storage_namespace::{
    object_key, DatabaseName, DatabasePath, IncarnationId, RelativeKey, StorageNamespace,
};
use thiserror::Error;
mod cache;
mod disk_cache;
pub use cache::ObjectReader;
pub use disk_cache::DiskCacheOptions;
pub mod index;
pub use index::IndexWriteTarget;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Only a missing HEAD means the database itself was not found.
    #[error("database '{0}' not found")]
    DatabaseNotFound(String),
    /// A validated reference points to an object missing from the backend.
    #[error("database '{0}' references a missing storage object")]
    MissingObject(String),
    #[error("database '{database}' HEAD validation failed: {reason}")]
    InvalidHead { database: String, reason: String },
    #[error("invalid or corrupt storage format")]
    Corrupt,
    #[error("unsupported storage layout; layout version {LAYOUT_VERSION} is required")]
    UnsupportedLayout,
    #[error("transaction conflict")]
    Conflict,
    #[error("write outcome unknown")]
    Unknown,
    #[error("database deleted")]
    Deleted,
    #[error("database is in maintenance")]
    Maintenance,
    #[error("resource budget exceeded")]
    Limit,
    #[error(transparent)]
    Stopped(#[from] request_context::Stop),
}
pub type Result<T> = std::result::Result<T, Error>;
pub struct DatabaseStore<S: BlobStore> {
    pub backend: Arc<S>,
    paths: DatabasePath,
    reader: Arc<ObjectReader<S>>,
    read_lease: Option<Arc<std::sync::Mutex<RequestContext>>>,
}
impl<S: BlobStore> Clone for DatabaseStore<S> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            paths: self.paths.clone(),
            reader: self.reader.clone(),
            read_lease: self.read_lease.clone(),
        }
    }
}
#[derive(Clone)]
pub struct HeadView {
    pub etag: String,
    pub head: Head,
    pub revision: ObjectRevision,
}
pub fn unique() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
impl<S: BlobStore + 'static> DatabaseStore<S> {
    pub fn new(backend: Arc<S>, database_name: &str) -> Result<Self> {
        Self::with_reader(Arc::new(ObjectReader::new(backend)), database_name)
    }
    pub fn with_reader(reader: Arc<ObjectReader<S>>, database_name: &str) -> Result<Self> {
        let name = DatabaseName::new(database_name).map_err(|_| Error::Corrupt)?;
        let backend = reader.backend.clone();
        backend.capabilities().validate()?;
        Ok(Self {
            backend,
            reader,
            read_lease: None,
            // The backend adapter alone adds the configured deployment prefix.
            paths: StorageNamespace::default().database(name),
        })
    }
    /// Anchor before fetching HEAD; tightening never restarts the clock.
    pub fn with_read_lease(mut self) -> Self {
        self.read_lease = Some(Arc::new(std::sync::Mutex::new(RequestContext::new(
            std::time::Duration::from_secs(60),
        ))));
        self
    }
    pub fn without_read_lease(mut self) -> Self {
        self.read_lease = None;
        self
    }
    async fn guarded<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        let Some(lease) = &self.read_lease else {
            return future.await;
        };
        let ctx = lease.lock().map_err(|_| Error::Corrupt)?.clone();
        ctx.check()?;
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => { ctx.check()?; return result; },
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => { ctx.check()?; },
                _ = ctx.cancelled() => { ctx.check()?; },
            }
        }
    }
    pub fn paths(&self) -> &DatabasePath {
        &self.paths
    }
    pub fn physical(&self, key: &RelativeKey) -> String {
        self.paths.resolve(key)
    }
    pub async fn read_head(&self) -> Result<HeadView> {
        let view = self.guarded(self.read_head_inner()).await?;
        if let Some(lease) = &self.read_lease {
            let mut ctx = lease.lock().map_err(|_| Error::Corrupt)?;
            *ctx = ctx.with_max_duration(std::time::Duration::from_millis(
                view.head.safety_policy.max_read_ms,
            ));
            ctx.check()?;
        }
        Ok(view)
    }
    async fn read_head_inner(&self) -> Result<HeadView> {
        let object = self
            .backend
            .read_current(&self.paths.head_key())
            .await
            .map_err(|e| match e {
                StoreError::NotFound => Error::DatabaseNotFound(self.paths.name().as_str().into()),
                other => Error::Store(other),
            })?;
        let invalid = |reason: &str| Error::InvalidHead {
            database: self.paths.name().as_str().into(),
            reason: reason.into(),
        };
        if object.body.len() > MAX_METADATA_BYTES {
            return Err(invalid("metadata exceeds the size limit"));
        }
        let raw: Value =
            serde_json::from_slice(&object.body).map_err(|_| invalid("invalid JSON"))?;
        if raw.get("storage_layout_version").and_then(Value::as_u64) != Some(LAYOUT_VERSION as u64)
        {
            return Err(Error::UnsupportedLayout);
        }
        let head: Head =
            serde_json::from_value(raw).map_err(|e| invalid(&head_schema_diagnostic(&e)))?;
        if !head.validate(self.paths.name().as_str()) {
            return Err(invalid("metadata values violate HEAD constraints"));
        }
        Ok(HeadView {
            etag: object.etag,
            head,
            revision: object.revision,
        })
    }
    /// Verified immutable bytes shared with the cache and other readers.
    /// Eviction releases the cache's reference; callers may keep the bytes alive.
    pub async fn get_bytes(&self, reference: &ObjectRef) -> Result<Arc<Vec<u8>>> {
        self.guarded(self.get_bytes_inner(reference)).await
    }
    async fn get_bytes_inner(&self, reference: &ObjectRef) -> Result<Arc<Vec<u8>>> {
        if !reference.validate()
            || reference.size > self.backend.capabilities().max_single_put_bytes
        {
            return Err(Error::Corrupt);
        }
        self.reader
            .read(&self.paths, reference)
            .await
            .map_err(|e| match e {
                Error::Store(StoreError::NotFound) => {
                    Error::MissingObject(self.paths.name().as_str().into())
                }
                other => other,
            })
    }
    async fn get_index_node(&self, reference: &ObjectRef) -> Result<Arc<IndexNode>> {
        self.guarded(async {
            if !reference.validate()
                || reference.size > MAX_METADATA_BYTES as u64
                || reference.size > self.backend.capabilities().max_single_put_bytes
            {
                return Err(Error::Corrupt);
            }
            self.reader
                .read_index_node(&self.paths, reference)
                .await
                .map_err(|e| match e {
                    Error::Store(StoreError::NotFound) => {
                        Error::MissingObject(self.paths.name().as_str().into())
                    }
                    other => other,
                })
        })
        .await
    }
    pub async fn get<T: DeserializeOwned>(&self, reference: &ObjectRef) -> Result<T> {
        if reference.size > MAX_METADATA_BYTES as u64 {
            return Err(Error::Corrupt);
        }
        let bytes = self.get_bytes(reference).await?;
        decode(bytes.as_slice())
    }
    pub async fn put_bytes(
        &self,
        inc: &str,
        epoch: u64,
        category: &str,
        bytes: Vec<u8>,
        ctx: &RequestContext,
    ) -> Result<ObjectRef> {
        ctx.check()?;
        let incarnation = IncarnationId::new(inc).map_err(|_| Error::Corrupt)?;
        let category = RelativeKey::new(category).map_err(|_| Error::Corrupt)?;
        let key =
            object_key(&incarnation, epoch, &category, &unique()).map_err(|_| Error::Corrupt)?;
        let physical = self.physical(&key);
        let reference = ObjectRef {
            key: key.into_string(),
            size: bytes.len() as u64,
            sha256: digest(&bytes),
        };
        if reference.size > self.backend.capabilities().max_single_put_bytes {
            return Err(Error::Limit);
        }
        match self
            .backend
            .create_if_absent(&physical, bytes.clone())
            .await
        {
            CasOutcome::Applied { .. } => {}
            CasOutcome::Unknown => {
                if !self
                    .backend
                    .read_current(&physical)
                    .await
                    .is_ok_and(|o| o.body == bytes)
                {
                    return Err(Error::Store(StoreError::Transport(
                        "dependency upload unconfirmed".into(),
                    )));
                }
            }
            CasOutcome::NotApplied { reason, .. } => return Err(Error::Store(reason)),
            CasOutcome::PreconditionFailed => return Err(Error::Corrupt),
        }
        // Only known metadata categories are admitted on writes. Data packs and
        // custom categories remain demand-loaded so bulk uploads preserve hot reads.
        if reference.size <= MAX_METADATA_BYTES as u64
            && matches!(
                category.as_str(),
                "manifests" | "segment-index" | "delta-index" | "snapshots"
            )
        {
            self.reader
                .admit_written(&self.paths, &reference, bytes, ctx)
                .await;
        }
        Ok(reference)
    }
    pub async fn put<T: Serialize>(
        &self,
        inc: &str,
        epoch: u64,
        category: &str,
        value: &T,
        ctx: &RequestContext,
    ) -> Result<ObjectRef> {
        let bytes = serde_json::to_vec(value).map_err(|_| Error::Corrupt)?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(Error::Limit);
        }
        self.put_bytes(inc, epoch, category, bytes, ctx).await
    }
    pub async fn manifest(&self, head: &Head) -> Result<Manifest> {
        let m: Manifest = self.get(&head.manifest_ref).await?;
        if !m.validate() {
            return Err(Error::Corrupt);
        }
        Ok(m)
    }
    /// Publish a candidate without recording or replaying a business request.
    /// A lost CAS response can only be confirmed while this exact HEAD is current.
    pub async fn publish_head(
        &self,
        base: Option<&HeadView>,
        head: Head,
        ctx: &RequestContext,
    ) -> Result<()> {
        observability::phase("publishing_head");
        let started = std::time::Instant::now();
        let result = self.publish_head_inner(base, head, ctx).await;
        tracing::debug!(target: "ponydb", event = "head_publication_completed",
            outcome = match &result { Ok(_) => "committed", Err(Error::Conflict) => "conflict", Err(Error::Unknown) => "unknown", Err(_) => "failed" },
            elapsed_ms = started.elapsed().as_millis() as u64);
        result
    }
    async fn publish_head_inner(
        &self,
        base: Option<&HeadView>,
        mut head: Head,
        ctx: &RequestContext,
    ) -> Result<()> {
        head.head_nonce = unique();
        head.updated_at_ms = now_ms();
        if let Some(base) = base {
            head.revision = base.head.revision.checked_add(1).ok_or(Error::Limit)?;
        }
        if !head.validate(self.paths.name().as_str()) {
            return Err(Error::Corrupt);
        }
        let bytes = serde_json::to_vec(&head).map_err(|_| Error::Corrupt)?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(Error::Limit);
        }
        ctx.check()?;
        let physical = self.paths.head_key();
        let outcome = match base {
            Some(base) => {
                self.backend
                    .compare_and_swap(&physical, &base.revision, bytes)
                    .await
            }
            None => self.backend.create_if_absent(&physical, bytes).await,
        };
        match outcome {
            CasOutcome::Applied { .. } => Ok(()),
            CasOutcome::NotApplied { reason, .. } => Err(Error::Store(reason)),
            CasOutcome::PreconditionFailed => Err(Error::Conflict),
            CasOutcome::Unknown => {
                observability::phase("confirming_publication");
                match self.read_head().await {
                    Ok(current) if current.head.head_nonce == head.head_nonce => Ok(()),
                    _ => Err(Error::Unknown),
                }
            }
        }
    }
    pub async fn apply_pages(
        &self,
        inc: &str,
        epoch: u64,
        mut manifest: Manifest,
        pages: FrozenPages,
        ctx: &RequestContext,
    ) -> Result<ObjectRef> {
        if pages.page_size != manifest.page_size
            || !pages.final_file_size.is_multiple_of(pages.page_size as u64)
        {
            return Err(Error::Corrupt);
        }
        let page_size = pages.page_size;
        let per_pack = (256 * 1024 / page_size).max(1);
        let entries: Vec<_> = pages.pages.into_iter().collect();
        let mut updates = BTreeMap::new();
        for pack in entries.chunks(per_pack) {
            ctx.check()?;
            let mut bytes = Vec::with_capacity(pack.len() * page_size);
            for (number, page) in pack {
                if page.len() != page_size
                    || number
                        .checked_mul(page_size as u64)
                        .is_none_or(|start| start >= pages.final_file_size)
                {
                    return Err(Error::Corrupt);
                }
                bytes.extend_from_slice(page);
            }
            let object = self.put_bytes(inc, epoch, "deltas", bytes, ctx).await?;
            for (i, (n, page)) in pack.iter().enumerate() {
                let location = PageLocation {
                    object: object.clone(),
                    offset: (i * page_size) as u64,
                    sha256: digest(page),
                };
                updates.insert(
                    format!("{n:016x}"),
                    Some(serde_json::to_value(location).map_err(|_| Error::Corrupt)?),
                );
            }
            manifest.overlay_bytes = manifest
                .overlay_bytes
                .checked_add(object.size)
                .ok_or(Error::Limit)?;
        }
        manifest.delta_root = self
            .index_update(
                manifest.delta_root,
                updates,
                IndexWriteTarget {
                    incarnation: inc,
                    epoch,
                    category: "delta-index",
                },
                ctx,
            )
            .await?;
        manifest.logical_size = pages.final_file_size;
        manifest.overlay_transactions += 1;
        self.put(inc, epoch, "manifests", &manifest, ctx).await
    }
    pub fn snapshot(&self, manifest: Manifest, ctx: RequestContext) -> Arc<StoreSnapshot<S>> {
        Arc::new(StoreSnapshot {
            store: self.clone(),
            manifest,
            ctx,
        })
    }
}
// Serde's type/value errors can contain stored values. Only retain a bounded
// field identifier from its schema diagnostics; never expose the raw error.
fn head_schema_diagnostic(error: &serde_json::Error) -> String {
    let message = error.to_string();
    for kind in ["unknown", "missing"] {
        if let Some(rest) = message.strip_prefix(&format!("{kind} field `")) {
            let field = if kind == "unknown" {
                rest.rsplit_once("`, expected ").map(|(field, _)| field)
            } else {
                rest.strip_suffix('`')
            };
            if let Some(field) = field {
                let field = if !field.is_empty()
                    && field.len() <= 64
                    && field
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    field
                } else {
                    "<redacted>"
                };
                return format!(
                    "{kind} field '{field}'; stored HEAD may be incompatible with this version"
                );
            }
        }
    }
    "schema mismatch: invalid field type or value".into()
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(Error::Corrupt);
    }
    serde_json::from_slice(bytes).map_err(|_| Error::Corrupt)
}
pub struct StoreSnapshot<S: BlobStore> {
    store: DatabaseStore<S>,
    pub manifest: Manifest,
    ctx: RequestContext,
}
impl<S: BlobStore + 'static> StoreSnapshot<S> {
    async fn object(&self, r: &ObjectRef) -> Result<Arc<Vec<u8>>> {
        self.ctx.check()?;
        let bytes = self.store.get_bytes(r).await?;
        self.ctx.check()?;
        Ok(bytes)
    }
    async fn page(&self, n: u64) -> Result<Vec<u8>> {
        let mut io = observability::PageIo::new("snapshot", "read", n);
        let result = self.page_inner(n).await;
        match &result {
            Ok(bytes) => io.finish("success", bytes.len() as u64),
            Err(_) => io.finish("failed", 0),
        }
        result
    }
    async fn page_inner(&self, n: u64) -> Result<Vec<u8>> {
        let page_size = self.manifest.page_size;
        if let Some(value) = self
            .store
            .index_get(self.manifest.delta_root.clone(), &format!("{n:016x}"))
            .await?
        {
            let location: PageLocation =
                serde_json::from_value(value).map_err(|_| Error::Corrupt)?;
            let bytes = self.object(&location.object).await?;
            let start = usize::try_from(location.offset).map_err(|_| Error::Corrupt)?;
            let end = start.checked_add(page_size).ok_or(Error::Corrupt)?;
            let page = bytes.get(start..end).ok_or(Error::Corrupt)?;
            if digest(page) != location.sha256 {
                return Err(Error::Corrupt);
            }
            return Ok(page.to_vec());
        }
        let offset = n.checked_mul(page_size as u64).ok_or(Error::Corrupt)?;
        let segment = offset / self.manifest.segment_size as u64;
        let value = self
            .store
            .index_get(self.manifest.base_root.clone(), &format!("{segment:016x}"))
            .await?
            .ok_or(Error::Corrupt)?;
        let reference: ObjectRef = serde_json::from_value(value).map_err(|_| Error::Corrupt)?;
        let bytes = self.object(&reference).await?;
        let start = (offset % self.manifest.segment_size as u64) as usize;
        Ok(bytes
            .get(start..start + page_size)
            .ok_or(Error::Corrupt)?
            .to_vec())
    }
    /// Read a verified range while preserving typed storage errors.
    pub async fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.ctx.check()?;
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.manifest.logical_size)
            || len > 1024 * 1024
        {
            return Err(Error::Corrupt);
        }
        let mut out = Vec::with_capacity(len);
        let mut pos = offset;
        let page_size = self.manifest.page_size;
        while out.len() < len {
            let page = self.page(pos / page_size as u64).await?;
            let within = (pos % page_size as u64) as usize;
            let n = (len - out.len()).min(page_size - within);
            out.extend_from_slice(&page[within..within + n]);
            pos += n as u64;
            self.ctx.check()?;
        }
        Ok(out)
    }
}
impl<S: BlobStore + 'static> AsyncSnapshot for StoreSnapshot<S> {
    fn len(&self) -> u64 {
        self.manifest.logical_size
    }
    fn read_at(
        &self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = remote_pager::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async move {
            self.read(offset, len)
                .await
                .map_err(|e| remote_pager::Error::Io(e.to_string()))
        })
    }
}
