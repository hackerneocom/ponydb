"""End-to-end coverage for schema."""
from .support import CliTestCase

COMMANDS = [('db', 'schema')]


class SchemaTests(CliTestCase):
    def assert_schema_rows(self, schema, expected):
        self.assertEqual(len(schema['results']), 1)
        self.assertEqual(schema['results'][0]['rows'], [
            [{'type': 'text', 'value': value} for value in record]
            for record in expected
        ])
        self.assertIs(schema['truncated'], False)

    def test_filters_and_dto(self):
        db = self.create_database()
        objects = [
            ('table', 't', 't', 'CREATE TABLE t(id INTEGER PRIMARY KEY, txt TEXT UNIQUE)'),
            ('table', 'other', 'other', 'CREATE TABLE other(txt TEXT)'),
            ('index', 't_txt', 't', 'CREATE INDEX t_txt ON t(txt)'),
            ('index', 'other_txt', 'other', 'CREATE INDEX other_txt ON other(txt)'),
            ('view', 't_view', 't_view', 'CREATE VIEW t_view AS SELECT * FROM t'),
            ('trigger', 't_insert', 't',
             'CREATE TRIGGER t_insert AFTER INSERT ON t BEGIN SELECT 1; END'),
            ('trigger', 'other_insert', 'other',
             'CREATE TRIGGER other_insert AFTER INSERT ON other BEGIN SELECT 1; END'),
        ]
        self.run_cli('schema fixture DDL', db + ['execute'], {
            'statements': [{'sql': obj[3]} for obj in objects],
        })
        by_name = {obj[1]: obj for obj in objects}
        cases = [
            ({}, ['other_txt', 't_txt', 'other', 't', 'other_insert', 't_insert', 't_view']),
            ({'table': 't'}, ['t_txt', 't', 't_insert']),
            ({'type': 'table'}, ['other', 't']),
            ({'type': 'index'}, ['other_txt', 't_txt']),
            ({'type': 'view'}, ['t_view']),
            ({'type': 'trigger'}, ['other_insert', 't_insert']),
            ({'table': 't', 'type': 'table'}, ['t']),
            ({'table': 't', 'type': 'index'}, ['t_txt']),
            ({'table': 't', 'type': 'view'}, []),
            ({'table': 't', 'type': 'trigger'}, ['t_insert']),
        ]
        for filters, names in cases:
            for source in ('arguments', 'dto'):
                with self.subTest(filters=filters, source=source):
                    args = db + ['schema']
                    data = filters if source == 'dto' else None
                    if source == 'arguments':
                        for key, value in filters.items():
                            args.extend(['--' + key, value])
                    schema = self.run_cli('schema filters', args, data)
                    self.assert_schema_rows(schema, [by_name[name] for name in names])
        self.run_cli('schema filter conflict', db + ['schema', '--type', 'table'],
            {'type': 'index'}, code=2, error='INVALID_REQUEST')

    def test_empty_results(self):
        db = self.create_database()
        self.assert_schema_rows(self.run_cli('empty database schema', db + ['schema']), [])
        self.run_cli('fixture DDL', db + ['execute', 'CREATE TABLE t(id INTEGER PRIMARY KEY)'])
        for filters in ({'type': 'trigger'}, {'table': 't', 'type': 'trigger'}):
            with self.subTest(filters=filters):
                args = db + ['schema', '--type', 'trigger']
                if 'table' in filters:
                    args.extend(['--table', filters['table']])
                self.assert_schema_rows(self.run_cli('schema without triggers', args), [])
                self.assert_schema_rows(self.run_cli('schema without triggers DTO', db + ['schema'], filters), [])

    def test_missing_table(self):
        db = self.create_database()
        for kind in (None, 'table', 'index', 'view', 'trigger'):
            with self.subTest(kind=kind):
                args = db + ['schema', '--table', 'missing']
                data = {'table': 'missing'}
                if kind is not None:
                    args.extend(['--type', kind])
                    data['type'] = kind
                for source, command, request in (
                    ('arguments', args, None), ('dto', db + ['schema'], data),
                ):
                    with self.subTest(source=source):
                        missing = self.run_cli('schema missing table', command, request,
                                               code=1, error='TABLE_NOT_FOUND')
                        self.assertEqual(missing['message'], "Table 'missing' not found in database 'main'")
                        self.assertIs(missing['retryable'], False)
                        self.assertEqual(missing['outcome'], 'not_committed')
