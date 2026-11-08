"""Exercise the HTTPS fixture used by release S3 tests without requiring Docker."""
from contextlib import contextmanager
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import shutil
import ssl
import sys
import tempfile
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from release_smoke import tls_proxy


@contextmanager
def backend():
    class Handler(BaseHTTPRequestHandler):
        def do_PUT(self):
            data = self.rfile.read(int(self.headers['Content-Length']))
            body = json.dumps({'body': data.decode(), 'host': self.headers['Host'],
                               'authorization': self.headers.get('Authorization')}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_HEAD(self):
            self.send_response(200)
            self.send_header('Content-Length', '1234')
            self.end_headers()

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {'port': server.server_port}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=10)


@unittest.skipUnless(shutil.which('openssl'), 'OpenSSL is needed for the private test CA')
class TlsFixtureTests(unittest.TestCase):
    def test_https_proxy_preserves_signed_host_and_upload_body(self):
        with tempfile.TemporaryDirectory() as temporary, backend() as fixture:
            with tls_proxy(fixture, Path(temporary)) as (ca, port):
                connection = http.client.HTTPSConnection('localhost', port,
                                                         context=ssl.create_default_context(cafile=str(ca)), timeout=5)
                try:
                    connection.request('PUT', '/bucket/key', body=b'payload',
                                       headers={'Authorization': 'signed-request'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(json.loads(response.read()), {
                        'body': 'payload', 'host': f'localhost:{port}', 'authorization': 'signed-request'})
                finally:
                    connection.close()

    def test_chunked_upload_and_head_content_length(self):
        with tempfile.TemporaryDirectory() as temporary, backend() as fixture:
            with tls_proxy(fixture, Path(temporary)) as (ca, port):
                context = ssl.create_default_context(cafile=str(ca))
                connection = http.client.HTTPSConnection('localhost', port, context=context, timeout=5)
                try:
                    connection.request('PUT', '/bucket/key', body=iter([b'part1', b'part2']), encode_chunked=True)
                    self.assertEqual(json.loads(connection.getresponse().read())['body'], 'part1part2')
                    connection.request('HEAD', '/bucket/key')
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.getheader('Content-Length'), '1234')
                    self.assertEqual(response.read(), b'')
                finally:
                    connection.close()

    def test_https_fixture_rejects_an_untrusted_ca(self):
        with tempfile.TemporaryDirectory() as temporary, backend() as fixture:
            with tls_proxy(fixture, Path(temporary)) as (_, port):
                connection = http.client.HTTPSConnection('localhost', port, timeout=5)
                try:
                    with self.assertRaises(ssl.SSLCertVerificationError):
                        connection.request('HEAD', '/bucket/key')
                finally:
                    connection.close()


if __name__ == '__main__':
    unittest.main()
