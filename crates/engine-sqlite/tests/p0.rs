use engine_sqlite::{prepare_transaction, Statement};
use remote_pager::{FrozenPages, Snapshot};
use rusqlite::{types::Value, Connection};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn bundled_sqlite_version() {
    let diagnostics = engine_sqlite::diagnostics().unwrap();
    assert_eq!(diagnostics.version, "3.54.1");
    assert!(diagnostics
        .compile_options
        .iter()
        .any(|option| option == "ENABLE_MATH_FUNCTIONS"));
}

struct Image {
    bytes: Vec<u8>,
    reads: AtomicUsize,
}
impl Snapshot for Image {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> remote_pager::Result<()> {
        self.reads.fetch_add(out.len(), Ordering::Relaxed);
        out.copy_from_slice(&self.bytes[offset as usize..offset as usize + out.len()]);
        Ok(())
    }
}
fn image(bytes: Vec<u8>) -> Arc<Image> {
    Arc::new(Image {
        bytes,
        reads: AtomicUsize::new(0),
    })
}
fn sql(s: &str) -> Statement {
    Statement {
        sql: s.into(),
        parameters: vec![],
    }
}
#[test]
fn math_functions_execute_through_the_authorizer_and_vfs() {
    let tx = prepare_transaction(
        image(vec![]),
        4096,
        1024 * 1024,
        &[Statement {
            sql: "SELECT sqrt(?1), pow(2, 3), log(100), ln(1), \
                  abs(sin(pi() / 2) - 1) < 1e-12, \
                  floor(-1.5), ceil(-1.5), trunc(-1.5), \
                  sqrt(-1), ln(0), sqrt(NULL), sqrt('invalid')"
                .into(),
            parameters: vec![Value::Integer(9)],
        }],
    )
    .unwrap();
    assert!(tx.read_only);
    assert_eq!(
        tx.results[0].rows[0],
        vec![
            Value::Real(3.0),
            Value::Real(8.0),
            Value::Real(2.0),
            Value::Real(0.0),
            Value::Integer(1),
            Value::Real(-2.0),
            Value::Real(-1.0),
            Value::Real(-1.0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ]
    );
}
#[test]
fn json_and_jsonb_functions_execute_through_the_authorizer_and_vfs() {
    let document =
        r#"{"name":"你好 🌍","id":9223372036854775807,"nil":null,"items":[1,2],"count":2}"#;
    for (constructor, setter, storage_type) in
        [("json", "json_set", "text"), ("jsonb", "jsonb_set", "blob")]
    {
        let tx = prepare_transaction(
            image(vec![]),
            4096,
            1024 * 1024,
            &[Statement {
                sql: format!(
                    "WITH doc(value) AS (SELECT {constructor}(?1)) \
                     SELECT typeof(value), json_valid(value, 9), \
                     json_extract(value, '$.name'), jsonb_extract(value, '$.id'), \
                     json_extract(value, '$.nil'), json_extract(value, '$.missing'), \
                     value -> '$.items', value ->> '$.items[1]', \
                     typeof(jsonb_extract(value, '$.items')), \
                     json(jsonb_extract(value, '$.items')), \
                     typeof({setter}(value, '$.count', 3)), \
                     json(json_remove({setter}(value, '$.count', 3), '$.nil')) FROM doc"
                ),
                parameters: vec![Value::Text(document.into())],
            }],
        )
        .unwrap();
        assert!(tx.read_only);
        assert_eq!(
            tx.results[0].rows,
            vec![vec![
                Value::Text(storage_type.into()),
                Value::Integer(1),
                Value::Text("你好 🌍".into()),
                Value::Integer(i64::MAX),
                Value::Null,
                Value::Null,
                Value::Text("[1,2]".into()),
                Value::Integer(2),
                Value::Text("blob".into()),
                Value::Text("[1,2]".into()),
                Value::Text(storage_type.into()),
                Value::Text(
                    r#"{"name":"你好 🌍","id":9223372036854775807,"items":[1,2],"count":3}"#.into()
                ),
            ]],
            "{constructor}"
        );
    }
}

#[test]
fn json_table_functions_accept_text_and_jsonb_without_creating_virtual_tables() {
    for constructor in ["json", "jsonb"] {
        for (each, tree, array_type) in [
            ("json_each", "json_tree", "text"),
            ("jsonb_each", "jsonb_tree", "blob"),
        ] {
            let document = format!(r#"{constructor}('{{"items":[10,20],"name":"你好"}}')"#);
            let tx = prepare_transaction(
                image(vec![]),
                4096,
                1024 * 1024,
                &[
                    sql(&format!(
                        "SELECT key, typeof(value), json(value) \
                         FROM {each}({document}) WHERE type = 'array'"
                    )),
                    sql(&format!(
                        "SELECT fullkey, atom FROM {tree}({document}) \
                         WHERE atom IS NOT NULL ORDER BY fullkey"
                    )),
                ],
            )
            .unwrap();
            assert!(tx.read_only);
            assert_eq!(
                tx.results[0].rows,
                vec![vec![
                    Value::Text("items".into()),
                    Value::Text(array_type.into()),
                    Value::Text("[10,20]".into()),
                ]],
                "{constructor}/{each}"
            );
            assert_eq!(
                tx.results[1].rows,
                vec![
                    vec![Value::Text("$.items[0]".into()), Value::Integer(10)],
                    vec![Value::Text("$.items[1]".into()), Value::Integer(20)],
                    vec![Value::Text("$.name".into()), Value::Text("你好".into())],
                ],
                "{constructor}/{tree}"
            );
        }
    }
}

fn reconstruct(base: &[u8], pages: FrozenPages) -> Vec<u8> {
    let mut bytes = base.to_vec();
    bytes.resize(pages.final_file_size as usize, 0);
    for (n, p) in pages.pages {
        let start = n as usize * pages.page_size;
        bytes[start..start + p.len()].copy_from_slice(&p);
    }
    bytes
}
fn check(bytes: &[u8], expected: i64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reference.db");
    std::fs::write(&path, bytes).unwrap();
    let c = Connection::open(path).unwrap();
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(c
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_none());
    assert_eq!(
        c.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        expected
    );
}
#[test]
fn ddl_dml_returning_reconstruction_all_page_sizes() {
    for page_size in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
        let tx = prepare_transaction(
            image(vec![]),
            page_size,
            16 * 1024 * 1024,
            &[
                sql("CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB, n INTEGER)"),
                sql("CREATE INDEX idx_t ON t(n)"),
                Statement {
                    sql: "INSERT INTO t VALUES(?1,?2,?3) RETURNING id".into(),
                    parameters: vec![
                        Value::Integer(i64::MAX),
                        Value::Blob(vec![23; 10000]),
                        Value::Null,
                    ],
                },
            ],
        )
        .unwrap();
        assert_eq!(tx.results[2][0][0], Value::Integer(i64::MAX));
        let bytes = reconstruct(&[], tx.pages);
        check(&bytes, 1);
        let tx = prepare_transaction(
            image(bytes.clone()),
            page_size,
            16 * 1024 * 1024,
            &[sql("UPDATE t SET n=42 RETURNING n")],
        )
        .unwrap();
        assert_eq!(tx.results[0][0][0], Value::Integer(42));
        check(&reconstruct(&bytes, tx.pages), 1);
    }
}
#[test]
fn failed_statement_and_deferred_constraint_have_no_candidate() {
    let base = prepare_transaction(
        image(vec![]),
        4096,
        1024 * 1024,
        &[sql("CREATE TABLE t(id INTEGER PRIMARY KEY)")],
    )
    .unwrap();
    let bytes = reconstruct(&[], base.pages);
    assert!(prepare_transaction(
        image(bytes.clone()),
        4096,
        1024 * 1024,
        &[
            sql("INSERT INTO t VALUES(1)"),
            sql("INSERT INTO t VALUES(1)")
        ]
    )
    .is_err());
    check(&bytes, 0);
    assert!(prepare_transaction(
        image(bytes),
        4096,
        1024 * 1024,
        &[
            sql("CREATE TABLE child(id REFERENCES t(id) DEFERRABLE INITIALLY DEFERRED)"),
            sql("INSERT INTO child VALUES(1)")
        ]
    )
    .is_err());
}
#[test]
fn forbidden_sql_and_multiple_statements_are_rejected() {
    for statement in [
        "ATTACH ':memory:' AS other",
        "PRAGMA journal_mode=WAL",
        "COMMIT",
        "SAVEPOINT x",
        "CREATE TABLE t(x); INSERT INTO t VALUES(1)",
        "SELECT 1; SELECT 2",
        "VACUUM",
    ] {
        assert!(
            prepare_transaction(image(vec![]), 4096, 1024 * 1024, &[sql(statement)]).is_err(),
            "{statement}"
        );
    }
}
#[test]
fn cold_point_lookup_is_lazy_and_candidates_are_isolated() {
    let tx=prepare_transaction(image(vec![]),4096,16*1024*1024,&[sql("CREATE TABLE t(id INTEGER PRIMARY KEY, payload BLOB)"),sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO t SELECT x,zeroblob(1000) FROM n")]).unwrap();
    let bytes = reconstruct(&[], tx.pages);
    check(&bytes, 2000);
    let base = image(bytes.clone());
    let read = prepare_transaction(
        base.clone(),
        4096,
        1024 * 1024,
        &[sql("SELECT id FROM t WHERE id=999")],
    )
    .unwrap();
    assert_eq!(read.results[0][0][0], Value::Integer(999));
    assert!(base.reads.load(Ordering::Relaxed) < bytes.len() / 4);
    assert!(read.pages.pages.is_empty());
    let a = prepare_transaction(
        base.clone(),
        4096,
        1024 * 1024,
        &[sql("DELETE FROM t WHERE id=1")],
    )
    .unwrap();
    let b = prepare_transaction(
        base,
        4096,
        1024 * 1024,
        &[sql("DELETE FROM t WHERE id IN (2,3)")],
    )
    .unwrap();
    check(&reconstruct(&bytes, a.pages), 1999);
    check(&reconstruct(&bytes, b.pages), 1998);
    check(&bytes, 2000);
}
#[test]
fn savepoint_rollback_after_cache_spill_freezes_final_pages() {
    use remote_pager::CowFile;
    use sqlite_vfs::Workspace;
    let ws = Workspace::open(CowFile::new(image(vec![]), 4096, 16 * 1024 * 1024).unwrap()).unwrap();
    ws.connection().execute_batch("PRAGMA journal_mode=DELETE; PRAGMA cache_size=4; BEGIN; CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB); INSERT INTO t VALUES(1,zeroblob(10000)); SAVEPOINT s; WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO t SELECT x,zeroblob(1000) FROM n; ROLLBACK TO s; RELEASE s; COMMIT;").unwrap();
    check(&reconstruct(&[], ws.freeze().unwrap()), 1);
}

#[test]
fn snapshot_io_failure_and_panic_do_not_escape_ffi() {
    struct Broken(bool);
    impl Snapshot for Broken {
        fn len(&self) -> u64 {
            4096
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> remote_pager::Result<()> {
            if self.0 {
                panic!("injected snapshot panic");
            }
            Err(remote_pager::Error::Io("injected missing object".into()))
        }
    }
    for panic in [false, true] {
        assert!(prepare_transaction(
            Arc::new(Broken(panic)),
            4096,
            1024 * 1024,
            &[sql("SELECT 1")]
        )
        .is_err());
    }
}

#[test]
fn journal_disk_quota_rejects_candidate_and_preserves_base() {
    use engine_sqlite::{prepare_with_limits, ExecutionLimits};
    let tx=prepare_transaction(image(vec![]),4096,16*1024*1024,&[sql("CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB)"),sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<200) INSERT INTO t SELECT x,zeroblob(2000) FROM n")]).unwrap();
    let bytes = reconstruct(&[], tx.pages);
    let limits = ExecutionLimits {
        max_local_bytes: 512,
        ..Default::default()
    };
    assert!(prepare_with_limits(
        image(bytes.clone()),
        4096,
        &[sql("UPDATE t SET b=zeroblob(3000)")],
        &limits,
        request_context::RequestContext::new(std::time::Duration::from_secs(5))
    )
    .is_err());
    check(&bytes, 200);
}
