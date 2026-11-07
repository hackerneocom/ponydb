"""End-to-end coverage for compact."""
from .support import CliTestCase, check, row

COMMANDS = [('db', 'compact')]


class CompactTests(CliTestCase):
    def test_defaults_limits_and_dto(self):
        db, inserted, snap = self.history()
        compact = self.run_cli('compact defaults', db + ['compact'])
        check('resume_required' not in compact, 'obsolete compact field')
        self.run_cli('compact custom budget', db + ['compact', '--budget-ms', '5000'])
        self.run_cli('compact custom segment limit', db + ['compact', '--max-segments', '16'])
        self.run_cli('compact explicit limits', db + ['compact', '--budget-ms', '5000', '--max-segments', '32'])
        self.run_cli('compact partial DTO defaults', db + ['compact'], {'max_segments': 16})
        self.run_cli('compact conflicting limits', db + ['compact', '--budget-ms', '10000'],
            {'budget_ms': 5000}, code=2, error='INVALID_REQUEST')
        check(row(self.run_cli('data after compaction', db + ['execute', 'SELECT count(*) FROM t']))[0]['value'] == '3', 'compact damaged rows')
        check(row(self.run_cli('historical snapshot read', db + ['execute', '--consistency', 'snapshot', '--snapshot-id', snap, 'SELECT count(*) FROM t']))[0]['value'] == '1', 'historical rows')
