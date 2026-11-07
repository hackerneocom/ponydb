"""End-to-end coverage for database export."""
import hashlib
import json
import sqlite3

from .support import CliTestCase, check, row

COMMANDS = [('db', 'export')]


class DatabaseExportTests(CliTestCase):
    def test_file_dto_and_conflicts(self):
        db, inserted, snap = self.history()
        snapshots = self.run_cli('fixture current snapshots', db + ['snapshot', 'list'])['snapshots']
        current = self.tmp / 'current.sqlite'
        current_head = self.run_cli('current export HEAD', db)
        summary = self.run_cli('current export', db + ['export', str(current)])
        check(summary['data_version'] == current_head['data_version'], 'current export version')
        check(summary['snapshot_id'] == next(s['snapshot_id'] for s in snapshots if s['data_version'] == current_head['data_version']), 'current export snapshot ID')
        check(summary['sha256'] == hashlib.sha256(current.read_bytes()).hexdigest(), 'current export hash')
        check(int(summary['total_bytes']) == current.stat().st_size, 'current export size')
        with sqlite3.connect(current) as conn:
            check(conn.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', 'current export integrity')
            check(conn.execute('SELECT count(*) FROM t').fetchone()[0] == 3, 'current export data')
        self.run_cli('current export never overwrites', db + ['export', str(current)], code=2, error='INVALID_REQUEST')
        self.run_cli('current export DTO stdin', db + ['export'], {'file': str(self.tmp / 'current-dto.sqlite')})
        request = self.tmp / 'current-export.json'
        request.write_text(json.dumps({'database': 'main', 'file': str(self.tmp / 'current-file.sqlite')}))
        self.run_cli('current export request file', db + ['export', '--input', str(request)])
        self.run_cli('current export DTO path conflict', db + ['export', str(self.tmp / 'conflict.sqlite')],
            {'file': str(self.tmp / 'other.sqlite')}, code=2, error='INVALID_REQUEST')
        self.run_cli('current export DTO database conflict', db + ['export'],
            {'database': 'other', 'file': str(self.tmp / 'conflict.sqlite')}, code=2, error='INVALID_REQUEST')
        self.run_cli('current export refuses snapshot ID', db + ['export'],
            {'snapshot_id': snap, 'file': str(self.tmp / 'conflict.sqlite')}, code=2, error='INVALID_REQUEST')
        self.run_cli('current export missing file', db + ['export'], {}, code=2, error='INVALID_REQUEST')
        self.run_cli('current export rejects stdout', db + ['export', '-'], code=2, error='INVALID_REQUEST')
        self.run_cli('restore current export', ['db', 'current-restored', 'restore', 'file', str(current)])
        check(row(self.run_cli('current export restored data', ['db', 'current-restored', 'execute', 'SELECT count(*) FROM t']))[0]['value'] == '3', 'current export restore rows')
