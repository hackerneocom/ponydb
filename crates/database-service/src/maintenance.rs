use super::*;
use std::collections::{BTreeMap, BTreeSet};
use versioned_store::IndexWriteTarget;
fn refs(value: &Value, out: &mut Vec<ObjectRef>) -> Result<()> {
    match value {
        Value::Object(map) => {
            if map.contains_key("key") && map.contains_key("size") && map.contains_key("sha256") {
                let r: ObjectRef = serde_json::from_value(value.clone())
                    .map_err(|_| error("STORAGE_CORRUPT", "Invalid graph reference"))?;
                if !r.validate() {
                    return Err(error("STORAGE_CORRUPT", "Unsafe graph reference"));
                }
                out.push(r);
            } else {
                for value in map.values() {
                    refs(value, out)?;
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                refs(value, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}
impl<S: BlobStore + 'static> Service<S> {
    pub(super) async fn compact(
        &self,
        p: &Principal,
        i: Compact,
        ctx: &RequestContext,
    ) -> Result<Value> {
        require(p, &i.database, "db:maintenance")?;
        if i.max_segments == 0 || i.max_segments > 32 || i.budget_ms == 0 || i.budget_ms > 60000 {
            return Err(error("INVALID_REQUEST", "Invalid compaction budget"));
        }
        observability::phase("compact_planning");
        let started = std::time::Instant::now();
        let db = self.db(&i.database)?;
        let base = db.read_head().await.map_err(store_error)?;
        self.active(&base.head)?;
        let mut manifest = db.manifest(&base.head).await.map_err(store_error)?;
        let source = db.snapshot(
            manifest.clone(),
            ctx.with_max_duration(Duration::from_millis(base.head.safety_policy.max_read_ms)),
        );
        let entries = db
            .index_list(manifest.delta_root.clone(), None, 1000)
            .await
            .map_err(store_error)?;
        let mut segments = BTreeSet::new();
        for (key, _) in entries {
            let page = u64::from_str_radix(&key, 16)
                .map_err(|_| error("STORAGE_CORRUPT", "Invalid page number"))?;
            segments.insert(page * manifest.page_size as u64 / manifest.segment_size as u64);
            if segments.len() >= i.max_segments {
                break;
            }
        }
        observability::phase("compact_building");
        let mut processed = 0;
        let mut base_updates = BTreeMap::new();
        let mut delta_updates = BTreeMap::new();
        for segment in segments {
            check(ctx)?;
            if started.elapsed().as_millis() >= i.budget_ms as u128 {
                break;
            }
            let start = segment * manifest.segment_size as u64;
            // Truncated physical entries can be discarded without reading beyond EOF.
            let size = manifest
                .logical_size
                .saturating_sub(start)
                .min(manifest.segment_size as u64) as usize;
            if size > 0 {
                let bytes = source.read(start, size).await.map_err(store_error)?;
                let object = db
                    .put_bytes(
                        &base.head.incarnation_id,
                        base.head.epoch,
                        "segments",
                        bytes,
                        ctx,
                    )
                    .await
                    .map_err(store_error)?;
                base_updates.insert(
                    format!("{segment:016x}"),
                    Some(serde_json::to_value(object).unwrap()),
                );
            }
            let first = start / manifest.page_size as u64;
            for page in first..first + manifest.segment_size as u64 / manifest.page_size as u64 {
                let key = format!("{page:016x}");
                if db
                    .index_get(manifest.delta_root.clone(), &key)
                    .await
                    .map_err(store_error)?
                    .is_some()
                {
                    delta_updates.insert(key, None);
                    manifest.overlay_bytes = manifest
                        .overlay_bytes
                        .saturating_sub(manifest.page_size as u64);
                }
            }
            processed += 1;
        }
        manifest.base_root = db
            .index_update(
                manifest.base_root,
                base_updates,
                IndexWriteTarget {
                    incarnation: &base.head.incarnation_id,
                    epoch: base.head.epoch,
                    category: "segment-index",
                },
                ctx,
            )
            .await
            .map_err(store_error)?;
        manifest.delta_root = db
            .index_update(
                manifest.delta_root,
                delta_updates,
                IndexWriteTarget {
                    incarnation: &base.head.incarnation_id,
                    epoch: base.head.epoch,
                    category: "delta-index",
                },
                ctx,
            )
            .await
            .map_err(store_error)?;
        if manifest.delta_root.is_none() {
            manifest.overlay_bytes = 0;
            manifest.overlay_transactions = 0;
        }
        manifest.generation_id = unique();
        let complete = manifest.delta_root.is_none();
        let mut next = base.head.clone();
        next.manifest_ref = db
            .put(
                &next.incarnation_id,
                next.epoch,
                "manifests",
                &manifest,
                ctx,
            )
            .await
            .map_err(store_error)?;
        // Same logical snapshot, new physical representation; old readers are protected by GC drain.
        let sid = format!("{}:{}", next.incarnation_id, next.data_version);
        let mut history = self.history(&db, &next, &sid).await?;
        history.manifest = next.manifest_ref.clone();
        next.snapshot_index_root = Some(
            db.index_insert(
                next.snapshot_index_root.clone(),
                &digest(sid.as_bytes()),
                serde_json::to_value(history).unwrap(),
                IndexWriteTarget {
                    incarnation: &next.incarnation_id,
                    epoch: next.epoch,
                    category: "snapshots",
                },
                ctx,
            )
            .await
            .map_err(store_error)?,
        );
        let response = json!({"outcome":if complete{"applied"}else{"partial"},"complete":complete,"database_available":true,"segments_processed":processed,"data_version":next.data_version.to_string()});
        db.publish_head(Some(&base), next, ctx)
            .await
            .map_err(store_error)?;
        Ok(response)
    }
    pub(super) async fn gc(&self, p: &Principal, i: Gc, ctx: &RequestContext) -> Result<Value> {
        require(p, &i.database, "db:maintenance")?;
        let (budget, max_objects) = self.options.gc_limits(&i)?;
        let db = self.db(&i.database)?.without_read_lease();
        let ctx = ctx.clone();
        // Dropping the caller cancels its context, but must not abort cleanup.
        let stop = self.gc_stop.clone();
        let captured = observability::capture();
        self.gc_tasks
            .spawn(captured.scope(async move {
                let work = run_gc(db, budget, max_objects, ctx.clone());
                tokio::pin!(work);
                tokio::select! {
                    result = &mut work => result,
                    _ = stop.cancelled() => { ctx.cancel(); work.await },
                }
            }))
            .await
            .map_err(|e| error("INTERNAL", &e.to_string()))?
    }
}

impl ServiceOptions {
    pub fn gc_limits(&self, input: &Gc) -> Result<(u64, usize)> {
        let budget = input.budget_ms.unwrap_or(self.gc_default_budget_ms);
        let objects = input.max_objects.unwrap_or(self.gc_max_objects);
        if budget == 0
            || budget > self.max_request_ms
            || objects == 0
            || objects > self.gc_max_objects
        {
            return Err(error("INVALID_REQUEST", "GC budget must be positive and within requests.max_timeout_ms; max_objects must be positive and within maintenance.gc_max_objects_per_call"));
        }
        Ok((budget, objects))
    }
}

fn local_error(e: impl std::fmt::Display) -> ApiError {
    error("RESOURCE_LIMIT", &format!("GC temporary index: {e}"))
}
fn gc_result(complete: bool, phase: &str, deleted: usize, available: bool) -> Value {
    json!({"outcome":if complete {"applied"} else {"partial"}, "complete":complete,
        "database_available":available, "phase":phase, "deleted_objects":deleted})
}
fn owns(head: &Head, incarnation: &str, owner: &GcState) -> bool {
    head.incarnation_id == incarnation && head.gc.as_ref() == Some(owner)
}
async fn backoff(attempt: u32, ctx: &RequestContext) -> Result<()> {
    check(ctx)?;
    let cap = 10u64.saturating_mul(1u64 << attempt.min(5)).min(250);
    let jitter = (uuid::Uuid::new_v4().as_u128() % (cap as u128 + 1)) as u64;
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(jitter)) => check(ctx),
        _ = ctx.cancelled() => check(ctx),
    }
}

/// Cleanup changes only the ownership record on the latest HEAD. Never republish
/// roots captured at claim time, even when the original request has expired.
async fn clear_gc<S: BlobStore + 'static>(
    db: &DatabaseStore<S>,
    incarnation: &str,
    owner: &GcState,
) -> Result<bool> {
    let cleanup = RequestContext::new(Duration::from_secs(30));
    let work = async {
        let mut attempt = 0;
        loop {
            check(&cleanup)?;
            let base = db.read_head().await.map_err(store_error)?;
            let available = base.head.status == "active";
            if !owns(&base.head, incarnation, owner) {
                return Ok(available);
            }
            let mut next = base.head.clone();
            next.gc = None;
            match db.publish_head(Some(&base), next, &cleanup).await {
                Ok(()) => return Ok(available),
                Err(StoreFailure::Conflict | StoreFailure::Unknown) => {
                    // A lost response is confirmed by absence of our ownership,
                    // including a successor's record, not by a stale HEAD nonce.
                    backoff(attempt, &cleanup).await?;
                    attempt += 1;
                }
                Err(e) => return Err(store_error(e)),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .map_err(|_| error("DEADLINE_EXCEEDED", "GC ownership cleanup timed out"))?
}

/// Read-only I/O can be abandoned on deadline; admitted mutations cannot.
async fn gc_read<T>(
    ctx: &RequestContext,
    work: impl Future<Output = std::result::Result<T, StoreFailure>>,
) -> Result<T> {
    tokio::pin!(work);
    loop {
        check(ctx)?;
        tokio::select! {
            result = &mut work => { check(ctx)?; return result.map_err(store_error); },
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            _ = ctx.cancelled() => { check(ctx)?; },
        }
    }
}

async fn run_gc<S: BlobStore + 'static>(
    db: DatabaseStore<S>,
    budget: u64,
    max_objects: usize,
    ctx: RequestContext,
) -> Result<Value> {
    let started = std::time::Instant::now();
    let exhausted = || started.elapsed() >= Duration::from_millis(budget);
    observability::phase("gc_claiming");
    let owner_id = unique();
    let mut attempt = 0;
    // Refresh both roots and epoch on every definite CAS failure.
    let claimed = loop {
        check(&ctx)?;
        let base = gc_read(&ctx, db.read_head()).await?;
        if exhausted() {
            return Ok(gc_result(
                false,
                "claiming",
                0,
                base.head.status == "active",
            ));
        }
        let mut next = base.head.clone();
        next.epoch = next
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("RESOURCE_LIMIT", "Epoch exhausted"))?;
        next.gc = Some(GcState {
            owner_id: owner_id.clone(),
            cutoff_epoch: next.epoch,
        });
        // Observe retirement only after reading a published HEAD. Unix time is
        // independent of the host timezone; clock accuracy is a deployment duty.
        let observed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|v| u64::try_from(v.as_millis()).ok())
            .ok_or_else(|| error("INVALID_REQUEST", "Invalid system clock"))?;
        let cutoff = observed.checked_sub(u64::from(next.snapshot_retention_days) * 86_400_000);
        let mut updates = BTreeMap::new();
        let mut cursor = None;
        let mut count = 0u64;
        let mut current_found = false;
        loop {
            check(&ctx)?;
            if exhausted() {
                return Ok(gc_result(
                    false,
                    "claiming",
                    0,
                    base.head.status == "active",
                ));
            }
            let entries = gc_read(
                &ctx,
                db.index_list(
                    base.head.snapshot_index_root.clone(),
                    cursor.as_deref(),
                    1000,
                ),
            )
            .await?;
            if entries.is_empty() {
                break;
            }
            cursor = entries.last().map(|(k, _)| k.clone());
            for (key, value) in entries {
                let mut h: History = serde_json::from_value(value)
                    .map_err(|_| error("STORAGE_CORRUPT", "Invalid snapshot"))?;
                if h.incarnation_id != next.incarnation_id
                    || h.data_version > next.data_version
                    || h.snapshot_id != format!("{}:{}", h.incarnation_id, h.data_version)
                    || key != digest(h.snapshot_id.as_bytes())
                {
                    return Err(error("STORAGE_CORRUPT", "Invalid snapshot identity"));
                }
                count += 1;
                if h.data_version == next.data_version {
                    current_found = true;
                    continue;
                }
                if expired(next.snapshot_retention_days, cutoff, h.retired_at_upper_ms) {
                    updates.insert(key, None);
                    next.snapshot_count = next
                        .snapshot_count
                        .checked_sub(1)
                        .ok_or_else(|| error("STORAGE_CORRUPT", "Invalid snapshot count"))?;
                } else if h.retired_at_upper_ms.is_none() {
                    h.retired_at_upper_ms = Some(observed);
                    updates.insert(key, Some(serde_json::to_value(h).unwrap()));
                }
            }
        }
        if count != base.head.snapshot_count || (count > 0 && !current_found) {
            return Err(error(
                "STORAGE_CORRUPT",
                "Invalid snapshot count or current snapshot",
            ));
        }
        if !updates.is_empty() {
            next.snapshot_index_root = db
                .index_update(
                    next.snapshot_index_root.clone(),
                    updates,
                    IndexWriteTarget {
                        incarnation: &next.incarnation_id,
                        epoch: next.epoch,
                        category: "snapshots",
                    },
                    &ctx,
                )
                .await
                .map_err(store_error)?;
        }
        if exhausted() {
            return Ok(gc_result(
                false,
                "claiming",
                0,
                base.head.status == "active",
            ));
        }
        match db.publish_head(Some(&base), next.clone(), &ctx).await {
            Ok(()) => break next,
            Err(StoreFailure::Conflict) => {
                backoff(attempt, &ctx).await?;
                attempt += 1;
            }
            Err(StoreFailure::Unknown) => {
                // Confirmation and cleanup survive cancellation after an admitted CAS.
                let confirm = RequestContext::new(Duration::from_secs(30));
                let current = match gc_read(&confirm, db.read_head()).await {
                    Ok(current) => current,
                    Err(failure) => {
                        let cleanup =
                            clear_gc(&db, &next.incarnation_id, next.gc.as_ref().unwrap()).await;
                        let mut unknown = error(
                            "OUTCOME_UNKNOWN",
                            &format!(
                                "GC claim confirmation failed: {}; confirmed deleted_objects=0",
                                failure.message
                            ),
                        );
                        if let Err(cleanup) = cleanup {
                            unknown
                                .message
                                .push_str(&format!("; cleanup: {}", cleanup.message));
                        }
                        return Err(unknown);
                    }
                };
                if owns(
                    &current.head,
                    &next.incarnation_id,
                    next.gc.as_ref().unwrap(),
                ) {
                    break next;
                }
                if current.head.epoch >= next.epoch {
                    return Ok(gc_result(
                        false,
                        "superseded",
                        0,
                        current.head.status == "active",
                    ));
                }
                // Never retry an ambiguous claim: a successor might already own it.
                return Err(error(
                    "OUTCOME_UNKNOWN",
                    "GC claim could not be confirmed; confirmed deleted_objects=0",
                ));
            }
            Err(e) => return Err(store_error(e)),
        }
    };
    let claimed_at = std::time::Instant::now();
    let owner = claimed.gc.as_ref().unwrap();
    let mut deleted = 0;
    observability::phase("gc_marking");
    let mut phase = "marking";
    let work: Result<bool> = async {
        let directory = tempfile::tempdir().map_err(local_error)?;
        let index = rusqlite::Connection::open(directory.path().join("gc.sqlite")).map_err(local_error)?;
        index.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
            CREATE TABLE seen(key TEXT PRIMARY KEY);
            CREATE TABLE frontier(key TEXT PRIMARY KEY, reference TEXT NOT NULL);
            CREATE TABLE candidates(key TEXT PRIMARY KEY);").map_err(local_error)?;
        for root in std::iter::once(&claimed.manifest_ref).chain(claimed.snapshot_index_root.iter()) {
            index.execute("INSERT OR IGNORE INTO frontier VALUES (?1,?2)", rusqlite::params![root.key, serde_json::to_string(root).map_err(local_error)?]).map_err(local_error)?;
        }
        // Ownership is checked once at entry to each phase, never per DELETE.
        if !owns(&gc_read(&ctx, db.read_head()).await?.head, &claimed.incarnation_id, owner) { phase = "superseded"; return Ok(false); }
        loop {
            tokio::task::yield_now().await;
            check(&ctx)?;
            if exhausted() { return Ok(false); }
            let reference: Option<String> = {
                use rusqlite::OptionalExtension;
                index.query_row("SELECT reference FROM frontier LIMIT 1", [], |r| r.get(0)).optional().map_err(local_error)?
            };
            let Some(reference) = reference else { break; };
            let reference: ObjectRef = serde_json::from_str(&reference).map_err(local_error)?;
            index.execute("DELETE FROM frontier WHERE key=?1", [&reference.key]).map_err(local_error)?;
            if index.execute("INSERT OR IGNORE INTO seen VALUES (?1)", [&reference.key]).map_err(local_error)? == 0 { continue; }
            // Binary leaf contents do not contain graph edges.
            let relative = reference.relative_key().map_err(|_| error("STORAGE_CORRUPT", "Invalid generation object key"))?;
            let object = relative.object_parts().ok_or_else(|| error("STORAGE_CORRUPT", "Invalid generation object key"))?;
            if matches!(object.category, "segments" | "deltas") { continue; }
            let bytes = match gc_read(&ctx, db.get_bytes(&reference)).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    if !owns(&gc_read(&ctx, db.read_head()).await?.head, &claimed.incarnation_id, owner) { phase = "superseded"; return Ok(false); }
                    return Err(e);
                }
            };
            let value: Value = versioned_store::decode(bytes.as_slice()).map_err(store_error)?;
            let mut children = Vec::new();
            refs(&value, &mut children)?;
            for child in children {
                index.execute("INSERT OR IGNORE INTO frontier SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM seen WHERE key=?1)", rusqlite::params![child.key, serde_json::to_string(&child).map_err(local_error)?]).map_err(local_error)?;
            }
        }
        observability::phase("gc_listing");
        phase = "listing";
        if !owns(&gc_read(&ctx, db.read_head()).await?.head, &claimed.incarnation_id, owner) { phase = "superseded"; return Ok(false); }
        let mut cursor = None;
        loop {
            tokio::task::yield_now().await;
            check(&ctx)?;
            if exhausted() { return Ok(false); }
            let page = gc_read(&ctx, async { db.backend.list(ListRequest {prefix: db.paths().incarnations_prefix(), delimiter: None, cursor, limit: 1000}).await.map_err(StoreFailure::Store) }).await?;
            for physical in page.objects {
                let key = db.paths().relative_key(&physical).map_err(|_| error("STORAGE_CORRUPT", "GC key escaped database"))?;
                let object = key.object_parts().ok_or_else(|| error("STORAGE_CORRUPT", "Invalid generation object key"))?;
                if object.epoch < owner.cutoff_epoch {
                    index.execute("INSERT OR IGNORE INTO candidates SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM seen WHERE key=?1)", [key.as_str()]).map_err(local_error)?;
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() { break; }
        }
        let has_candidates: bool = index.query_row("SELECT EXISTS(SELECT 1 FROM candidates)", [], |r| r.get(0)).map_err(local_error)?;
        if !has_candidates {
            if !owns(&gc_read(&ctx, db.read_head()).await?.head, &claimed.incarnation_id, owner) { phase = "superseded"; return Ok(false); }
            phase = "complete";
            return Ok(true);
        }
        observability::phase("gc_draining");
        phase = "draining";
        while claimed_at.elapsed() < Duration::from_millis(claimed.safety_policy.drain_ms) {
            check(&ctx)?;
            if exhausted() { return Ok(false); }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                _ = ctx.cancelled() => { check(&ctx)?; },
            }
        }
        observability::phase("gc_sweeping");
        phase = "sweeping";
        if !owns(&gc_read(&ctx, db.read_head()).await?.head, &claimed.incarnation_id, owner) { phase = "superseded"; return Ok(false); }
        let mut attempts = 0;
        loop {
            tokio::task::yield_now().await;
            check(&ctx)?;
            let key: Option<String> = {
                use rusqlite::OptionalExtension;
                index.query_row("SELECT key FROM candidates LIMIT 1", [], |r| r.get(0)).optional().map_err(local_error)?
            };
            let Some(key) = key else { phase = "complete"; return Ok(true); };
            if exhausted() || attempts == max_objects { return Ok(false); }
            attempts += 1;
            // This admitted DELETE is awaited even if the request is cancelled.
            let relative = storage_namespace::RelativeKey::new(&key).map_err(|_| error("STORAGE_CORRUPT", "Invalid GC candidate key"))?;
            match db.backend.delete(&db.physical(&relative)).await {
                DeleteOutcome::Deleted => deleted += 1,
                DeleteOutcome::Missing => {},
                DeleteOutcome::Unknown => return Err(error("OUTCOME_UNKNOWN", "GC DELETE outcome is unknown")),
                DeleteOutcome::NotApplied(e) => return Err(store_error(StoreFailure::Store(e))),
            }
            index.execute("DELETE FROM candidates WHERE key=?1", [&key]).map_err(local_error)?;
        }
    }.await;
    observability::phase("gc_cleanup");
    let cleanup = clear_gc(&db, &claimed.incarnation_id, owner).await;
    match (work, cleanup) {
        (Ok(complete), Ok(available)) => Ok(gc_result(complete, phase, deleted, available)),
        (Err(mut e), cleanup) => {
            if e.outcome != "unknown" && deleted > 0 {
                e.outcome = "partial".into();
            }
            e.message
                .push_str(&format!("; confirmed deleted_objects={deleted}"));
            if let Err(cleanup) = cleanup {
                e.message
                    .push_str(&format!("; cleanup: {}", cleanup.message));
            }
            Err(e)
        }
        (Ok(_), Err(mut e)) => {
            if deleted > 0 {
                e.outcome = "partial".into();
            }
            e.message
                .push_str(&format!("; confirmed deleted_objects={deleted}"));
            Err(e)
        }
    }
}

fn expired(days: u32, cutoff: Option<u64>, retired: Option<u64>) -> bool {
    days == 0
        || cutoff
            .zip(retired)
            .is_some_and(|(cutoff, retired)| retired < cutoff)
}

#[cfg(test)]
mod retention_tests {
    use super::expired;
    #[test]
    fn retirement_boundary_and_missing_observation() {
        assert!(!expired(7, Some(100), None));
        assert!(!expired(7, Some(100), Some(100)));
        assert!(!expired(7, Some(100), Some(101)));
        assert!(expired(7, Some(100), Some(99)));
        assert!(!expired(7, None, Some(0)));
        assert!(expired(0, None, None));
    }
}
