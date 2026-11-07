"""End-to-end coverage for gc."""
from .support import CliTestCase, check, row

COMMANDS = [('db', 'gc')]


class GcTests(CliTestCase):
    def test_partial_work_defaults_and_integrity(self):
        db, inserted, snap = self.history()
        self.run_cli('prepare retired segments', db + ['compact'])
        for n in range(30):
            args = ['db', 'main', 'gc', '--budget-ms', '5000', '--max-objects', '1' if n == 0 else '1000']
            value = self.run_cli(f'GC batch {n}', args, code=(0, 6))
            check(value['database_available'], 'GC must remain online')
            check(value['outcome'] == ('applied' if value['complete'] else 'partial'), 'GC completion outcome')
            check('pass_id' not in value and 'resume_required' not in value and 'retention_pruning' not in value, 'obsolete GC fields')
            if n == 0:
                check(not value['complete'], 'GC partial branch not exercised')
            if value['complete']:
                break
        else:
            raise AssertionError('GC did not converge')
        self.run_cli('GC defaults', db + ['gc'], code=(0, 6))
        self.run_cli('GC conflicting limits', db + ['gc', '--budget-ms', '100'], {'budget_ms': 200}, code=2, error='INVALID_REQUEST')
        check(row(self.run_cli('data after GC', db + ['execute', 'SELECT count(*) FROM t']))[0]['value'] == '3', 'GC damaged rows')
        # Default retention: the first GC observes retirement; history remains available.
        self.run_cli('history after GC', db + ['execute', '--consistency', 'snapshot', '--snapshot-id', snap, 'SELECT count(*) FROM t'])
