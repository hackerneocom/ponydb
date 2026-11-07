//! CLI-only import of a standalone SQLite backup into immutable database segments.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, Write},
    path::PathBuf,
};
use versioned_store::IndexWriteTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreStage {
    Copying,
    Validating,
    Hashing,
    Uploading,
    Building,
    Publishing,
    Complete,
}
#[derive(Clone, Copy, Debug)]
pub struct RestoreProgress {
    pub stage: RestoreStage,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}
pub type Progress = Arc<dyn Fn(RestoreProgress) + Send + Sync>;
fn report(progress: &Progress, stage: RestoreStage, completed_bytes: u64, total_bytes: u64) {
    if completed_bytes == 0 || stage == RestoreStage::Complete {
        observability::phase(match stage {
            RestoreStage::Copying => "restore_copying",
            RestoreStage::Validating => "restore_validating",
            RestoreStage::Hashing => "restore_hashing",
            RestoreStage::Uploading => "restore_uploading",
            RestoreStage::Building => "restore_building",
            RestoreStage::Publishing => "restore_publishing",
            RestoreStage::Complete => "restore_complete",
        });
    }
    progress(RestoreProgress {
        stage,
        completed_bytes,
        total_bytes,
    });
}
struct PreparedRestore {
    directory: tempfile::TempDir,
    total: u64,
    page_size: usize,
    sha256: String,
}
fn copy_backup(
    source: &mut impl Read,
    file: &mut impl Write,
    total: u64,
    ctx: &RequestContext,
    progress: &Progress,
) -> Result<()> {
    let mut buffer = [0; 65536];
    let mut copied = 0u64;
    report(progress, RestoreStage::Copying, 0, total);
    loop {
        check(ctx)?;
        let n = source
            .read(&mut buffer)
            .map_err(|_| error("INVALID_REQUEST", "Cannot read backup file"))?;
        if n == 0 {
            break;
        }
        copied = copied
            .checked_add(n as u64)
            .filter(|n| *n <= total)
            .ok_or_else(|| error("INVALID_REQUEST", "Backup file changed while copying"))?;
        file.write_all(&buffer[..n]).map_err(|_| {
            error(
                "RESOURCE_LIMIT",
                "Cannot copy backup; check temporary disk space",
            )
        })?;
        report(progress, RestoreStage::Copying, copied, total);
    }
    if copied != total {
        return Err(error(
            "INVALID_REQUEST",
            "Backup file changed while copying",
        ));
    }
    file.flush()
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot flush restore temporary file"))?;
    Ok(())
}
fn prepare(path: PathBuf, ctx: &RequestContext, progress: &Progress) -> Result<PreparedRestore> {
    check(ctx)?;
    if path.as_os_str() == "-" {
        return Err(error(
            "INVALID_REQUEST",
            "Restore requires a local SQLite backup file",
        ));
    }
    let mut source = std::fs::File::open(path)
        .map_err(|_| error("INVALID_REQUEST", "Cannot open backup file"))?;
    let metadata = source
        .metadata()
        .map_err(|_| error("INVALID_REQUEST", "Cannot stat backup file"))?;
    if !metadata.is_file() || metadata.len() > i64::MAX as u64 {
        return Err(error("INVALID_REQUEST", "Invalid SQLite backup file"));
    }
    let total = metadata.len();
    // SQLite may create WAL/SHM sidecars even for a read-only connection.
    // Own the whole workspace so those files are also removed on every exit.
    let directory = tempfile::tempdir().map_err(|_| {
        error(
            "RESOURCE_LIMIT",
            "Cannot create restore temporary directory",
        )
    })?;
    let backup = directory.path().join("backup.sqlite");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&backup)
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot create restore temporary file"))?;
    copy_backup(&mut source, &mut file, total, ctx, progress)?;
    let mut buffer = [0; 65536];
    check(ctx)?;
    report(progress, RestoreStage::Validating, 0, total);
    let page_size = validate_file(&backup, ctx);
    check(ctx)?;
    let page_size = page_size?;
    report(progress, RestoreStage::Hashing, 0, total);
    file.rewind()
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot rewind restore temporary file"))?;
    let mut hash = Sha256::new();
    let mut hashed = 0;
    loop {
        check(ctx)?;
        let n = file
            .read(&mut buffer)
            .map_err(|_| error("RESOURCE_LIMIT", "Cannot read restore temporary file"))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        hashed += n as u64;
        report(progress, RestoreStage::Hashing, hashed, total);
    }
    file.rewind()
        .map_err(|_| error("RESOURCE_LIMIT", "Cannot rewind restore temporary file"))?;
    Ok(PreparedRestore {
        directory,
        total,
        page_size,
        sha256: format!("{:x}", hash.finalize()),
    })
}
impl<S: BlobStore + 'static> Service<S> {
    /// Import a local backup. Only final HEAD publication changes database visibility.
    /// The caller cancels the context and awaits this future to join local work safely.
    pub async fn restore_file(
        &self,
        principal: &Principal,
        database: &str,
        path: PathBuf,
        ctx: RequestContext,
        progress: Progress,
    ) -> Result<Value> {
        let mut logs = observability::Request::new("restore_file", Some(database));
        let result = logs
            .scope(self.restore_file_inner(principal, database, path, ctx, progress))
            .await;
        log_result(&mut logs, &result);
        result
    }
    async fn restore_file_inner(
        &self,
        principal: &Principal,
        database: &str,
        path: PathBuf,
        ctx: RequestContext,
        progress: Progress,
    ) -> Result<Value> {
        let _guard = CallGuard(ctx.clone());
        check(&ctx)?;
        require(principal, database, "db:restore")?;
        // Restore reads only the mutable HEAD, then builds a new incarnation.
        // A snapshot reader lease would incorrectly cap a long local import and
        // prevent confirming publication after that lease expires.
        let db = self.db(database)?.without_read_lease();
        let worker_ctx = ctx.clone();
        let worker_progress = progress.clone();
        let captured = observability::capture();
        let prepared = tokio::task::spawn_blocking(move || {
            captured.run(|| prepare(path, &worker_ctx, &worker_progress))
        })
        .await
        .map_err(|_| error("RESOURCE_LIMIT", "Restore preparation worker failed"))??;
        check(&ctx)?;
        let PreparedRestore {
            directory,
            total,
            page_size,
            sha256,
        } = prepared;
        let mut input = tokio::fs::File::open(directory.path().join("backup.sqlite"))
            .await
            .map_err(|_| error("RESOURCE_LIMIT", "Cannot read restore temporary file"))?;
        let build = async {
            use tokio::io::AsyncReadExt;
            let old = self.creation_base(&db).await?;
            check(&ctx)?;
            let epoch = old.as_ref().map_or(Ok(1), |h| {
                h.head
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| error("RESOURCE_LIMIT", "Epoch exhausted"))
            })?;
            let inc = unique();
            let mut manifest = Manifest {
                version: 1,
                page_size,
                logical_size: total,
                segment_size: self.options.segment_size,
                generation_id: unique(),
                base_root: None,
                delta_root: None,
                overlay_bytes: 0,
                overlay_transactions: 0,
            };
            report(&progress, RestoreStage::Uploading, 0, total);
            let mut updates = std::collections::BTreeMap::new();
            let mut uploaded = 0;
            for segment in 0..total.div_ceil(manifest.segment_size as u64) {
                check(&ctx)?;
                let size = (total - uploaded).min(manifest.segment_size as u64) as usize;
                let mut bytes = vec![0; size];
                input
                    .read_exact(&mut bytes)
                    .await
                    .map_err(|_| error("RESOURCE_LIMIT", "Cannot read restore temporary file"))?;
                let object = db
                    .put_bytes(&inc, epoch, "segments", bytes, &ctx)
                    .await
                    .map_err(store_error)?;
                updates.insert(
                    format!("{segment:016x}"),
                    Some(serde_json::to_value(object).unwrap()),
                );
                uploaded += size as u64;
                report(&progress, RestoreStage::Uploading, uploaded, total);
                // Bound the in-memory index update batch for arbitrarily large backups.
                if updates.len() == 1024 {
                    manifest.base_root = db
                        .index_update(
                            manifest.base_root,
                            std::mem::take(&mut updates),
                            IndexWriteTarget {
                                incarnation: &inc,
                                epoch,
                                category: "segment-index",
                            },
                            &ctx,
                        )
                        .await
                        .map_err(store_error)?;
                }
            }
            report(&progress, RestoreStage::Building, 0, total);
            manifest.base_root = db
                .index_update(
                    manifest.base_root,
                    updates,
                    IndexWriteTarget {
                        incarnation: &inc,
                        epoch,
                        category: "segment-index",
                    },
                    &ctx,
                )
                .await
                .map_err(store_error)?;
            let manifest_ref = db
                .put(&inc, epoch, "manifests", &manifest, &ctx)
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
                name: database.to_owned(),
                description: String::new(),
                metadata: json!({}),
                status: "active".into(),
                manifest_ref,
                snapshot_index_root: None,
                snapshot_retention_days: self.options.retention_days,
                snapshot_count: 0,
                safety_policy: SafetyPolicy {
                    max_read_ms: self.options.max_read_ms,
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
            self.record_snapshot(&db, &mut head, &ctx).await?;
            Ok::<_, ApiError>((old, head))
        };
        let (old, head) = tokio::select! {
            result = build => result?,
            _ = ctx.cancelled() => return Err(error("DEADLINE_EXCEEDED", "Restore cancelled")),
        };
        check(&ctx)?;
        report(&progress, RestoreStage::Publishing, 0, total);
        // Await the CAS and its result confirmation even if cancellation arrives now.
        db.publish_head(old.as_ref(), head, &ctx)
            .await
            .map_err(store_error)?;
        report(&progress, RestoreStage::Complete, total, total);
        Ok(
            json!({"committed":true,"outcome":"committed","name":database,"data_version":"1",
            "total_bytes":total.to_string(),"sha256":sha256,"page_size":page_size}),
        )
    }
}

