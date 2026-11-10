"""End-to-end coverage for execute."""
import base64
import json
import time

from .support import CliTestCase, check, row

COMMANDS = [('db', 'execute')]


class ExecuteTests(CliTestCase):
    def assert_sql_rows(self, label, db, sql, expected):
        result = self.run_cli(label, db + ['execute', sql])
        self.assertEqual([[cell.get('value') for cell in values]
                          for values in result['results'][0]['rows']], expected)

    def test_math(self):
        db = self.create_database()
        math = self.run_cli('bundled math functions', db + ['execute', 'SELECT sqrt(9), pow(2, 3), log(100), sqrt(-1)'])
        check(row(math) == [{'type': 'real', 'value': 3.0},
                            {'type': 'real', 'value': 8.0},
                            {'type': 'real', 'value': 2.0},
                            {'type': 'null'}], 'math result types or values differ')
        self.run_cli('non-finite math result', db + ['execute', 'SELECT exp(1000)'],
            code=1, error='SQL_ERROR')

    def test_json_and_jsonb(self):
        db = self.create_database()
        document = {'name': '你好 🌍', 'count': 1, 'items': [10, 20]}
        self.run_cli('JSON and JSONB insert', db + ['execute'], {'statements': [
            {'sql': 'CREATE TABLE json_docs(text_doc TEXT, binary_doc BLOB)'},
            {'sql': 'INSERT INTO json_docs VALUES(json(?1), jsonb(?1))',
             'params': [{'type': 'text', 'value': json.dumps(document, ensure_ascii=False)}]},
        ]})
        stored = row(self.run_cli('JSON and JSONB fresh process persistence', db + ['execute',
                         'SELECT text_doc, json(binary_doc), binary_doc, json_valid(binary_doc, 8) FROM json_docs']))
        check(all(value['type'] == 'text' and json.loads(value['value']) == document
                  for value in stored[:2]), 'JSON text or JSONB content differs')
        check(stored[2]['type'] == 'blob' and stored[2]['value'], 'JSONB is not a blob')
        check(stored[3] == {'type': 'integer', 'value': '1'}, 'invalid stored JSONB')
        restored = row(self.run_cli('JSONB blob parameter roundtrip', db + ['execute'],
                           {'sql': 'SELECT json(?1)', 'params': [stored[2]]}))
        check(restored[0]['type'] == 'text' and json.loads(restored[0]['value']) == document,
              'JSONB blob parameter did not roundtrip')
        traversed = self.run_cli('JSON and JSONB table functions', db + ['execute',
                        "SELECT a.atom, b.atom FROM json_each('[10,20]') AS a "
                        "JOIN jsonb_each(jsonb('[10,20]')) AS b ON a.key=b.key ORDER BY a.key"])
        check(traversed['results'][0]['rows'] == [
            [{'type': 'integer', 'value': '10'}] * 2,
            [{'type': 'integer', 'value': '20'}] * 2,
        ], 'JSON table traversal differs')
        updated = row(self.run_cli('JSON and JSONB update', db + ['execute',
                          "UPDATE json_docs SET text_doc=json_set(text_doc, '$.count', 2), "
                          "binary_doc=jsonb_set(binary_doc, '$.count', 2) "
                          'RETURNING text_doc, json(binary_doc)']))
        document['count'] = 2
        check(all(value['type'] == 'text' and json.loads(value['value']) == document
                  for value in updated), 'JSON or JSONB update differs')
        self.run_cli('malformed JSON rolls back JSON and JSONB updates', db + ['execute'],
            {'statements': [
                {'sql': "UPDATE json_docs SET text_doc=json_set(text_doc, '$.count', 99), "
                        "binary_doc=jsonb_set(binary_doc, '$.count', 99)"},
                {'sql': "SELECT jsonb('{invalid')"},
            ]}, code=1, error='SQL_ERROR')
        counts = row(self.run_cli('JSON and JSONB rollback persisted', db + ['execute',
                         "SELECT text_doc ->> '$.count', jsonb_extract(binary_doc, '$.count') FROM json_docs"]))
        check(counts == [{'type': 'integer', 'value': '2'}] * 2, 'JSON update was not rolled back')
        self.run_cli('drop JSON test table', db + ['execute', 'DROP TABLE json_docs'])

    def test_typed_parameters_and_transactions(self):
        db = self.create_database()
        self.run_cli('DDL', db + ['execute', 'CREATE TABLE t(id INTEGER PRIMARY KEY, n INTEGER UNIQUE, txt TEXT, b BLOB, r REAL, nil)'])
        sql = self.tmp / 'insert.sql'
        sql.write_text('\ufeffINSERT INTO t VALUES(1,?,?,?,?,?)', encoding='utf-8')
        params = self.tmp / 'params.json'
        params.write_text(json.dumps([{'type': 'integer', 'value': '9223372036854775807'}, {'type': 'text', 'value': '你好 🌍'}, {'type': 'blob', 'value': 'AP8='}, {'type': 'real', 'value': 1.25}, {'type': 'null'}]))
        insert_args = db + ['execute', '--sql-file', str(sql), '--params-file', str(params)]
        inserted = self.run_cli('BOM SQL and typed params', insert_args)
        self.run_cli('duplicate insert executes and fails constraint', insert_args, code=1, error='SQL_ERROR')
        selected = self.run_cli('fresh process persistence', db + ['execute', 'SELECT n,txt,b,r,nil FROM t'])
        check(row(selected) == json.loads(params.read_text()), 'typed values differ')
        table = self.run_cli('table output', db + ['--output', 'table', 'execute', 'SELECT txt FROM t'])
        check('你好 🌍' in table, 'table output')
        self.run_cli('transaction commit', db + ['execute'], {'statements': [{'sql': 'INSERT INTO t(id,n) VALUES(2,2)'}, {'sql': 'INSERT INTO t(id,n) VALUES(3,3)'}]})
        self.run_cli('transaction rollback', db + ['execute'], {'statements': [{'sql': 'INSERT INTO t(id,n) VALUES(4,4)'}, {'sql': 'INSERT INTO t(id,n) VALUES(5,2)'}]}, code=1, error='SQL_ERROR')
        check(row(self.run_cli('rollback persisted no partial rows', db + ['execute', 'SELECT count(*) FROM t']))[0]['value'] == '3', 'transaction not atomic')
        self.run_cli('SQL stdin', db + ['execute', '--sql-file', '-'], text='SELECT 42')
        for label, content in [('duplicate JSON key', '{"sql":"SELECT 1","sql":"SELECT 2"}'), ('unknown DTO field', '{"sql":"SELECT 1","unknown":true}')]:
            self.run_cli(label, db + ['execute', '--input', '-'], text=content, code=2, error='INVALID_REQUEST')
        self.run_cli('conflicting flag and DTO', db + ['execute', 'SELECT 2'], {'sql': 'SELECT 1'}, code=2, error='INVALID_REQUEST')
        self.run_cli('multiple stdin sources', db + ['execute', '--sql-file', '-', '--params-file', '-'], code=2, error='INVALID_REQUEST')
        self.run_cli('invalid deadline', ['--deadline-ms', '0', 'db', 'main'], code=2, error='INVALID_REQUEST')

    def test_indexed_table_crud_and_trigger_effects(self):
        db = self.create_database()
        self.run_cli('indexed table and audit triggers', db + ['execute'], {'statements': [
            {'sql': 'CREATE TABLE items(id INTEGER PRIMARY KEY, sku TEXT NOT NULL, qty INTEGER)'},
            {'sql': 'CREATE UNIQUE INDEX items_sku ON items(sku)'},
            {'sql': 'CREATE INDEX items_qty ON items(qty)'},
            {'sql': 'CREATE TABLE audit(seq INTEGER PRIMARY KEY, action TEXT, item_id INTEGER, '
                    'old_sku TEXT, new_sku TEXT, old_qty INTEGER, new_qty INTEGER)'},
            {'sql': "CREATE TRIGGER items_insert AFTER INSERT ON items BEGIN "
                    "INSERT INTO audit(action,item_id,new_sku,new_qty) VALUES('insert',NEW.id,NEW.sku,NEW.qty); END"},
            {'sql': "CREATE TRIGGER items_update AFTER UPDATE ON items BEGIN "
                    'INSERT INTO audit(action,item_id,old_sku,new_sku,old_qty,new_qty) '
                    "VALUES('update',NEW.id,OLD.sku,NEW.sku,OLD.qty,NEW.qty); END"},
            {'sql': "CREATE TRIGGER items_delete AFTER DELETE ON items BEGIN "
                    "INSERT INTO audit(action,item_id,old_sku,old_qty) VALUES('delete',OLD.id,OLD.sku,OLD.qty); END"},
        ]})
        self.run_cli('insert into indexed table', db + ['execute',
                     "INSERT INTO items VALUES(1,'alpha',10),(2,'beta',20)"])
        self.assert_sql_rows('unique index lookup after insert', db,
                             "SELECT id,sku,qty FROM items INDEXED BY items_sku WHERE sku='alpha'",
                             [['1', 'alpha', '10']])
        before = self.run_cli('HEAD before duplicate indexed inserts', db)
        self.run_cli('unique index rejects duplicate insert', db + ['execute',
                     "INSERT INTO items VALUES(3,'alpha',30)"], code=1, error='SQL_ERROR')
        self.assertEqual(self.run_cli('HEAD after duplicate indexed insert', db)['etag'], before['etag'])
        self.assert_sql_rows('update indexed columns', db,
                             "UPDATE items SET sku='gamma',qty=15 WHERE sku='alpha' RETURNING id,sku,qty",
                             [['1', 'gamma', '15']])
        self.assert_sql_rows('old unique index entry removed', db,
                             "SELECT id FROM items INDEXED BY items_sku WHERE sku='alpha'", [])
        self.assert_sql_rows('new unique index entry persisted', db,
                             "SELECT id FROM items INDEXED BY items_sku WHERE sku='gamma'", [['1']])
        self.assert_sql_rows('secondary index entry updated', db,
                             'SELECT id,qty FROM items INDEXED BY items_qty WHERE qty IN (10,15)', [['1', '15']])
        before = self.run_cli('HEAD before duplicate indexed update', db)
        self.run_cli('unique index rejects duplicate update', db + ['execute',
                     "UPDATE items SET sku='beta' WHERE id=1"], code=1, error='SQL_ERROR')
        self.assertEqual(self.run_cli('HEAD after duplicate indexed update', db)['etag'], before['etag'])
        self.assert_sql_rows('delete from indexed table', db,
                             "DELETE FROM items WHERE sku='beta' RETURNING id", [['2']])
        self.assert_sql_rows('unique index entry deleted', db,
                             "SELECT id FROM items INDEXED BY items_sku WHERE sku='beta'", [])
        self.assert_sql_rows('secondary index entry deleted', db,
                             'SELECT id FROM items INDEXED BY items_qty WHERE qty=20', [])
        self.run_cli('reuse deleted unique index key', db + ['execute', "INSERT INTO items VALUES(3,'beta',30)"])
        expected_items = [['1', 'gamma', '15'], ['3', 'beta', '30']]
        expected_audit = [
            ['insert', '1', None, 'alpha', None, '10'],
            ['insert', '2', None, 'beta', None, '20'],
            ['update', '1', 'alpha', 'gamma', '10', '15'],
            ['delete', '2', 'beta', None, '20', None],
            ['insert', '3', None, 'beta', None, '30'],
        ]
        audit_sql = 'SELECT action,item_id,old_sku,new_sku,old_qty,new_qty FROM audit ORDER BY seq'
        self.assert_sql_rows('all table trigger effects persisted', db, audit_sql, expected_audit)
        before = self.run_cli('HEAD before indexed transaction rollback', db)
        self.run_cli('indexed writes and trigger effects roll back together', db + ['execute'], {'statements': [
            {'sql': "UPDATE items SET sku='rolled-back',qty=99 WHERE id=1"},
            {'sql': 'DELETE FROM items WHERE id=3'},
            {'sql': "INSERT INTO items VALUES(4,'rolled-back',40)"},
        ]}, code=1, error='SQL_ERROR')
        self.assertEqual(self.run_cli('HEAD after indexed transaction rollback', db)['etag'], before['etag'])
        for index in ('items_sku', 'items_qty'):
            with self.subTest(index=index):
                self.assert_sql_rows('index contents after rollback', db,
                                     f'SELECT id,sku,qty FROM items INDEXED BY {index} ORDER BY id', expected_items)
        self.assert_sql_rows('table trigger effects rolled back', db, audit_sql, expected_audit)

    def test_view_crud_with_instead_of_triggers(self):
        db = self.create_database()
        self.run_cli('view and backing table', db + ['execute'], {'statements': [
            {'sql': 'CREATE TABLE records(id INTEGER PRIMARY KEY, title TEXT UNIQUE, active INTEGER)'},
            {'sql': "INSERT INTO records VALUES(1,'hidden',0)"},
            {'sql': 'CREATE VIEW active_records AS SELECT id,title FROM records WHERE active=1'},
        ]})
        self.assert_sql_rows('view filter excludes inactive rows', db, 'SELECT * FROM active_records', [])
        before = self.run_cli('HEAD before writes to view without triggers', db)
        for sql in ("INSERT INTO active_records VALUES(2,'first')",
                    "UPDATE active_records SET title='changed'", 'DELETE FROM active_records'):
            with self.subTest(sql=sql):
                failure = self.run_cli('view without write trigger rejects DML', db + ['execute', sql],
                                       code=1, error='SQL_ERROR')
                self.assertIn('view', failure['message'])
        self.assertEqual(self.run_cli('HEAD after writes to view without triggers', db)['etag'], before['etag'])
        self.run_cli('view write triggers', db + ['execute'], {'statements': [
            {'sql': 'CREATE TRIGGER view_insert INSTEAD OF INSERT ON active_records BEGIN '
                    'INSERT INTO records(id,title,active) VALUES(NEW.id,NEW.title,1); END'},
            {'sql': 'CREATE TRIGGER view_update INSTEAD OF UPDATE ON active_records BEGIN '
                    'UPDATE records SET id=NEW.id,title=NEW.title WHERE id=OLD.id; END'},
            {'sql': 'CREATE TRIGGER view_delete INSTEAD OF DELETE ON active_records BEGIN '
                    'DELETE FROM records WHERE id=OLD.id; END'},
        ]})
        self.run_cli('insert through view', db + ['execute',
                     "INSERT INTO active_records VALUES(2,'first'),(3,'second')"])
        self.assert_sql_rows('view insert persisted', db, 'SELECT id,title FROM active_records ORDER BY id',
                             [['2', 'first'], ['3', 'second']])
        self.assert_sql_rows('view insert changed backing table', db, 'SELECT * FROM records ORDER BY id',
                             [['1', 'hidden', '0'], ['2', 'first', '1'], ['3', 'second', '1']])
        self.run_cli('update through view', db + ['execute', "UPDATE active_records SET title='updated' WHERE id=2"])
        self.assert_sql_rows('view update persisted', db, 'SELECT id,title FROM active_records WHERE id=2',
                             [['2', 'updated']])
        self.assert_sql_rows('view update changed backing table', db, 'SELECT title,active FROM records WHERE id=2',
                             [['updated', '1']])
        self.run_cli('delete through view', db + ['execute', 'DELETE FROM active_records WHERE id=3'])
        self.assert_sql_rows('view delete persisted', db, 'SELECT id,title FROM active_records ORDER BY id',
                             [['2', 'updated']])
        backing = [['1', 'hidden', '0'], ['2', 'updated', '1']]
        self.assert_sql_rows('view delete changed backing table', db, 'SELECT * FROM records ORDER BY id', backing)
        before = self.run_cli('HEAD before view transaction rollback', db)
        self.run_cli('view trigger writes roll back on backing constraint', db + ['execute'], {'statements': [
            {'sql': "UPDATE active_records SET title='transient' WHERE id=2"},
            {'sql': "INSERT INTO active_records VALUES(4,'transient')"},
        ]}, code=1, error='SQL_ERROR')
        self.assertEqual(self.run_cli('HEAD after view transaction rollback', db)['etag'], before['etag'])
        self.assert_sql_rows('view contents after rollback', db, 'SELECT id,title FROM active_records', [['2', 'updated']])
        self.assert_sql_rows('view trigger effects rolled back', db, 'SELECT * FROM records ORDER BY id', backing)
        self.run_cli('replace view definition', db + ['execute'], {'statements': [
            {'sql': 'DROP VIEW active_records'},
            {'sql': 'CREATE VIEW active_records AS SELECT id,title FROM records WHERE active=0'},
        ]})
        self.assert_sql_rows('replacement view definition persisted', db, 'SELECT * FROM active_records', [['1', 'hidden']])
        self.run_cli('drop view', db + ['execute', 'DROP VIEW active_records'])
        self.run_cli('dropped view unavailable', db + ['execute', 'SELECT * FROM active_records'],
                     code=1, error='SQL_ERROR')
        self.assert_sql_rows('view DDL preserves backing table', db, 'SELECT * FROM records ORDER BY id', backing)

    def test_historical_consistency(self):
        db, inserted, snap = self.history()
        before = self.run_cli('HEAD before consistent reads', db)
        historical = self.run_cli('historical snapshot read', db + ['execute', '--consistency',
                                  'snapshot', '--snapshot-id', snap, 'SELECT count(*) FROM t'])
        self.assertEqual(row(historical), [{'type': 'integer', 'value': '1'}])
        self.assertEqual(historical['data_version'], inserted['data_version'])
        for options in ([], ['--consistency', 'strong'],
                        ['--consistency', 'at_least', '--commit-token', inserted['commit_token']]):
            with self.subTest(options=options):
                current = self.run_cli('current consistent read', db + ['execute', *options,
                                       'SELECT count(*) FROM t'])
                self.assertEqual(row(current), [{'type': 'integer', 'value': '3'}])
                self.assertEqual(current['data_version'], before['data_version'])
                self.assertGreaterEqual(int(current['data_version']), int(inserted['data_version']))
        self.assertEqual(self.run_cli('HEAD after consistent reads', db)['etag'], before['etag'])

    def test_invalid_consistency_inputs_leave_head_unchanged(self):
        db, inserted, snap = self.history()
        before = self.run_cli('HEAD before invalid consistent reads', db)
        other = self.create_database('other')
        other_token = self.run_cli('other database token', other)['commit_token']
        token = inserted['commit_token']
        decoded = json.loads(base64.urlsafe_b64decode(token + '=' * (-len(token) % 4)))
        future = {**decoded, 'data_version': str(int(before['data_version']) + 1)}
        foreign = {**decoded, 'namespace': 'another-namespace'}
        invalid_tokens = ['!invalid!', base64.urlsafe_b64encode(b'not JSON').decode().rstrip('='), other_token]
        invalid_tokens.extend(base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip('=')
                              for value in (future, foreign))
        cases = [(['--consistency', 'snapshot'], 'INVALID_REQUEST'),
                 (['--consistency', 'snapshot', '--snapshot-id', snap + '-unavailable'], 'SNAPSHOT_EXPIRED'),
                 (['--consistency', 'at_least'], 'INVALID_REQUEST')]
        cases.extend((['--consistency', 'at_least', '--commit-token', value], 'INVALID_REQUEST')
                     for value in invalid_tokens)
        for options, error in cases:
            with self.subTest(options=options):
                self.run_cli('invalid consistent read', db + ['execute', *options, 'SELECT count(*) FROM t'],
                             code=2 if error == 'INVALID_REQUEST' else 1, error=error)
        self.assertEqual(self.run_cli('HEAD after invalid consistent reads', db)['etag'], before['etag'])

    def test_batch_results_versions_and_rollback(self):
        db = self.create_database()
        before = self.run_cli('HEAD before batch', db)
        committed = self.run_cli('dependent mixed statement batch', db + ['execute'], {'statements': [
            {'sql': 'CREATE TABLE t(x INTEGER UNIQUE)'},
            {'sql': 'INSERT INTO t VALUES(?) RETURNING x',
             'params': [{'type': 'integer', 'value': '7'}]},
            {'sql': 'SELECT x + 1 AS next FROM t'},
        ]})
        self.assertEqual(committed['outcome'], 'committed')
        self.assertIs(committed['committed'], True)
        self.assertEqual(int(committed['data_version']), int(before['data_version']) + 1)
        self.assertEqual(committed['results'], [
            {'columns': [], 'rows': [], 'rows_affected': '0'},
            {'columns': ['x'], 'rows': [[{'type': 'integer', 'value': '7'}]], 'rows_affected': '1'},
            {'columns': ['next'], 'rows': [[{'type': 'integer', 'value': '8'}]], 'rows_affected': '0'},
        ])
        queries = self.run_cli('query-only batch still commits', db + ['execute'], {'statements': [
            {'sql': 'SELECT x FROM t'}, {'sql': 'SELECT 42 AS answer'},
        ]})
        self.assertEqual(queries['outcome'], 'committed')
        self.assertIs(queries['committed'], True)
        self.assertEqual(int(queries['data_version']), int(committed['data_version']) + 1)
        self.assertEqual([result['rows'][0][0]['value'] for result in queries['results']], ['7', '42'])
        before_failure = self.run_cli('HEAD before failed batch', db)
        self.assertEqual(before_failure['commit_token'], queries['commit_token'])
        failed = self.run_cli('DDL and DML batch rollback', db + ['execute'], {'statements': [
            {'sql': 'CREATE TABLE rolled_back(x)'},
            {'sql': 'INSERT INTO t VALUES(8)'},
            {'sql': 'INSERT INTO t VALUES(7)'},
        ]}, code=1, error='SQL_ERROR')
        self.assertEqual(failed['outcome'], 'not_committed')
        after = self.run_cli('HEAD after failed batch', db)
        for key in ('etag', 'revision', 'data_version', 'commit_token'):
            self.assertEqual(after[key], before_failure[key], key)
        self.assertEqual(row(self.run_cli('batch data survives rollback', db + ['execute',
                             'SELECT count(*), sum(x) FROM t'])),
                         [{'type': 'integer', 'value': '1'}, {'type': 'integer', 'value': '7'}])
        self.assertEqual(row(self.run_cli('DDL rolled back', db + ['execute',
                             "SELECT count(*) FROM sqlite_schema WHERE name='rolled_back'"])),
                         [{'type': 'integer', 'value': '0'}])

    def test_sql_boundaries_and_protected_operations(self):
        db = self.create_database()
        self.run_cli('SQL boundary fixture', db + ['execute', 'CREATE TABLE t(x)'])
        before = self.run_cli('HEAD before rejected SQL', db)
        sql_file = self.tmp / 'multiple.sql'
        sql_file.write_text('INSERT INTO t VALUES(1); INSERT INTO t VALUES(2)', encoding='utf-8')
        for args in (['INSERT INTO t VALUES(1); SELECT 1'], ['SELECT 1; INSERT INTO t VALUES(1)'],
                     ['--sql-file', str(sql_file)]):
            with self.subTest(args=args):
                self.run_cli('multiple statements rejected', db + ['execute', *args],
                             code=1, error='SQL_ERROR')
        for sql in ('BEGIN', 'COMMIT', 'ROLLBACK', 'SAVEPOINT s', "ATTACH ':memory:' AS other",
                    'PRAGMA journal_mode=WAL', 'PRAGMA user_version=1', "SELECT load_extension('missing')"):
            with self.subTest(sql=sql):
                self.run_cli('protected SQL rejected', db + ['execute', sql], code=1, error='SQL_ERROR')
        self.assertEqual(row(self.run_cli('semicolon inside one statement', db + ['execute',
                             "SELECT ';' /* ; */;"])), [{'type': 'text', 'value': ';'}])
        self.assertEqual(row(self.run_cli('rejected SQL persisted no rows', db + ['execute',
                             'SELECT count(*) FROM t'])), [{'type': 'integer', 'value': '0'}])
        self.assertEqual(self.run_cli('HEAD after rejected SQL', db)['etag'], before['etag'])

    def test_writes_reject_read_options(self):
        db, inserted, snap = self.history()
        before = self.run_cli('HEAD before invalid write options', db)
        cases = [['--max-rows', '1'], ['--snapshot-id', snap],
                 ['--consistency', 'snapshot', '--snapshot-id', snap],
                 ['--commit-token', inserted['commit_token']],
                 ['--consistency', 'at_least', '--commit-token', inserted['commit_token']]]
        for options in cases:
            with self.subTest(options=options):
                self.run_cli('write rejects read options', db + ['execute', 'INSERT INTO t VALUES(4,4)',
                             *options], code=2, error='INVALID_REQUEST')
        self.assertEqual(row(self.run_cli('invalid writes persisted no rows', db + ['execute',
                             'SELECT count(*) FROM t'])), [{'type': 'integer', 'value': '3'}])
        self.assertEqual(self.run_cli('HEAD after invalid write options', db)['etag'], before['etag'])

    def test_parameter_count_and_params_stdin(self):
        db = self.create_database()
        params = [{'type': 'integer', 'value': '-9223372036854775808'}, {'type': 'text', 'value': 'stdin'}]
        self.assertEqual(row(self.run_cli('parameter file stdin', db + ['execute', 'SELECT ?, ?',
                             '--params-file', '-'], text=json.dumps(params))), params)
        self.run_cli('parameter count too small', db + ['execute', 'SELECT ?, ?'],
                     {'params': params[:1]}, code=1, error='SQL_ERROR')
        self.run_cli('parameter count too large', db + ['execute', 'SELECT ?'],
                     {'params': params}, code=1, error='SQL_ERROR')

    def test_resource_limits_do_not_publish_failed_writes(self):
        db = self.create_database()
        self.run_cli('resource limit fixture', db + ['execute', 'CREATE TABLE t(x)'])
        before = self.run_cli('HEAD before budget failures', db)
        env = {**self.env, 'PONYDB_QUERY__MAX_ROWS': '2', 'PONYDB_QUERY__DEFAULT_MAX_ROWS': '2',
               'PONYDB_QUERY__MAX_RESULT_BYTES': '64'}
        byte_env = {**env, 'PONYDB_QUERY__MAX_RESULT_BYTES': '16'}
        exact = self.run_cli('exact row policy limit', db + ['execute', 'SELECT 1 UNION ALL SELECT 2',
                             '--max-rows', '2'], env=env)
        self.assertEqual(len(exact['results'][0]['rows']), 2)
        blob = self.run_cli('exact result byte limit', db + ['execute', 'SELECT zeroblob(16)'], env=byte_env)
        self.assertEqual(base64.b64decode(row(blob)[0]['value']), bytes(16))
        for limit in ('0', '3'):
            with self.subTest(limit=limit):
                self.run_cli('invalid row policy limit', db + ['execute', 'SELECT 1', '--max-rows', limit],
                             code=1, error='RESOURCE_LIMIT', env=env)
        for sql, limits in (('SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3', env),
                            ('INSERT INTO t VALUES(1),(2),(3) RETURNING x', env),
                            ('SELECT zeroblob(17)', byte_env), ("SELECT '你好世界你好'", byte_env),
                            ('INSERT INTO t VALUES(zeroblob(17)) RETURNING x', byte_env)):
            with self.subTest(sql=sql):
                self.run_cli('result budget exceeded', db + ['execute', sql],
                             code=1, error='RESOURCE_LIMIT', env=limits)
        self.run_cli('row budget shared across batch statements', db + ['execute'], {'statements': [
            {'sql': 'INSERT INTO t VALUES(1)'},
            {'sql': 'SELECT 1 UNION ALL SELECT 2'}, {'sql': 'SELECT 3'},
        ]}, code=1, error='RESOURCE_LIMIT', env=env)
        self.run_cli('byte budget shared across batch statements', db + ['execute'], {'statements': [
            {'sql': 'INSERT INTO t VALUES(1)'},
            {'sql': 'SELECT zeroblob(9)'}, {'sql': 'SELECT zeroblob(8)'},
        ]}, code=1, error='RESOURCE_LIMIT', env=byte_env)
        self.assertEqual(row(self.run_cli('budget failures persisted no rows', db + ['execute',
                             'SELECT count(*) FROM t'])), [{'type': 'integer', 'value': '0'}])
        self.assertEqual(self.run_cli('HEAD after budget failures', db)['etag'], before['etag'])

    def test_request_deadline_rolls_back_batch(self):
        # Distinguish the CLI deadline from the database's separate read safety timeout.
        self.config.write_text('[maintenance]\nmax_read_duration_ms = 60000\n', encoding='utf-8')
        db = self.create_database()
        self.run_cli('deadline fixture', db + ['execute', 'CREATE TABLE t(x)'])
        before = self.run_cli('HEAD before deadline', db)
        long_query = ('WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL '
                      'SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n')
        started = time.monotonic()
        failure = self.run_cli('CLI request deadline', db + ['execute', '--deadline-ms', '500'],
                              {'statements': [{'sql': 'INSERT INTO t VALUES(1)'}, {'sql': long_query}]},
                              code=1, error='DEADLINE_EXCEEDED',
                              env={**self.env, 'PONYDB_CACHE__DISK_BUDGET_MIB': '0'})
        self.assertLess(time.monotonic() - started, 10, 'CLI ignored the explicit request deadline')
        self.assertEqual(failure['outcome'], 'not_committed')
        self.assertEqual(row(self.run_cli('query after deadline', db + ['execute', 'SELECT count(*) FROM t'])),
                         [{'type': 'integer', 'value': '0'}])
        self.assertEqual(self.run_cli('HEAD after deadline', db)['etag'], before['etag'])

    def test_parameter_flags_request_file_and_sql_boundary(self):
        db = self.create_database()
        params = [{'type': 'integer', 'value': '7'}, {'type': 'text', 'value': '参数'}]
        args = db + ['execute', 'SELECT ?, ?']
        for param in params:
            args.extend(['--param', json.dumps(param)])
        self.assertEqual(row(self.run_cli('repeated param options', args)), params)
        request = self.tmp / 'execute.json'
        request.write_text(json.dumps({'database': 'main', 'sql': 'SELECT ?, ?', 'params': params}),
                           encoding='utf-8')
        self.assertEqual(row(self.run_cli('SQL request file', db + ['execute', '--input', str(request)])), params)
        self.assertEqual(row(self.run_cli('leading hyphen SQL', db + ['execute', '--', '-- comment\nSELECT 42'])),
                         [{'type': 'integer', 'value': '42'}])
        limited = self.run_cli('within row budget', db + ['execute', 'SELECT 1', '--max-rows', '1'])
        self.assertEqual(len(limited['results'][0]['rows']), 1)
        self.run_cli('exceeded row budget', db + ['execute', 'SELECT 1 UNION ALL SELECT 2', '--max-rows', '1'],
                     code=1, error='RESOURCE_LIMIT')


