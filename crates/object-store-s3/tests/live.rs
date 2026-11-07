//! Explicit opt-in, creates a unique bucket and deletes only its own fixtures.
use object_store_api::*;
use object_store_s3::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires P0_S3_ENDPOINT and AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY"]
async fn live_s3_contract() {
    let endpoint = std::env::var("P0_S3_ENDPOINT").expect("P0_S3_ENDPOINT");
    let credentials = Credentials::new(
        std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY"),
        std::env::var("AWS_SESSION_TOKEN").ok(),
        None,
        "p0-test",
    );
    let bucket = format!(
        "p0-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(&endpoint)
        .force_path_style(true)
        .credentials_provider(credentials.clone())
        .build();
    let client = aws_sdk_s3::Client::from_conf(config);
    client.create_bucket().bucket(&bucket).send().await.unwrap();
    let cfg = S3Config {
        endpoint: endpoint.clone(),
        region: "us-east-1".into(),
        force_path_style: true,
        bucket: bucket.clone(),
        prefix: "isolated/".into(),
        max_object_bytes: 1024 * 1024,
        timeout: Duration::from_secs(5),
    };
    let store = S3Store::new(cfg.clone(), credentials.clone()).unwrap();
    object_store_test_support::contract(&store).await;
    assert!(matches!(
        store
            .create_if_absent("limit", vec![0; 1024 * 1024 + 1])
            .await,
        CasOutcome::NotApplied { .. }
    ));
    assert!(matches!(
        store.create_if_absent("limit", vec![0; 1024 * 1024]).await,
        CasOutcome::Applied { .. }
    ));
    assert_eq!(
        store.read_current("limit").await.unwrap().body.len(),
        1024 * 1024
    );
    let revision = store.read_current("limit").await.unwrap().revision;
    let other = S3Store::new(
        S3Config {
            prefix: "different/".into(),
            ..cfg.clone()
        },
        credentials.clone(),
    )
    .unwrap();
    assert!(matches!(
        other.compare_and_swap("limit", &revision, vec![]).await,
        CasOutcome::NotApplied {
            reason: StoreError::InvalidInput,
            ..
        }
    ));
    assert!(matches!(
        store.delete("limit").await,
        DeleteOutcome::Deleted
    ));
    // A valid S3 object with corrupted protocol checksum must not become zero pages.
    client
        .put_object()
        .bucket(&bucket)
        .key("isolated/corrupt")
        .metadata("sha256", "bad")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b"bad"))
        .send()
        .await
        .unwrap();
    assert!(matches!(
        store.read_current("corrupt").await,
        Err(StoreError::Integrity)
    ));
    store.delete("corrupt").await;
    lazy_sqlite(&store).await;
    if endpoint.starts_with("http://") {
        lost_response(&store, &cfg, credentials.clone()).await;
    }
    client.delete_bucket().bucket(&bucket).send().await.unwrap();
    println!("ST-01/ST-02/ST-04/ST-05: live S3 contract, 16 create/CAS races, ranges, checksums, pagination, delimiter, namespace, PUT limit, deletion: PASS");
}

