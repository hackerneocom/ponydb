"""Database creation, explicit names, DTOs and metadata input."""
import json

from .support import CliTestCase, check

COMMANDS = [('databases', 'create')]


class DatabaseCreateTests(CliTestCase):
    def test_positional_name_metadata_and_dto(self):
        metadata = self.tmp / 'metadata.json'
        metadata.write_text(json.dumps({'来源': 'e2e'}), encoding='utf-8')
        created = self.run_cli('create positional name', [
            'databases', 'create', 'main', '--description', '中文描述',
            '--metadata-file', str(metadata)])
        check(created['name'] == 'main', 'create name mismatch')
        detail = self.run_cli('created metadata', ['db', 'main'])
        check(detail['description'] == '中文描述' and detail['metadata'] == {'来源': 'e2e'},
              'create options were not persisted')
        self.run_cli('duplicate database', ['databases', 'create', 'main'],
                     code=1, error='DATABASE_EXISTS')
        self.run_cli('matching create DTO', ['databases', 'create', 'dto'],
                     {'name': 'dto', 'description': 'DTO', 'metadata': {'test': True}})
        self.run_cli('create DTO name conflict', ['databases', 'create', 'conflict'],
                     {'name': 'other'}, code=2, error='INVALID_REQUEST')
        self.run_cli('create metadata stdin', ['databases', 'create', 'stdin', '--metadata-file', '-'],
                     text='{"stdin":true}')
        check(self.run_cli('stdin metadata', ['db', 'stdin'])['metadata'] == {'stdin': True},
              'metadata stdin mismatch')
        for name in ('help', 'update', '中文库'):
            with self.subTest(name=name):
                self.run_cli('create keyword/Unicode name', ['databases', 'create', name])
                check(self.run_cli('get keyword/Unicode name', ['db', name])['name'] == name,
                      'database operand was parsed as a command')