fn validate_file(path: &std::path::Path, ctx: &RequestContext) -> Result<usize> {
    let mut f = std::fs::File::open(path)
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Cannot open source"))?;
    let mut header = [0; 100];
    f.read_exact(&mut header)
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Truncated header"))?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(error("INVALID_SQLITE_BACKUP", "Unsupported SQLite header"));
    }
    // The remote VFS uses rollback journals and cannot open WAL-format pages.
    // Reject before storage access rather than publishing an unreadable database.
    if header[18] != 1 || header[19] != 1 {
        return Err(error("INVALID_SQLITE_BACKUP",
            "Restore requires a standalone SQLite backup in DELETE journal mode; WAL-mode files are not supported"));
    }
    let encoded = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded == 1 {
        65536
    } else {
        encoded as usize
    };
    let len = f
        .metadata()
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Cannot stat source"))?
        .len();
    if len > i64::MAX as u64
        || !(512..=65536).contains(&page_size)
        || !page_size.is_power_of_two()
        || !len.is_multiple_of(page_size as u64)
    {
        return Err(error(
            "INVALID_SQLITE_BACKUP",
            "Invalid page size or file length",
        ));
    }
    let mut c = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| error("INVALID_SQLITE_BACKUP", "SQLite open failed"))?;
    c.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA mmap_size=0;")
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Validation setup failed"))?;
    let guard = ctx.clone();
    c.progress_handler(1000, Some(move || guard.check().is_err()))
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Validation setup failed"))?;
    let integrity: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Integrity scan failed"))?;
    if integrity != "ok" {
        return Err(error("INVALID_SQLITE_BACKUP", "Integrity check failed"));
    }
    if c.prepare("PRAGMA foreign_key_check")
        .and_then(|mut s| s.query([])?.next().map(|r| r.is_some()))
        .map_err(|_| error("INVALID_SQLITE_BACKUP", "Foreign key scan failed"))?
    {
        return Err(error("INVALID_SQLITE_BACKUP", "Foreign key violations"));
    }
    let mut s = c
        .prepare(
            "SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .map_err(|_| error("UNSUPPORTED_SQLITE_SCHEMA", "Cannot read schema"))?;
    let tables = s
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|_| error("UNSUPPORTED_SQLITE_SCHEMA", "Cannot scan schema"))?;
    for table in tables {
        check(ctx)?;
        let (name, sql) =
            table.map_err(|_| error("UNSUPPORTED_SQLITE_SCHEMA", "Invalid schema"))?;
        if sql.to_uppercase().contains("CREATE VIRTUAL TABLE") {
            return Err(error(
                "UNSUPPORTED_SQLITE_SCHEMA",
                "Virtual tables are not supported in restores",
            ));
        }
        c.prepare(&format!(
            "SELECT * FROM \"{}\" LIMIT 0",
            name.replace('"', "\"\"")
        ))
        .map_err(|_| {
            error(
                "UNSUPPORTED_SQLITE_SCHEMA",
                "Schema requires unsupported functions or collations",
            )
        })?;
    }
    check(ctx)?;
    Ok(page_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_backup_length_is_rejected_without_copying_excess_bytes() {
        for total in [5, 7] {
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed = events.clone();
            let progress: Progress = Arc::new(move |event| observed.lock().unwrap().push(event));
            let mut output = Vec::new();
            let err = copy_backup(
                &mut &b"backup"[..],
                &mut output,
                total,
                &RequestContext::unbounded(),
                &progress,
            )
            .unwrap_err();
            assert_eq!(err.code, "INVALID_REQUEST");
            assert_eq!(err.message, "Backup file changed while copying");
            assert!(output.len() as u64 <= total);
            assert!(events.lock().unwrap().iter().all(|event| {
                event.stage == RestoreStage::Copying && event.completed_bytes < total
            }));
        }
    }

    #[test]
    fn cancellation_during_copy_stops_before_reading_the_next_chunk() {
        let ctx = RequestContext::unbounded();
        let cancel = ctx.clone();
        let progress: Progress = Arc::new(move |event| {
            if event.completed_bytes > 0 {
                cancel.cancel();
            }
        });
        let bytes = vec![7; 65537];
        let mut source = std::io::Cursor::new(&bytes);
        let mut output = Vec::new();
        let err = copy_backup(
            &mut source,
            &mut output,
            bytes.len() as u64,
            &ctx,
            &progress,
        )
        .unwrap_err();
        assert_eq!(err.code, "DEADLINE_EXCEEDED");
        assert_eq!(source.position(), 65536);
        assert_eq!(output, bytes[..65536]);
    }

    struct FullDisk;
    impl Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "injected full disk",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn insufficient_disk_is_a_clear_error_without_completing_the_copy() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let progress: Progress = Arc::new(move |event| observed.lock().unwrap().push(event));
        let err = copy_backup(
            &mut &b"backup"[..],
            &mut FullDisk,
            6,
            &RequestContext::unbounded(),
            &progress,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESOURCE_LIMIT");
        assert!(err.message.contains("disk space"));
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|event| event.completed_bytes == 0));
    }
}
