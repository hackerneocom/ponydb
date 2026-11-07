"""End-to-end coverage for version."""
from .support import CliTestCase, check

COMMANDS = [('version',)]


class VersionTests(CliTestCase):
    needs_storage = False

    def test_versions_and_output(self):
        check(self.run_cli('version', ['version'])['operations'] == 11, 'operation count')
        check(self.run_cli('table version', ['version', '--output', 'table']).find('sqlite_version') >= 0, 'missing table version')
