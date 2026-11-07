//! Materialize a current or retained logical version without publishing any objects.
use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug, Serialize)]
pub struct ExportResult {
    pub snapshot_id: String,
    pub data_version: String,
    pub total_bytes: String,
    pub sha256: String,
}

impl<S: BlobStore + 'static> Service<S> {
    /// Pins the active database's current HEAD and writes its standalone SQLite image.
    /// The caller owns the temporary destination and publishes only on success.
    pub async fn export_database<W: AsyncWrite + Unpin>(
        &self,
        principal: &Principal,
        database: &str,
        ctx: RequestContext,
        writer: &mut W,
    ) -> Result<ExportResult> {
        self.export_version(principal, database, None, ctx, writer)
            .await
    }

    /// Writes to a caller-owned temporary destination. The caller publishes only on success.
    pub async fn export_snapshot<W: AsyncWrite + Unpin>(
        &self,
        principal: &Principal,
        input: SnapshotExport,
        ctx: RequestContext,
        writer: &mut W,
    ) -> Result<ExportResult> {
        self.export_version(
            principal,
            &input.database,
            Some(&input.snapshot_id),
            ctx,
            writer,
        )
        .await
    }

    async fn export_version<W: AsyncWrite + Unpin>(
        &self,
        principal: &Principal,
        database: &str,
        snapshot_id: Option<&str>,
        ctx: RequestContext,
        writer: &mut W,
    ) -> Result<ExportResult> {
        let operation = if snapshot_id.is_some() {
            "snapshot_export"
        } else {
            "database_export"
        };
        let mut logs = observability::Request::new(operation, Some(database));
        let result = logs
            .scope(self.export_version_inner(principal, database, snapshot_id, ctx, writer))
            .await;
        match &result {
            Ok(value) => {
                if let Ok(version) = value.data_version.parse() {
                    logs.set_data_version(version);
                }
                logs.finish("success", "");
            }
            Err(e) => logs.finish(observability::error_outcome(&e.code, &e.outcome), &e.code),
        }
        result
    }
    async fn export_version_inner<W: AsyncWrite + Unpin>(
        &self,
        principal: &Principal,
        database: &str,
        snapshot_id: Option<&str>,
        ctx: RequestContext,
        writer: &mut W,
    ) -> Result<ExportResult> {
        observability::phase("export_resolving_snapshot");
        let _guard = CallGuard(ctx.clone());
        check(&ctx)?;
        require(principal, database, "db:backup")?;
        if snapshot_id == Some("") {
            return Err(error("INVALID_REQUEST", "snapshot_id must not be empty"));
        }
        let db = self.db(database)?;
        let head = db.read_head();
        tokio::pin!(head);
        let base = loop {
            tokio::select! {
                result = &mut head => break result.map_err(store_error)?,
                _ = ctx.cancelled() => return Err(error("DEADLINE_EXCEEDED", "Export cancelled")),
                _ = tokio::time::sleep(Duration::from_millis(10)) => check(&ctx)?,
            }
        };
        let read_ctx =
            ctx.with_max_duration(Duration::from_millis(base.head.safety_policy.max_read_ms));
        let work = async {
            check(&read_ctx)?;
            let (snapshot_id, data_version, manifest_ref) = match snapshot_id {
                Some(id) => {
                    let history = self.history(&db, &base.head, id).await?;
                    (history.snapshot_id, history.data_version, history.manifest)
                }
                None => {
                    self.active(&base.head)?;
                    (
                        format!("{}:{}", base.head.incarnation_id, base.head.data_version),
                        base.head.data_version,
                        base.head.manifest_ref.clone(),
                    )
                }
            };
            let manifest: Manifest = db.get(&manifest_ref).await.map_err(store_error)?;
            if !manifest.validate() {
                return Err(error("STORAGE_CORRUPT", "Invalid snapshot manifest"));
            }
            if manifest.logical_size > self.options.export_max_bytes {
                return Err(error(
                    "RESOURCE_LIMIT",
                    "Snapshot exceeds export file size limit",
                ));
            }
            let total = manifest.logical_size;
            let snapshot = db.snapshot(manifest, read_ctx.clone());
            observability::phase("export_materializing");
            let mut hash = Sha256::new();
            let mut offset = 0;
            while offset < total {
                check(&read_ctx)?;
                let len = (total - offset).min(1024 * 1024) as usize;
                let bytes = snapshot.read(offset, len).await.map_err(store_error)?;
                writer
                    .write_all(&bytes)
                    .await
                    .map_err(|_| error("RESOURCE_LIMIT", "Cannot write export file"))?;
                hash.update(&bytes);
                offset += len as u64;
            }
            observability::phase("export_flushing");
            writer
                .flush()
                .await
                .map_err(|_| error("RESOURCE_LIMIT", "Cannot flush export file"))?;
            check(&read_ctx)?;
            Ok(ExportResult {
                snapshot_id,
                data_version: data_version.to_string(),
                total_bytes: total.to_string(),
                sha256: format!("{:x}", hash.finalize()),
            })
        };
        // Also stop stalled storage and local writes, which may not check the context themselves.
        tokio::pin!(work);
        loop {
            tokio::select! {
                result = &mut work => return result,
                _ = ctx.cancelled() => return Err(error("DEADLINE_EXCEEDED", "Export cancelled")),
                _ = tokio::time::sleep(Duration::from_millis(10)) => check(&read_ctx)?,
            }
        }
    }
}
