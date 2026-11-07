"""Actual help output, command coverage and rejection before S3 access."""
from .catalog import test_modules
from .support import CliTestCase

COMMANDS = []


class ArgumentTests(CliTestCase):
    needs_storage = False

    def test_help_and_command_coverage(self):
        expected = {tuple(command) for module in test_modules().values() for command in module.COMMANDS}
        found = set()

        def visit(path):
            process = self.process([*path, '--help'])
            self.assertEqual(process.returncode, 0, process.stderr)
            self.assertEqual(process.stderr, '')
            self.assertIn('Usage:', process.stdout)
            if path and path[0] in ('server', 'mcp'):
                self.assertNotIn('--output', process.stdout)
            if path and (path[0] in ('server', 'config', 'version') or 'restore' in path):
                self.assertNotIn('--deadline-ms', process.stdout)
            commands = []
            in_commands = False
            for line in process.stdout.splitlines():
                if line == 'Commands:':
                    in_commands = True
                elif in_commands and line and not line.startswith(' '):
                    in_commands = False
                elif in_commands and line.strip():
                    name = line.split()[0]
                    if name != 'help':
                        commands.append(name)
            normalized = tuple(part for part in path if part != 'DATABASE_NAME')
            if not commands or path == ['db', 'DATABASE_NAME']:
                found.add(normalized)
            for command in commands:
                visit([*path, command, *(['DATABASE_NAME'] if not path and command == 'db' else [])])

        visit([])
        self.assertEqual(found, expected, 'Every CLI operation needs a command test module; check COMMANDS declarations')

    def test_required_exclusive_enumerated_and_unrelated_options(self):
        cases = [
            ['databases', 'create'], ['db'], ['db', 'main', 'execute'],
            ['db', 'main', 'export'], ['db', 'main', 'snapshot', 'get'],
            ['db', 'main', 'snapshot', 'export'], ['db', 'main', 'restore', 'file'],
            ['db', 'main', 'restore', 'file', '--input', '-'],
            ['db', 'main', 'execute', 'SELECT 1', '--sql-file', 'missing.sql'],
            ['db', 'main', 'execute', 'SELECT 1', '--param', '{}', '--params-file', '-'],
            ['db', 'main', 'schema', '--type', 'invalid'],
            ['config', 'show', '--target', 'invalid'],
            ['--output', 'invalid', 'version'],
            ['db', 'main', 'execute', 'SELECT 1', '--consistency', 'invalid'],
            ['server', '--sql-file', '-'], ['mcp', '--listen', '127.0.0.1:0'],
            ['config', 'show', '--include-deleted'],
            ['version', '--input', '-'],
            ['databases', 'create', 'main', '--limit', '1'],
            ['databases', 'list', '--description', 'invalid'],
            ['db', 'main', '--if-match', 'etag'],
            ['db', 'main', 'update', '--sql-file', '-'],
            ['db', 'main', 'delete', '--description', 'invalid'],
            ['db', 'main', 'schema', '--max-rows', '1'],
            ['db', 'main', 'compact', '--max-objects', '1'],
            ['db', 'main', 'gc', '--max-segments', '1'],
            ['db', 'main', 'snapshot', 'list', '--snapshot-id', 'id'],
            ['db', 'main', 'snapshot', 'get', 'id', '--limit', '1'],
        ]
        for args in cases:
            with self.subTest(args=args):
                process = self.process(args)
                self.assertEqual(process.returncode, 2, process.stderr)
                self.assertEqual(process.stdout, '')
                self.assertIn('error:', process.stderr)

    def test_global_options_validated_at_every_position(self):
        env = {**self.env, 'PONYDB_STORAGE__ENDPOINT': 'http://127.0.0.1:1'}
        for command, option in [
            (['server'], ['--output', 'json']),
            (['mcp'], ['--output', 'table']),
            (['server'], ['--deadline-ms', '1']),
            (['version'], ['--deadline-ms', '1']),
            (['config', 'show'], ['--deadline-ms', '1']),
            (['config', 'validate'], ['--deadline-ms', '1']),
            (['db', 'main', 'restore', 'file', 'missing.sqlite'], ['--deadline-ms', '1']),
        ]:
            for args in (option + command, command + option):
                with self.subTest(args=args):
                    self.run_cli('invalid global context', args, code=2, error='INVALID_REQUEST', env=env)
        self.run_cli('input before action', ['db', 'main', '--input', '-', 'execute', 'SELECT 1'],
                     text='{}', code=2, error='INVALID_REQUEST', env=env)

    def test_strict_dto_and_local_validation(self):
        env = {**self.env, 'PONYDB_STORAGE__ENDPOINT': 'http://127.0.0.1:1'}
        for content in ('[]', '{"sql":"SELECT 1","sql":"SELECT 2"}',
                        '{"sql":"SELECT 1","unknown":true}',
                        '{"sql":123}', '{"statements":[]}', '{}'):
            with self.subTest(content=content):
                self.run_cli('strict DTO', ['db', 'main', 'execute', '--input', '-'],
                             text=content, code=2, error='INVALID_REQUEST', env=env)
        for action, data in [('compact', {'max_segments': 0}), ('gc', {'max_objects': 0}),
                             ('schema', {'type': 'invalid'}), ('update', {'description': 123}),
                             ('delete', {'if_match': False})]:
            with self.subTest(action=action):
                self.run_cli('local business validation', ['db', 'main', action], data,
                             code=2, error='INVALID_REQUEST', env=env)
