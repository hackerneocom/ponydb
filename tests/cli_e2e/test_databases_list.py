"""End-to-end coverage for databases list."""
import json

from .support import CliTestCase, check

COMMANDS = [('databases', 'list')]


class DatabaseListTests(CliTestCase):
    def test_authentication_diagnostics(self):
        invalid_access = 'ponydb-e2e-invalid-access-key'
        denied = self.run_cli('storage error diagnostics', ['databases', 'list'], code=1,
                     error='STORAGE_UNAVAILABLE', env={**self.env,
                     'PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID': invalid_access})
        check(denied['message'].startswith('S3 ListObjectsV2 failed (HTTP 403, '),
              'missing storage diagnostics')
        check(denied['retryable'] is False and denied['outcome'] == 'not_committed',
              'incorrect storage error semantics')
        check(all(secret not in json.dumps(denied) for secret in (self.access, self.secret, invalid_access)),
              'storage diagnostics exposed credentials')

    def test_deleted_filter(self):
        db = self.create_database()
        check(len(self.run_cli('list', ['databases', 'list'])['databases']) == 1, 'list count')
        self.run_cli('delete without ETag', ['db', 'main', 'delete'])
        active = self.run_cli('list excludes deleted', ['databases', 'list'])['databases']
        check('main' not in [d['name'] for d in active], 'deleted database listed')
        deleted = self.run_cli('list includes deleted', ['databases', 'list', '--include-deleted'])['databases']
        check('main' in [d['name'] for d in deleted], 'tombstone missing')

        # An omitted flag must preserve true supplied by a DTO.
        dto = self.run_cli('include deleted DTO', ['databases', 'list'], {'include_deleted': True})
        check(dto['databases'] == deleted, 'omitted flag overwrote DTO value')
        self.run_cli('include deleted conflict', ['databases', 'list', '--include-deleted'],
                     {'include_deleted': False}, code=2, error='INVALID_REQUEST')

    def test_pagination_and_dto(self):
        for name in ('alpha', 'beta', 'gamma'):
            self.create_database(name)
        names, cursor = [], None
        for _ in range(4):
            data = {'limit': 1}
            if cursor:
                data['cursor'] = cursor
            page = self.run_cli('database page DTO', ['databases', 'list'], data)
            self.assertLessEqual(len(page['databases']), 1)
            names.extend(item['name'] for item in page['databases'])
            cursor = page['next_cursor']
            if not cursor:
                break
        self.assertIsNone(cursor, 'database pagination did not terminate')
        self.assertEqual(names, ['alpha', 'beta', 'gamma'])
        page = self.run_cli('database page options', ['databases', 'list', '--limit', '1'])
        next_page = self.run_cli('database cursor option', [
            'databases', 'list', '--limit', '1', '--cursor', page['next_cursor']])
        self.assertEqual(next_page['databases'][0]['name'], 'beta')
