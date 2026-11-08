#!/usr/bin/env python3
"""Test the actual release binaries and containers against disposable S3 storage."""
import argparse
from contextlib import contextmanager
import datetime
import hashlib
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

from release import ROOT, capture, require, run, sha256

RUSTFS = ('rustfs/rustfs:1.0.4@sha256:'
          'ed9210d6411d77ffceb70d94b964a3cb7d225983368bc25bebe01e995293dcc2')
COMPATIBILITY_IMAGES = (
    'debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc',
    'alpine:3.23@sha256:f3d55900449227ae9e237ede521e7b665d53a253b42065ba90b9db9275e198ad',
)
ACCESS, SECRET = 'release-test-access', 'release-test-secret'
TOKEN = 'ponydb-release-smoke-admin-token'
BUCKET = 'ponydb-release-test'


def request(address, method='GET', body=None, headers=None, status=200):
    headers = headers or {}
    if body is not None:
        body = json.dumps(body).encode()
        headers = {'Content-Type': 'application/json', **headers}
    req = urllib.request.Request(address, data=body, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        data = response.read()
        require(response.status == status, f'{method} {address}: {response.status}: {data[:1024]!r}')
        return json.loads(data) if data else None


def wait_ready(address):
    deadline = time.monotonic() + 90
    while True:
        try:
            request(address)
            return
        except (OSError, ValueError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.25)


def create_bucket(endpoint):
    # This fixture owns the entire RustFS instance, so bucket creation is isolated.
    host = endpoint.removeprefix('http://')
    now = datetime.datetime.now(datetime.timezone.utc)
    stamp, day = now.strftime('%Y%m%dT%H%M%SZ'), now.strftime('%Y%m%d')
    digest = hashlib.sha256(b'').hexdigest()
    signed = 'host;x-amz-content-sha256;x-amz-date'
    canonical = '\n'.join(['PUT', '/' + BUCKET, '',
                           f'host:{host}\nx-amz-content-sha256:{digest}\nx-amz-date:{stamp}\n', signed, digest])
    scope = day + '/us-east-1/s3/aws4_request'
    message = 'AWS4-HMAC-SHA256\n' + stamp + '\n' + scope + '\n' + hashlib.sha256(canonical.encode()).hexdigest()
    key = ('AWS4' + SECRET).encode()
    for part in (day, 'us-east-1', 's3', 'aws4_request'):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, message.encode(), hashlib.sha256).hexdigest()
    req = urllib.request.Request(endpoint + '/' + BUCKET, method='PUT', data=b'', headers={
        'x-amz-date': stamp, 'x-amz-content-sha256': digest,
        'Authorization': f'AWS4-HMAC-SHA256 Credential={ACCESS}/{scope}, SignedHeaders={signed}, Signature={signature}',
    })
    with urllib.request.urlopen(req, timeout=30) as response:
        require(response.status == 200, 'Could not create fixture bucket')


def storage_environment(endpoint, prefix):
    return {'PONYDB_STORAGE__ENDPOINT': endpoint, 'PONYDB_STORAGE__BUCKET': BUCKET,
            'PONYDB_STORAGE__REGION': 'us-east-1', 'PONYDB_STORAGE__PREFIX': prefix,
            'PONYDB_STORAGE__CREDENTIALS__ACCESS_KEY_ID': ACCESS,
            'PONYDB_STORAGE__CREDENTIALS__SECRET_ACCESS_KEY': SECRET}


def certificates(directory):
    ca, ca_key = directory / 'ca.pem', directory / 'ca.key'
    key, csr, cert = directory / 'server.key', directory / 'server.csr', directory / 'server.pem'
    run('openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
        '-subj', '/CN=ponydb-release-test-ca', '-addext', 'basicConstraints=critical,CA:TRUE',
        '-keyout', ca_key, '-out', ca, stderr=subprocess.DEVNULL)
    run('openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=release-tls',
        '-keyout', key, '-out', csr, stderr=subprocess.DEVNULL)
    extensions = directory / 'server.ext'
    extensions.write_text('subjectAltName=DNS:localhost,DNS:release-tls,IP:127.0.0.1\n'
                          'basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n')
    run('openssl', 'x509', '-req', '-in', csr, '-CA', ca, '-CAkey', ca_key, '-CAcreateserial',
        '-days', '1', '-extfile', extensions, '-out', cert, stderr=subprocess.DEVNULL)
    return ca, cert, key


