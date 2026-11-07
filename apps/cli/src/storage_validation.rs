//! Bounded live checks of the storage contract, isolated from database objects.
use futures_util::future::join_all;
use object_store_api::*;
use std::{collections::BTreeSet, time::Duration};

const CLEANUP_BUDGET: Duration = Duration::from_secs(30);
const RACE_ROUNDS: usize = 4;
const MAX_PROBE_BYTES: u64 = 2 * 1024 * 1024;

struct Probe<'a, S> {
    store: &'a S,
    prefix: String,
    touched: BTreeSet<String>,
    step: &'static str,
    uncertain: bool,
}

/// Uses the configured credentials and prefix. Never deletes keys obtained from LIST.
/// A successful finite probe is evidence of the tested behavior, not a proof for all races.
pub async fn validate(store: &impl BlobStore, budget: Duration) -> Result<(), String> {
    let mut probe = Probe {
        store,
        prefix: format!("_ponydb-validation/{}/", uuid::Uuid::new_v4().simple()),
        touched: BTreeSet::new(),
        step: "declared storage capabilities",
        uncertain: false,
    };
    let result = match tokio::time::timeout(budget, probe.run()).await {
        Ok(result) => result,
        Err(_) => {
            // Dropping an SDK future cannot retract a request already accepted remotely.
            probe.uncertain |= !probe.touched.is_empty();
            Err(format!("timed out after {} ms", budget.as_millis()))
        }
    };
    let mut failures = match result {
        Ok(()) => Vec::new(),
        Err(reason) => vec![format!("Storage check '{}' failed: {reason}", probe.step)],
    };
    if !probe.touched.is_empty() {
        match tokio::time::timeout(CLEANUP_BUDGET, probe.cleanup()).await {
            Ok(errors) => failures.extend(errors),
            Err(_) => failures.push("Probe cleanup timed out after 30 seconds".into()),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        if failures.len() > 1 || probe.step == "delete and verify cleanup" || probe.uncertain {
            failures.push(format!(
                "Inspect probe prefix '{}' relative to storage.prefix for residual objects{}",
                probe.prefix,
                if probe.uncertain {
                    "; an uncertain or timed-out PUT may still finish later"
                } else {
                    ""
                }
            ));
        }
        Err(failures.join("; "))
    }
}

fn remote(error: StoreError) -> String {
    match error {
        StoreError::Remote {
            operation,
            status,
            code,
            message,
        } => {
            let code = code
                .filter(|s| !s.is_empty())
                .map(|s| format!(", {s}"))
                .unwrap_or_default();
            let message = message
                .filter(|s| !s.is_empty())
                .map(|s| format!(": {s}"))
                .unwrap_or_default();
            format!("S3 {operation} failed (HTTP {status}{code}){message}")
        }
        error => error.to_string(),
    }
}

fn outcome(outcome: &CasOutcome) -> String {
    match outcome {
        CasOutcome::Applied { .. } => "write was applied".into(),
        CasOutcome::PreconditionFailed => "precondition failed".into(),
        CasOutcome::NotApplied { reason, .. } => remote(reason.clone()),
        CasOutcome::Unknown => {
            "write outcome is unknown; storage capabilities could not be verified".into()
        }
    }
}

impl<S: BlobStore> Probe<'_, S> {
    async fn write(
        &mut self,
        key: &str,
        body: &[u8],
        revision: Option<&ObjectRevision>,
    ) -> CasOutcome {
        let fresh = self.touched.insert(key.to_owned());
        let result = match revision {
            Some(revision) => {
                self.store
                    .compare_and_swap(key, revision, body.to_vec())
                    .await
            }
            None => self.store.create_if_absent(key, body.to_vec()).await,
        };
        if fresh
            && matches!(
                result,
                CasOutcome::NotApplied { .. } | CasOutcome::PreconditionFailed
            )
        {
            self.touched.remove(key);
        }
        self.uncertain |= matches!(result, CasOutcome::Unknown);
        result
    }

    async fn read(&self, key: &str, expected: &[u8]) -> Result<ObjectRevision, String> {
        let object = self.store.read_current(key).await.map_err(remote)?;
        if object.body != expected || object.attributes.size != expected.len() as u64 {
            return Err("immediate GET returned stale, truncated, or different bytes/size".into());
        }
        Ok(object.revision)
    }

    async fn race(
        &mut self,
        key: &str,
        revision: Option<&ObjectRevision>,
    ) -> Result<Vec<u8>, String> {
        self.touched.insert(key.to_owned());
        // CAS candidates must both differ from the base: an identical S3 body
        // can retain its ETag and would legitimately allow a second CAS.
        let (left, right) = if revision.is_some() {
            (b"cas-left-value".to_vec(), b"cas-right-data".to_vec())
        } else {
            (b"create-left-value".to_vec(), b"create-right-data".to_vec())
        };
        let (a, b) = match revision {
            Some(revision) => tokio::join!(
                self.store.compare_and_swap(key, revision, left.clone()),
                self.store.compare_and_swap(key, revision, right.clone()),
            ),
            None => tokio::join!(
                self.store.create_if_absent(key, left.clone()),
                self.store.create_if_absent(key, right.clone()),
            ),
        };
        self.uncertain |= matches!(a, CasOutcome::Unknown) || matches!(b, CasOutcome::Unknown);
        match (&a, &b) {
            (CasOutcome::Applied { .. }, CasOutcome::PreconditionFailed) => Ok(left),
            (CasOutcome::PreconditionFailed, CasOutcome::Applied { .. }) => Ok(right),
            _ => Err(format!(
                "expected exactly one applied write and one precondition failure; left: {}; right: {}",
                outcome(&a), outcome(&b),
            )),
        }
    }

    async fn listing(&self, delimiter: Option<String>) -> Result<(), String> {
        let mut expected_objects = BTreeSet::new();
        let mut expected_prefixes = BTreeSet::new();
        for key in &self.touched {
            if delimiter.is_some() {
                if let Some(pos) = key[self.prefix.len()..].find('/') {
                    expected_prefixes.insert(key[..self.prefix.len() + pos + 1].to_owned());
                    continue;
                }
            }
            expected_objects.insert(key.clone());
        }
        let mut objects = BTreeSet::new();
        let mut prefixes = BTreeSet::new();
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        // One key per page forces actual continuation handling, with a strict page bound.
        for _ in 0..=self.touched.len() {
            let page = self
                .store
                .list(ListRequest {
                    prefix: self.prefix.clone(),
                    delimiter: delimiter.clone(),
                    cursor,
                    limit: 1,
                })
                .await
                .map_err(remote)?;
            if page.objects.len() + page.common_prefixes.len() > 1 {
                return Err("LIST ignored max-keys=1".into());
            }
            for key in page.objects {
                if !expected_objects.contains(&key) || !objects.insert(key) {
                    return Err(
                        "LIST returned an unexpected, duplicate, or out-of-prefix object".into(),
                    );
                }
            }
            for prefix in page.common_prefixes {
                if !expected_prefixes.contains(&prefix) || !prefixes.insert(prefix) {
                    return Err("LIST returned an unexpected or duplicate common prefix".into());
                }
            }
            cursor = page.next_cursor;
            if let Some(token) = &cursor {
                if !cursors.insert(token.clone()) {
                    return Err("LIST repeated its continuation cursor".into());
                }
            } else {
                return if objects == expected_objects && prefixes == expected_prefixes {
                    Ok(())
                } else {
                    Err(
                        "immediate paginated LIST omitted uploaded objects or common prefixes"
                            .into(),
                    )
                };
            }
        }
        Err("LIST did not terminate within the probe's page limit".into())
    }

    async fn run(&mut self) -> Result<(), String> {
        self.store.capabilities().validate().map_err(remote)?;
        self.step = "LIST access and isolated prefix";
        let page = self
            .store
            .list(ListRequest {
                prefix: self.prefix.clone(),
                delimiter: None,
                cursor: None,
                limit: 1,
            })
            .await
            .map_err(remote)?;
        if !page.objects.is_empty()
            || !page.common_prefixes.is_empty()
            || page.next_cursor.is_some()
        {
            return Err("new random probe prefix is not empty; refusing to modify it".into());
        }
        let key = format!("{}object", self.prefix);
        self.step = "conditional create (PutObject If-None-Match: *)";
        let result = self.write(&key, b"initial-value", None).await;
        if !matches!(result, CasOutcome::Applied { .. }) {
            return Err(outcome(&result));
        }
        self.step = "strong GET, SHA-256 metadata, and integrity after create";
        let old = self.read(&key, b"initial-value").await?;
        self.step = "duplicate create must reject If-None-Match: *";
        let result = self.write(&key, b"must-not-overwrite", None).await;
        if !matches!(result, CasOutcome::PreconditionFailed) {
            return Err(format!(
                "expected precondition failure, but {}; conditional create may be ignored",
                outcome(&result)
            ));
        }
        self.read(&key, b"initial-value").await?;
        self.step = "matching CAS (PutObject If-Match)";
        let result = self.write(&key, b"updated-value", Some(&old)).await;
        if !matches!(result, CasOutcome::Applied { .. }) {
            return Err(outcome(&result));
        }
        self.read(&key, b"updated-value").await?;
        self.step = "stale CAS must reject If-Match";
        let result = self.write(&key, b"must-not-overwrite", Some(&old)).await;
        if !matches!(result, CasOutcome::PreconditionFailed) {
            return Err(format!(
                "expected precondition failure, but {}; conditional CAS may be ignored",
                outcome(&result)
            ));
        }
        self.read(&key, b"updated-value").await?;
        for round in 0..RACE_ROUNDS {
            let key = format!("{}race/{round}", self.prefix);
            self.step = "concurrent conditional create (If-None-Match: *)";
            let winner = self.race(&key, None).await?;
            let old = self.read(&key, &winner).await?;
            self.step = "concurrent CAS from the same revision (If-Match)";
            let winner = self.race(&key, Some(&old)).await?;
            self.read(&key, &winner).await?;
        }
        self.step = "Range GET";
        let range = self.store.read_range(&key, 1..8).await.map_err(remote)?;
        if range != b"pdated-" {
            return Err("Range GET returned incorrect bytes or ignored the requested range".into());
        }
        self.step = "probe PUT/GET (up to 2 MiB) and immediate integrity check";
        let body = vec![
            0x5a;
            self.store
                .capabilities()
                .max_single_put_bytes
                .min(MAX_PROBE_BYTES) as usize
        ];
        let large = format!("{}payload", self.prefix);
        let result = self.write(&large, &body, None).await;
        if !matches!(result, CasOutcome::Applied { .. }) {
            return Err(outcome(&result));
        }
        self.read(&large, &body).await?;
        self.step = "nested object creation for delimiter listing";
        for suffix in ["nested/a", "nested/b"] {
            let key = format!("{}{suffix}", self.prefix);
            let result = self.write(&key, b"nested-probe", None).await;
            if !matches!(result, CasOutcome::Applied { .. }) {
                return Err(outcome(&result));
            }
        }
        self.step = "strong paginated LIST (ListObjectsV2)";
        self.listing(None).await?;
        self.step = "paginated LIST with delimiter '/'";
        self.listing(Some("/".into())).await?;
        self.step = "delete and verify cleanup";
        Ok(())
    }

    async fn cleanup(&self) -> Vec<String> {
        let results = join_all(self.touched.iter().map(|key| async move {
            match self.store.delete(key).await {
                DeleteOutcome::Deleted | DeleteOutcome::Missing => {}
                DeleteOutcome::NotApplied(reason) => {
                    return Some(format!("DELETE probe '{key}' failed: {}", remote(reason)))
                }
                DeleteOutcome::Unknown => {
                    return Some(format!("DELETE probe '{key}' outcome is unknown"))
                }
            }
            match self.store.read_current(key).await {
                Err(StoreError::NotFound) => None,
                Err(reason) => Some(format!(
                    "GET after DELETE probe '{key}' failed: {}",
                    remote(reason)
                )),
                Ok(_) => Some(format!(
                    "DELETE probe '{key}' did not make the object immediately absent"
                )),
            }
        }))
        .await;
        let mut errors: Vec<_> = results.into_iter().flatten().collect();
        match self
            .store
            .list(ListRequest {
                prefix: self.prefix.clone(),
                delimiter: None,
                cursor: None,
                limit: 1,
            })
            .await
        {
            Ok(page)
                if page.objects.is_empty()
                    && page.common_prefixes.is_empty()
                    && page.next_cursor.is_none() => {}
            Ok(_) => errors.push("Probe prefix is not empty after cleanup".into()),
            Err(reason) => errors.push(format!(
                "LIST after probe cleanup failed: {}",
                remote(reason)
            )),
        }
        errors
    }
}
