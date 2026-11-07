"""Real HTTP and stdio clients and bounded server/process lifecycles."""
from contextlib import contextmanager
import json
import socket
import time
import urllib.error
import urllib.request

ADMIN_TOKEN = 'ponydb-e2e-server-admin-token'
READER_TOKEN = 'ponydb-e2e-server-reader-token'


def metadata(version):
    return {'io.modelcontextprotocol/protocolVersion': version,
            'io.modelcontextprotocol/clientCapabilities': {}}


class HttpClient:
    def __init__(self, case, address, version):
        self.case, self.address, self.version = case, address, version

    def request(self, method, path, data=None, *, status=200, token=ADMIN_TOKEN,
                headers=None, raw=False):
        request_headers = {'Content-Type': 'application/json'}
        if token is not None:
            request_headers['Authorization'] = 'Bearer ' + token
        request_headers.update(headers or {})
        request_headers = {key: value for key, value in request_headers.items() if value is not None}
        if data is not None:
            data = data.encode('utf-8') if isinstance(data, str) else json.dumps(data).encode('utf-8')
        request = urllib.request.Request(self.address + path, data=data, method=method, headers=request_headers)
        try:
            response = urllib.request.urlopen(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            body = response.read()
            self.case.assertEqual(response.status, status,
                                  f'{method} {path}: {body[:2048]!r}')
            if raw:
                return response.headers, body
            self.case.assertIn('application/json', response.headers.get('Content-Type', ''))
            return response.headers, json.loads(body)

    def rpc(self, identifier, method, *, params=None, headers=None, **kwargs):
        params = {'_meta': metadata(self.version), **(params or {})}
        request_headers = {'Mcp-Protocol-Version': self.version, 'Mcp-Method': method,
                           'Accept': 'application/json, text/event-stream'}
        if method == 'tools/call':
            request_headers['Mcp-Name'] = params.get('name')
        request_headers.update(headers or {})
        _, value = self.request('POST', '/mcp',
                                {'jsonrpc': '2.0', 'id': identifier, 'method': method, 'params': params},
                                headers=request_headers, **kwargs)
        if kwargs.get('raw'):
            return value
        self.case.assertEqual(value.get('jsonrpc'), '2.0')
        self.case.assertEqual(value.get('id'), identifier)
        return value


@contextmanager
def http_server(case, extra_env=None):
    principals = [
        {'id': 'admin', 'token': ADMIN_TOKEN, 'databases': ['*'], 'scopes': ['*']},
        {'id': 'reader', 'token': READER_TOKEN, 'databases': ['http'],
         'scopes': ['db:list', 'db:read', 'data:read', 'schema:read']},
    ]
    env = {**case.env, 'PONYDB_SERVER__AUTH__PRINCIPALS': json.dumps(principals), **(extra_env or {})}
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    version = case.run_cli('HTTP MCP protocol version', ['version'])['mcp_protocol_version']
    client = HttpClient(case, f'http://127.0.0.1:{port}', version)
    with case.running(['server', '--listen', f'127.0.0.1:{port}'], env=env) as (process, stderr):
        deadline = time.monotonic() + 20
        while True:
            if process.poll() is not None:
                stderr.seek(0)
                case.fail('server exited during startup: ' + stderr.read())
            try:
                _, health = client.request('GET', '/healthz', token=None)
                case.assertEqual(health, {'status': 'ok'})
                break
            except (urllib.error.URLError, TimeoutError):
                if time.monotonic() >= deadline:
                    stderr.seek(0)
                    case.fail('server startup timed out: ' + stderr.read())
                time.sleep(0.05)
        yield client
        process.terminate()
        stdout, _ = process.communicate(timeout=10)
        case.assertEqual(process.returncode, 0)
        case.assertEqual(stdout, '', 'HTTP server leaked output to stdout')
        stderr.seek(0)
        case.assertEqual(case.filter_cache_diagnostics('server', stderr.read()), '')


class StdioClient:
    def __init__(self, case, process, version):
        self.case, self.process, self.version = case, process, version
        self.next_id = 0

    def message(self, identifier, method, **params):
        return {'jsonrpc': '2.0', 'id': identifier, 'method': method,
                'params': {'_meta': metadata(self.version), **params}}

    def send(self, message):
        line = message if isinstance(message, str) else json.dumps(message)
        self.process.stdin.write(line + '\n')
        self.process.stdin.flush()

    def request(self, method, **params):
        self.next_id += 1
        return self.case.rpc(self.process, self.message(self.next_id, method, **params))

    def call(self, name, arguments, error=None):
        value = self.request('tools/call', name=name, arguments=arguments)
        self.case.assertNotIn('error', value, value)
        result = value['result']
        self.case.assertEqual(result['resultType'], 'complete')
        self.case.assertEqual(result['isError'], error is not None, result)
        content = result['structuredContent']
        self.case.assertEqual(content['ok'], error is None)
        self.case.assertEqual(json.loads(result['content'][0]['text']), content)
        if error:
            self.case.assertEqual(content['error']['code'], error)
            return content['error']
        return content['result']


@contextmanager
def stdio_mcp(case, deadline_ms=30000, extra_env=None):
    version = case.run_cli('stdio MCP protocol version', ['version'])['mcp_stdio_protocol_version']
    with case.running(['mcp', '--deadline-ms', str(deadline_ms)],
                      env={**case.env, **(extra_env or {})}) as (process, stderr):
        yield StdioClient(case, process, version)
        process.stdin.close()
        process.wait(timeout=10)
        case.assertEqual(process.returncode, 0)
        case.assertEqual(process.stdout.read(), '', 'unexpected or late MCP stdout')
        stderr.seek(0)
        case.assertEqual(case.filter_cache_diagnostics('mcp', stderr.read()), '')