@contextmanager
def storage():
    identifier = 'ponydb-release-' + uuid.uuid4().hex[:12]
    network, name = identifier, identifier + '-s3'
    run('docker', 'network', 'create', network)
    try:
        run('docker', 'run', '--detach', '--name', name, '--network', network,
            '--network-alias', 'storage', '--publish', '127.0.0.1::9000',
            '--env', 'RUSTFS_ADDRESS=:9000', '--env', 'RUSTFS_CONSOLE_ENABLE=false',
            '--env', 'RUSTFS_ACCESS_KEY=' + ACCESS, '--env', 'RUSTFS_SECRET_KEY=' + SECRET,
            RUSTFS, '/data')
        port = int(capture('docker', 'port', name, '9000/tcp').rsplit(':', 1)[1])
        endpoint = f'http://127.0.0.1:{port}'
        wait_ready(endpoint + '/health')
        create_bucket(endpoint)
        gateway = json.loads(capture('docker', 'network', 'inspect', network))[0]['IPAM']['Config'][0]['Gateway']
        yield {'network': network, 'endpoint': endpoint, 'port': port, 'gateway': gateway}
    except BaseException:
        subprocess.run(['docker', 'logs', name], check=False, timeout=30)
        raise
    finally:
        subprocess.run(['docker', 'rm', '--force', '--volumes', name], check=False, timeout=30)
        subprocess.run(['docker', 'network', 'rm', network], check=False, timeout=30)


@contextmanager
def tls_proxy(fixture, directory):
    ca, cert, key = certificates(directory)

    class Proxy(BaseHTTPRequestHandler):
        def forward(self):
            # Preserve Host: it is covered by the client's SigV4 signature.
            connection = http.client.HTTPConnection('127.0.0.1', fixture['port'], timeout=30)
            try:
                if self.headers.get('Transfer-Encoding', '').lower() == 'chunked':
                    chunks = []
                    while True:
                        size = int(self.rfile.readline().split(b';', 1)[0], 16)
                        if not size:
                            while self.rfile.readline() not in (b'\r\n', b''):
                                pass
                            break
                        chunks.append(self.rfile.read(size))
                        require(self.rfile.read(2) == b'\r\n', 'Invalid chunk framing')
                    body = b''.join(chunks)
                else:
                    body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                headers = {key: value for key, value in self.headers.items()
                           if key.lower() not in ('connection', 'transfer-encoding')}
                if self.headers.get('Transfer-Encoding'):
                    headers['Content-Length'] = str(len(body))
                connection.request(self.command, self.path, body=body, headers=headers)
                response = connection.getresponse()
                data = response.read()
                self.send_response(response.status)
                for header, value in response.getheaders():
                    if header.lower() not in ('connection', 'transfer-encoding', 'content-length'):
                        self.send_header(header, value)
                length = response.getheader('Content-Length', '0') if self.command == 'HEAD' else str(len(data))
                self.send_header('Content-Length', length)
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(data)
            finally:
                connection.close()

        do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = forward

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(('0.0.0.0', 0), Proxy)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield ca, server.server_port
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=10)


def container_test(image, fixture, version, commit, expected_binary):
    config = json.loads(capture('docker', 'image', 'inspect', image))[0]['Config']
    require(config['User'] == 'ponydb', 'Release image must use the unprivileged ponydb user')
    require(capture('docker', 'run', '--rm', '--entrypoint', '/usr/bin/id', image, '-u') == '10001',
            'Unexpected runtime UID')
    labels = config['Labels']
    require(labels['org.opencontainers.image.version'] == version
            and labels['org.opencontainers.image.revision'] == commit, 'Image source/version differs from release')
    diagnostic = json.loads(capture('docker', 'run', '--rm', image, 'version'))
    require(diagnostic['program'] == version, 'Image program version differs from release')
    env = storage_environment('http://storage:9000', 'container/' + uuid.uuid4().hex + '/')
    env.update({'PONYDB_SERVER__LISTEN': '0.0.0.0:8080',
                'PONYDB_SERVER__AUTH': json.dumps({'principals': [
                    {'id': 'admin', 'token': TOKEN, 'databases': ['*'], 'scopes': ['*']}]}),
                'PONYDB_MAINTENANCE__MAX_READ_DURATION_MS': '3000'})
    environment = [arg for key, value in env.items() for arg in ('--env', key + '=' + value)]
    headers = {'Authorization': 'Bearer ' + TOKEN}
    for attempt in range(2):
        name = 'ponydb-image-test-' + uuid.uuid4().hex[:12]
        try:
            run('docker', 'run', '--detach', '--name', name, '--network', fixture['network'],
                '--publish', '127.0.0.1::8080', '--tmpfs', '/cache:uid=10001,gid=10001,mode=0700',
                *environment, image)
            port = capture('docker', 'port', name, '8080/tcp').rsplit(':', 1)[1]
            address = 'http://127.0.0.1:' + port
            wait_ready(address + '/healthz')
            with tempfile.TemporaryDirectory() as temporary:
                binary = Path(temporary) / 'ponydb'
                run('docker', 'cp', name + ':/usr/local/bin/ponydb', binary)
                require(sha256(binary) == expected_binary, 'Image binary differs from downloadable CLI')
            request(address + '/v1/databases', status=401)
            if attempt == 0:
                request(address + '/v1/databases', 'POST', {'name': 'smoke'}, headers, status=201)
                request(address + '/v1/databases/smoke/execute', 'POST', {'statements': [
                    {'sql': 'CREATE TABLE t(n INTEGER)'}, {'sql': 'INSERT INTO t VALUES(42)'}]}, headers)
            value = request(address + '/v1/databases/smoke/execute', 'POST',
                            {'sql': 'SELECT n, sqrt(9), json_extract(jsonb_object(\'x\', 1), \'$.x\') FROM t'}, headers)
            require(value['results'][0]['rows'][0][0]['value'] == '42', 'S3 data did not survive fresh container/cache')
            protocol = diagnostic['mcp_protocol_version']
            rpc_headers = {**headers, 'MCP-Protocol-Version': protocol, 'MCP-Method': 'tools/call',
                           'MCP-Name': 'sql_execute', 'Accept': 'application/json, text/event-stream'}
            rpc = request(address + '/mcp', 'POST', {
                'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {
                    '_meta': {'io.modelcontextprotocol/protocolVersion': protocol,
                              'io.modelcontextprotocol/clientCapabilities': {}},
                    'name': 'sql_execute', 'arguments': {'database': 'smoke', 'sql': 'SELECT n FROM t'}}}, rpc_headers)
            require(rpc['result']['isError'] is False, 'Container HTTP MCP query failed')
            run('docker', 'stop', '--time', '10', name)
            require(capture('docker', 'inspect', '--format', '{{.State.ExitCode}}', name) == '0',
                    'Container did not exit cleanly on SIGTERM')
        except BaseException:
            subprocess.run(['docker', 'logs', name], check=False, timeout=30)
            raise
        finally:
            subprocess.run(['docker', 'rm', '--force', '--volumes', name], check=False, timeout=30)


