//! S3 conditional operations. SDK retries are disabled for writes so a lost
//! success response can never be turned into a definitive 412 by an implicit retry.
pub use aws_sdk_s3::config::Credentials;
use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig, BehaviorVersion, Region},
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    Client,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use object_store_api::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ops::Range, sync::Arc, time::Duration};

mod diagnostics;
use diagnostics::S3Request;
fn error_category(error: &StoreError) -> &'static str {
    match error {
        StoreError::NotFound => "not_found",
        StoreError::AccessDenied => "access_denied",
        StoreError::Throttled => "throttled",
        StoreError::Integrity => "integrity_error",
        StoreError::InvalidInput => "invalid_input",
        StoreError::Unsupported => "unsupported",
        StoreError::Transport(_) => "transport_error",
        StoreError::Remote { .. } => "remote_error",
    }
}

#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub force_path_style: bool,
    pub bucket: String,
    pub prefix: String,
    pub max_object_bytes: u64,
    pub timeout: Duration,
}
#[derive(Clone)]
pub struct S3Store {
    client: Client,
    endpoint: String,
    bucket: String,
    prefix: String,
    scope: RevisionScope,
    scope_id: String,
    max: u64,
    secrets: Arc<Vec<String>>,
}
#[derive(Clone, Debug)]
struct S3Revision(String);
#[derive(Serialize, Deserialize)]
struct Cursor {
    scope: String,
    prefix: String,
    delimiter: Option<String>,
    token: String,
}
impl S3Store {
    pub fn new(config: S3Config, credentials: Credentials) -> Result<Self, StoreError> {
        let mut endpoint =
            url::Url::parse(&config.endpoint).map_err(|_| StoreError::InvalidInput)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err(StoreError::InvalidInput);
        }
        endpoint.set_path("/");
        if config.bucket.is_empty()
            || !config
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || config.max_object_bytes == 0
            || config.max_object_bytes > 5 * 1024 * 1024 * 1024
            || config.timeout.is_zero()
        {
            return Err(StoreError::InvalidInput);
        }
        let prefix = if config.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", config.prefix.trim_end_matches('/'))
        };
        validate_prefix(&prefix)?;
        let scope_id = format!("{}|{}|{}", endpoint, config.bucket, prefix);
        let mut secrets = vec![
            credentials.access_key_id().to_owned(),
            credentials.secret_access_key().to_owned(),
        ];
        if let Some(token) = credentials.session_token() {
            secrets.push(token.to_owned());
        }
        secrets.retain(|value| !value.is_empty());
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let sdk = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region))
            .endpoint_url(endpoint.to_string())
            .force_path_style(config.force_path_style)
            .credentials_provider(credentials)
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(config.timeout)
                    .operation_attempt_timeout(config.timeout)
                    .build(),
            )
            .build();
        Ok(Self {
            client: Client::from_conf(sdk),
            endpoint: endpoint.to_string(),
            bucket: config.bucket,
            prefix,
            scope: RevisionScope::new("s3", &scope_id),
            scope_id,
            max: config.max_object_bytes,
            secrets: Arc::new(secrets),
        })
    }
    fn remote_error<E: ProvideErrorMetadata>(
        &self,
        operation: &'static str,
        error: &SdkError<E>,
    ) -> StoreError {
        let Some(status) = error.raw_response().map(|r| r.status().as_u16()) else {
            return StoreError::Transport("S3 request failed".into());
        };
        let metadata = error.as_service_error();
        StoreError::Remote {
            operation,
            status,
            code: metadata
                .and_then(|e| e.code())
                .map(|s| self.sanitize(s, 128)),
            message: metadata
                .and_then(|e| e.message())
                .map(|s| self.sanitize(s, 1024)),
        }
    }
    fn read_error<E: ProvideErrorMetadata>(
        &self,
        operation: &'static str,
        error: &SdkError<E>,
    ) -> StoreError {
        if error
            .raw_response()
            .is_some_and(|r| r.status().as_u16() == 404)
        {
            StoreError::NotFound
        } else {
            self.remote_error(operation, error)
        }
    }
    fn sanitize(&self, value: &str, limit: usize) -> String {
        let value = diagnostics::redact(value, &self.secrets);
        let mut chars = value.chars().map(|c| if c.is_control() { ' ' } else { c });
        let mut result: String = chars.by_ref().take(limit).collect();
        if chars.next().is_some() {
            result.push_str("...");
        }
        result
    }
    fn key(&self, key: &str) -> Result<String, StoreError> {
        validate_key(key)?;
        Ok(format!("{}{key}", self.prefix))
    }
    async fn put(&self, key: &str, body: Vec<u8>, expected: Option<&ObjectRevision>) -> CasOutcome {
        let bytes = body.len() as u64;
        let mut io = S3Request::new("PutObject", || format!("{}{}", self.prefix, key));
        let result = async {
            let physical = match self.key(key) {
                Ok(k) => k,
                Err(e) => return not_applied(e, false),
            };
            if body.len() as u64 > self.max {
                return not_applied(StoreError::InvalidInput, false);
            }
            let token = match expected {
                Some(r) => match self.scope.decode::<S3Revision>(key, r) {
                    Ok(t) => Some(t.0.clone()),
                    Err(e) => return not_applied(e, false),
                },
                None => None,
            };
            let digest = Sha256::digest(&body);
            let mut request = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(physical)
                .metadata("sha256", format!("{digest:x}"))
                .checksum_sha256(STANDARD.encode(digest))
                .body(ByteStream::from(body));
            request = if let Some(token) = token {
                request.if_match(token)
            } else {
                request.if_none_match("*")
            };
            io.attempt();
            match request.customize().interceptor(io.recorder()).send().await {
                Ok(r) => CasOutcome::Applied {
                    new_revision: r
                        .e_tag()
                        .map(|etag| self.scope.issue(key, S3Revision(etag.into()))),
                },
                Err(e) => {
                    io.error(&e, self);
                    let outcome = classify_write(
                        e.raw_response().map(|r| r.status().as_u16()),
                        matches!(e, SdkError::ConstructionFailure(_)),
                    );
                    match outcome {
                        CasOutcome::NotApplied { retryable, .. } if e.raw_response().is_some() => {
                            not_applied(self.remote_error("PutObject", &e), retryable)
                        }
                        outcome => outcome,
                    }
                }
            }
        }
        .await;
        let outcome = match &result {
            CasOutcome::Applied { .. } => "applied",
            CasOutcome::PreconditionFailed => "precondition_failed",
            CasOutcome::NotApplied { reason, .. } => error_category(reason),
            CasOutcome::Unknown => "unknown",
        };
        io.finish(
            outcome,
            if matches!(result, CasOutcome::Applied { .. }) {
                bytes
            } else {
                0
            },
            true,
        );
        result
    }
}
fn not_applied(reason: StoreError, retryable: bool) -> CasOutcome {
    CasOutcome::NotApplied { reason, retryable }
}
/// Public for deterministic transport-fault verification without an SDK retry loop.
pub fn classify_write(status: Option<u16>, construction_failure: bool) -> CasOutcome {
    if construction_failure {
        return not_applied(StoreError::InvalidInput, false);
    }
    match status {
        Some(412) => CasOutcome::PreconditionFailed,
        Some(401 | 403) => not_applied(StoreError::AccessDenied, false),
        Some(429) => not_applied(StoreError::Throttled, true),
        Some(400 | 404 | 405 | 411 | 413 | 416 | 422) => {
            not_applied(StoreError::InvalidInput, false)
        }
        // 409/5xx/dispatch/timeout are conservative: no claim of non-application.
        _ => CasOutcome::Unknown,
    }
}
fn validate_key(key: &str) -> Result<(), StoreError> {
    if key.is_empty()
        || key
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || key.contains(['\0', '\\'])
    {
        Err(StoreError::InvalidInput)
    } else {
        Ok(())
    }
}
fn validate_prefix(prefix: &str) -> Result<(), StoreError> {
    if prefix.is_empty() {
        Ok(())
    } else {
        validate_key(prefix.strip_suffix('/').unwrap_or(prefix))
    }
}
impl BlobStore for S3Store {
    fn cache_namespace(&self) -> Option<String> {
        Some(format!("s3:{}", self.scope_id))
    }
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            strong_read: true,
            atomic_create: true,
            atomic_cas: true,
            range_read: true,
            paginated_list: true,
            per_object_delete: true,
            max_single_put_bytes: self.max,
        }
    }
    async fn read_current(&self, key: &str) -> Result<VersionedObject, StoreError> {
        let mut io = S3Request::new("GetObject", || format!("{}{}", self.prefix, key));
        let result = async {
            let physical = self.key(key)?;
            io.attempt();
            let r = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(physical)
                .customize()
                .interceptor(io.recorder())
                .send()
                .await
                .map_err(|e| {
                    io.error(&e, self);
                    self.read_error("GetObject", &e)
                })?;
            let size = r
                .content_length()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n <= self.max)
                .ok_or(StoreError::Integrity)?;
            let etag = r.e_tag().ok_or(StoreError::Integrity)?.to_owned();
            let hash = r
                .metadata()
                .and_then(|m| m.get("sha256"))
                .ok_or(StoreError::Integrity)?
                .to_owned();
            let mut stream = r.body;
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| StoreError::Integrity)?;
                if body.len() as u64 + chunk.len() as u64 > size {
                    return Err(StoreError::Integrity);
                }
                body.extend_from_slice(&chunk);
            }
            if body.len() as u64 != size || format!("{:x}", Sha256::digest(&body)) != hash {
                return Err(StoreError::Integrity);
            }
            Ok(VersionedObject {
                etag: etag.clone(),
                body,
                revision: self.scope.issue(key, S3Revision(etag)),
                attributes: ObjectAttributes { size },
            })
        }
        .await;
        match &result {
            Ok(value) => io.finish("success", value.body.len() as u64, false),
            Err(error) => io.finish(error_category(error), 0, false),
        }
        result
    }
    async fn read_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        let mut io = S3Request::new("GetObjectRange", || format!("{}{}", self.prefix, key));
        let result = async {
            if range.start >= range.end || range.end - range.start > self.max {
                return Err(StoreError::InvalidInput);
            }
            let physical = self.key(key)?;
            io.attempt();
            let r = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(physical)
                .range(format!("bytes={}-{}", range.start, range.end - 1))
                .customize()
                .interceptor(io.recorder())
                .send()
                .await
                .map_err(|e| {
                    io.error(&e, self);
                    self.read_error("GetObject", &e)
                })?;
            let expected = format!("bytes {}-{}/", range.start, range.end - 1);
            if !r.content_range().is_some_and(|v| v.starts_with(&expected))
                || r.content_length() != i64::try_from(range.end - range.start).ok()
            {
                return Err(StoreError::Integrity);
            }
            let size = (range.end - range.start) as usize;
            let mut stream = r.body;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| StoreError::Integrity)?;
                if bytes.len() + chunk.len() > size {
                    return Err(StoreError::Integrity);
                }
                bytes.extend_from_slice(&chunk);
            }
            if bytes.len() != size {
                return Err(StoreError::Integrity);
            }
            Ok(bytes)
        }
        .await;
        match &result {
            Ok(value) => io.finish("success", value.len() as u64, false),
            Err(error) => io.finish(error_category(error), 0, false),
        }
        result
    }
    async fn create_if_absent(&self, key: &str, body: Vec<u8>) -> CasOutcome {
        self.put(key, body, None).await
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &ObjectRevision,
        body: Vec<u8>,
    ) -> CasOutcome {
        self.put(key, body, Some(expected)).await
    }
    async fn list(&self, request: ListRequest) -> Result<ObjectPage, StoreError> {
        let mut io = S3Request::new("ListObjectsV2", || {
            format!("{}{}", self.prefix, request.prefix)
        });
        let result = async {
            validate_prefix(&request.prefix)?;
            if request.limit == 0
                || request.limit > 1000
                || request.delimiter.as_deref().is_some_and(|s| s != "/")
            {
                return Err(StoreError::InvalidInput);
            }
            let token = if let Some(cursor) = &request.cursor {
                if cursor.len() > 16384 {
                    return Err(StoreError::InvalidInput);
                }
                let cursor: Cursor =
                    serde_json::from_str(cursor).map_err(|_| StoreError::InvalidInput)?;
                if cursor.scope != self.scope_id
                    || cursor.prefix != request.prefix
                    || cursor.delimiter != request.delimiter
                {
                    return Err(StoreError::InvalidInput);
                }
                Some(cursor.token)
            } else {
                None
            };
            io.attempt();
            let r = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(format!("{}{}", self.prefix, request.prefix))
                .set_delimiter(request.delimiter.clone())
                .set_continuation_token(token)
                .max_keys(request.limit as i32)
                .customize()
                .interceptor(io.recorder())
                .send()
                .await
                .map_err(|e| {
                    io.error(&e, self);
                    self.read_error("ListObjectsV2", &e)
                })?;
            let relative = |key: &str| -> Result<String, StoreError> {
                key.strip_prefix(&self.prefix)
                    .filter(|k| k.starts_with(&request.prefix))
                    .map(str::to_owned)
                    .ok_or(StoreError::Integrity)
            };
            let objects = r
                .contents()
                .iter()
                .map(|o| relative(o.key().ok_or(StoreError::Integrity)?))
                .collect::<Result<_, _>>()?;
            let common_prefixes = r
                .common_prefixes()
                .iter()
                .map(|p| relative(p.prefix().ok_or(StoreError::Integrity)?))
                .collect::<Result<_, _>>()?;
            let next_cursor = if r.is_truncated() == Some(true) {
                Some(
                    serde_json::to_string(&Cursor {
                        scope: self.scope_id.clone(),
                        prefix: request.prefix,
                        delimiter: request.delimiter,
                        token: r
                            .next_continuation_token()
                            .ok_or(StoreError::Integrity)?
                            .into(),
                    })
                    .map_err(|_| StoreError::Integrity)?,
                )
            } else {
                None
            };
            Ok(ObjectPage {
                objects,
                common_prefixes,
                next_cursor,
            })
        }
        .await;
        match &result {
            Ok(_) => io.finish("success", 0, false),
            Err(error) => io.finish(error_category(error), 0, false),
        }
        result
    }
    async fn delete(&self, key: &str) -> DeleteOutcome {
        let mut io = S3Request::new("DeleteObject", || format!("{}{}", self.prefix, key));
        let result = async {
            let physical = match self.key(key) {
                Ok(k) => k,
                Err(e) => return DeleteOutcome::NotApplied(e),
            };
            io.attempt();
            match self
                .client
                .delete_object()
                .bucket(&self.bucket)
                .key(physical)
                .customize()
                .interceptor(io.recorder())
                .send()
                .await
            {
                Ok(_) => DeleteOutcome::Deleted, // S3 treats deleting an absent key as success.
                Err(e) => {
                    io.error(&e, self);
                    match e.raw_response().map(|r| r.status().as_u16()) {
                        Some(404) => DeleteOutcome::Missing,
                        Some(401 | 403 | 429) => {
                            DeleteOutcome::NotApplied(self.remote_error("DeleteObject", &e))
                        }
                        _ => DeleteOutcome::Unknown,
                    }
                }
            }
        }
        .await;
        io.finish(
            match &result {
                DeleteOutcome::Deleted => "deleted",
                DeleteOutcome::Missing => "missing",
                DeleteOutcome::NotApplied(error) => error_category(error),
                DeleteOutcome::Unknown => "unknown",
            },
            0,
            false,
        );
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_outcomes_never_downgrade_uncertainty() {
        for status in [None, Some(500), Some(503), Some(409)] {
            assert!(matches!(classify_write(status, false), CasOutcome::Unknown));
        }
        assert!(matches!(
            classify_write(Some(412), false),
            CasOutcome::PreconditionFailed
        ));
        assert!(matches!(
            classify_write(None, true),
            CasOutcome::NotApplied {
                retryable: false,
                ..
            }
        ));
        assert!(matches!(
            classify_write(Some(403), false),
            CasOutcome::NotApplied {
                reason: StoreError::AccessDenied,
                ..
            }
        ));
        assert!(matches!(
            classify_write(Some(429), false),
            CasOutcome::NotApplied {
                reason: StoreError::Throttled,
                retryable: true
            }
        ));
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn database_names_are_preserved_without_allowing_path_traversal() {
        for name in ["中文", "a b", "$notes", "Notes", "a#b", "a%b"] {
            assert!(validate_key(&format!("databases/{name}/head.json")).is_ok());
        }
        for key in [
            "",
            "/root",
            "databases/../head.json",
            "a//b",
            "a\\b",
            "a\0b",
        ] {
            assert!(validate_key(key).is_err());
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn store(endpoint: String) -> S3Store {
        S3Store::new(
            S3Config {
                endpoint,
                region: "us-east-1".into(),
                force_path_style: true,
                bucket: "test".into(),
                prefix: String::new(),
                max_object_bytes: 1024,
                timeout: Duration::from_secs(2),
            },
            Credentials::new(
                "test-access",
                "test-access-secret",
                Some("test-session".into()),
                None,
                "test",
            ),
        )
        .unwrap()
    }
    async fn response(status: u16, body: &str) -> (S3Store, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status} Error\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nX-Private: header-secret\r\nx-amz-request-id: server-test-request\r\n\r\n{body}", body.len());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let n = stream.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (store(endpoint), server)
    }
    #[tokio::test]
    async fn addressing_configuration_controls_the_request_url() {
        for force_path_style in [true, false] {
            let store = S3Store::new(
                S3Config {
                    endpoint: "https://s3.example.com".into(),
                    region: "us-east-1".into(),
                    bucket: "test".into(),
                    prefix: String::new(),
                    force_path_style,
                    max_object_bytes: 1024,
                    timeout: Duration::from_secs(2),
                },
                Credentials::new("access", "secret", None, None, "test"),
            )
            .unwrap();
            let uri = Arc::new(std::sync::Mutex::new(None));
            let captured = uri.clone();
            // Stop before transmission: this tests SDK addressing without DNS or remote access.
            let result = store
                .client
                .list_objects_v2()
                .bucket("test")
                .customize()
                .map_request(move |request| {
                    *captured.lock().unwrap() = Some(request.uri().to_owned());
                    Err(std::io::Error::other("test stopped before transmission"))
                })
                .send()
                .await;
            assert!(result.is_err());
            let uri = uri
                .lock()
                .unwrap()
                .clone()
                .expect("request was constructed");
            let uri = url::Url::parse(&uri).unwrap();
            assert_eq!(
                uri.host_str(),
                Some(if force_path_style {
                    "s3.example.com"
                } else {
                    "test.s3.example.com"
                })
            );
            assert_eq!(uri.path(), if force_path_style { "/test/" } else { "/" });
        }
    }
    #[test]
    fn diagnostics_redact_before_cleaning_and_truncating() {
        let store = store("http://localhost:9000".into());
        let text = store.sanitize(
            "test-access-secret test-access test-session\n\u{1b}中文",
            1024,
        );
        assert_eq!(text, "[REDACTED] [REDACTED] [REDACTED]  中文");
        assert_eq!(
            store.sanitize(&"中".repeat(1025), 1024),
            format!("{}...", "中".repeat(1024))
        );
        assert_eq!(store.sanitize(&"x".repeat(128), 128).len(), 128);
    }
    #[tokio::test]
    async fn sdk_metadata_is_sanitized_and_unknown_codes_are_preserved() {
        let (store, server) = response(403, "<Error><Code>Unknown-test-access-secret</Code><Message>test-access test-session&#10;denied</Message><Resource>body-secret</Resource></Error>").await;
        let error = store
            .list(ListRequest {
                prefix: String::new(),
                delimiter: None,
                cursor: None,
                limit: 1,
            })
            .await
            .unwrap_err();
        assert_eq!(
            error,
            StoreError::Remote {
                operation: "ListObjectsV2",
                status: 403,
                code: Some("Unknown-[REDACTED]".into()),
                message: Some("[REDACTED] [REDACTED] denied".into())
            }
        );
        assert!(!format!("{error:?}").contains("header-secret"));
        assert!(!format!("{error:?}").contains("body-secret"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn missing_metadata_and_not_found_keep_their_semantics() {
        for status in [403, 404, 503] {
            let (store, server) = response(status, "<Error/>").await;
            let error = store.read_current("object").await.unwrap_err();
            if status == 404 {
                assert_eq!(error, StoreError::NotFound);
            } else {
                assert_eq!(
                    error,
                    StoreError::Remote {
                        operation: "GetObject",
                        status,
                        code: None,
                        message: None
                    }
                );
            }
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn write_and_delete_diagnostics_preserve_uncertainty() {
        for status in [403, 412, 500] {
            let (store, server) = response(
                status,
                "<Error><Code>Denied</Code><Message>reason</Message></Error>",
            )
            .await;
            let result = store.create_if_absent("object", vec![]).await;
            match status {
                403 => assert!(matches!(
                    result,
                    CasOutcome::NotApplied {
                        reason: StoreError::Remote {
                            operation: "PutObject",
                            status: 403,
                            ..
                        },
                        retryable: false
                    }
                )),
                412 => assert!(matches!(result, CasOutcome::PreconditionFailed)),
                _ => assert!(matches!(result, CasOutcome::Unknown)),
            }
            server.await.unwrap();
        }
        for status in [403, 404, 500] {
            let (store, server) = response(status, "<Error><Code>Denied</Code></Error>").await;
            let result = store.delete("object").await;
            match status {
                403 => assert!(matches!(
                    result,
                    DeleteOutcome::NotApplied(StoreError::Remote {
                        operation: "DeleteObject",
                        ..
                    })
                )),
                404 => assert!(matches!(result, DeleteOutcome::Missing)),
                _ => assert!(matches!(result, DeleteOutcome::Unknown)),
            }
            server.await.unwrap();
        }
    }

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    async fn failure_log(store: &S3Store) -> serde_json::Value {
        use tracing::{instrument::WithSubscriber, Instrument};
        use tracing_subscriber::{layer::SubscriberExt, Layer};
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(buffer.clone())
                .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                    meta.target().starts_with("ponydb")
                })),
        );
        async {
            let span =
                tracing::info_span!(target: "ponydb", "request", request_id = "diagnostic-test");
            assert!(store.read_current("object").instrument(span).await.is_err());
        }
        .with_subscriber(subscriber)
        .await;
        let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        for secret in [
            "test-access",
            "test-session",
            "header-secret",
            "body-secret",
            "unknown-signature",
            "unknown-auth",
        ] {
            assert!(!output.contains(secret), "leaked {secret}: {output}");
        }
        let event = output
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|value| value["fields"]["event"] == "s3_request_failed")
            .expect("failure diagnostic");
        assert_eq!(event["span"]["request_id"], "diagnostic-test");
        let fields = &event["fields"];
        assert_eq!(fields["attempts"], 1);
        assert_eq!(fields["hostname"], "127.0.0.1");
        assert!(fields["port"].as_u64().unwrap() > 0);
        assert_eq!(fields["timeout_config_observed"], true);
        assert_eq!(fields["connect_timeout_ms"], 3100);
        assert!(fields["read_timeout_ms"].is_null());
        assert_eq!(
            fields["operation_timeout_ms"],
            store
                .client
                .config()
                .timeout_config()
                .unwrap()
                .operation_timeout()
                .unwrap()
                .as_millis() as u64
        );
        assert_eq!(
            fields["operation_attempt_timeout_ms"],
            fields["operation_timeout_ms"]
        );
        event
    }

    #[tokio::test]
    async fn failure_event_preserves_response_metadata_and_redacts_sources() {
        let (store, server) = response(403, "<Error><Code>Denied</Code><Message>test-access-secret test-session Authorization: unknown-auth X-Amz-Signature=unknown-signature</Message><Resource>body-secret</Resource></Error>").await;
        let event = failure_log(&store).await;
        let fields = &event["fields"];
        assert_eq!(fields["sdk_error_type"], "ServiceError");
        assert_eq!(fields["status"], 403);
        assert_eq!(fields["server_request_id"], "server-test-request");
        let chain: serde_json::Value =
            serde_json::from_str(fields["error_chain"].as_str().unwrap()).unwrap();
        assert_eq!(chain[0]["message"], "service error");
        assert!(chain.as_array().unwrap().len() >= 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failure_event_preserves_dispatch_sources_and_operation_timeouts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Close without any HTTP response to exercise the real connector chain.
            drop(stream);
        });
        let store = store(endpoint);
        let event = failure_log(&store).await;
        assert_eq!(event["fields"]["sdk_error_type"], "DispatchFailure");
        let chain: serde_json::Value =
            serde_json::from_str(event["fields"]["error_chain"].as_str().unwrap()).unwrap();
        assert!(chain.as_array().unwrap().len() >= 3);
        server.await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut store = store;
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        store.endpoint = endpoint.clone();
        store.client = Client::from_conf(
            store
                .client
                .config()
                .to_builder()
                .endpoint_url(endpoint)
                .timeout_config(
                    TimeoutConfig::builder()
                        .operation_timeout(Duration::from_millis(50))
                        .operation_attempt_timeout(Duration::from_millis(50))
                        .build(),
                )
                .build(),
        );
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let event = failure_log(&store).await;
        assert_eq!(event["fields"]["sdk_error_type"], "TimeoutError");
        assert_eq!(event["fields"]["operation_timeout_ms"], 50);
        server.abort();
    }

    #[tokio::test]
    async fn stalled_tls_handshake_uses_the_sdk_default_connect_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("https://{}", listener.local_addr().unwrap());
        let mut store = store(endpoint);
        store.client = Client::from_conf(
            store
                .client
                .config()
                .to_builder()
                .timeout_config(
                    TimeoutConfig::builder()
                        .operation_timeout(Duration::from_secs(5))
                        .operation_attempt_timeout(Duration::from_secs(5))
                        .build(),
                )
                .build(),
        );
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            // TCP connects, but the server never answers the TLS ClientHello.
            std::future::pending::<()>().await;
        });
        let event = failure_log(&store).await;
        let fields = &event["fields"];
        assert_eq!(fields["sdk_error_type"], "DispatchFailure");
        assert_eq!(fields["dispatch_error_kind"], "timeout");
        assert!(fields["elapsed_ms"].as_u64().unwrap() >= 3000);
        assert!(fields["error_chain"].as_str().unwrap().contains("3.1s"));
        server.abort();
    }
}
