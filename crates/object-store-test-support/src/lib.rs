//! Contract fixtures only; this store is not a persistence backend.
use object_store_api::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};
struct Entry {
    body: Vec<u8>,
    revision: u64,
}
#[derive(Default)]
struct State {
    objects: BTreeMap<String, Entry>,
    next: u64,
}
pub struct MemoryStore {
    scope: RevisionScope,
    state: Mutex<State>,
    pub lose_next_response: AtomicBool,
}
impl Default for MemoryStore {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            scope: RevisionScope::new(
                "composite-test",
                NEXT.fetch_add(1, Ordering::Relaxed).to_string(),
            ),
            state: Mutex::default(),
            lose_next_response: AtomicBool::new(false),
        }
    }
}
impl MemoryStore {
    fn revision(&self, key: &str, n: u64) -> ObjectRevision {
        self.scope.issue(key, (n, 17u64))
    }
    fn put(&self, key: &str, expected: Option<&ObjectRevision>, body: Vec<u8>) -> CasOutcome {
        let mut s = self.state.lock().unwrap();
        let allowed = match expected {
            None => !s.objects.contains_key(key),
            Some(token) => match self.scope.decode::<(u64, u64)>(key, token) {
                Ok(&(n, 17)) => s.objects.get(key).is_some_and(|e| e.revision == n),
                _ => {
                    return CasOutcome::NotApplied {
                        reason: StoreError::InvalidInput,
                        retryable: false,
                    }
                }
            },
        };
        if !allowed {
            return CasOutcome::PreconditionFailed;
        }
        s.next += 1;
        let revision = s.next;
        s.objects.insert(key.into(), Entry { body, revision });
        if self.lose_next_response.swap(false, Ordering::SeqCst) {
            CasOutcome::Unknown
        } else {
            CasOutcome::Applied {
                new_revision: Some(self.revision(key, revision)),
            }
        }
    }
}
impl BlobStore for MemoryStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            strong_read: true,
            atomic_create: true,
            atomic_cas: true,
            range_read: true,
            paginated_list: true,
            per_object_delete: true,
            max_single_put_bytes: 1024 * 1024,
        }
    }
    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        let s = self.state.lock().unwrap();
        let e = s.objects.get(key).ok_or(StoreError::NotFound)?;
        Ok(VersionedObject {
            etag: format!("\"{}\"", e.revision),
            body: e.body.clone(),
            revision: self.revision(key, e.revision),
            attributes: ObjectAttributes {
                size: e.body.len() as u64,
            },
        })
    }
    async fn read_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        let s = self.state.lock().unwrap();
        let e = s.objects.get(key).ok_or(StoreError::NotFound)?;
        if range.start >= range.end || range.end > e.body.len() as u64 {
            return Err(StoreError::InvalidInput);
        }
        Ok(e.body[range.start as usize..range.end as usize].to_vec())
    }
    async fn create_if_absent(&self, key: &str, body: Vec<u8>) -> CasOutcome {
        self.put(key, None, body)
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> CasOutcome {
        self.put(key, Some(expected), body)
    }
    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        if request.limit == 0 || request.delimiter.as_deref().is_some_and(|d| d != "/") {
            return Err(StoreError::InvalidInput);
        }
        let s = self.state.lock().unwrap();
        let mut entries = BTreeSet::new();
        for key in s.objects.keys().filter(|k| k.starts_with(&request.prefix)) {
            let entry = if request.delimiter.is_some() {
                match key[request.prefix.len()..].find('/') {
                    Some(pos) => (key[..request.prefix.len() + pos + 1].to_owned(), true),
                    None => (key.clone(), false),
                }
            } else {
                (key.clone(), false)
            };
            entries.insert(entry);
        }
        let start = request
            .cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| StoreError::InvalidInput)?;
        let next_cursor =
            (entries.len() > start + request.limit).then(|| (start + request.limit).to_string());
        let mut objects = vec![];
        let mut common_prefixes = vec![];
        for (k, prefix) in entries.into_iter().skip(start).take(request.limit) {
            if prefix {
                common_prefixes.push(k)
            } else {
                objects.push(k)
            }
        }
        Ok(ObjectPage {
            objects,
            common_prefixes,
            next_cursor,
        })
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        if self.state.lock().unwrap().objects.remove(key).is_some() {
            DeleteOutcome::Deleted
        } else {
            DeleteOutcome::Missing
        }
    }
}
/// Uses only the protocol API, including concurrent contenders and stale tokens.
/// Call with an isolated test namespace. All keys are deleted at successful completion.
pub async fn contract(store: &impl BlobStore) {
    store.capabilities().validate().unwrap();
    for i in 0..16 {
        let key = format!("race/{i}");
        let (a, b) = tokio::join!(
            store.create_if_absent(&key, b"left".to_vec()),
            store.create_if_absent(&key, b"right".to_vec())
        );
        assert_eq!(
            [a, b]
                .iter()
                .filter(|x| matches!(x, CasOutcome::Applied { .. }))
                .count(),
            1
        );
        let old = store.read_current(&key).await.unwrap();
        assert!([b"left".as_slice(), b"right".as_slice()].contains(&old.body.as_slice()));
        let (a, b) = tokio::join!(
            store.compare_and_swap(&key, &old.revision, b"alpha".to_vec()),
            store.compare_and_swap(&key, &old.revision, b"bravo".to_vec())
        );
        assert_eq!(
            [a, b]
                .iter()
                .filter(|x| matches!(x, CasOutcome::Applied { .. }))
                .count(),
            1
        );
        let current = store.read_current(&key).await.unwrap();
        assert!([b"alpha".as_slice(), b"bravo".as_slice()].contains(&current.body.as_slice()));
        assert!(matches!(
            store
                .compare_and_swap(&key, &old.revision, b"stale".to_vec())
                .await,
            CasOutcome::PreconditionFailed
        ));
        assert!(matches!(
            store
                .compare_and_swap("other", &current.revision, vec![])
                .await,
            CasOutcome::NotApplied {
                reason: StoreError::InvalidInput,
                ..
            }
        ));
        assert_eq!(
            store.read_range(&key, 1..4).await.unwrap(),
            current.body[1..4]
        );
        assert!(store.read_range(&key, 0..6).await.is_err());
    }
    let mut cursor = None;
    let mut keys = vec![];
    loop {
        let p = store
            .list(ListRequest {
                prefix: "race/".into(),
                delimiter: None,
                cursor,
                limit: 3,
            })
            .await
            .unwrap();
        keys.extend(p.objects);
        cursor = p.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 16);
    let p = store
        .list(ListRequest {
            prefix: String::new(),
            delimiter: Some("/".into()),
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(p.common_prefixes.contains(&"race/".into()));
    for key in keys {
        assert!(matches!(store.delete(&key).await, DeleteOutcome::Deleted));
        assert!(matches!(
            store.read_current(&key).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.delete(&key).await,
            DeleteOutcome::Deleted | DeleteOutcome::Missing
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn composite_store_contract() {
        contract(&MemoryStore::default()).await;
    }
    #[tokio::test]
    async fn applied_lost_response_then_stale_retry_stays_reconcilable() {
        let store = MemoryStore::default();
        store.create_if_absent("head", b"old".to_vec()).await;
        let old = store.read_current("head").await.unwrap();
        store.lose_next_response.store(true, Ordering::SeqCst);
        assert!(matches!(
            store
                .compare_and_swap("head", &old.revision, b"new".to_vec())
                .await,
            CasOutcome::Unknown
        ));
        assert!(matches!(
            store
                .compare_and_swap("head", &old.revision, b"new".to_vec())
                .await,
            CasOutcome::PreconditionFailed
        ));
        assert_eq!(store.read_current("head").await.unwrap().body, b"new");
    }
}
