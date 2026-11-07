"""End-to-end coverage for schema."""
from .support import CliTestCase, check

COMMANDS = [('db', 'schema')]


class SchemaTests(CliTestCase):
    def test_filters_and_dto(self):
        db = self.create_database()
        self.run_cli('fixture DDL', db + ['execute', 'CREATE TABLE t(id INTEGER PRIMARY KEY, txt TEXT)'])
        self.run_cli('schema', ['db', 'main', 'schema'])
        self.run_cli('schema index DDL', db + ['execute', 'CREATE INDEX t_txt ON t(txt)'])
        schema = self.run_cli('schema table filter', db + ['schema', '--table', 't'])
        check(all(r[2]['value'] == 't' for r in schema['results'][0]['rows']), 'schema table mismatch')
        schema = self.run_cli('schema combined filter', db + ['schema', '--table', 't', '--type', 'index'])
        check([r[1]['value'] for r in schema['results'][0]['rows']] == ['t_txt'], 'schema index mismatch')
        schema = self.run_cli('schema DTO filter', db + ['schema'], {'table': 't', 'type': 'index'})
        check(len(schema['results'][0]['rows']) == 1, 'schema DTO mismatch')
        missing = self.run_cli('schema missing table', db + ['schema', '--table', 'missing'],
                      code=1, error='TABLE_NOT_FOUND')
        check(missing['message'] == "Table 'missing' not found in database 'main'" and
              missing['retryable'] is False and missing['outcome'] == 'not_committed',
              'incorrect missing table diagnostics')
        self.run_cli('schema missing table with type', db + ['schema', '--table', 'missing', '--type', 'index'],
            code=1, error='TABLE_NOT_FOUND')
        self.run_cli('schema missing table DTO', db + ['schema'], {'table': 'missing'},
            code=1, error='TABLE_NOT_FOUND')
        schema = self.run_cli('schema existing table without triggers', db + ['schema', '--table', 't', '--type', 'trigger'])
        check(schema['results'][0]['rows'] == [], 'schema expected empty rows')
        schema = self.run_cli('schema no triggers', db + ['schema', '--type', 'trigger'])
        check(schema['results'][0]['rows'] == [], 'schema expected empty rows')
        self.run_cli('schema filter conflict', db + ['schema', '--type', 'table'],
            {'type': 'index'}, code=2, error='INVALID_REQUEST')
