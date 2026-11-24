"""Barrier and HTTP fixture contract checks for login interruption."""
import ast
import copy
import hashlib
import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from login_interruption_test_source import TREE
from pathlib import Path
from unittest.mock import Mock
import io
import tempfile


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


class BarrierContract(unittest.TestCase):
    def test_gui_fixture_registers_once_and_records_only_effect_counts(self):
        factory = next(node for node in TREE.body if isinstance(node, ast.FunctionDef) and node.name == 'account_fixture_handler')
        handler = next(node for node in ast.walk(factory) if isinstance(node, ast.ClassDef) and node.name == 'Handler')
        with tempfile.TemporaryDirectory() as temp:
            receipt = Path(temp) / 'counts.json'
            state = {'vault': None, 'posts': 0, 'gets': 0, 'registers': 0, 'logins': 0,
                     'barrier': None, 'receipt': str(receipt), 'token': 'private-fixture-token'}
            scope = dict(BaseHTTPRequestHandler=object, json=json, Path=Path, state=state)
            exec(compile(ast.Module(body=[handler], type_ignores=[]), '<actual-shared-handler>', 'exec'), scope)
            instance = scope['Handler']()
            instance.send_error, instance.reply = Mock(), Mock()

            def post(path, payload, token=None):
                instance.path = path
                body = json.dumps(payload).encode()
                instance.rfile = io.BytesIO(body)
                instance.headers = {'Content-Length': str(len(body))}
                if token:
                    instance.headers['Authorization'] = 'Bearer ' + token
                instance.do_POST()

            post('/register', {'user': 'synthetic', 'pass_hash': 'synthetic-hash', 'salt': 'synthetic-salt'})
            instance.reply.assert_called_once_with({'user': 'synthetic', 'token': state['token']})
            self.assertEqual(json.loads(receipt.read_text()), {'posts': 0, 'registers': 1, 'logins': 0})
            # Both wraps come from actual products in the GUI run. This unit test
            # verifies transport/counters only and supplies no fake super or pending.
            post('/api/self/vault', {'version': 4, 'wrapped_urk': 'encrypted-register-wrap'}, state['token'])
            post('/api/self/vault', {'version': 4, 'wrapped_urk': 'encrypted-reset-wrap'}, state['token'])
            self.assertEqual(state['vault']['wrapped_urk'], 'encrypted-reset-wrap')
            self.assertEqual(json.loads(receipt.read_text()), {'posts': 2, 'registers': 1, 'logins': 0})
            frozen = receipt.read_bytes()
            post('/register', {'user': 'synthetic', 'pass_hash': 'synthetic-hash', 'salt': 'synthetic-salt'})
            instance.send_error.assert_called_with(409)
            post('/api/self/vault', {'wrapped_urk': 'unauthorized'}, 'wrong-token')
            instance.send_error.assert_called_with(401)
            self.assertEqual(receipt.read_bytes(), frozen)

    def test_reset_uses_authenticated_vault_storage_and_readback(self):
        main = next(node for node in TREE.body if isinstance(node, ast.FunctionDef) and node.name == 'account_fixture_handler')
        handler = next(node for node in ast.walk(main) if isinstance(node, ast.ClassDef) and node.name == 'Handler')
        state = {'vault': {'version': 4, 'wrapped_urk': 'original'}, 'posts': 0, 'gets': 0, 'logins': 0, 'barrier': None}
        scope = dict(BaseHTTPRequestHandler=object, json=json, state=state)
        exec(compile(ast.Module(body=[handler], type_ignores=[]), '<actual-vault-handler>', 'exec'), scope)
        instance = scope['Handler']()
        instance.path = '/api/self/vault'
        replacement = {'version': 4, 'wrapped_urk': 'replacement'}
        body = json.dumps(replacement).encode()
        instance.headers = {'Content-Length': str(len(body))}
        instance.send_error, instance.reply = Mock(), Mock()
        for method in (instance.do_GET, instance.do_POST):
            instance.rfile = io.BytesIO(body)
            before = copy.deepcopy(state)
            method()
            instance.send_error.assert_called_with(401)
            self.assertEqual(state, before)
            instance.reply.assert_not_called()
        instance.path = '/login'
        instance.rfile = io.BytesIO(b'{}')
        instance.headers = {'Content-Length': '2'}
        instance.do_POST()
        token = instance.reply.call_args.args[0]['token']
        self.assertEqual(state['logins'], 1)
        instance.reply.reset_mock()
        instance.path = '/api/self/vault'
        instance.headers = {'Authorization': 'Bearer ' + token, 'Content-Length': str(len(body))}
        instance.do_GET()
        instance.reply.assert_called_once_with({'version': 4, 'wrapped_urk': 'original'})
        instance.rfile = io.BytesIO(body)
        instance.do_POST()
        self.assertEqual(state['vault'], replacement)
        self.assertEqual(state['posts'], 1)
        instance.do_GET()
        instance.reply.assert_called_with(replacement)
        self.assertEqual(state['posts'], 1)

        # Exercise the actual HTTP handler: extra preflight pulls must not move
        # the failed post-copy push into a later login.
        scope = dict(BaseHTTPRequestHandler=BaseHTTPRequestHandler, json=json, require=require,
                     state=state, reached=threading.Event(), release=threading.Event(), v2=None)
        factory = next(node for node in TREE.body
                       if isinstance(node, ast.FunctionDef) and node.name == 'account_fixture_handler')
        exec(compile(ast.Module(body=[factory], type_ignores=[]), '<actual-http-handler>', 'exec'), scope)
        state.update(sync_blobs={}, sync_pulls=0, transfer_stage='initial_login', transfer_http=[])
        server = ThreadingHTTPServer(('127.0.0.1', 0), scope['account_fixture_handler'](
            state, scope['reached'], scope['release']))
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        origin = 'http://127.0.0.1:' + str(server.server_port)
        push = Request(origin + '/push/batch', data=json.dumps({'items': [
            {'id': 'synthetic-memory', 'deleted': False}]}).encode(),
            headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
        try:
            for _ in range(3):
                with urlopen(origin + '/pull?since=0', timeout=5) as response:
                    self.assertEqual(json.load(response)['total'], 0)
            with self.assertRaises(HTTPError) as failure:
                urlopen(push, timeout=5)
            self.assertEqual(failure.exception.code, 503)
            failure.exception.close()
            self.assertEqual(state['sync_blobs'], {})
            self.assertEqual(state['transfer_http'][-1], dict(stage='initial_login',
                method='POST', route='/push/batch', status=503))
            state['transfer_stage'] = 'retry_login'
            with urlopen(origin + '/pull', timeout=5) as response:
                self.assertEqual(json.load(response)['total'], 0)
            with urlopen(push, timeout=5) as response:
                self.assertEqual(json.load(response), {'replaced': [True]})
            with urlopen(origin + '/pull', timeout=5) as response:
                self.assertEqual(json.load(response)['total'], 1)
            self.assertTrue(all(event['status'] == 200 for event in state['transfer_http']
                                if event['stage'] == 'retry_login'))
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)

    def test_timeout_latches_all_observation_and_mutation_endpoints_closed(self):
        main = next(node for node in TREE.body if isinstance(node, ast.FunctionDef) and node.name == 'account_fixture_handler')
        handler = next(node for node in ast.walk(main) if isinstance(node, ast.ClassDef) and node.name == 'Handler')
        for barrier in ('before_publish', 'after_publish'):
            with self.subTest(barrier=barrier):
                state = {'vault': {'original': True}, 'posts': 0, 'gets': 0, 'barrier': barrier}
                release, reached = Mock(), Mock()
                release.wait.return_value = False
                scope = {'BaseHTTPRequestHandler': object, 'json': json, 'state': state,
                         'release': release, 'reached': reached}
                exec(compile(ast.Module(body=[handler], type_ignores=[]), '<actual-handler>', 'exec'), scope)
                instance = scope['Handler']()
                instance.path = '/api/self/vault'
                body = json.dumps({'candidate': True}).encode()
                instance.headers = {'Content-Length': str(len(body)), 'Authorization': 'Bearer synthetic-token'}
                instance.rfile = io.BytesIO(body)
                instance.send_error, instance.reply = Mock(), Mock()
                if barrier == 'before_publish':
                    # Both candidate preparation passes precede the publication check.
                    for expected_gets in (1, 2):
                        instance.do_GET()
                        self.assertEqual(state['gets'], expected_gets)
                        self.assertEqual(state['posts'], 0)
                        reached.set.assert_not_called()
                        release.wait.assert_not_called()
                        instance.send_error.assert_not_called()
                        instance.reply.assert_called_once_with(state['vault'])
                        instance.reply.reset_mock()
                    instance.do_GET()
                    self.assertEqual(state['gets'], 3)
                    self.assertEqual(state['posts'], 0)
                else:
                    instance.do_POST()
                self.assertTrue(state['barrier_failed'])
                release.wait.assert_called_once_with(120)
                reached.set.assert_called_once()
                instance.reply.assert_not_called()
                instance.send_error.assert_called_once_with(503)
                frozen = copy.deepcopy(state)
                instance.path = '/api/self/password'
                instance.do_POST()
                instance.path = '/api/self/vault'
                instance.do_GET()
                self.assertEqual(state, frozen)
                self.assertEqual(instance.send_error.call_count, 3)
                instance.reply.assert_not_called()
