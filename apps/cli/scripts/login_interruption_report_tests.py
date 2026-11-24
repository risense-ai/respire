"""Offline checks for ReportContract."""
import ast
import json
import re
import uuid
from pathlib import Path
import subprocess
import tempfile
import unittest
from login_interruption_test_source import SOURCE, TREE


class ReportContract(unittest.TestCase):
    def test_host_rpc_failure_keeps_only_fixed_diagnostic_and_still_fails(self):
        from types import SimpleNamespace
        host = next(node for node in TREE.body
                    if isinstance(node, ast.FunctionDef) and node.name == 'host_login_continuation')
        rpc = next(node for node in host.body if isinstance(node, ast.FunctionDef) and node.name == 'rpc')
        marker = ('runtime maintenance is incomplete; use the host runtime restart or the indicated '
                  'snapshot/login recovery before accessing the library')
        diagnostic = next(node for node in TREE.body
                          if isinstance(node, ast.FunctionDef) and node.name == 'safe_cli_diagnostic')
        categories = ast.literal_eval(next(node.value for node in diagnostic.body
            if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == 'categories'
                                                    for target in node.targets)))
        for error, category in ((marker + '; private-account-material', 'host_maintenance_pending'),
                                ('host control response channel failure; private-account-material', 'control_response_channel'),
                                ('host control deadline exceeded; private-account-material', 'control_deadline'),
                                ('cannot wait for host control Runtime exit; private-account-material', 'control_exit_wait'),
                                ('parent authorization ended or control channel was interrupted; private-account-material', 'control_parent_or_channel'),
                                ('auth_diagnostic=auth_parent_revoked_or_channel_interrupted; private-account-material', 'control_parent_or_channel'),
                                ('private-account-material', 'unclassified')) + tuple(
                (text + '; private-account-material', category)
                for category, text in categories if category.startswith(('key_', 'config_', 'runtime_'))):
            with self.subTest(category=category):
                observation = {}

                def reject(args, **kwargs):
                    request = json.loads(kwargs['input'])
                    reply = {'v': 1, 'id': request['id'], 'ok': False, 'exit': 1,
                             'envelope': {'status': 'fail', 'summary': {}, 'errors': [error]}}
                    return subprocess.CompletedProcess(args, 1, json.dumps(reply).encode(), b'private-stderr')

                def require(value, message):
                    if not value:
                        raise RuntimeError(message)

                scope = dict(globals(), require=require, observation=observation, binary=Path('fixture-cli'), command_env={},
                             subprocess=SimpleNamespace(run=reject))
                exec(compile(ast.Module(body=[diagnostic, rpc], type_ignores=[]), '<actual-host-rpc>', 'exec'), scope)
                with self.assertRaisesRegex(RuntimeError, 'login_continuation_transport_mismatch'):
                    scope['rpc']('auth.reset', {}, 2)
                recorded = observation['last_rpc']['diagnostic']
                self.assertEqual(recorded['category'], category)
                self.assertEqual(observation['last_rpc']['exit'], 1)
                self.assertNotIn('private-', json.dumps(observation))

    def reporter(self):
        from contextlib import contextmanager
        from types import SimpleNamespace
        scope = dict(contextmanager=contextmanager, subprocess=subprocess, json=json)
        nodes = [node for node in TREE.body if
            isinstance(node, ast.FunctionDef) and node.name == 'fixture_report' or
            isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'FAILURE_CODES' for t in node.targets)]
        exec(compile(ast.Module(body=nodes, type_ignores=[]), '<actual-report>', 'exec'), scope)
        return scope['fixture_report'], SimpleNamespace

    def test_all_phases_write_safe_report_without_swallowing_failure(self):
        reporter, namespace = self.reporter()
        for phase in ('bootstrap', 'runtime_install', 'host_login_continue', 'initial_authentication', 'before_publish',
                      'owned_child_cleanup', 'runtime_cleanup', 'credential_cleanup', 'server_cleanup'):
            for error in (RuntimeError('secret-token-and-path'), ValueError('secret-token-and-path'),
                          RuntimeError('original_operation_identity_mismatch'),
                          subprocess.TimeoutExpired('secret-command', 1, output=b'secret-output')):
                with self.subTest(phase=phase, kind=type(error).__name__), tempfile.TemporaryDirectory() as td:
                    report = {'passed': True, 'cases': {}}
                    with self.assertRaises(type(error)) as raised:
                        with reporter(namespace(root=Path(td)), report):
                            report['stage'] = phase
                            raise error
                    self.assertIs(raised.exception, error)
                    saved = json.loads((Path(td) / 'interruption-coverage.json').read_text())
                    self.assertFalse(saved['passed'])
                    self.assertEqual(saved['stage'], phase)
                    self.assertIn(saved['failure_code'], ('unclassified_failure', 'subprocess_timeout',
                                                          'original_operation_identity_mismatch'))
                    self.assertNotIn('secret-', json.dumps(saved))

    def test_cleanup_failure_never_keeps_pass(self):
        reporter, namespace = self.reporter()
        with tempfile.TemporaryDirectory() as td:
            report = {'passed': True}
            with self.assertRaises(RuntimeError):
                with reporter(namespace(root=Path(td)), report):
                    report['stage'] = 'credential_cleanup'
                    raise RuntimeError('native_fixture_cleanup_failed')
            self.assertEqual(report['failure_code'], 'native_fixture_cleanup_failed')
            self.assertFalse(json.loads((Path(td) / 'interruption-coverage.json').read_text())['passed'])

    def test_success_and_outer_scope_cover_setup_and_cleanup(self):
        reporter, namespace = self.reporter()
        with tempfile.TemporaryDirectory() as td:
            report = {'passed': False}
            with reporter(namespace(root=Path(td)), report):
                report.update(passed=True, stage='complete')
            self.assertTrue(json.loads((Path(td) / 'interruption-coverage.json').read_text())['passed'])
        main = next(n for n in TREE.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
        outer = next(n for n in main.body if isinstance(n, ast.With))
        body = ast.unparse(outer)
        self.assertIn('install_published_runtime', body)
        cleanup = next(node for node in TREE.body
                       if isinstance(node, ast.FunctionDef) and node.name == 'cleanup_fixture')
        self.assertTrue(any(isinstance(node, ast.Try)
                            and any(isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Call)
                                    and ast.unparse(stmt.value.func) == 'cleanup_fixture'
                                    for stmt in node.finalbody)
                            for node in ast.walk(outer)))
        for item in ('stop_owned_control', 'keys.cleanup()', 'server.shutdown()'):
            self.assertIn(item, ast.unparse(cleanup))
        self.assertNotIn('diagnostic[:1500]', SOURCE)
