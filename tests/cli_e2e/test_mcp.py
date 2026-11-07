"""Real stdio MCP business operations, protocol recovery and cancellation."""
import json

from .support import CliTestCase, row
from .transports import stdio_mcp

COMMANDS = [('mcp',)]
LONG_QUERY = ('WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL '
              'SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n')


class McpTests(CliTestCase):
    def test_all_business_tools_and_persistence(self):
        with stdio_mcp(self, extra_env={'PONYDB_MCP__ENABLED': 'false'}) as mcp:
            # mcp.enabled disables HTTP MCP only; stdio still serves every tool.
            discovery = mcp.request('server/discover')['result']
            self.assertIn(mcp.version, discovery['supportedVersions'])
            tools = mcp.request('tools/list')['result']['tools']
            expected_tools = {tool['name'] for tool in tools}
            exercised = set()

            def call(name, arguments, error=None):
                exercised.add(name)
                return mcp.call(name, arguments, error=error)

            self.assertEqual(call('database_create', {'name': 'mcp'})['name'], 'mcp')
            detail = call('database_get', {'database': 'mcp'})
            self.assertEqual(detail['etag'], self.run_cli('stdio/CLI ETag', ['db', 'mcp'])['etag'])
            call('database_update', {'database': 'mcp', 'if_match': detail['etag'], 'description': 'MCP 更新'})
            call('database_update', {'database': 'mcp', 'if_match': detail['etag'], 'description': 'stale'},
                 error='METADATA_PRECONDITION_FAILED')
            self.assertEqual(self.run_cli('MCP update persisted', ['db', 'mcp'])['description'], 'MCP 更新')
            committed = call('sql_execute', {'database': 'mcp', 'statements': [
                {'sql': 'CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT)'},
                {'sql': 'INSERT INTO t VALUES(1, ?)', 'params': [{'type': 'text', 'value': '你好 🌍'}]},
            ]})
            call('sql_execute', {'database': 'mcp', 'statements': [
                {'sql': "INSERT INTO t VALUES(2,'rollback')"},
                {'sql': "INSERT INTO t VALUES(1,'duplicate')"},
            ]}, error='SQL_ERROR')
            self.assertEqual(row(self.run_cli('MCP rollback persisted', ['db', 'mcp', 'execute', 'SELECT count(*) FROM t']))[0]['value'], '1')
            self.run_cli('CLI write visible to MCP', ['db', 'mcp', 'execute', "INSERT INTO t VALUES(2,'current')"])
            self.assertEqual(row(call('sql_execute', {'database': 'mcp', 'sql': 'SELECT count(*) FROM t'}))[0]['value'], '2')
            schema = call('schema_get', {'database': 'mcp', 'table': 't', 'type': 'table'})
            self.assertEqual([record[1]['value'] for record in schema['results'][0]['rows']], ['t'])
            snapshots = call('snapshot_list', {'database': 'mcp'})['snapshots']
            snap = next(item['snapshot_id'] for item in snapshots if item['data_version'] == committed['data_version'])
            descriptor = call('snapshot_get', {'database': 'mcp', 'snapshot_id': snap})
            self.assertEqual(descriptor['data_version'], committed['data_version'])
            historical = call('sql_execute', {'database': 'mcp', 'sql': 'SELECT count(*) FROM t',
                                               'consistency': 'snapshot', 'snapshot_id': snap})
            self.assertEqual(row(historical)[0]['value'], '1')
            call('database_compact', {'database': 'mcp', 'budget_ms': 5000, 'max_segments': 32})
            gc = call('database_gc', {'database': 'mcp', 'budget_ms': 5000, 'max_objects': 1})
            self.assertTrue(gc['database_available'])
            self.assertEqual(gc['outcome'], 'applied' if gc['complete'] else 'partial')
            self.assertEqual(row(call('sql_execute', {'database': 'mcp', 'sql': 'SELECT count(*) FROM t'}))[0]['value'], '2')
            self.assertEqual([item['name'] for item in call('database_list', {})['databases']], ['mcp'])
            etag = call('database_get', {'database': 'mcp'})['etag']
            call('database_delete', {'database': 'mcp', 'if_match': etag})
            call('sql_execute', {'database': 'mcp', 'sql': 'SELECT 1'}, error='DATABASE_DELETED')
            call('database_get', {'database': 'missing'}, error='DATABASE_NOT_FOUND')
            self.assertEqual(exercised, expected_tools, 'Every advertised MCP tool needs an end-to-end call')

    def test_protocol_errors_are_recoverable_and_metadata_is_per_request(self):
        with stdio_mcp(self) as mcp:
            for wire, code in [('not json', -32700), ('[]', -32600)]:
                mcp.send(wire)
                invalid = self.receive_rpc(mcp.process)
                self.assertIsNone(invalid['id'])
                self.assertEqual(invalid['error']['code'], code)
            for method, params, code in [
                ('tools/list', {'_meta': {}}, -32602),
                ('tools/list', {'_meta': {'io.modelcontextprotocol/protocolVersion': '2099-01-01',
                                         'io.modelcontextprotocol/clientCapabilities': {}}}, -32022),
                ('unknown-method', {}, -32601),
                ('tools/call', {'name': 'unknown-tool', 'arguments': {}}, -32602),
                ('tools/call', {'name': 'database_get', 'arguments': {'database': 'missing', 'unknown': True}}, -32602),
            ]:
                with self.subTest(method=method, params=params):
                    response = mcp.request(method, **params)
                    self.assertEqual(response['error']['code'], code)
            self.assertIn('tools', mcp.request('tools/list')['result'])
            # A successful request never supplies metadata to a later request.
            response = self.rpc(mcp.process, {'jsonrpc': '2.0', 'id': 'no-meta', 'method': 'tools/list'})
            self.assertEqual(response['error']['code'], -32602)
            self.assertEqual(mcp.request('ping')['result']['resultType'], 'complete')

    def test_legacy_versions_and_modern_requests_share_a_process(self):
        self.create_database('legacy')
        for requested in ('2024-11-05', '2099-01-01'):
            with self.subTest(requested=requested), stdio_mcp(self) as mcp:
                initialized = self.rpc(mcp.process, {
                    'jsonrpc': '2.0', 'id': 'initialize', 'method': 'initialize',
                    'params': {'protocolVersion': requested, 'capabilities': {},
                               'clientInfo': {'name': 'e2e', 'version': '1'}}})
                self.assertEqual(initialized['result']['protocolVersion'],
                                 requested if requested == '2024-11-05' else '2025-11-25')
                not_ready = self.rpc(mcp.process, {'jsonrpc': '2.0', 'id': 'not-ready', 'method': 'tools/list'})
                self.assertEqual(not_ready['error']['code'], -32600)
                mcp.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
                listed = self.rpc(mcp.process, {'jsonrpc': '2.0', 'id': 'legacy', 'method': 'tools/call',
                                               'params': {'name': 'database_list', 'arguments': {}}})['result']
                self.assertNotIn('resultType', listed)
                content = json.loads(listed['content'][0]['text'])
                self.assertEqual([item['name'] for item in content['result']['databases']], ['legacy'])
                self.assertEqual('structuredContent' in listed, requested != '2024-11-05')
                modern = mcp.call('database_get', {'database': 'legacy'})
                self.assertEqual(modern['name'], 'legacy')

    def test_cancellation_keeps_ping_and_followup_queries_responsive(self):
        self.create_database('cancel')
        with stdio_mcp(self) as mcp:
            mcp.send(mcp.message('cancel-me', 'tools/call', name='sql_execute',
                                 arguments={'database': 'cancel', 'sql': LONG_QUERY}))
            self.assertEqual(mcp.request('ping')['result']['resultType'], 'complete')
            mcp.send({'jsonrpc': '2.0', 'method': 'notifications/cancelled', 'params': {'requestId': 'cancel-me'}})
            result = mcp.call('sql_execute', {'database': 'cancel', 'sql': 'SELECT 42'})
            self.assertEqual(row(result)[0]['value'], '42')
            # The context closes stdin, waits for EOF shutdown and rejects late output.

    def test_request_deadline_does_not_stop_the_session(self):
        self.create_database('deadline')
        with stdio_mcp(self, deadline_ms=100) as mcp:
            error = mcp.call('sql_execute', {'database': 'deadline', 'sql': LONG_QUERY}, error='DEADLINE_EXCEEDED')
            self.assertEqual(error['outcome'], 'not_committed')
            self.assertEqual(mcp.request('ping')['result']['resultType'], 'complete')

    def test_eof_cancels_inflight_query_and_exits(self):
        self.create_database('disconnect')
        with stdio_mcp(self) as mcp:
            mcp.send(mcp.message('inflight-at-eof', 'tools/call', name='sql_execute',
                                 arguments={'database': 'disconnect', 'sql': LONG_QUERY}))
            self.assertEqual(mcp.request('ping')['result']['resultType'], 'complete')
            # Closing stdin in the fixture must cancel work and exit within ten seconds.
        result = self.run_cli('database usable after MCP EOF', [
            'db', 'disconnect', 'execute', 'SELECT 42'])
        self.assertEqual(row(result)[0]['value'], '42')
