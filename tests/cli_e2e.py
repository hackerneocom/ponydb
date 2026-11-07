#!/usr/bin/env python3
"""Run command-level CLI E2E tests; Python standard library only.

Build: cargo build --locked -p ponydb
All: ACCESS_KEY=... SECRET_KEY=... python3 tests/cli_e2e.py
Select: python3 tests/cli_e2e.py --suite execute --suite schema
Transports: python3 tests/cli_e2e.py --suite server --suite mcp
Offline: python3 tests/cli_e2e.py --offline
List: python3 tests/cli_e2e.py --list
See tests/README.md for S3 setup, isolation, and the command coverage map.
"""
import argparse
import unittest

from cli_e2e.catalog import test_modules
from cli_e2e.support import CliTestCase


def main():
    modules = test_modules()
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--suite', action='append', choices=modules,
                        help='Run a command test module; repeat to select multiple modules.')
    parser.add_argument('--list', action='store_true', help='List modules and their commands without running tests.')
    parser.add_argument('--offline', action='store_true', help='Skip tests requiring S3; run help, arguments, version and config show.')
    parser.add_argument('--failfast', action='store_true', help='Stop after the first failure (cleanup still runs).')
    args = parser.parse_args()
    selected = list(dict.fromkeys(args.suite or modules))
    if args.list:
        for name in selected:
            commands = ', '.join(' '.join(path) for path in modules[name].COMMANDS)
            print(f'{name}: {commands or "cross-command checks"}')
        return 0
    CliTestCase.offline = args.offline
    loader = unittest.TestLoader()
    suite = unittest.TestSuite(loader.loadTestsFromModule(modules[name]) for name in selected)
    result = unittest.TextTestRunner(verbosity=2, failfast=args.failfast).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == '__main__':
    raise SystemExit(main())
