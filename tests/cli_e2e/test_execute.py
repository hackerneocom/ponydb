"""End-to-end coverage for execute."""
import json

from .support import CliTestCase, check, row

COMMANDS = [('db', 'execute')]


class ExecuteTests(CliTestCase):
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

    def test_historical_consistency(self):
        db, inserted, snap = self.history()
        check(row(self.run_cli('historical snapshot read', db + ['execute', '--consistency', 'snapshot', '--snapshot-id', snap, 'SELECT count(*) FROM t']))[0]['value'] == '1', 'historical rows')
        self.run_cli('at least consistency', db + ['execute', '--consistency', 'at_least', '--commit-token', inserted['commit_token'], 'SELECT count(*) FROM t'])

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