class ExecuteInputTests(CliTestCase):
    needs_storage = False

    def test_batch_input_conflicts_are_rejected_before_storage(self):
        env = {**self.env, 'PONYDB_STORAGE__ENDPOINT': 'http://127.0.0.1:1'}
        batch = {'statements': [{'sql': 'SELECT 1'}]}
        sql_file = self.tmp / 'query.sql'
        sql_file.write_text('SELECT 2', encoding='utf-8')
        params_file = self.tmp / 'params.json'
        params_file.write_text('[]', encoding='utf-8')
        for extra in ({'sql': 'SELECT 2'}, {'params': []}, {'max_rows': 1},
                      {'consistency': 'snapshot'}, {'consistency': 'at_least'},
                      {'snapshot_id': 'unavailable'}, {'commit_token': 'invalid'}):
            with self.subTest(extra=extra):
                self.run_cli('conflicting batch DTO', ['db', 'main', 'execute'], {**batch, **extra},
                             code=2, error='INVALID_REQUEST', env=env)
        for extra in (['SELECT 2'], ['--sql-file', str(sql_file)], ['--params-file', str(params_file)],
                      ['--param', '{"type":"null"}'], ['--max-rows', '1'],
                      ['--consistency', 'snapshot'], ['--consistency', 'at_least'],
                      ['--snapshot-id', 'unavailable'], ['--commit-token', 'invalid']):
            with self.subTest(extra=extra):
                self.run_cli('conflicting explicit batch input', ['db', 'main', 'execute', *extra], batch,
                             code=2, error='INVALID_REQUEST', env=env)
        for data in ({'sql': ''}, {'sql': ' \n '}, {'statements': []},
                     {'statements': [{'sql': ' '}]}, {'statements': [{'sql': 'SELECT 1', 'unknown': True}]}):
            with self.subTest(data=data):
                self.run_cli('invalid SQL input form', ['db', 'main', 'execute'], data,
                             code=2, error='INVALID_REQUEST', env=env)

    def test_invalid_typed_parameters_are_rejected_before_storage(self):
        env = {**self.env, 'PONYDB_STORAGE__ENDPOINT': 'http://127.0.0.1:1'}
        parameters = [
            {'type': 'integer', 'value': '9223372036854775808'},
            {'type': 'integer', 'value': '-9223372036854775809'},
            {'type': 'integer', 'value': '1.5'}, {'type': 'integer', 'value': 1},
            {'type': 'blob', 'value': '!invalid!'}, {'type': 'real', 'value': 'NaN'},
            {'type': 'text', 'value': False}, {'type': 'unknown', 'value': 'x'},
            {'type': 'null', 'unknown': True},
        ]
        params_file = self.tmp / 'invalid-params.json'
        for param in parameters:
            params_file.write_text(json.dumps([param]), encoding='utf-8')
            for args, data in ((['SELECT ?', '--param', json.dumps(param)], None),
                               (['SELECT ?', '--params-file', str(params_file)], None),
                               ([], {'sql': 'SELECT ?', 'params': [param]}),
                               ([], {'statements': [{'sql': 'SELECT ?', 'params': [param]}]})):
                with self.subTest(param=param, args=args, data=data):
                    self.run_cli('invalid typed parameter', ['db', 'main', 'execute', *args], data,
                                 code=2, error='INVALID_REQUEST', env=env)

    def test_invalid_files_and_stdin_are_rejected_before_storage(self):
        env = {**self.env, 'PONYDB_STORAGE__ENDPOINT': 'http://127.0.0.1:1'}
        execute = ['db', 'main', 'execute']
        for option in ('--sql-file', '--params-file', '--input'):
            sql = ['SELECT ?'] if option == '--params-file' else []
            with self.subTest(option=option, content='missing'):
                self.run_cli('missing execute file', execute + sql + [option, str(self.tmp / 'missing')],
                             code=2, error='INVALID_REQUEST', env=env)
            invalid = self.tmp / 'invalid-file'
            for label, content in (('invalid UTF-8', b'\xff'), ('oversized input', b' ' * (8 * 1024 * 1024 + 1))):
                with self.subTest(option=option, content=label):
                    invalid.write_bytes(content)
                    failure = self.run_cli(label, execute + sql + [option, str(invalid)],
                                           code=2, error='INVALID_REQUEST', env=env)
                    if label == 'oversized input':
                        self.assertIn('input exceeds 8 MiB', failure['message'])
                    elif option == '--sql-file':
                        self.assertIn('not valid UTF-8', failure['message'])
        for content in ('{}', '[', '[{"type":"integer","value":"1","value":"2"}]'):
            with self.subTest(params=content):
                self.run_cli('invalid parameter stdin', execute + ['SELECT ?', '--params-file', '-'],
                             text=content, code=2, error='INVALID_REQUEST', env=env)
        failure = self.run_cli('oversized SQL stdin', execute + ['--sql-file', '-'],
                               text=' ' * (8 * 1024 * 1024 + 1), code=2, error='INVALID_REQUEST', env=env)
        self.assertIn('input exceeds 8 MiB', failure['message'])
        self.run_cli('DTO and parameter stdin conflict', execute + ['--input', '-', '--params-file', '-'],
                     code=2, error='INVALID_REQUEST', env=env)
        self.run_cli('DTO and SQL stdin conflict', execute + ['--input', '-', '--sql-file', '-'],
                     code=2, error='INVALID_REQUEST', env=env)