use engine_sqlite::{worker::WorkerPool, ExecutionLimits, Statement};
use remote_pager::bridge::AsyncSnapshot;
use request_context::RequestContext;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
struct Segments {
    store: S3Store,
    len: usize,
    bytes: AtomicUsize,
}
impl AsyncSnapshot for Segments {
    fn len(&self) -> u64 {
        self.len as u64
    }
    fn read_at(
        &self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = remote_pager::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async move {
            let mut out = Vec::with_capacity(len);
            let mut pos = offset as usize;
            while out.len() < len {
                let segment = pos / (64 * 1024);
                let within = pos % (64 * 1024);
                let body = self
                    .store
                    .read_current(&format!("segments/{segment}"))
                    .await
                    .map_err(|e| remote_pager::Error::Io(e.to_string()))?
                    .body;
                self.bytes.fetch_add(body.len(), Ordering::SeqCst);
                let n = (len - out.len()).min(
                    body.len()
                        .checked_sub(within)
                        .ok_or(remote_pager::Error::Invalid)?,
                );
                if n == 0 {
                    return Err(remote_pager::Error::Invalid);
                }
                out.extend_from_slice(&body[within..within + n]);
                pos += n;
            }
            Ok(out)
        })
    }
}
struct Empty;
impl AsyncSnapshot for Empty {
    fn len(&self) -> u64 {
        0
    }
    fn read_at(
        &self,
        _: u64,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = remote_pager::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async { Err(remote_pager::Error::Invalid) })
    }
}
fn sql(s: &str) -> Statement {
    Statement {
        sql: s.into(),
        parameters: vec![],
    }
}
async fn lazy_sqlite(store: &S3Store) {
    let pool = WorkerPool::new(2, 4, 2).unwrap();
    let tx=pool.prepare("fixture".into(),"test".into(),Arc::new(Empty),4096,vec![sql("CREATE TABLE t(id INTEGER PRIMARY KEY, payload BLOB)"),sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO t SELECT x,zeroblob(1000) FROM n")],ExecutionLimits::default(),RequestContext::new(Duration::from_secs(10))).await.unwrap();
    let len = tx.pages.final_file_size as usize;
    let mut bytes = vec![0; len];
    for (n, p) in tx.pages.pages {
        let start = n as usize * 4096;
        bytes[start..start + 4096].copy_from_slice(&p);
    }
    let mut keys = vec![];
    for (n, chunk) in bytes.chunks(64 * 1024).enumerate() {
        let key = format!("segments/{n}");
        assert!(matches!(
            store.create_if_absent(&key, chunk.to_vec()).await,
            CasOutcome::Applied { .. }
        ));
        keys.push(key);
    }
    drop(bytes); // New cold VFS has no local database image or page cache.
    let source = Arc::new(Segments {
        store: store.clone(),
        len,
        bytes: AtomicUsize::new(0),
    });
    let read = pool
        .prepare(
            "fixture".into(),
            "test".into(),
            source.clone(),
            4096,
            vec![sql("SELECT id FROM t WHERE id=999")],
            ExecutionLimits::default(),
            RequestContext::new(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert!(read.pages.pages.is_empty());
    assert_eq!(read.results[0][0][0], engine_value(999));
    assert!(
        source.bytes.load(Ordering::SeqCst) < len / 2,
        "cold read fetched too much"
    );
    println!(
        "P0 cold S3 point lookup: {} bytes fetched / {} logical bytes",
        source.bytes.load(Ordering::SeqCst),
        len
    );
    let write = pool
        .prepare(
            "fixture".into(),
            "test".into(),
            Arc::new(Segments {
                store: store.clone(),
                len,
                bytes: AtomicUsize::new(0),
            }),
            4096,
            vec![sql("UPDATE t SET payload=x'01' WHERE id=999 RETURNING id")],
            ExecutionLimits::default(),
            RequestContext::new(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert!(!write.pages.pages.is_empty());
    assert!(write.pages.pages.len() < 16);
    for key in keys {
        assert!(matches!(store.delete(&key).await, DeleteOutcome::Deleted));
    }
}
fn engine_value(n: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(n)
}

async fn read_http(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut bytes = vec![];
    let header_end = loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 65536);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let headers = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    let len = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(|v| v.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    assert!(len < 2 * 1024 * 1024);
    assert!(
        !headers.contains("transfer-encoding: chunked"),
        "fixture expects fixed length"
    );
    bytes.resize(header_end + len, 0);
    socket.read_exact(&mut bytes[header_end..]).await.unwrap();
    bytes
}
async fn lost_response(direct: &S3Store, cfg: &S3Config, credentials: Credentials) {
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
    };
    assert!(matches!(
        direct.create_if_absent("lost-head", b"old".to_vec()).await,
        CasOutcome::Applied { .. }
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = url::Url::parse(&cfg.endpoint).unwrap();
    let upstream = format!(
        "{}:{}",
        endpoint.host_str().unwrap(),
        endpoint.port_or_known_default().unwrap()
    );
    // A single owned proxy task, joined below. First GET passes, then one applied
    // PUT response is dropped, finally a stale retry receives the backend's 412.
    let proxy = tokio::spawn(async move {
        for index in 0..3 {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let request = read_http(&mut incoming).await;
            let mut backend = TcpStream::connect(&upstream).await.unwrap();
            backend.write_all(&request).await.unwrap();
            let response = read_http(&mut backend).await;
            let header = String::from_utf8_lossy(&response[..response.len().min(100)]);
            if index == 1 {
                assert!(
                    header.starts_with("HTTP/1.1 200"),
                    "backend did not apply conditional write"
                );
                println!(
                    "ST-03 proxy cut: backend returned 200 after PUT, client response discarded"
                );
            } else {
                incoming.write_all(&response).await.unwrap();
            }
            incoming.shutdown().await.unwrap();
        }
    });
    let proxied = S3Store::new(
        S3Config {
            endpoint: format!("http://{address}"),
            ..cfg.clone()
        },
        credentials,
    )
    .unwrap();
    let old = proxied.read_current("lost-head").await.unwrap();
    assert!(matches!(
        proxied
            .compare_and_swap("lost-head", &old.revision, b"new".to_vec())
            .await,
        CasOutcome::Unknown
    ));
    assert!(matches!(
        proxied
            .compare_and_swap("lost-head", &old.revision, b"new".to_vec())
            .await,
        CasOutcome::PreconditionFailed
    ));
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(direct.read_current("lost-head").await.unwrap().body, b"new");
    direct.delete("lost-head").await;
}
