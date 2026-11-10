use engine_sqlite::{worker::WorkerPool, Error, ExecutionLimits, Statement};
use remote_pager::{bridge::AsyncSnapshot, Result};
use request_context::RequestContext;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
struct Source {
    bytes: Vec<u8>,
    delay: Duration,
    calls: AtomicUsize,
}
impl AsyncSnapshot for Source {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(
        &self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(self.bytes[offset as usize..offset as usize + len].to_vec())
        })
    }
}
fn source(len: usize, delay: Duration) -> Arc<Source> {
    Arc::new(Source {
        bytes: vec![0; len],
        delay,
        calls: AtomicUsize::new(0),
    })
}
fn sql(s: &str) -> Statement {
    Statement {
        sql: s.into(),
        parameters: vec![],
    }
}
fn ctx() -> RequestContext {
    RequestContext::new(Duration::from_secs(5))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_calls_release_admission_before_the_result_is_observed() {
    let pool = WorkerPool::new(1, 1, 1).unwrap();
    for _ in 0..128 {
        pool.inspect(
            "db".into(),
            "user".into(),
            source(0, Duration::ZERO),
            4096,
            vec![sql("CREATE TABLE t(x)")],
            ExecutionLimits::default(),
            ctx(),
        )
        .await
        .unwrap();
        pool.prepare(
            "db".into(),
            "user".into(),
            source(0, Duration::ZERO),
            4096,
            vec![sql("CREATE TABLE t(x)")],
            ExecutionLimits::default(),
            ctx(),
        )
        .await
        .unwrap();
        let error = pool
            .prepare(
                "other".into(),
                "user".into(),
                source(0, Duration::ZERO),
                4096,
                vec![sql("SELECT * FROM absent")],
                ExecutionLimits::default(),
                ctx(),
            )
            .await
            .unwrap_err();
        assert!(!matches!(error, Error::Busy), "{error:?}");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn current_thread_runtime_does_not_deadlock_sql_or_io() {
    let pool = WorkerPool::new(2, 4, 4).unwrap();
    let base = source(0, Duration::ZERO);
    let result = pool
        .prepare(
            "db".into(),
            "user".into(),
            base,
            4096,
            vec![
                sql("CREATE TABLE t(x)"),
                sql("INSERT INTO t VALUES(1) RETURNING x"),
            ],
            ExecutionLimits::default(),
            ctx(),
        )
        .await
        .unwrap();
    assert!(!result.pages.pages.is_empty());
    // Re-open the generated file via delayed async reads on this one-thread runtime.
    let mut bytes = vec![0; result.pages.final_file_size as usize];
    for (n, p) in result.pages.pages {
        let start = n as usize * 4096;
        bytes[start..start + 4096].copy_from_slice(&p);
    }
    let base = Arc::new(Source {
        bytes,
        delay: Duration::from_millis(2),
        calls: AtomicUsize::new(0),
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        pool.prepare(
            "db".into(),
            "user".into(),
            base.clone(),
            4096,
            vec![sql("SELECT x FROM t")],
            ExecutionLimits::default(),
            ctx(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.pages.pages.is_empty());
    assert!(base.calls.load(Ordering::SeqCst) > 0);
}
#[tokio::test]
async fn io_deadline_cancellation_and_queue_limits() {
    let pool = WorkerPool::new(1, 1, 2).unwrap();
    let slow = source(4096, Duration::from_secs(60));
    let first_ctx = ctx();
    let first = pool.prepare(
        "a".into(),
        "u".into(),
        slow.clone(),
        4096,
        vec![sql("SELECT 1")],
        ExecutionLimits::default(),
        first_ctx.clone(),
    );
    tokio::pin!(first);
    tokio::select! {r=&mut first=>panic!("finished early {r:?}"),_=tokio::time::sleep(Duration::from_millis(30))=>{}}
    assert!(slow.calls.load(Ordering::SeqCst) > 0);
    let busy = pool
        .prepare(
            "a".into(),
            "v".into(),
            source(0, Duration::ZERO),
            4096,
            vec![],
            ExecutionLimits::default(),
            ctx(),
        )
        .await;
    assert!(matches!(busy, Err(Error::Busy)));
    let queued = pool.prepare(
        "b".into(),
        "u".into(),
        source(0, Duration::ZERO),
        4096,
        vec![],
        ExecutionLimits::default(),
        RequestContext::new(Duration::from_millis(10)),
    );
    tokio::pin!(queued);
    tokio::select! {r=&mut queued=>panic!("finished early {r:?}"),_=tokio::time::sleep(Duration::from_millis(30))=>{}}
    let full = pool
        .prepare(
            "c".into(),
            "v".into(),
            source(0, Duration::ZERO),
            4096,
            vec![],
            ExecutionLimits::default(),
            ctx(),
        )
        .await;
    assert!(matches!(full, Err(Error::Busy)));
    first_ctx.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), &mut first)
            .await
            .unwrap(),
        Err(Error::Stopped(_))
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), &mut queued)
            .await
            .unwrap(),
        Err(Error::Stopped(_))
    ));
    let timed = pool
        .prepare(
            "d".into(),
            "u".into(),
            slow,
            4096,
            vec![],
            ExecutionLimits::default(),
            RequestContext::new(Duration::from_millis(20)),
        )
        .await;
    assert!(matches!(timed, Err(Error::Stopped(_))));
}
#[tokio::test]
async fn cpu_deadline_and_dropped_future_release_worker() {
    let pool = WorkerPool::new(1, 1, 1).unwrap();
    let r=pool.prepare("a".into(),"u".into(),source(0,Duration::ZERO),4096,vec![sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n")],ExecutionLimits::default(),RequestContext::new(Duration::from_millis(20))).await;
    assert!(matches!(r, Err(Error::Stopped(_))));
    let slow = source(4096, Duration::from_secs(60));
    let context = ctx();
    {
        let f = pool.prepare(
            "a".into(),
            "u".into(),
            slow,
            4096,
            vec![],
            ExecutionLimits::default(),
            context.clone(),
        );
        tokio::pin!(f);
        tokio::select! {_=&mut f=>panic!("unexpected completion"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
    }
    assert!(context.check().is_err());
    // Cancellation is immediate, but the worker releases admission after cleanup.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match pool
                .prepare(
                    "a".into(),
                    "u".into(),
                    source(0, Duration::ZERO),
                    4096,
                    vec![sql("SELECT 1")],
                    ExecutionLimits::default(),
                    ctx(),
                )
                .await
            {
                Ok(_) => break,
                Err(Error::Busy) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("request after cancellation failed: {error:?}"),
            }
        }
    })
    .await
    .expect("cancelled worker did not release admission within 2 seconds");
}

#[tokio::test]
async fn inspection_compiles_without_stepping_sql() {
    let pool = WorkerPool::new(1, 4, 4).unwrap();
    for (statement, readonly) in [
        ("SELECT abs(-9223372036854775808)", true),
        ("CREATE TABLE t(x)", false),
    ] {
        let inspected = pool
            .inspect(
                "db".into(),
                "user".into(),
                source(0, Duration::ZERO),
                4096,
                vec![sql(statement)],
                ExecutionLimits::default(),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(inspected.read_only, readonly);
        assert!(inspected.results.is_empty());
        assert!(inspected.pages.pages.is_empty());
    }
    assert!(pool
        .prepare(
            "db".into(),
            "user".into(),
            source(0, Duration::ZERO),
            4096,
            vec![sql("SELECT abs(-9223372036854775808)")],
            ExecutionLimits::default(),
            ctx()
        )
        .await
        .is_err());
}
