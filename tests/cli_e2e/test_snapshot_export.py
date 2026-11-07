"""End-to-end coverage for snapshot export."""
import hashlib
import sqlite3

from .support import CliTestCase, check, row

COMMANDS = [('db', 'snapshot', 'export')]


class SnapshotExportTests(CliTestCase):
    def test_file_and_dto(self):
        db, inserted, snap = self.history()
        exported = self.tmp / 'snapshot.sqlite'
        summary = self.run_cli('snapshot export', db + ['snapshot', 'export', snap, str(exported)])
        check(summary['sha256'] == hashlib.sha256(exported.read_bytes()).hexdigest(), 'export hash')
        check(int(summary['total_bytes']) == exported.stat().st_size, 'export size')
        with sqlite3.connect(exported) as conn:
            check(conn.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', 'export integrity')
            check(conn.execute('SELECT count(*) FROM t').fetchone()[0] == 1, 'export historical data')
        self.run_cli('export never overwrites', db + ['snapshot', 'export', snap, str(exported)], code=2, error='INVALID_REQUEST')
        self.run_cli('export DTO', db + ['snapshot', 'export'], {'snapshot_id': snap, 'file': str(self.tmp / 'dto.sqlite')})
        self.run_cli('restore exported snapshot', ['db', 'snapshot-restored', 'restore', 'file', str(exported)])
        check(row(self.run_cli('export restore data', ['db', 'snapshot-restored', 'execute', 'SELECT count(*) FROM t']))[0]['value'] == '1', 'export restore rows')
