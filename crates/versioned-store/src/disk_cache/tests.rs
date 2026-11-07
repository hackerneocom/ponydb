use super::*;
fn cache(dir: &std::path::Path, namespace: &str, bytes: u64) -> Arc<DiskCache> {
    DiskCache::new(
        DiskCacheOptions {
            directory: dir.into(),
            max_bytes: bytes,
        },
        Some(namespace.into()),
    )
    .unwrap()
}
fn key(body: &[u8]) -> Key {
    (
        "databases/test/object".into(),
        body.len() as u64,
        format!("{:x}", Sha256::digest(body)),
    )
}
fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(5)
}
async fn idle(c: &DiskCache) {
    drop(c.gate.acquire().await.unwrap());
}
async fn write(c: &Arc<DiskCache>, body: &[u8]) {
    idle(c).await;
    c.write(&key(body), Arc::new(body.to_vec()), deadline())
        .await;
    idle(c).await;
}

#[tokio::test]
async fn persists_verifies_and_isolates() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    write(&c, b"first").await;
    let other = cache(dir.path(), "a", 1024);
    assert_eq!(
        other
            .read(&key(b"first"), deadline())
            .await
            .unwrap()
            .as_slice(),
        b"first"
    );
    idle(&other).await;
    assert!(cache(dir.path(), "b", 1024)
        .read(&key(b"first"), deadline())
        .await
        .is_none());
    let db = c.open().unwrap();
    db.execute("UPDATE objects SET body=?1", [b"wrong".as_slice()])
        .unwrap();
    assert!(other.read(&key(b"first"), deadline()).await.is_none());
    write(&other, b"first").await;
    assert!(other.read(&key(b"first"), deadline()).await.is_some());
}
#[tokio::test]
async fn quotas_restart_shrink_and_oversized_bypass() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 10);
    write(&c, b"first").await;
    write(&c, b"second").await;
    let db = c.open().unwrap();
    assert_eq!(
        db.query_row("SELECT bytes FROM usage", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    write(&c, b"too large to cache").await;
    assert_eq!(
        db.query_row("SELECT bytes FROM usage", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    let smaller = cache(dir.path(), "a", 4);
    assert!(smaller.read(&key(b"second"), deadline()).await.is_none());
    assert_eq!(
        db.query_row("SELECT bytes FROM usage", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(DiskCache::new(
        DiskCacheOptions {
            directory: dir.path().join("disabled"),
            max_bytes: 0
        },
        Some("a".into())
    )
    .is_none());
    assert!(!dir.path().join("disabled").exists());
}
#[tokio::test]
async fn busy_initialization_retries_and_unknown_format_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    let db = c.open().unwrap();
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(c.read(&key(b"first"), deadline()).await.is_none());
    assert!(!c.state.lock().unwrap().disabled);
    db.execute_batch("ROLLBACK").unwrap();
    write(&c, b"first").await;
    assert!(c.read(&key(b"first"), deadline()).await.is_some());
    idle(&c).await;
    db.execute_batch("PRAGMA user_version=999").unwrap();
    let unknown = cache(dir.path(), "a", 1024);
    assert!(unknown.read(&key(b"first"), deadline()).await.is_none());
    assert!(unknown.state.lock().unwrap().disabled);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
}
#[tokio::test]
async fn timeout_keeps_permit_until_task_exits() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_cache = c.clone();
    let job = tokio::spawn(async move {
        task_cache
            .run(
                tokio::time::Instant::now() + Duration::from_millis(100),
                move |_, _, _| {
                    let _ = started_tx.send(());
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
            .await
    });
    started_rx.await.unwrap();
    assert!(job.await.unwrap().is_none());
    assert!(c.gate.clone().try_acquire_owned().is_err());
    assert!(c.read(&key(b"first"), deadline()).await.is_none());
    release_tx.send(()).unwrap();
    idle(&c).await;
    write(&c, b"first").await;
    assert!(c.read(&key(b"first"), deadline()).await.is_some());
}
#[tokio::test]
async fn warm_hits_do_not_update_lru_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    write(&c, b"first").await;
    let db = c.open().unwrap();
    let version: i64 = db
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    for _ in 0..10 {
        assert!(c.read(&key(b"first"), deadline()).await.is_some());
        idle(&c).await;
    }
    assert_eq!(
        version,
        db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        db.query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

// A real child process, using the test binary, proves persistence beyond Arc/reader lifetimes.
#[test]
fn process_worker() {
    let Ok(dir) = std::env::var("PONYDB_DISK_TEST_DIR") else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let c = cache(std::path::Path::new(&dir), "process", 1024);
        if std::env::var("PONYDB_DISK_TEST_MODE").unwrap() == "write" {
            for _ in 0..50 {
                write(&c, b"persistent").await;
                if c.read(&key(b"persistent"), deadline()).await.is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("persistent fill failed");
        } else {
            assert!(c.read(&key(b"persistent"), deadline()).await.is_some());
        }
    });
}
#[test]
fn independent_processes_share_verified_bytes() {
    let dir = tempfile::tempdir().unwrap();
    for mode in ["write", "read"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "disk_cache::tests::process_worker"])
            .env("PONYDB_DISK_TEST_DIR", dir.path())
            .env("PONYDB_DISK_TEST_MODE", mode)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[tokio::test]
async fn concurrent_initialization_remains_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let c = cache(dir.path(), "a", 1024);
        jobs.push(tokio::spawn(async move {
            for _ in 0..200 {
                write(&c, b"first").await;
                if c.read(&key(b"first"), deadline()).await.is_some() {
                    return;
                }
                idle(&c).await;
                assert!(!c.state.lock().unwrap().disabled);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("cache did not recover from initialization contention");
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
}
#[tokio::test]
async fn invalid_directory_and_full_database_are_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, b"keep").unwrap();
    let c = cache(&file, "a", 1024);
    assert!(c.read(&key(b"first"), deadline()).await.is_none());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    let c = cache(dir.path(), "a", 1024);
    let result = c
        .run(deadline(), |_, db, _| {
            let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            db.pragma_update(None, "max_page_count", pages)?;
            db.execute(
                "INSERT INTO objects VALUES('full', zeroblob(1048576), 0)",
                [],
            )
        })
        .await;
    assert!(result.is_none());
    assert!(c.state.lock().unwrap().retry.is_some());
    assert!(!c.state.lock().unwrap().disabled);
}

#[test]
fn cancelled_queued_work_does_not_create_cache_files() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dir.path().join("cache");
    let c = cache(&directory, "a", 1024);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.await.unwrap();
        let reader = c.clone();
        let job = tokio::spawn(async move { reader.read(&key(b"first"), deadline()).await });
        tokio::task::yield_now().await;
        job.abort();
        let _ = job.await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        idle(&c).await;
        assert!(!directory.exists());
    });
}
#[tokio::test]
async fn read_only_access_converges_after_large_budget_reduction() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    let mut db = c.open().unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..300 {
        tx.execute(
            "INSERT INTO objects VALUES(?1,zeroblob(10),0)",
            [i.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    for _ in 0..3 {
        c.read(&key(b"missing"), deadline()).await;
    }
    let bytes: i64 = db
        .query_row("SELECT bytes FROM usage", [], |r| r.get(0))
        .unwrap();
    assert!(bytes <= 1024);
}
#[tokio::test]
async fn lru_touch_and_reclamation_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024 * 1024);
    write(&c, &vec![1; 512 * 1024]).await;
    let db = c.open().unwrap();
    db.execute("DELETE FROM objects", []).unwrap();
    write(&c, b"first").await;
    let before: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    db.execute("UPDATE objects SET touched=0", []).unwrap();
    c.state.lock().unwrap().maintenance = None;
    assert!(c.read(&key(b"first"), deadline()).await.is_some());
    idle(&c).await;
    assert!(
        db.query_row("SELECT touched FROM objects", [], |r| r.get::<_, i64>(0))
            .unwrap()
            > 0
    );
    let after: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    assert!(after < before);
    assert!(before - after <= 64);
}
#[test]
#[cfg(unix)]
fn newly_created_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache");
    let c = cache(&path, "a", 1024);
    c.open().unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(path.join("objects-v1.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn concurrent_processes_initialize_and_account_once() {
    let dir = tempfile::tempdir().unwrap();
    let children: Vec<_> = (0..4)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "disk_cache::tests::process_worker"])
                .env("PONYDB_DISK_TEST_DIR", dir.path())
                .env("PONYDB_DISK_TEST_MODE", "write")
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let c = cache(dir.path(), "process", 1024);
    let db = c.open().unwrap();
    assert_eq!(
        db.query_row("SELECT bytes,entries FROM usage", [], |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        (10, 1)
    );
}

#[tokio::test]
async fn structural_corruption_disables_without_deleting_database() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 1024);
    let db = c.open().unwrap();
    db.execute_batch("DROP TABLE usage").unwrap();
    assert!(c.read(&key(b"first"), deadline()).await.is_none());
    assert!(c.state.lock().unwrap().disabled);
    assert!(dir.path().join("objects-v1.sqlite3").exists());
}

#[tokio::test]
async fn speculative_write_admission_never_evicts_existing_objects() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache(dir.path(), "a", 8);
    write(&c, b"hotbody").await;
    for n in 0..20 {
        let body = n.to_string().into_bytes();
        c.write_if_room(&key(&body), Arc::new(body), deadline())
            .await;
        idle(&c).await;
    }
    assert!(c.read(&key(b"hotbody"), deadline()).await.is_some());
    idle(&c).await;
    let db = c.open().unwrap();
    assert_eq!(
        db.query_row("SELECT bytes,entries FROM usage", [], |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        (8, 2)
    );
}
