//! Provider-independent storage contract. No unconditional overwrite operation exists.
use std::{any::Any, future::Future, ops::Range, sync::Arc};
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("object not found")]
    NotFound,
    #[error("access denied")]
    AccessDenied,
    #[error("throttled")]
    Throttled,
    #[error("integrity check failed")]
    Integrity,
    #[error("invalid range, key, cursor, or revision scope")]
    InvalidInput,
    #[error("required storage capabilities missing")]
    Unsupported,
    #[error("transport error: {0}")]
    Transport(String),
    /// Remote metadata must be sanitized by the adapter before construction.
    #[error("remote object store request failed")]
    Remote {
        operation: &'static str,
        status: u16,
        code: Option<String>,
        message: Option<String>,
    },
}
/// Created by the adapter for its canonical backend and endpoint/bucket/prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionScope {
    backend: String,
    namespace: String,
}
impl RevisionScope {
    pub fn new(backend: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            namespace: namespace.into(),
        }
    }
    pub fn issue<T: Any + Send + Sync>(&self, key: &str, token: T) -> ObjectRevision {
        ObjectRevision {
            scope: self.clone(),
            key: key.into(),
            token: Arc::new(token),
        }
    }
    /// Adapter-only codec boundary: validate scope before issuing any network request.
    pub fn decode<'a, T: Any>(
        &self,
        key: &str,
        revision: &'a ObjectRevision,
    ) -> Result<&'a T, StoreError> {
        if revision.scope != *self || revision.key != key {
            return Err(StoreError::InvalidInput);
        }
        revision
            .token
            .downcast_ref()
            .ok_or(StoreError::InvalidInput)
    }
}
/// Opaque in the protocol layer; never serialized into portable manifests.
#[derive(Clone)]
pub struct ObjectRevision {
    scope: RevisionScope,
    key: String,
    token: Arc<dyn Any + Send + Sync>,
}
impl std::fmt::Debug for ObjectRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectRevision").finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub struct VersionedObject {
    /// Object validator returned by this same read, including provider quoting.
    pub etag: String,
    pub body: Vec<u8>,
    pub revision: ObjectRevision,
    pub attributes: ObjectAttributes,
}
#[derive(Debug, Default)]
pub struct ObjectAttributes {
    pub size: u64,
}
#[derive(Debug)]
pub enum CasOutcome {
    Applied {
        new_revision: Option<ObjectRevision>,
    },
    PreconditionFailed,
    /// A request may have been applied. A retry's 412 cannot erase this uncertainty.
    Unknown,
    NotApplied {
        reason: StoreError,
        retryable: bool,
    },
}
#[derive(Debug)]
pub enum DeleteOutcome {
    Deleted,
    Missing,
    Unknown,
    NotApplied(StoreError),
}
#[derive(Clone, Debug)]
pub struct StoreCapabilities {
    pub strong_read: bool,
    pub atomic_create: bool,
    pub atomic_cas: bool,
    pub range_read: bool,
    pub paginated_list: bool,
    pub per_object_delete: bool,
    pub max_single_put_bytes: u64,
}
impl StoreCapabilities {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.strong_read
            && self.atomic_create
            && self.atomic_cas
            && self.range_read
            && self.paginated_list
            && self.per_object_delete
            && self.max_single_put_bytes > 0
        {
            Ok(())
        } else {
            Err(StoreError::Unsupported)
        }
    }
}
/// Adapter must bind an opaque cursor to namespace, prefix and delimiter.
#[derive(Debug)]
pub struct ListRequest {
    pub prefix: String,
    pub delimiter: Option<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}
#[derive(Debug)]
pub struct ObjectPage {
    pub objects: Vec<String>,
    pub common_prefixes: Vec<String>,
    pub next_cursor: Option<String>,
}

pub trait BlobStore: Send + Sync {
    /// Stable, credential-free identity for persistent immutable-object caching.
    fn cache_namespace(&self) -> Option<String> {
        None
    }
    fn capabilities(&self) -> StoreCapabilities;
    /// Body and revision must come from the same read/version.
    fn read_current(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<VersionedObject, StoreError>> + Send;
    /// Only for immutable objects; a missing or truncated range is an error.
    fn read_range(
        &self,
        key: &str,
        range: Range<u64>,
    ) -> impl Future<Output = Result<Vec<u8>, StoreError>> + Send;
    fn create_if_absent(&self, key: &str, body: Vec<u8>)
        -> impl Future<Output = CasOutcome> + Send;
    fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> impl Future<Output = CasOutcome> + Send;
    fn list(
        &self,
        request: ListRequest,
    ) -> impl Future<Output = Result<ObjectPage, StoreError>> + Send;
    fn delete(&self, key: &str) -> impl Future<Output = DeleteOutcome> + Send;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_composite_revision_rejects_other_scopes_and_types() {
        let scope = RevisionScope::new("fake", "bucket/prefix");
        let token = scope.issue("head", (u64::MAX, 42u64));
        assert_eq!(
            scope.decode::<(u64, u64)>("head", &token).unwrap(),
            &(u64::MAX, 42)
        );
        assert!(scope.decode::<(u64, u64)>("other", &token).is_err());
        assert!(scope.decode::<String>("head", &token).is_err());
        assert!(RevisionScope::new("s3", "bucket/prefix")
            .decode::<(u64, u64)>("head", &token)
            .is_err());
        assert!(RevisionScope::new("fake", "other/prefix")
            .decode::<(u64, u64)>("head", &token)
            .is_err());
        assert!(!format!("{token:?}").contains("42"));
    }
    #[test]
    fn unsupported_backend_cannot_be_enabled() {
        let mut caps = StoreCapabilities {
            strong_read: true,
            atomic_create: true,
            atomic_cas: false,
            range_read: true,
            paginated_list: true,
            per_object_delete: true,
            max_single_put_bytes: 1024,
        };
        assert_eq!(caps.validate(), Err(StoreError::Unsupported));
        caps.atomic_cas = true;
        assert!(caps.validate().is_ok());
    }
}
