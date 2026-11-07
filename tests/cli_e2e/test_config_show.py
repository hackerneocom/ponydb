"""End-to-end coverage for config."""
import json

from .support import CliTestCase, check

COMMANDS = [('config', 'show')]


class ConfigShowTests(CliTestCase):
    needs_storage = False

    def test_redaction_and_defaults(self):
        shown = json.dumps(self.run_cli('config redaction', ['config', 'show']))
        check(self.access not in shown and self.secret not in shown, 'credentials exposed')
        check(json.loads(shown)['config']['storage']['force_path_style'] is True,
              'path-style default')
        virtual = self.run_cli('virtual-hosted config override', ['config', 'show'],
                      env={**self.env, 'PONYDB_STORAGE__FORCE_PATH_STYLE': 'false'})
        check(virtual['config']['storage']['force_path_style'] is False,
              'virtual-hosted override')

        for target in ('cli', 'server', 'all'):
            with self.subTest(target=target):
                shown = self.run_cli('config scope', ['config', 'show', '--target', target])
                serialized = json.dumps(shown)
                check(self.access not in serialized and self.secret not in serialized,
                      'config scope exposed credentials')
