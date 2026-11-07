"""HTTP REST and HTTP MCP against a real server process and S3."""
import json
import sqlite3

from .support import CliTestCase, row
from .transports import READER_TOKEN, http_server

COMMANDS = [('server',)]


class ServerTests(CliTestCase):
    def test_authentication_permissions_and_etags(self):
        with http_server(self) as http:
            for token in (None, 'invalid-bearer-token'):
                with self.subTest(token=token):
                    _, denied = http.request('GET', '/v1/databases', token=token, status=401)
                    self.assertEqual(denied['error']['code'], 'UNAUTHENTICATED')
            http.request('POST', '/v1/databases', {'name': 'http'}, status=201)
            self.create_database('private')
            headers, detail = http.request('GET', '/v1/databases/http', token=READER_TOKEN)
            etag = headers['ETag']
            self.assertEqual(etag, detail['etag'])
            self.assertEqual(etag, self.run_cli('HTTP/CLI ETag', ['db', 'http'])['etag'])
            _, listed = http.request('GET', '/v1/databases', token=READER_TOKEN)
            self.assertEqual([item['name'] for item in listed['databases']], ['http'])
            for method, path, body in [
                ('GET', '/v1/databases/private', None),
                ('POST', '/v1/databases', {'name': 'forbidden'}),
                ('PATCH', '/v1/databases/http', {'description': 'forbidden'}),
                ('POST', '/v1/databases/http/execute', {'sql': 'CREATE TABLE forbidden(x)'}),
            ]:
                with self.subTest(method=method, path=path):
                    _, denied = http.request(method, path, body, token=READER_TOKEN, status=403)
                    self.assertEqual(denied['error']['code'], 'FORBIDDEN')
            http.request('PATCH', '/v1/databases/http', {'description': 'HTTP 更新'},
                         headers={'If-Match': etag})
            self.assertEqual(self.run_cli('HTTP update persisted', ['db', 'http'])['description'], 'HTTP 更新')
            for method, body in [('PATCH', {'description': 'stale'}), ('DELETE', None)]:
                _, stale = http.request(method, '/v1/databases/http', body,
                                        headers={'If-Match': etag}, status=412)
                self.assertEqual(stale['error']['code'], 'METADATA_PRECONDITION_FAILED')
            current, _ = http.request('GET', '/v1/databases/http')
            _, conflict = http.request('PATCH', '/v1/databases/http', {'if_match': 'different'},
                                       headers={'If-Match': current['ETag']}, status=400)
            self.assertEqual(conflict['error']['code'], 'INVALID_REQUEST')
            http.request('DELETE', '/v1/databases/http', headers={'If-Match': current['ETag']})
            _, deleted = http.request('POST', '/v1/databases/http/execute', {'sql': 'SELECT 1'}, status=410)
            self.assertEqual(deleted['error']['code'], 'DATABASE_DELETED')
            _, listed = http.request('GET', '/v1/databases?include_deleted=true')
            self.assertIn('http', [item['name'] for item in listed['databases']])

    def test_sql_transactions_streaming_history_and_download(self):
        with http_server(self) as http:
            http.request('POST', '/v1/databases', {'name': 'http'}, status=201)
            path = '/v1/databases/http'
            _, committed = http.request('POST', path + '/execute', {'statements': [
                {'sql': 'CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT)'},
                {'sql': 'INSERT INTO t VALUES(1, ?)', 'params': [{'type': 'text', 'value': '你好 🌍'}]},
                {'sql': 'SELECT value FROM t'},
            ]})
            self.assertEqual(committed['results'][2]['rows'][0][0]['value'], '你好 🌍')
            _, snapshots = http.request('GET', path + '/snapshots')
            snap = next(item['snapshot_id'] for item in snapshots['snapshots']
                        if item['data_version'] == committed['data_version'])
            _, descriptor = http.request('GET', path + '/snapshots/' + snap)
            self.assertEqual(descriptor['data_version'], committed['data_version'])
            _, failed = http.request('POST', path + '/execute', {'statements': [
                {'sql': "INSERT INTO t VALUES(2,'rollback')"},
                {'sql': "INSERT INTO t VALUES(1,'duplicate')"},
            ]}, status=400)
            self.assertEqual(failed['error']['code'], 'SQL_ERROR')
            persisted = self.run_cli('HTTP rollback visible to CLI', ['db', 'http', 'execute', 'SELECT count(*) FROM t'])
            self.assertEqual(row(persisted)[0]['value'], '1')
            self.run_cli('CLI write visible to HTTP', ['db', 'http', 'execute', "INSERT INTO t VALUES(2,'current')"])
            headers, body = http.request('POST', path + '/execute', {'sql': 'SELECT value FROM t ORDER BY id'},
                                         headers={'Accept': 'application/x-ndjson'}, raw=True)
            self.assertIn('application/x-ndjson', headers['Content-Type'])
            lines = [json.loads(line) for line in body.splitlines()]
            self.assertEqual([line['type'] for line in lines], ['result_start', 'rows', 'done'])
            self.assertEqual([record[0]['value'] for record in lines[1]['rows']], ['你好 🌍', 'current'])
            self.assertEqual(lines[2]['outcome'], 'read_only')
            _, historical = http.request('POST', path + '/execute',
                                         {'sql': 'SELECT count(*) FROM t', 'consistency': 'snapshot', 'snapshot_id': snap})
            self.assertEqual(row(historical)[0]['value'], '1')
            _, schema = http.request('GET', path + '/schema?table=t&type=table')
            self.assertEqual([record[1]['value'] for record in schema['results'][0]['rows']], ['t'])
            for token, status, code in [(None, 401, 'UNAUTHENTICATED'), (READER_TOKEN, 403, 'FORBIDDEN')]:
                _, denied = http.request('GET', path + '/snapshots/' + snap + '/export', token=token, status=status)
                self.assertEqual(denied['error']['code'], code)
            headers, backup = http.request('GET', path + '/snapshots/' + snap + '/export', raw=True)
            self.assertEqual(headers['Content-Type'], 'application/vnd.sqlite3')
            self.assertEqual(int(headers['Content-Length']), len(backup))
            self.assertIn('snapshot.sqlite', headers['Content-Disposition'])
            file = self.tmp / 'http-snapshot.sqlite'
            file.write_bytes(backup)
            with sqlite3.connect(file) as connection:
                self.assertEqual(connection.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
                self.assertEqual(connection.execute('SELECT value FROM t').fetchall(), [('你好 🌍',)])
            http.request('POST', path + '/compact', {'budget_ms': 5000, 'max_segments': 32})
            _, gc = http.request('POST', path + '/gc', {'budget_ms': 5000, 'max_objects': 1})
            self.assertTrue(gc['database_available'])
            self.assertEqual(gc['outcome'], 'applied' if gc['complete'] else 'partial')
            _, intact = http.request('POST', path + '/execute', {'sql': 'SELECT count(*) FROM t'})
            self.assertEqual(row(intact)[0]['value'], '2')

    def test_invalid_requests_and_missing_resources(self):
        with http_server(self) as http:
            http.request('POST', '/v1/databases', {'name': 'http'}, status=201)
            for body in ('not json', '[]', '{"sql":"SELECT 1","sql":"SELECT 2"}',
                         {'sql': 'SELECT 1', 'unknown': True}, {'sql': 123},
                         {'database': 'other', 'sql': 'SELECT 1'}):
                with self.subTest(body=body):
                    _, invalid = http.request('POST', '/v1/databases/http/execute', body, status=400)
                    self.assertEqual(invalid['error']['code'], 'INVALID_REQUEST')
            for path, code, status in [
                ('/v1/databases/missing', 'DATABASE_NOT_FOUND', 404),
                ('/v1/databases/http/schema?table=missing', 'TABLE_NOT_FOUND', 404),
                ('/v1/databases/http/snapshots/missing/export', 'SNAPSHOT_EXPIRED', 410),
            ]:
                _, missing = http.request('GET', path, status=status)
                self.assertEqual(missing['error']['code'], code)
                self.assertFalse(missing['error']['retryable'])
            http.request('POST', '/v1/databases/http/restore', {}, status=404, raw=True)

    def test_http_mcp_tools_and_transport_validation(self):
        with http_server(self) as http:
            discover = http.rpc(1, 'server/discover')['result']
            self.assertIn(http.version, discover['supportedVersions'])
            tools = http.rpc(2, 'tools/list')['result']['tools']
            self.assertEqual(len(tools), self.run_cli('tool count', ['version'])['operations'])
            created = http.rpc(3, 'tools/call', params={
                'name': 'database_create', 'arguments': {'name': 'http'}})['result']
            self.assertFalse(created['isError'])
            result = http.rpc(4, 'tools/call', params={'name': 'sql_execute', 'arguments': {
                'database': 'http', 'statements': [{'sql': 'CREATE TABLE mcp(x)'},
                                                {'sql': 'INSERT INTO mcp VALUES(42)'}]}})['result']
            self.assertTrue(result['structuredContent']['ok'])
            self.assertEqual(row(self.run_cli('HTTP MCP persistence', ['db', 'http', 'execute', 'SELECT x FROM mcp']))[0]['value'], '42')
            denied = http.rpc(5, 'tools/call', token=READER_TOKEN, params={
                'name': 'sql_execute', 'arguments': {'database': 'http', 'sql': 'INSERT INTO mcp VALUES(99)'}})['result']
            self.assertTrue(denied['isError'])
            self.assertEqual(denied['structuredContent']['error']['code'], 'FORBIDDEN')
            for headers, code in [
                ({'Mcp-Protocol-Version': '2099-01-01'}, -32001),
                ({'Mcp-Method': 'tools/call'}, -32600),
            ]:
                with self.subTest(headers=headers):
                    response = http.rpc(6, 'tools/list', headers=headers, status=400)
                    self.assertEqual(response['error']['code'], code)
            response = http.rpc(7, 'tools/call', params={'name': 'database_list', 'arguments': {}},
                                headers={'Mcp-Name': 'database_get'}, status=400)
            self.assertEqual(response['error']['code'], -32600)
            response = http.rpc(8, 'tools/list', params={'_meta': {}})
            self.assertEqual(response['error']['code'], -32602)
            http.request('POST', '/mcp', {'jsonrpc': '2.0'}, headers={'Origin': 'https://example.invalid'},
                         status=403, raw=True)
            http.request('POST', '/mcp', {'jsonrpc': '2.0'}, token=None, status=401)
            http.rpc(9, 'tools/list', headers={'Accept': 'application/json'}, status=406, raw=True)

    def test_http_mcp_disabled_keeps_rest_available(self):
        with http_server(self, {'PONYDB_MCP__ENABLED': 'false'}) as http:
            http.request('POST', '/mcp', {}, status=404, raw=True)
            http.request('POST', '/v1/databases', {'name': 'http'}, status=201)
            self.assertEqual(self.run_cli('REST with MCP disabled', ['db', 'http'])['name'], 'http')

    def test_restart_preserves_data_and_observes_external_writes(self):
        with http_server(self) as http:
            http.request('POST', '/v1/databases', {'name': 'http'}, status=201)
            http.request('POST', '/v1/databases/http/execute', {'statements': [
                {'sql': 'CREATE TABLE persisted(x)'}, {'sql': 'INSERT INTO persisted VALUES(1)'},
            ]})
        self.run_cli('write while HTTP server stopped', [
            'db', 'http', 'execute', 'INSERT INTO persisted VALUES(2)'])
        with http_server(self) as http:
            _, result = http.request('POST', '/v1/databases/http/execute',
                                     {'sql': 'SELECT x FROM persisted ORDER BY x'})
            self.assertEqual([record[0]['value'] for record in result['results'][0]['rows']], ['1', '2'])
