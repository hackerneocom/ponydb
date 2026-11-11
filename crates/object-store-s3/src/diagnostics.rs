//! Read-only SDK observations. Never format requests, responses, headers or bodies.
use super::S3Store;
use aws_sdk_s3::{
    config::{interceptors::*, timeout::TimeoutConfig, ConfigBag, Intercept, RuntimeComponents},
    error::{BoxError, SdkError},
};
use regex_lite::Regex;
use serde::Serialize;
use std::{
    error::Error,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct Observation {
    attempts: u32,
    hostname: Option<String>,
    port: Option<u16>,
    proxy_used: Option<bool>,
    timeout: Option<TimeoutConfig>,
    status: Option<u16>,
    server_request_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Recorder(Arc<Mutex<Observation>>);

impl Intercept for Recorder {
    fn name(&self) -> &'static str {
        "ponydb_s3_diagnostics"
    }

    fn read_before_serialization(
        &self,
        _: &BeforeSerializationInterceptorContextRef<'_>,
        _: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        // The runtime has merged behavior-version defaults and operation overrides
        // by this hook. Client::config() alone misses the default 3100ms connect timeout.
        self.0.lock().unwrap().timeout = cfg.load::<TimeoutConfig>().cloned();
        Ok(())
    }

    fn read_before_attempt(
        &self,
        _: &BeforeTransmitInterceptorContextRef<'_>,
        _: &RuntimeComponents,
        _: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        self.0.lock().unwrap().attempts += 1;
        Ok(())
    }

    fn read_before_transmit(
        &self,
        context: &BeforeTransmitInterceptorContextRef<'_>,
        _: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let mut observation = self.0.lock().unwrap();
        observation.timeout = cfg.load::<TimeoutConfig>().cloned();
        if let Ok(url) = url::Url::parse(context.request().uri()) {
            observation.hostname = url.host_str().map(str::to_owned);
            observation.port = url.port_or_known_default();
        }
        // Same matcher as the locked Smithy default HTTPS client, including
        // HTTPS_PROXY/HTTP_PROXY/ALL_PROXY and NO_PROXY. Never format the matcher:
        // it may contain proxy credentials. No system proxy settings are consulted.
        observation.proxy_used = context.request().uri().parse().ok().map(|uri| {
            hyper_util::client::proxy::matcher::Matcher::from_env()
                .intercept(&uri)
                .is_some()
        });
        Ok(())
    }

    fn read_after_transmit(
        &self,
        context: &BeforeDeserializationInterceptorContextRef<'_>,
        _: &RuntimeComponents,
        _: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let response = context.response();
        let mut observation = self.0.lock().unwrap();
        observation.status = Some(response.status().as_u16());
        // Only these diagnostic headers are allowlisted; never log the header map.
        observation.server_request_id = ["x-amz-request-id", "x-oss-request-id", "x-request-id"]
            .into_iter()
            .find_map(|name| response.headers().get(name).map(str::to_owned));
        Ok(())
    }
}

pub(super) struct S3Request {
    operation: &'static str,
    key: Option<String>,
    started: Instant,
    attempted: bool,
    observation: Arc<Mutex<Observation>>,
    outcome: &'static str,
    bytes: u64,
}

impl S3Request {
    pub(super) fn new(operation: &'static str, key: impl FnOnce() -> String) -> Self {
        Self {
            operation,
            key: tracing::enabled!(target: "ponydb::s3", tracing::Level::TRACE).then(key),
            started: Instant::now(),
            attempted: false,
            observation: Arc::new(Mutex::new(Observation::default())),
            outcome: "aborted",
            bytes: 0,
        }
    }

    pub(super) fn recorder(&self) -> Recorder {
        Recorder(self.observation.clone())
    }

    pub(super) fn attempt(&mut self) {
        self.attempted = true;
        observability::count(observability::Counter::S3Requests, 1);
    }

    pub(super) fn error<E: Error + 'static>(&mut self, error: &SdkError<E>, store: &S3Store) {
        let mut observation = self.observation.lock().unwrap();
        if let Some(response) = error.raw_response() {
            observation.status = Some(response.status().as_u16());
        }
        if !tracing::enabled!(target: "ponydb::s3", tracing::Level::DEBUG) {
            return;
        }
        let (sdk_error_type, dispatch_error_kind) = classify(error);
        let timeout = observation.timeout.as_ref();
        let millis = |duration: Option<Duration>| duration.map(|d| d.as_millis() as u64);
        let chain = error_chain(error, &store.secrets);
        // The caller's request/operation spans supply request_id for CLI, HTTP and MCP.
        // Missing timeouts mean disabled; timeout_config_observed=false means unavailable.
        tracing::debug!(target: "ponydb::s3",
            event = "s3_request_failed", operation = self.operation,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            attempts = observation.attempts,
            endpoint = store.sanitize(&store.endpoint, usize::MAX),
            hostname = observation.hostname.as_deref().map(|host| store.sanitize(host, usize::MAX)), port = observation.port,
            proxy_used = observation.proxy_used, proxy_source = "environment",
            sdk_error_type, dispatch_error_kind,
            error_chain = %serde_json::to_string(&chain).expect("error chain is serializable"),
            timeout_config_observed = timeout.is_some(),
            connect_timeout_ms = millis(timeout.and_then(TimeoutConfig::connect_timeout)),
            read_timeout_ms = millis(timeout.and_then(TimeoutConfig::read_timeout)),
            operation_timeout_ms = millis(timeout.and_then(TimeoutConfig::operation_timeout)),
            operation_attempt_timeout_ms = millis(timeout.and_then(TimeoutConfig::operation_attempt_timeout)),
            status = observation.status,
            server_request_id = observation.server_request_id.as_deref().map(|id| store.sanitize(id, usize::MAX)),
        );
    }

    pub(super) fn finish(&mut self, outcome: &'static str, bytes: u64, write: bool) {
        self.outcome = outcome;
        self.bytes = bytes;
        observability::count(
            if write {
                observability::Counter::S3WriteBytes
            } else {
                observability::Counter::S3ReadBytes
            },
            bytes,
        );
    }
}

