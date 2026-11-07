"""Isolated CLI processes, SQLite fixtures, and prefix-scoped S3 cleanup."""
import datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.parse
import urllib.request
import uuid
import xml.etree.ElementTree as ET
from contextlib import contextmanager

ROOT = Path(__file__).resolve().parents[2]
CACHE_REASONS = {
    'busy', 'format invalid or corrupt', 'I/O unavailable', 'wait timed out',
    'entry checksum invalid', 'maintenance unavailable',
    'backend_identity_unavailable', 'user_cache_directory_unavailable',
}


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def row(value):
    return value['results'][0]['rows'][0]


def cache_diagnostic(line):
    try:
        value = json.loads(line)
    except json.JSONDecodeError:
        return False
    return (isinstance(value, dict) and value.get('level') == 'WARN'
            and value.get('target') == 'ponydb::cache'
            and value.get('fields', {}).get('event') == 'disk_cache_degraded'
            and value.get('fields', {}).get('reason') in CACHE_REASONS)


class CliTestCase(unittest.TestCase):
    needs_storage = True
    offline = False

    def setUp(self):
        if self.offline and self.needs_storage:
            self.skipTest('requires S3; omit --offline to run')
        self.endpoint = os.environ.get('E2E_ENDPOINT', 'http://localhost:9000').rstrip('/')
        self.bucket = os.environ.get('E2E_BUCKET', 'ponydb-test')
        self.region = os.environ.get('E2E_REGION', 'us-east-1')
        binary = 'ponydb.exe' if os.name == 'nt' else 'ponydb'
        self.cli = str(Path(os.environ.get('E2E_CLI', ROOT / 'target/debug' / binary)).resolve())
        self.assertTrue(Path(self.cli).is_file(), f'Build CLI first: cargo build --locked -p ponydb ({self.cli})')
        self.prefix = 'cli-e2e/' + uuid.uuid4().hex + '/'
        if self.needs_storage:
            self.assertTrue(os.environ.get('ACCESS_KEY') and os.environ.get('SECRET_KEY'),
                            'Set ACCESS_KEY and SECRET_KEY; see tests/README.md (or use --offline)')
        self.access = os.environ.get('ACCESS_KEY', 'offline-access-key')
        self.secret = os.environ.get('SECRET_KEY', 'offline-secret-key')
        directory = tempfile.TemporaryDirectory(prefix='ponydb-cli-e2e-')
        self.addCleanup(directory.cleanup)
        self.tmp = Path(directory.name)
        self.config = self.tmp / 'config.toml'
        self.config.write_text('[maintenance]\nmax_read_duration_ms = 3000\n', encoding='utf-8')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('PONYDB_')}
        self.env.update({
            'PONYDB_STORAGE__ENDPOINT': self.endpoint,
            'PONYDB_STORAGE__BUCKET': self.bucket,
            'PONYDB_STORAGE__REGION': self.region,
            'PONYDB_STORAGE__PREFIX': self.prefix,
            'PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID': self.access,
            'PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY': self.secret,
            'PONYDB_CACHE__DIRECTORY': str(self.tmp / 'cache'),
        })
        if self.needs_storage:
            print(f'\n{self.id()}: {self.endpoint}/{self.bucket}/{self.prefix}', flush=True)
            self.addCleanup(self.cleanup_storage)

    def command(self, args):
        return [self.cli, '--config', str(self.config), *args]

    def process(self, args, text=None, env=None):
        return subprocess.run(self.command(args), input=text, capture_output=True,
                              text=True, encoding='utf-8', env=self.env if env is None else env,
                              cwd=self.tmp, timeout=120)

    def filter_cache_diagnostics(self, label, stderr):
        remaining = []
        for line in stderr.splitlines(keepends=True):
            if cache_diagnostic(line):
                print(f'WARN {label}: {line.rstrip()}', file=sys.stderr, flush=True)
            else:
                remaining.append(line)
        return ''.join(remaining)

    def run_cli(self, label, args, data=None, code=0, error=None, text=None, env=None):
        if data is not None:
            check(text is None, label + ': choose DTO or raw stdin')
            args = [*args, '--input', '-']
            text = json.dumps(data)
        p = self.process(args, text=text, env=env)
        check(p.returncode in (code if isinstance(code, tuple) else (code,)),
              f'{label}: exit {p.returncode}, expected {code}; stdout={p.stdout}; stderr={p.stderr}')
        stderr = self.filter_cache_diagnostics(label, p.stderr)
        if error:
            check(not p.stdout, label + ': error leaked to stdout')
            start = stderr.find('{')
            check(start >= 0, label + ': missing JSON error: ' + stderr)
            check(all(line.startswith('Restore: ') for line in stderr[:start].splitlines()),
                  label + ': unexpected error prefix')
            value = json.loads(stderr[start:])
            check(value['code'] == error, f'{label}: {value}')
        else:
            if 'restore' in args and 'file' in args:
                check('Restore: complete' in stderr, label + ': missing completion progress')
                check(all(line.startswith('Restore: ') for line in stderr.splitlines()),
                      label + ': invalid progress log')
            else:
                check(not stderr, label + ': unexpected stderr: ' + stderr)
            value = (p.stdout if '--output' in args and args[args.index('--output') + 1] == 'table'
                     else json.loads(p.stdout))
        if isinstance(code, tuple):
            check(p.returncode == (6 if value.get('outcome') == 'partial' else 0),
                  label + ': incorrect partial exit code')
        return value

    def s3(self, method, key='', query=None, body=None):
        """Minimal SigV4 client, restricted to this test's prefix."""
        if key:
            check(key.startswith(self.prefix), 'S3 request escaped test prefix')
        else:
            check(method == 'GET' and query and query.get('prefix') == self.prefix,
                  'Only prefix-scoped bucket listing is allowed')
        path = '/' + urllib.parse.quote(self.bucket + '/' + key, safe='/~')
        query = '&'.join(urllib.parse.quote(str(k), safe='~') + '=' +
                         urllib.parse.quote(str(v), safe='~')
                         for k, v in sorted((query or {}).items()))
        now = datetime.datetime.now(datetime.timezone.utc)
        stamp, day = now.strftime('%Y%m%dT%H%M%SZ'), now.strftime('%Y%m%d')
        host = urllib.parse.urlsplit(self.endpoint).netloc
        digest = hashlib.sha256(body or b'').hexdigest()
        headers = f'host:{host}\nx-amz-content-sha256:{digest}\nx-amz-date:{stamp}\n'
        signed = 'host;x-amz-content-sha256;x-amz-date'
        if body is not None:
            headers += f'x-amz-meta-sha256:{digest}\n'
            signed += ';x-amz-meta-sha256'
        canonical = '\n'.join([method, path, query, headers, signed, digest])
        scope = day + '/' + self.region + '/s3/aws4_request'
        message = 'AWS4-HMAC-SHA256\n' + stamp + '\n' + scope + '\n' + hashlib.sha256(canonical.encode()).hexdigest()
        signing = ('AWS4' + self.secret).encode()
        for part in [day, self.region, 's3', 'aws4_request']:
            signing = hmac.new(signing, part.encode(), hashlib.sha256).digest()
        signature = hmac.new(signing, message.encode(), hashlib.sha256).hexdigest()
        request = urllib.request.Request(
            self.endpoint + path + ('?' + query if query else ''), data=body, method=method,
            headers={'x-amz-date': stamp, 'x-amz-content-sha256': digest,
                     **({'x-amz-meta-sha256': digest} if body is not None else {}),
                     'Authorization': f'AWS4-HMAC-SHA256 Credential={self.access}/{scope}, SignedHeaders={signed}, Signature={signature}'})
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.read()

    def keys(self):
        result, token = [], None
        while True:
            query = {'list-type': '2', 'prefix': self.prefix}
            if token:
                query['continuation-token'] = token
            doc = ET.fromstring(self.s3('GET', query=query))
            ns = {'s': 'http://s3.amazonaws.com/doc/2006-03-01/'}
            result.extend(item.text for item in doc.findall('s:Contents/s:Key', ns))
            token = doc.findtext('s:NextContinuationToken', namespaces=ns)
            if not token:
                return sorted(result)

    def cleanup_storage(self):
        objects = self.keys()
        for key in objects:
            self.s3('DELETE', key=key)
        self.assertEqual(self.keys(), [], 'cleanup left objects under ' + self.prefix)
        print(f'CLEANUP removed {len(objects)} objects; prefix empty', flush=True)

    def create_database(self, name='main'):
        self.run_cli('create fixture database', ['databases', 'create', name])
        return ['db', name]

    def history(self):
        """Three persisted rows; the retained snapshot contains only the first."""
        db = self.create_database()
        self.run_cli('fixture DDL', db + ['execute', 'CREATE TABLE t(id INTEGER PRIMARY KEY, n INTEGER UNIQUE)'])
        inserted = self.run_cli('fixture insert', db + ['execute', 'INSERT INTO t VALUES(1,1)'])
        snapshots = self.run_cli('fixture snapshots', db + ['snapshot', 'list'])['snapshots']
        snap = next(s['snapshot_id'] for s in snapshots if s['data_version'] == inserted['data_version'])
        self.run_cli('fixture current data', db + ['execute'], {'statements': [
            {'sql': 'INSERT INTO t VALUES(2,2)'}, {'sql': 'INSERT INTO t VALUES(3,3)'}]})
        return db, inserted, snap

    @contextmanager
    def running(self, args, env=None):
        # A file prevents unconsumed server diagnostics from filling a pipe.
        with tempfile.TemporaryFile(mode='w+', encoding='utf-8') as stderr:
            process = subprocess.Popen(self.command(args), stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=stderr,
                                       text=True, encoding='utf-8', env=self.env if env is None else env,
                                       cwd=self.tmp)
            try:
                yield process, stderr
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
                process.stdin.close()
                process.stdout.close()

    def rpc(self, process, message):
        process.stdin.write(json.dumps(message) + '\n')
        process.stdin.flush()
        value = self.receive_rpc(process)
        self.assertEqual(value.get('id'), message.get('id'))
        return value

    def receive_rpc(self, process):
        """Bounded line read; the process context also terminates a stalled reader."""
        result = queue.Queue()
        reader = threading.Thread(target=lambda: result.put(process.stdout.readline()), daemon=True)
        reader.start()
        try:
            line = result.get(timeout=30)
        except queue.Empty:
            self.fail('MCP response timed out')
        reader.join(timeout=1)
        value = json.loads(line)
        self.assertEqual(value.get('jsonrpc'), '2.0')
        return value
