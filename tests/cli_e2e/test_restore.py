"""End-to-end coverage for restore."""
import hashlib
import sqlite3

from .support import CliTestCase, check, row

COMMANDS = [('db', 'restore', 'file')]


class RestoreTests(CliTestCase):
    def test_local_file_and_new_incarnation(self):
        db, inserted, snap = self.history()
        backup = self.tmp / 'backup.sqlite'
        with sqlite3.connect(backup) as conn:
            conn.execute('CREATE TABLE restored(x TEXT)')
            conn.execute("INSERT INTO restored VALUES('恢复成功')")
        file_args = ['db', 'file-restore', 'restore', 'file', str(backup)]
        committed = self.run_cli('db restore local file', file_args,
                        env={**self.env, 'PONYDB_REQUESTS__DEFAULT_TIMEOUT_MS': '1'})
        check(committed['committed'] and committed['outcome'] == 'committed', 'restore publication')
        check(committed['sha256'] == hashlib.sha256(backup.read_bytes()).hexdigest(), 'restore checksum')
        check(row(self.run_cli('local file restored data', ['db', 'file-restore', 'execute', 'SELECT x FROM restored']))[0]['value'] == '恢复成功', 'file restore data')
        self.run_cli('restore existing rejected', db + ['restore', 'file', str(backup)], code=1, error='DATABASE_EXISTS')
        invalid = self.tmp / 'invalid.sqlite'
        invalid.write_bytes(b'not SQLite')
        self.run_cli('invalid backup rejected', ['db', 'invalid-restore', 'restore', 'file', str(invalid)], code=1, error='INVALID_SQLITE_BACKUP')
        self.run_cli('invalid backup did not publish', ['db', 'invalid-restore'], code=1, error='DATABASE_NOT_FOUND')
        before_delete = self.run_cli('metadata before delete and restore', db)
        self.run_cli('delete without ETag', ['db', 'main', 'delete'])
        self.run_cli('deleted SQL rejected', db + ['execute', 'SELECT 1'], code=1, error='DATABASE_DELETED')
        self.run_cli('deleted current export rejected', db + ['export', str(self.tmp / 'deleted.sqlite')], code=1, error='DATABASE_DELETED')
        check(not (self.tmp / 'deleted.sqlite').exists(), 'deleted export created a file')
        active = self.run_cli('list excludes deleted', ['databases', 'list'])['databases']
        check('main' not in [d['name'] for d in active], 'deleted database listed')
        deleted = self.run_cli('list includes deleted', ['databases', 'list', '--include-deleted'])['databases']
        check('main' in [d['name'] for d in deleted], 'tombstone missing')
        self.run_cli('deleted snapshot remains available', db + ['snapshot', 'get', snap])
        self.run_cli('restore deleted database name', db + ['restore', 'file', str(backup)])
        check(row(self.run_cli('recreated data', db + ['execute', 'SELECT x FROM restored']))[0]['value'] == '恢复成功', 'recreation failed')
        restored_meta = self.run_cli('metadata after restore', db)
        check(restored_meta['name'] == 'main' and
              int(restored_meta['revision']) > int(before_delete['revision']) and
              restored_meta['data_version'] == '1', 'restore changed identity or reset HEAD revision')
        restored_snapshots = self.run_cli('new incarnation snapshots', db + ['snapshot', 'list'])['snapshots']
        check(len(restored_snapshots) == 1 and restored_snapshots[0]['snapshot_id'] != snap,
              'restore inherited old history')
        self.run_cli('old ETag rejected after restore', db + ['update', '--if-match', before_delete['etag'],
            '--description', 'stale'], code=4, error='METADATA_PRECONDITION_FAILED')
        self.run_cli('old snapshot unavailable', db + ['snapshot', 'get', snap], code=1, error='SNAPSHOT_EXPIRED')
        self.run_cli('old token rejected', db + ['execute', '--consistency', 'at_least', '--commit-token', inserted['commit_token'], 'SELECT 1'], code=2, error='INVALID_REQUEST')
        objects = self.keys()
        check(not any('/requests/' in key for key in objects), 'unexpected request objects')
        for key in objects:
            parts = key[len(self.prefix):].split('/')
            check(parts[0] == 'databases', 'deployment prefix applied more than once')
            if len(parts) > 2 and parts[2] == 'incarnations':
                check(len(parts) >= 8 and parts[4] == 'generations' and
                      len(parts[5]) == 16 and all(c in '0123456789abcdef' for c in parts[5]) and
                      int(parts[5], 16) > 0, 'immutable object outside layout v1')
