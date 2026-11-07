"""End-to-end coverage for snapshot get."""
from .support import CliTestCase, check

COMMANDS = [('db', 'snapshot', 'get')]


class SnapshotGetTests(CliTestCase):
    def test_descriptor_and_dto(self):
        db, inserted, snap = self.history()
        descriptor = self.run_cli('snapshot get', db + ['snapshot', 'get', snap])
        check(descriptor['snapshot_id'] == snap and descriptor['data_version'] == inserted['data_version'], 'descriptor mismatch')
        check(self.run_cli('snapshot get DTO', db + ['snapshot', 'get'], {'snapshot_id': snap}) == descriptor, 'DTO descriptor mismatch')
        self.run_cli('snapshot ID conflict', db + ['snapshot', 'get', snap], {'snapshot_id': 'other'}, code=2, error='INVALID_REQUEST')