def linux(args):
    binary = args.binary.resolve()
    arch = 'amd64' if platform.machine() == 'x86_64' else 'arm64'
    with storage() as fixture, tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        with tls_proxy(fixture, directory) as (ca, port):
            for base in COMPATIBILITY_IMAGES:
                env = storage_environment(f'https://release-tls:{port}', 'compat/' + uuid.uuid4().hex + '/')
                env['SSL_CERT_FILE'] = '/ca.pem'
                environment = [arg for key, value in env.items() for arg in ('--env', key + '=' + value)]
                run('docker', 'run', '--rm', '--network', fixture['network'],
                    '--add-host', 'release-tls:' + fixture['gateway'],
                    '--mount', f'type=bind,src={binary},dst=/ponydb,readonly',
                    '--mount', f'type=bind,src={ca},dst=/ca.pem,readonly',
                    *environment, base, '/ponydb', 'config', 'validate', '--target', 'cli', timeout=180)
            env = {key: value for key, value in os.environ.items() if not key.startswith('PONYDB_')}
            env.update({'E2E_CLI': str(binary), 'E2E_ENDPOINT': f'https://localhost:{port}',
                        'E2E_BUCKET': BUCKET, 'E2E_REGION': 'us-east-1', 'ACCESS_KEY': ACCESS,
                        'SECRET_KEY': SECRET, 'SSL_CERT_FILE': str(ca), 'PYTHONDONTWRITEBYTECODE': '1'})
            run('python3', 'tests/cli_e2e.py', env=env, timeout=900)
        (directory / 'binaries' / arch).mkdir(parents=True)
        shutil.copy2(binary, directory / 'binaries' / arch / 'ponydb')
        image = 'ponydb:release-test-' + uuid.uuid4().hex[:12]
        try:
            run('docker', 'buildx', 'build', '--load', '--target', 'release',
                '--build-context', 'binaries=' + str(directory / 'binaries'),
                '--build-arg', 'VERSION=' + args.version, '--build-arg', 'REVISION=' + args.commit,
                '--tag', image, '.', timeout=600)
            container_test(image, fixture, args.version, args.commit, sha256(binary))
        finally:
            subprocess.run(['docker', 'image', 'rm', image], check=False, timeout=30)


def registries(args):
    metadata = json.loads(args.metadata.read_text(encoding='utf-8'))
    require(metadata['commit'] == capture('git', 'rev-parse', 'HEAD'), 'Image checkpoint has a different source')
    target = ('x86_64' if platform.machine() == 'x86_64' else 'aarch64') + '-unknown-linux-musl'
    binary_hash = next(entry['binary_sha256'] for entry in metadata['artifacts'] if entry['target'] == target)
    # An empty Docker config makes these real anonymous pulls, independent of runner logins.
    with tempfile.TemporaryDirectory() as temporary, storage() as fixture:
        previous = os.environ.get('DOCKER_CONFIG')
        os.environ['DOCKER_CONFIG'] = temporary
        try:
            for key in ('ghcr', 'dockerhub'):
                image = metadata['images'][key] + '@' + metadata['images']['digest']
                run('docker', 'pull', image)
                container_test(image, fixture, metadata['version'], metadata['commit'], binary_hash)
        finally:
            if previous is None:
                os.environ.pop('DOCKER_CONFIG', None)
            else:
                os.environ['DOCKER_CONFIG'] = previous


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    command = commands.add_parser('linux')
    command.add_argument('--binary', type=Path, required=True)
    command.add_argument('--version', required=True)
    command.add_argument('--commit', required=True)
    command = commands.add_parser('registries')
    command.add_argument('--metadata', type=Path, required=True)
    args = parser.parse_args()
    globals()[args.command](args)


if __name__ == '__main__':
    main()