impl Drop for S3Request {
    fn drop(&mut self) {
        tracing::trace!(target: "ponydb::s3", event = "s3_request_completed", operation = self.operation,
            key = self.key.as_deref(), attempted = self.attempted, status = self.observation.lock().unwrap().status,
            outcome = self.outcome, bytes = self.bytes, elapsed_ms = self.started.elapsed().as_millis() as u64);
    }
}

fn classify<E, R>(error: &SdkError<E, R>) -> (&'static str, Option<&'static str>) {
    match error {
        SdkError::ConstructionFailure(_) => ("ConstructionFailure", None),
        SdkError::TimeoutError(_) => ("TimeoutError", None),
        SdkError::ResponseError(_) => ("ResponseError", None),
        SdkError::ServiceError(_) => ("ServiceError", None),
        SdkError::DispatchFailure(dispatch) => (
            "DispatchFailure",
            Some(if dispatch.is_timeout() {
                "timeout"
            } else if dispatch.is_io() {
                "io"
            } else if dispatch.is_user() {
                "user"
            } else {
                "other"
            }),
        ),
        _ => ("Unknown", None),
    }
}

#[derive(Debug, Serialize)]
struct ErrorEntry {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    io_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_os_error: Option<i32>,
}

fn error_chain(error: &(dyn Error + 'static), secrets: &[String]) -> Vec<ErrorEntry> {
    let mut chain = Vec::new();
    let mut current = Some(error);
    while let Some(error) = current {
        let io = error.downcast_ref::<std::io::Error>();
        chain.push(ErrorEntry {
            message: redact(&error.to_string(), secrets),
            io_kind: io.map(|io| format!("{:?}", io.kind())),
            raw_os_error: io.and_then(std::io::Error::raw_os_error),
        });
        current = error.source();
    }
    chain
}

/// Redact before flattening control characters or applying output limits.
/// Preserve diagnostic text, but discard URL userinfo, paths and query strings,
/// and sensitive header/assignment tails even for values not in our credentials.
pub(super) fn redact(value: &str, secrets: &[String]) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    static SENSITIVE: OnceLock<Regex> = OnceLock::new();
    let url_pattern = URL.get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>"']+"#).unwrap());
    let sensitive = SENSITIVE.get_or_init(|| {
        Regex::new(r#"(?im)\b(authorization|proxy-authorization|x-amz-[a-z0-9-]+|x-oss-[a-z0-9-]+|awsaccesskeyid|signature|credential|session[ _-]?token|security[ _-]?token|(?:aws[ _-]?)?access[ _-]?key[ _-]?id|(?:aws[ _-]?)?secret[ _-]?access[ _-]?key)["'\\\s]*(?:=|:|>|%3d|%3a)[^\r\n]*(?:\r?\n[ \t]+[^\r\n]*)*"#).unwrap()
    });
    let value = url_pattern.replace_all(value, |caps: &regex_lite::Captures<'_>| {
        url::Url::parse(&caps[0])
            .ok()
            .filter(|url| url.host_str().is_some())
            .map(|url| {
                format!(
                    "{}://{}/{}",
                    url.scheme(),
                    &url[url::Position::BeforeHost..url::Position::AfterPort],
                    if url.path() == "/" && url.query().is_none() && url.fragment().is_none() {
                        ""
                    } else {
                        "[REDACTED]"
                    }
                )
            })
            .unwrap_or_else(|| "[REDACTED_URL]".into())
    });
    let mut value = sensitive
        .replace_all(&value, "${1}=[REDACTED]")
        .into_owned();
    for secret in secrets {
        value = value.replace(secret, "[REDACTED]");
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        value = value.replace(&encoded, "[REDACTED]");
    }
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::error::ConnectorError;

    #[test]
    fn classifies_sdk_variants_and_dispatch_kinds() {
        type Sdk = SdkError<std::io::Error, ()>;
        let io = || std::io::Error::other("original failure");
        for (error, expected) in [
            (
                Sdk::construction_failure(io()),
                ("ConstructionFailure", None),
            ),
            (Sdk::timeout_error(io()), ("TimeoutError", None)),
            (Sdk::response_error(io(), ()), ("ResponseError", None)),
            (Sdk::service_error(io(), ()), ("ServiceError", None)),
            (
                Sdk::dispatch_failure(ConnectorError::timeout(io().into())),
                ("DispatchFailure", Some("timeout")),
            ),
            (
                Sdk::dispatch_failure(ConnectorError::io(io().into())),
                ("DispatchFailure", Some("io")),
            ),
            (
                Sdk::dispatch_failure(ConnectorError::user(io().into())),
                ("DispatchFailure", Some("user")),
            ),
            (
                Sdk::dispatch_failure(ConnectorError::other(io().into(), None)),
                ("DispatchFailure", Some("other")),
            ),
        ] {
            assert_eq!(classify(&error), expected);
        }
    }

    #[test]
    fn traverses_sources_without_truncating_and_keeps_os_errors() {
        let io = std::io::Error::from_raw_os_error(10060);
        let kind = format!("{:?}", io.kind());
        let sdk = SdkError::<std::io::Error, ()>::dispatch_failure(ConnectorError::io(io.into()));
        let chain = error_chain(&sdk, &[]);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].message, "dispatch failure");
        assert_eq!(chain[1].message, "io error");
        assert_eq!(chain[2].io_kind.as_deref(), Some(kind.as_str()));
        assert_eq!(chain[2].raw_os_error, Some(10060));

        let message = format!("TLS handshake: {}: certificate rejected", "x".repeat(8192));
        let sdk = SdkError::<std::io::Error, ()>::construction_failure(std::io::Error::other(
            message.clone(),
        ));
        let chain = error_chain(&sdk, &[]);
        assert_eq!(chain[1].message, message);
    }

    #[test]
    fn redacts_unknown_authorization_signatures_tokens_and_url_credentials() {
        let input = concat!(
            "TLS failed https://proxy-user:proxy-password@s3.example.com/private-object?X-Amz-Signature=signature-secret&X-Amz-Security-Token=token-secret\n",
            "Authorization: AWS4-HMAC-SHA256 Credential=other-access, SignedHeaders=host, Signature=header-signature\n",
            "\"X-Amz-Security-Token\": \"header-token\"\n",
            "X-Amz-Credential%3Dquery-credential\n",
            "AWSAccessKeyId=legacy-access&Signature=legacy-signature\n",
            "Authorization:\r\n Bearer folded-secret\r\n",
            "\\\"Authorization\\\": \\\"Bearer escaped-secret\\\"\n",
            "<Signature>xml-signature</Signature>\n",
            "AWS_SECRET_ACCESS_KEY=unknown-secret-key\n",
            "TCP connection refused (os error 10061)\n",
        );
        let value = redact(input, &[]);
        for secret in [
            "proxy-user",
            "proxy-password",
            "private-object",
            "signature-secret",
            "token-secret",
            "other-access",
            "header-signature",
            "header-token",
            "query-credential",
            "legacy-access",
            "legacy-signature",
            "folded-secret",
            "escaped-secret",
            "xml-signature",
            "unknown-secret-key",
        ] {
            assert!(!value.contains(secret), "leaked {secret}: {value}");
        }
        assert!(value.contains("s3.example.com"));
        assert!(value.contains("TCP connection refused (os error 10061)"));
        let value = redact("known+/token known%2B%2Ftoken", &["known+/token".into()]);
        assert_eq!(value, "[REDACTED] [REDACTED]");
        assert_eq!(
            redact("https://s3.example.com/", &[]),
            "https://s3.example.com/"
        );
    }
}
