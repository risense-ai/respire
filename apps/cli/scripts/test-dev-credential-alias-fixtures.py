#!/usr/bin/env python3
"""Offline fixture regressions; no native credential store or network access."""
import ast
from contextlib import closing, nullcontext
import importlib.util
import json
import sqlite3
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from credential_alias_test_support import HERE, load, native, secret_service, profile, legacy, USER, ALIAS, OTHER, PASSWORD, SUPER


from credential_runtime_identity_tests import KeylessRuntimeIdentityTests, RestartRetainedCredentialTests


from credential_alias_test_support import MemoryKeys


from credential_alias_custody_tests import AliasCustodyCases


class AliasTests(AliasCustodyCases, unittest.TestCase):
    def setUp(self):
        self.keys = MemoryKeys()
        self.session = dict(user=USER, keyring_account=ALIAS, keyring_backend='native')
        self.fixture = dict(account={'user': USER}, alias=ALIAS, code=SUPER, password=PASSWORD)
        self.smoke = object.__new__(profile.Smoke)
        self.smoke.keys = self.keys
        self.keys.created(ALIAS)
        self.keys.track_created_login(self.session, USER, SUPER, PASSWORD)

    def test_current_target_uses_original_alias_not_username(self):
        self.keys.reads.clear()
        self.smoke.verify_retained_current_credentials(self.session, self.fixture)
        self.assertEqual(self.keys.reads, [('rsrs', 'super:' + ALIAS), ('rsrs', 'pass:' + ALIAS)])

    def test_foreign_user_alias_missing_alias_rejected_before_read(self):
        for change in ({'user': 'foreign'}, {'keyring_account': OTHER}, {'keyring_account': None}):
            with self.subTest(change=list(change)):
                self.keys.reads.clear()
                with self.assertRaisesRegex(RuntimeError, '^existing_target_credential_identity_changed$'):
                    self.smoke.verify_retained_current_credentials(dict(self.session, **change), self.fixture)
                self.assertFalse(self.keys.reads)

    def test_unowned_alias_rejected(self):
        self.keys.entries.clear()
        with self.assertRaisesRegex(RuntimeError, '^credential_not_owned_by_fixture$'):
            self.smoke.verify_retained_current_credentials(self.session, self.fixture)

    def test_wrong_or_missing_credential_not_hidden(self):
        for value in ('wrong', None):
            self.keys.values['rsrs', 'pass:' + ALIAS] = value
            with self.assertRaises(RuntimeError):
                self.smoke.verify_retained_current_credentials(self.session, self.fixture)

    def test_tracking_rejects_foreign_user_and_invalid_alias(self):
        for session in ({'user': 'foreign', 'keyring_account': OTHER}, {'user': USER},
                        {'user': USER, 'keyring_account': 'foreign'}):
            keys = MemoryKeys()
            with self.assertRaises(RuntimeError):
                keys.track_created_login(session, USER, SUPER, PASSWORD)
            self.assertFalse(keys.entries)
            self.assertFalse(keys.reads)
        operation = '33333333-3333-4333-8333-333333333333'
        alias = 'login-' + operation.replace('-', '')
        candidate = dict(user='', addr='', vault_version=4, auth_operation_id=operation,
                         keyring_account=alias, keyring_backend='native')
        keys = MemoryKeys()
        keys.values['rsrs', 'super:' + alias] = SUPER
        control = ('rsrs', 'key-control:' + alias)
        payload = dict(password='', super_password=SUPER, account_secret='', totp=None,
            material=dict(keyring_account=alias, keyring_backend='native', reset=None, challenge=None))
        keys.values[control] = json.dumps(payload)
        keys.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
        keys.values['rsrs', 'pass:' + alias] = PASSWORD
        keys.track_created_login(dict(candidate, user=USER, addr=legacy.UPSTREAM), USER, SUPER, PASSWORD,
                                 recovery_operation=operation)
        self.assertEqual(keys.entries, {('rsrs', 'super:' + alias), ('rsrs', 'pass:' + alias), control})
        with self.assertRaises(RuntimeError):
            keys.track_created_login(dict(candidate, user=USER), USER, SUPER, PASSWORD, recovery_operation=operation)
        for field, value in (('auth_operation_id', 'wrong'), ('keyring_account', OTHER), ('vault_version', 3)):
            with self.assertRaises(RuntimeError):
                MemoryKeys().track_created_login(dict(candidate, **{field: value}), '', SUPER, None,
                                                recovery_operation=operation)
        self.assertTrue(keys.cleanup()['passed'])
        self.assertFalse(keys.values)
        for invalid in ('invalid JSON', json.dumps(dict(payload, super_password='foreign')),
                        json.dumps(dict(payload, password=PASSWORD)),
                        json.dumps(dict(payload, material=dict(payload['material'], keyring_account=OTHER))),
                        json.dumps(dict(payload, material=dict(payload['material'], keyring_backend='kernel')))):
            rejected = MemoryKeys()
            rejected.values['rsrs', 'super:' + alias] = SUPER
            rejected.values[control] = invalid
            with self.assertRaises(RuntimeError):
                rejected.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
            self.assertNotIn(control, rejected.entries)
            rejected.cleanup()
            self.assertEqual(rejected.values, {control: invalid})
        changed = MemoryKeys()
        changed.values.update({('rsrs', 'super:' + alias): SUPER, control: json.dumps(payload)})
        changed.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
        changed.values[control] = 'third-party-private-material'
        report = changed.cleanup()
        self.assertFalse(report['passed'])
        self.assertEqual(changed.values, {control: 'third-party-private-material'})
        for secret in (SUPER, alias, 'third-party-private-material'):
            self.assertNotIn(secret, json.dumps(report))
        with patch.object(native.sys, 'platform', 'darwin'):
            mac = MemoryKeys()
            mac.values.update({('rsrs', 'super:' + alias): SUPER, control: json.dumps(payload)})
            verifier = Mock()
            mac.track_created_login(candidate, '', SUPER, None, recovery_operation=operation,
                                    recovery_verifier=verifier)
            verifier.assert_called_once_with(payload)
            self.assertNotIn(control, mac.reads)
            verifier.side_effect = RuntimeError('native-material-conflict')
            rejected = mac.cleanup()
            self.assertFalse(rejected['passed'])
            self.assertIn(control, mac.values)
            verifier.side_effect = None
            self.assertTrue(mac.cleanup()['passed'])
            self.assertEqual(verifier.call_count, 3)
            self.assertFalse(mac.values)
            unverified = MemoryKeys()
            unverified.values.update({('rsrs', 'super:' + alias): SUPER, control: json.dumps(payload)})
            with self.assertRaisesRegex(RuntimeError, 'native_verifier_required'):
                unverified.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
            self.assertNotIn(control, unverified.entries)
            self.assertNotIn(control, unverified.reads)
            self.assertFalse(unverified.cleanup()['passed'])
            self.assertEqual(unverified.values, {control: json.dumps(payload)})
            with tempfile.TemporaryDirectory() as directory:
                manager = object.__new__(native.MacKeys)
                native.TrackedKeys.__init__(manager, Path(directory))
                manager.created, manager.homes = True, {}
                manager.keychain = Path(directory) / 'fixture.keychain-db'
                manager._security = Mock(side_effect=[SimpleNamespace(returncode=code) for code in (0, 0, 44)])
                failed = Mock(side_effect=RuntimeError('native-material-conflict'))
                with self.assertRaisesRegex(RuntimeError, 'native-material-conflict'):
                    manager.track_created_login(candidate, '', SUPER, None, recovery_operation=operation,
                                                recovery_verifier=failed)
                self.assertNotIn(control, manager.entries)
                self.assertEqual(manager.recovery_conflicts, {control})
                report = manager.cleanup()  # Real Mac/base cleanup chain, not a mocked failure report.
                self.assertFalse(report['passed'])
                self.assertIn({'code': 'recovery_control_verification_failed'}, report['remaining_entries'])
                self.assertTrue(manager.created)
                failed.assert_called_once_with(payload)
                for call in manager._security.call_args_list:
                    self.assertNotIn('-w', call.args[1])
                    self.assertNotEqual(call.args[1][0], 'delete-keychain')
                    self.assertNotIn(control[1], call.args[1])
                self.assertEqual(manager._security.call_count, 3)
                for secret in (SUPER, alias, 'native-material-conflict'):
                    self.assertNotIn(secret, json.dumps(report))
                manager = object.__new__(native.MacKeys)
                native.TrackedKeys.__init__(manager, Path(directory))
                manager.created, manager.homes = True, {}
                manager.keychain = Path(directory) / 'fixture.keychain-db'
                manager._security = Mock(side_effect=[SimpleNamespace(returncode=code)
                    for code in (0, 0, 0, 0, 44, 0, 44, 0, 44, 0)])
                verifier = Mock()
                manager.track_created_login(candidate, '', SUPER, None, recovery_operation=operation,
                                            recovery_verifier=verifier)
                manager.track_created_login(dict(candidate, user=USER, addr=legacy.UPSTREAM), USER,
                                            SUPER, PASSWORD, recovery_operation=operation)
                self.assertTrue(manager.cleanup()['passed'])
                self.assertFalse(manager.created)
                self.assertEqual(verifier.call_count, 2)
                for call in manager._security.call_args_list:
                    self.assertNotIn('-w', call.args[1])
                self.assertEqual(manager._security.call_count, 10)

    def test_wrong_password_tracked_for_cleanup_without_secret_output(self):
        keys = MemoryKeys()
        keys.created(OTHER, None)
        with self.assertRaisesRegex(RuntimeError, '^login_credential_fixture_value_changed$') as raised:
            keys.track_created_login(dict(user=USER, keyring_account=OTHER), USER, None, 'wrong')
        report = keys.cleanup()
        self.assertTrue(report['passed'])
        self.assertFalse(keys.values)
        for forbidden in (PASSWORD, SUPER, OTHER, USER):
            self.assertNotIn(forbidden, str(raised.exception) + json.dumps(report))

    def test_tty_password_login_versions_track_before_return(self):
        for version in (1, 2):
            with self.subTest(version=version):
                keys = MemoryKeys()
                smoke = object.__new__(legacy.Smoke)
                smoke.keys = keys
                aliases = iter((ALIAS, OTHER))
                current = {}
                def cli(env, *args, **kwargs):
                    prompts = kwargs['prompts']
                    self.assertEqual(prompts[-1][1] == 'n', not bool(current.get('token')))
                    if not current.get('token'):
                        prompt = prompts[-1][0].lstrip()
                        self.assertIn('"' + prompt + '"', (HERE.parent / 'cli/src/login.rs').read_text(encoding='utf-8'))
                    alias = next(aliases)
                    keys.created(alias, None if version == 1 else SUPER)
                    current.update(user=USER, keyring_account=alias, token="synthetic-token", vault_version=version)
                    if version in (1, 3):
                        current['legacy_secret_backend'] = 'native'
                        keys.values['rsrs', 'legacy-secret:' + alias] = 'synthetic-factor'
                    return {'summary': {'ok': True}}
                smoke.cli = cli
                smoke.session = lambda env: current
                for env in ({}, {'RSRS_PASS': PASSWORD}):
                    smoke.password_login(env, USER, PASSWORD, version, SUPER)
                    self.assertIn(('rsrs', 'pass:' + current['keyring_account']), keys.entries)
                self.assertEqual(len(keys.entries), 6 if version == 3 else 4)
                self.assertTrue(keys.cleanup()['passed'])
                self.assertFalse(keys.values)

    def test_interactive_factor_env_is_removed_only_from_login_copy(self):
        for version in (1, 2):
            keys = MemoryKeys()
            smoke = object.__new__(legacy.Smoke)
            smoke.keys = keys
            current = {'user': USER}
            env = {'RSRS_SUPER': SUPER, 'RSRS_DATA_DIR': 'owned', 'RSRS_PASS': PASSWORD}
            before = dict(env)
            def cli(child_env, *args, **kwargs):
                self.assertIsNot(child_env, env)
                self.assertEqual(child_env.get('RSRS_SUPER'), SUPER if version == 1 else None)
                prompts = kwargs['prompts']
                self.assertEqual(prompts[1], ('  login password: ', PASSWORD))
                self.assertEqual(len(prompts), 3 if version == 1 else 4)
                self.assertEqual(prompts[-1], ('  Merge selected local memories into this server account (skip identical bodies; keep existing records)? [y/N]: ', 'n'))
                if version != 1:
                    self.assertEqual(prompts[2], ('  super password (A3-…): ', SUPER))
                self.assertNotIn(SUPER, args)
                self.assertNotIn(PASSWORD, args)
                keys.created(ALIAS, None if version == 1 else SUPER)
                current.update(keyring_account=ALIAS, token='synthetic-token', vault_version=version)
                if version in (1, 3):
                    current['legacy_secret_backend'] = 'native'
                    keys.values['rsrs', 'legacy-secret:' + ALIAS] = 'synthetic-factor'
                return {'summary': {}}
            smoke.cli = cli
            smoke.session = lambda env: dict(current)
            smoke.password_login(env, USER, PASSWORD, version, SUPER)
            self.assertEqual(env, before)
            self.assertTrue(keys.cleanup()['passed'])
            self.assertFalse(keys.values)

    def test_v1_fresh_login_blocks_use_hidden_factor_without_a_seeded_session(self):
        tree = ast.parse((HERE / 'legacy_vault_scenarios.py').read_text(encoding='utf-8'))
        run = next(node for node in ast.walk(tree)
                   if isinstance(node, ast.FunctionDef) and node.name == 'run_version')
        for variable in ('old_login_env', 'final_env'):
            start = next(index for index, node in enumerate(run.body)
                         if isinstance(node, ast.Assign)
                         and any(isinstance(target, ast.Name) and target.id == variable for target in node.targets))
            end = next(index for index, node in enumerate(run.body[start:], start)
                       if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                       and isinstance(node.value.func, ast.Attribute) and node.value.func.attr == 'password_login')
            code = compile(ast.Module(body=run.body[start:end + 1], type_ignores=[]),
                           '<actual-v1-login-setup>', 'exec')
            for fails in (False, True):
                with self.subTest(profile=variable, fails=fails), tempfile.TemporaryDirectory() as temp:
                    smoke = object.__new__(legacy.Smoke)
                    smoke.root, smoke.model = Path(temp), Path(temp) / 'models'
                    smoke.base_env, smoke.report = {}, {}
                    smoke.args = SimpleNamespace(binary=Path('fixture-cli'))
                    smoke.save = Mock()
                    keys = MemoryKeys()
                    keys.configure_env = Mock()
                    smoke.keys = keys
                    factor = 'fixture-account-secret'

                    def cli(env, *args, **kwargs):
                        self.assertFalse((Path(env['RSRS_DATA_DIR']) / 'session.json').exists())
                        self.assertEqual(kwargs['prompts'][-2], ('  original v1 Account Secret: ', factor))
                        self.assertEqual(kwargs['prompts'][-1][1], 'n')
                        self.assertNotIn(factor, args)
                        if fails:
                            raise RuntimeError('auth_failed')
                        keys.created(ALIAS, None)
                        keys.values['rsrs', 'legacy-secret:' + ALIAS] = factor
                        target = Path(env['RSRS_DATA_DIR']) / 'accounts' / 'fixture-account'
                        target.mkdir(parents=True)
                        (Path(env['RSRS_DATA_DIR']) / 'client.json').write_text(
                            json.dumps({'data_dir': str(target)}), encoding='utf-8')
                        smoke.write_session(dict(env, RSRS_DATA_DIR=str(target)), dict(user=USER,
                            keyring_account=ALIAS, token='fixture-token', vault_version=1,
                            keyring_backend='native', legacy_secret_backend='native'))
                        return {'summary': {'ok': True, 'dir': str(target)}}

                    smoke.cli = cli
                    scope = dict(self=smoke, version=1, user=USER, password=PASSWORD,
                                 original_super=factor, secret=factor)
                    with patch.object(legacy, 'install_published_runtime'):
                        if fails:
                            with self.assertRaisesRegex(RuntimeError, '^auth_failed$'):
                                exec(code, scope)
                            self.assertFalse(keys.entries)
                        else:
                            exec(code, scope)
                            self.assertEqual(keys.entries, {('rsrs', 'pass:' + ALIAS), ('rsrs', 'legacy-secret:' + ALIAS)})
                    self.assertTrue(keys.cleanup()['passed'])
                    self.assertFalse(keys.values)
                    root = Path(scope[variable]['RSRS_DATA_DIR'])
                    session_path = smoke.profile(scope[variable]) / 'session.json'
                    if not fails:
                        self.assertNotEqual(session_path.parent, root)
                        self.assertFalse((root / 'session.json').exists())
                    session_path.write_text('{', encoding='utf-8')
                    with self.assertRaises(json.JSONDecodeError):
                        smoke.session(scope[variable])
                    config_path = root / 'client.json'
                    for selected, failure_code in ((str(root.parent), 'login_profile_selection_outside_fixture'),
                                                   ('', 'login_profile_selection_invalid'),
                                                   (None, 'login_profile_selection_invalid')):
                        config_path.write_text(json.dumps({'data_dir': selected}), encoding='utf-8')
                        with self.assertRaisesRegex(RuntimeError, '^' + failure_code + '$'):
                            smoke.session(scope[variable])
                    config_path.write_text('{', encoding='utf-8')
                    with self.assertRaises(json.JSONDecodeError):
                        smoke.session(scope[variable])

    def test_v3_fresh_login_blocks_use_stdin_rpc_and_keep_failures_fatal(self):
        tree = ast.parse((HERE / 'legacy_vault_scenarios.py').read_text(encoding='utf-8'))
        run = next(node for node in ast.walk(tree)
                   if isinstance(node, ast.FunctionDef) and node.name == 'run_version')
        for variable in ('old_login_env', 'final_env'):
            start = next(index for index, node in enumerate(run.body)
                         if isinstance(node, ast.Assign)
                         and any(isinstance(target, ast.Name) and target.id == variable for target in node.targets))
            end = next(index for index, node in enumerate(run.body[start:], start)
                       if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                       and isinstance(node.value.func, ast.Attribute) and node.value.func.attr == 'password_login')
            code = compile(ast.Module(body=run.body[start:end + 1], type_ignores=[]),
                           '<actual-v3-login-setup>', 'exec')
            failures = dict(auth='cli_failed_login', pending='cli_failed_login',
                transport='legacy_login_rpc_transport_mismatch', wire_version='legacy_login_rpc_transport_mismatch',
                wire_exit='legacy_login_rpc_transport_mismatch', wire_ok='legacy_login_rpc_transport_mismatch',
                transaction='legacy_login_rpc_transaction_mismatch', operation='legacy_login_rpc_transaction_mismatch',
                outcome='legacy_login_rpc_transaction_mismatch', identity='legacy_login_rpc_identity_mismatch',
                required_inputs='legacy_login_rpc_identity_mismatch', status='legacy_login_rpc_not_committed',
                errors='cli_error_login', stop='owned_runtime_stop_failed', json='invalid_cli_json_login')
            for failure in (None, *failures):
                with self.subTest(profile=variable, failure=failure), tempfile.TemporaryDirectory() as temp:
                    smoke = object.__new__(legacy.Smoke)
                    smoke.root, smoke.model = Path(temp), Path(temp) / 'models'
                    smoke.base_env, smoke.report, smoke.diagnostic_secrets = {}, {}, {PASSWORD, SUPER, 'fixture-secret-key'}
                    smoke.args = SimpleNamespace(binary=Path('fixture-cli'))
                    smoke.save = Mock()
                    keys = MemoryKeys()
                    keys.configure_env = Mock()
                    smoke.keys = keys
                    requests, stops = [], []
                    factor = 'fixture-secret-key'
                    committed = failure not in ('auth', 'pending')

                    def child(argv, **kwargs):
                        env = kwargs['env']
                        if '--stop' in argv:
                            stops.append(argv)
                            return SimpleNamespace(returncode=1 if failure == 'stop' else 0)
                        self.assertEqual(argv, ['fixture-cli', '--json', '--host-rpc-stdio'])
                        self.assertNotIn('RSRS_SUPER', env)
                        self.assertFalse((Path(env['RSRS_DATA_DIR']) / 'session.json').exists())
                        request = json.loads(kwargs['input'])
                        requests.append(request)
                        self.assertEqual(request['v'], 1)
                        self.assertEqual(request['method'], 'auth.login')
                        self.assertEqual(request['params'], dict(addr=legacy.UPSTREAM, user=USER,
                            password=PASSWORD, super=SUPER, continuation=json.dumps({'secret_key': factor})))
                        self.assertNotIn(factor, str(argv))
                        self.assertNotIn(PASSWORD, str(argv))
                        self.assertNotIn(SUPER, str(argv))
                        operation = '12345678-1234-4234-8234-123456789abc'
                        self.assertNotEqual(request['id'], operation)
                        receipt = dict(operation_id=operation, outcome='committed',
                            source_profile=str(Path(env['RSRS_DATA_DIR']).resolve()),
                            login=dict(user=USER, addr=legacy.UPSTREAM, required_inputs=[]))
                        envelope = dict(status='ok', summary=receipt, errors=[])
                        if committed:
                            keys.created(ALIAS)
                            keys.values['rsrs', 'legacy-secret:' + ALIAS] = factor
                            target = Path(env['RSRS_DATA_DIR']) / 'accounts' / 'fixture-account'
                            target.mkdir(parents=True)
                            (Path(env['RSRS_DATA_DIR']) / 'client.json').write_text(
                                json.dumps({'data_dir': str(target)}), encoding='utf-8')
                            smoke.write_session(dict(env, RSRS_DATA_DIR=str(target)), dict(user=USER,
                                keyring_account=ALIAS, token='fixture-token', vault_version=3,
                                keyring_backend='native', legacy_secret_backend='native'))
                        exit_code = 1 if failure == 'auth' else 2 if failure == 'pending' else 0
                        if failure in ('auth', 'pending'):
                            envelope.update(status='fail' if failure == 'auth' else 'pending',
                                errors=[PASSWORD + SUPER + factor] if failure == 'auth' else [])
                            receipt['outcome'] = 'awaiting_input'
                        if failure == 'transaction':
                            receipt['source_profile'] = 'foreign-profile'
                        if failure == 'operation':
                            receipt['operation_id'] = request['id']
                        if failure == 'outcome':
                            receipt['outcome'] = 'awaiting_input'
                        if failure == 'identity':
                            receipt['login']['user'] = 'foreign-user'
                        if failure == 'required_inputs':
                            receipt['login']['required_inputs'] = ['secret_key']
                        if failure == 'status':
                            envelope['status'] = 'pending'
                        if failure == 'errors':
                            envelope['errors'] = [PASSWORD + SUPER + factor]
                        reply = dict(v=2 if failure == 'wire_version' else 1,
                            id='foreign' if failure == 'transport' else request['id'],
                            exit=1 if failure == 'wire_exit' else exit_code,
                            ok=False if failure == 'wire_ok' else exit_code == 0, envelope=envelope)
                        return SimpleNamespace(returncode=exit_code,
                            stdout=b'{' if failure == 'json' else json.dumps(reply).encode(), stderr=b'')

                    scope = dict(self=smoke, version=3, user=USER, password=PASSWORD,
                                 original_super=SUPER, secret=factor)
                    with patch.object(legacy, 'install_published_runtime'), \
                            patch.object(legacy.subprocess, 'run', side_effect=child), \
                            patch.object(legacy, 'run_hidden', side_effect=AssertionError('v3_must_use_stdin_rpc')):
                        if failure:
                            with self.assertRaisesRegex(RuntimeError, '^' + failures[failure] + '$') as raised:
                                exec(code, scope)
                            for forbidden in (PASSWORD, SUPER, factor):
                                self.assertNotIn(forbidden, str(raised.exception) + json.dumps(smoke.report))
                        else:
                            exec(code, scope)
                    self.assertEqual(len(requests), 1)
                    self.assertEqual(len(stops), 1)
                    self.assertEqual(keys.entries, {('rsrs', prefix + ALIAS) for prefix in
                        ('pass:', 'super:', 'legacy-secret:')} if committed else set())
                    self.assertEqual('RSRS_SUPER' in scope[variable], variable == 'final_env')
                    self.assertTrue(keys.cleanup()['passed'])
                    self.assertFalse(keys.values)

    def test_committed_session_tracked_even_when_post_auth_checks_fail(self):
        for reason in ('fixture_tty_missing_prompt', 'invalid_cli_json_login', 'owned_runtime_stop_failed'):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as td:
                keys = MemoryKeys()
                smoke = object.__new__(legacy.Smoke)
                smoke.keys = keys
                root = Path(td)
                env = {'RSRS_DATA_DIR': str(root)}

                def cli(*args, **kwargs):
                    keys.created(ALIAS)
                    target = root / 'accounts' / 'fixture-account'
                    target.mkdir(parents=True)
                    (root / 'client.json').write_text(json.dumps({'data_dir': str(target)}), encoding='utf-8')
                    smoke.write_session(dict(env, RSRS_DATA_DIR=str(target)),
                        dict(user=USER, keyring_account=ALIAS, token='synthetic-token'))
                    raise RuntimeError(reason)

                smoke.cli = cli
                with self.assertRaisesRegex(RuntimeError, '^' + reason + '$'):
                    smoke.password_login(env, USER, PASSWORD, 2, SUPER)
                self.assertFalse((root / 'session.json').exists())
                self.assertEqual(keys.entries, {('rsrs', 'super:' + ALIAS), ('rsrs', 'pass:' + ALIAS)})
                report = keys.cleanup()
                self.assertTrue(report['passed'])
                self.assertFalse(keys.values)
                for secret in (PASSWORD, SUPER, ALIAS, USER):
                    self.assertNotIn(secret, json.dumps(report))

    def test_failed_login_does_not_claim_unchanged_alias_or_partial_session(self):
        for original, after in (({'user': USER, 'keyring_account': ALIAS, 'token': 'old'},
                                  {'user': USER, 'keyring_account': ALIAS, 'token': 'old'}),
                                 ({'user': USER}, {'user': USER, 'keyring_account': ALIAS})):
            keys = MemoryKeys()
            keys.created(ALIAS)
            smoke = object.__new__(legacy.Smoke)
            smoke.keys = keys
            current = dict(original)
            def cli(*args, **kwargs):
                current.clear(); current.update(after)
                raise RuntimeError('auth_failed')
            smoke.cli = cli
            smoke.session = lambda env: dict(current)
            with self.assertRaisesRegex(RuntimeError, '^auth_failed$'):
                smoke.password_login({}, USER, PASSWORD, 2, SUPER)
            self.assertFalse(keys.entries)
            self.assertFalse(keys.reads)
            self.assertTrue(keys.values)

    def test_success_without_new_session_is_rejected(self):
        smoke = object.__new__(legacy.Smoke)
        smoke.keys = MemoryKeys()
        smoke.session = lambda env: {'user': USER, 'keyring_account': ALIAS, 'token': 'old'}
        smoke.cli = lambda *args, **kwargs: {'summary': {}}
        with self.assertRaisesRegex(RuntimeError, '^login_credential_fixture_new_session_missing$'):
            smoke.password_login({}, USER, PASSWORD, 2, SUPER)
        self.assertFalse(smoke.keys.entries)

    def test_v1_migration_tracks_before_replacement_super_assertion_fails(self):
        # Execute the actual fixture block, including its assertion, rather
        # than a duplicate implementation of the successful-auth ordering.
        tree = ast.parse((HERE / 'legacy_vault_scenarios.py').read_text())
        run = next(node for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name == 'run_version')
        start = next(i for i, node in enumerate(run.body)
            if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Attribute) and node.value.func.attr == 'cli'
            and any(isinstance(arg, ast.Constant) and arg.value == '--vault' for arg in node.value.args))
        end = next(i for i, node in enumerate(run.body) if isinstance(node, ast.Expr)
            and isinstance(node.value, ast.Call)
            and any(isinstance(arg, ast.Constant) and arg.value == 'migration_generated_replacement_super'
                for arg in node.value.args))
        code = compile(ast.Module(body=run.body[start:end + 1], type_ignores=[]),
            '<actual-v1-migration-block>', 'exec')
        keys = MemoryKeys()
        smoke = object.__new__(legacy.Smoke)
        smoke.keys = keys
        committed = dict(user=USER, keyring_account=OTHER, token='synthetic-token-never-report')
        def cli(env, *args, **kwargs):
            keys.created(OTHER, None)
            return {'summary': {'super_issued': 'unexpected-synthetic-super-never-report'}}
        smoke.cli = cli
        smoke.session = lambda env: committed
        account = {}
        with self.assertRaisesRegex(RuntimeError, '^migration_generated_replacement_super$') as raised:
            exec(code, {'self': smoke, 'upgraded_env': {}, 'UPSTREAM': legacy.UPSTREAM,
                'user': USER, 'password': PASSWORD, 'version': 1, 'account': account,
                'require': legacy.require})
        self.assertEqual(keys.entries, {('rsrs', 'pass:' + OTHER)})
        self.assertEqual(account['token'], committed['token'])
        report = keys.cleanup()
        self.assertTrue(report['passed'])
        self.assertFalse(keys.values)
        for forbidden in (PASSWORD, OTHER, USER, committed['token'],
                'unexpected-synthetic-super-never-report'):
            self.assertNotIn(forbidden, str(raised.exception) + json.dumps(report))

    def test_failed_auth_does_not_claim_alias(self):
        smoke = object.__new__(legacy.Smoke)
        smoke.keys = MemoryKeys()
        smoke.cli = lambda *a, **kw: (_ for _ in ()).throw(RuntimeError('auth_failed'))
        smoke.session = lambda env: {'user': USER}
        with self.assertRaisesRegex(RuntimeError, 'auth_failed'):
            smoke.password_login({}, USER, PASSWORD, 1, SUPER)
        self.assertFalse(smoke.keys.entries)



from credential_snapshot_tests import SnapshotTests


from credential_upgrade_library_tests import UpgradeLibraryStateTests


from credential_original_factor_tests import OriginalFactorTests


if __name__ == '__main__':
    unittest.main()
