"""End-to-end coverage for logging."""
import json

from .support import CliTestCase, check, row

COMMANDS = []


class LoggingTests(CliTestCase):
    def test_privacy_and_output_isolation(self):
        db = self.create_database()
        for level in ('debug', 'trace'):
            env = {**self.env, 'PONYDB_LOGGING__LEVEL': level, 'PONYDB_CACHE__DISK_BUDGET_MIB': '0'}
            request = {'sql': 'SELECT ? /* DO_NOT_LOG_SQL */',
                       'params': [{'type': 'text', 'value': 'DO_NOT_LOG_VALUE'}]}
            process = self.process([*db, 'execute', '--input', '-'], text=json.dumps(request), env=env)
            check(process.returncode == 0, f'{level} logging command failed: {process.stderr}')
            check(row(json.loads(process.stdout))[0]['value'] == 'DO_NOT_LOG_VALUE', level + ': result changed')
            events = [json.loads(line) for line in process.stderr.splitlines()]
            summaries = [event for event in events if event.get('fields', {}).get('event') == 'request_completed']
            check(len(summaries) == 1, level + ': missing or duplicate request summary')
            check(summaries[0]['fields']['outcome'] == 'read_only', level + ': incorrect outcome')
            check(summaries[0]['fields']['s3_requests'] > 0, level + ': missing S3 count')
            check(summaries[0]['fields']['page_reads'] > 0, level + ': missing page count')
            for marker in ('DO_NOT_LOG_SQL', 'DO_NOT_LOG_VALUE', self.access, self.secret, self.endpoint, str(self.config)):
                check(marker not in process.stderr, level + ': prohibited field in diagnostics')
            detail = any(event.get('fields', {}).get('event') == 's3_request_completed' for event in events)
            check(detail == (level == 'trace'), level + ': incorrect detail level')
            if level == 'trace':
                check(any(event.get('fields', {}).get('key') == self.prefix + 'databases/main/head.json'
                          for event in events), 'trace: actual object key missing')
