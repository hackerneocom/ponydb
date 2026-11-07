"""End-to-end coverage for snapshot list."""
import json

from .support import CliTestCase, check

COMMANDS = [('db', 'snapshot', 'list')]


class SnapshotListTests(CliTestCase):
    def test_history(self):
        db, inserted, snap = self.history()
        snapshots = self.run_cli('snapshot list', db + ['snapshot', 'list'])['snapshots']
        check(len(snapshots) >= 3, 'missing history')
        snap = next(s['snapshot_id'] for s in snapshots if s['data_version'] == inserted['data_version'])

        seen, cursor = [], None
        for _ in range(len(snapshots) + 1):
            data = {'limit': 1}
            if cursor:
                data['cursor'] = cursor
            page = self.run_cli('snapshot page DTO', db + ['snapshot', 'list'], data)
            self.assertLessEqual(len(page['snapshots']), 1)
            seen.extend(item['snapshot_id'] for item in page['snapshots'])
            cursor = page['next_cursor']
            if not cursor:
                break
        self.assertIsNone(cursor, 'snapshot pagination did not terminate')
        self.assertEqual(seen, [item['snapshot_id'] for item in snapshots])

    def test_incompatible_head_diagnostics(self):
        self.run_cli('create database for HEAD schema check', ['databases', 'create', 'old-head'])
        head_key = self.prefix + 'databases/old-head/head.json'
        head = json.loads(self.s3('GET', key=head_key))
        head['restore'] = None
        head['restore_result'] = None
        self.s3('PUT', key=head_key, body=json.dumps(head).encode())
        incompatible = self.run_cli('HEAD schema diagnostics', ['db', 'old-head', 'snapshot', 'list'],
                           code=1, error='STORAGE_CORRUPT')
        check(incompatible['message'] == "Database 'old-head' HEAD validation failed: unknown field 'restore'; stored HEAD may be incompatible with this version" and
              incompatible['retryable'] is False and incompatible['outcome'] == 'not_committed',
              'incorrect HEAD schema diagnostics')
