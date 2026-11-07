"""End-to-end coverage for database get."""
import json

from .support import CliTestCase, check

COMMANDS = [('db',)]


class DatabaseGetTests(CliTestCase):
    def test_explicit_name_and_dto(self):
        self.create_database()
        missing = self.run_cli('missing database diagnostics', ['db', 'missing'], code=1,
                      error='DATABASE_NOT_FOUND')
        check(missing['message'] == "Database 'missing' not found" and
              missing['retryable'] is False and missing['outcome'] == 'not_committed',
              'incorrect missing database diagnostics')
        meta = self.run_cli('get explicit name', ['db', 'main'])
        check(meta['name'] == 'main', 'name roundtrip')
        self.run_cli('get matching name and DTO', ['db', 'main'], {'database': 'main'})
        self.run_cli('get conflicting name and DTO', ['db', 'main'],
            {'database': 'other'}, code=2, error='INVALID_REQUEST')

    def test_missing_manifest(self):
        self.run_cli('create database for missing manifest check', ['databases', 'create', 'broken'])
        manifests = [key for key in self.keys()
                     if key.startswith(self.prefix + 'databases/broken/') and '/manifests/' in key]
        check(manifests, 'missing manifest fixture')
        for key in manifests:
            self.s3('DELETE', key=key)
        # Bypass read- and PUT-filled caches when probing remote manifest corruption.
        self.env['PONYDB_CACHE__DISK_BUDGET_MIB'] = '0'
        corrupt = self.run_cli('missing manifest diagnostics', ['db', 'broken'], code=1,
                      error='STORAGE_CORRUPT')
        check(corrupt['message'] == "Database 'broken' storage is incomplete: a referenced object is missing" and
              corrupt['retryable'] is False and corrupt['outcome'] == 'not_committed',
              'incorrect missing manifest diagnostics')
        self.env.pop('PONYDB_CACHE__DISK_BUDGET_MIB')
