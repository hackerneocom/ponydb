"""End-to-end coverage for database update."""
import json

from .support import CliTestCase, check

COMMANDS = [('db', 'update')]


class DatabaseUpdateTests(CliTestCase):
    def test_optional_etag_and_conflicts(self):
        db = self.create_database()
        meta = self.run_cli('original metadata', db)
        self.run_cli('metadata update', ['db', 'main', 'update', '--if-match', meta['etag']], {'description': '更新', 'metadata': {'test': True}})
        self.run_cli('stale metadata ETag', ['db', 'main', 'update', '--if-match', meta['etag'], '--description', 'stale'], code=4, error='METADATA_PRECONDITION_FAILED')
        self.run_cli('metadata DTO without ETag', ['db', 'main', 'update'],
            {'description': 'without ETag'})
        self.run_cli('metadata without ETag', ['db', 'main', 'update', '--description', 'unconditional'])
        check(self.run_cli('updated metadata', db)['description'] == 'unconditional', 'description not persisted')

        etag = self.run_cli('current metadata ETag', db)['etag']
        self.run_cli('matching DTO ETag', db + ['update'],
                     {'if_match': etag, 'snapshot_retention_days': 14})
        self.assertEqual(self.run_cli('retention roundtrip', db)['snapshot_retention_days'], 14)
        self.run_cli('conflicting ETag sources', db + ['update', '--if-match', etag],
                     {'if_match': 'different'}, code=2, error='INVALID_REQUEST')
        self.run_cli('update metadata stdin', db + ['update', '--metadata-file', '-'], text='{"stdin":true}')
        self.assertEqual(self.run_cli('updated stdin metadata', db)['metadata'], {'stdin': True})

    def test_metadata_size_limits(self):
        limit = 64 * 1024
        before = self.keys()
        oversized = {'x': 'a' * (limit - 7)}
        self.run_cli('oversized create metadata DTO', ['databases', 'create', 'too-large'],
            {'metadata': oversized}, code=2, error='INVALID_REQUEST')
        check(self.keys() == before, 'invalid metadata create touched storage')
        metadata = {'x': 'a' * (limit - 8)}
        metadata_file = self.tmp / 'metadata.json'
        metadata_file.write_text(json.dumps(metadata, indent=2), encoding='utf-8')
        check(metadata_file.stat().st_size > limit, 'pretty JSON fixture is not above limit')
        db = ['db', 'metadata-limits']
        self.run_cli('maximum metadata file create', ['databases', 'create', 'metadata-limits',
            '--metadata-file', str(metadata_file)])
        value = self.run_cli('maximum metadata roundtrip', db)
        check(value['metadata'] == metadata, 'metadata file changed during compact serialization')
        metadata_file.write_text(json.dumps(oversized), encoding='utf-8')
        self.run_cli('oversized metadata file update', db + ['update', '--metadata-file', str(metadata_file)],
            code=2, error='INVALID_REQUEST')
        self.run_cli('oversized metadata DTO update', db + ['update'], {'metadata': oversized},
            code=2, error='INVALID_REQUEST')
        self.run_cli('oversized description DTO update', db + ['update'], {'description': 'a' * (16 * 1024 + 1)},
            code=2, error='INVALID_REQUEST')
        after = self.run_cli('invalid metadata update preserves HEAD', db)
        check(after == value, 'invalid metadata update changed HEAD')
        unicode_metadata = {'x': '中' * ((limit - 8) // 3) + 'a' * ((limit - 8) % 3)}
        self.run_cli('maximum UTF-8 metadata DTO update', db + ['update'], {'metadata': unicode_metadata})
        check(self.run_cli('UTF-8 metadata roundtrip', db)['metadata'] == unicode_metadata,
              'UTF-8 metadata changed')
