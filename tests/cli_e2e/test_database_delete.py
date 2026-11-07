"""End-to-end coverage for database delete."""
from .support import CliTestCase, check

COMMANDS = [('db', 'delete')]


class DatabaseDeleteTests(CliTestCase):
    def test_etag_and_deleted_access(self):
        db, inserted, snap = self.history()
        meta = self.run_cli('original metadata', db)
        self.run_cli('change ETag', db + ['update', '--description', 'changed'])
        self.run_cli('stale delete ETag', ['db', 'main', 'delete', '--if-match', meta['etag']], code=4, error='METADATA_PRECONDITION_FAILED')
        self.run_cli('delete without ETag', ['db', 'main', 'delete'])
        self.run_cli('deleted SQL rejected', db + ['execute', 'SELECT 1'], code=1, error='DATABASE_DELETED')
        self.run_cli('deleted current export rejected', db + ['export', str(self.tmp / 'deleted.sqlite')], code=1, error='DATABASE_DELETED')
        check(not (self.tmp / 'deleted.sqlite').exists(), 'deleted export created a file')
        active = self.run_cli('list excludes deleted', ['databases', 'list'])['databases']
        check('main' not in [d['name'] for d in active], 'deleted database listed')
        deleted = self.run_cli('list includes deleted', ['databases', 'list', '--include-deleted'])['databases']
        check('main' in [d['name'] for d in deleted], 'tombstone missing')
        self.run_cli('deleted snapshot remains available', db + ['snapshot', 'get', snap])

    def test_dto_etag(self):
        db = self.create_database('file-restore')
        self.run_cli('delete DTO', ['db', 'file-restore', 'delete'], {'if_match': self.run_cli('get restored ETag', ['db', 'file-restore'])['etag']})
        self.run_cli('DTO delete persisted', db + ['execute', 'SELECT 1'], code=1, error='DATABASE_DELETED')

    def test_explicit_etag_and_dto_conflict(self):
        db = self.create_database()
        etag = self.run_cli('delete ETag', db)['etag']
        self.run_cli('delete ETag source conflict', db + ['delete', '--if-match', etag],
                     {'if_match': 'different'}, code=2, error='INVALID_REQUEST')
        self.run_cli('delete explicit ETag', db + ['delete', '--if-match', etag])
        self.run_cli('explicit delete persisted', db + ['execute', 'SELECT 1'],
                     code=1, error='DATABASE_DELETED')
