"""Online configuration validation and sanitized authentication errors."""
import json

from .support import CliTestCase, check

COMMANDS = [('config', 'validate')]


class ConfigValidateTests(CliTestCase):
    def test_storage_contract_and_invalid_credentials(self):
        validated = self.run_cli('config validate storage contract', ['config', 'validate'])
        check(validated['valid'] and validated['storage_capabilities_verified'],
              'storage contract was not verified')
        check(self.keys() == [], 'config validate left probe objects behind')
        invalid_access = 'ponydb-e2e-invalid-access-key'
        invalid_storage = self.run_cli('config validate rejects inaccessible storage',
                              ['config', 'validate'], code=2, error='INVALID_CONFIG',
                              env={**self.env,
                              'PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID': invalid_access})
        check('LIST access' in invalid_storage['message'] and
              'HTTP 403' in invalid_storage['message'], 'missing validation diagnostics')
        check(all(secret not in json.dumps(invalid_storage)
                  for secret in (self.access, self.secret, invalid_access)),
              'validation exposed credentials')
