"""Original Vault factor and credential custody regressions."""
import ast
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from unittest.mock import patch
from credential_alias_test_support import HERE
from credential_alias_test_support import load
from credential_alias_test_support import profile
from credential_alias_test_support import legacy
from credential_alias_test_support import USER
from credential_alias_test_support import ALIAS
from credential_alias_test_support import PASSWORD
from credential_alias_test_support import SUPER


class OriginalFactorTests(unittest.TestCase):
    def test_original_v1_factors_unwrap_unchanged_urk_and_reject_wrong_factors(self):
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        from cryptography.exceptions import InvalidTag
        salt, nonce, urk, secret = 'ab' * 16, bytes.fromhex('cd' * 12), bytes(range(32)), 'original-account-secret'
        old = AESGCM(legacy.legacy_kek(1, PASSWORD, secret, salt)).encrypt(nonce, urk, None)
        recovered = AESGCM(legacy.legacy_kek(1, PASSWORD, secret, salt)).decrypt(nonce, old, None)
        new = AESGCM(legacy.legacy_kek(1, PASSWORD, secret, salt, current=True)).encrypt(nonce, recovered, None)
        self.assertEqual(AESGCM(legacy.legacy_kek(1, PASSWORD, secret, salt, current=True)).decrypt(nonce, new, None), urk)
        for password, factor in (('wrong', secret), (PASSWORD, 'wrong')):
            with self.assertRaises(InvalidTag):
                AESGCM(legacy.legacy_kek(1, password, factor, salt, current=True)).decrypt(nonce, new, None)

    def test_host_commit_assertion_rejects_plaintext_volatile_backend_wrong_environment(self):
        tree = ast.parse((HERE / 'legacy_vault_scenarios.py').read_text())
        assertion = next(node for node in ast.walk(tree) if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name) and node.func.id == 'require'
            and len(node.args) == 2 and isinstance(node.args[1], ast.Constant)
            and node.args[1].value == 'headless_v1_persistent_password_commit_invalid')
        code = compile(ast.Expression(assertion), '<fixture-assertion>', 'eval')
        for backend in ('native', 'secret_service'):
            eval(code, {'require': legacy.require, 'headless': {'keyring_backend': backend},
                'headless_env': {'RSRS_PASS': PASSWORD}, 'password': PASSWORD})
        for session, env in (({}, {'RSRS_PASS': PASSWORD}),
                ({'keyring_backend': 'kernel'}, {'RSRS_PASS': PASSWORD}),
                ({'keyring_backend': 'native', 'pass': PASSWORD}, {'RSRS_PASS': PASSWORD}),
                ({'keyring_backend': 'native'}, {'RSRS_PASS': 'wrong'})):
            with self.assertRaisesRegex(RuntimeError, '^headless_v1_persistent_password_commit_invalid$'):
                eval(code, {'require': legacy.require, 'headless': session,
                    'headless_env': env, 'password': PASSWORD})
        historical = next(node for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name == 'historical_database_sync')
        start = next(index for index, node in enumerate(historical.body)
            if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
            and any(isinstance(arg, ast.Constant) and arg.value == 'historical_registration_terminal_identity_mismatch'
                    for arg in node.value.args))
        end = next(index for index, node in enumerate(historical.body[start:], start)
            if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
            and any(isinstance(arg, ast.Constant) and arg.value == 'config' for arg in node.value.args))
        checks = compile(ast.Module(body=historical.body[start:end], type_ignores=[]), '<registration-contract>', 'exec')
        source = HERE.resolve()
        receipt = dict(outcome='committed', source_profile=str(source), target_profile=str(source))
        vault = dict(wrapped_urk=SUPER, kdf_salt='synthetic-salt', urk_nonce='synthetic-nonce',
            keyring_account=ALIAS, keyring_backend='native')
        before = (['original-encrypted-row'], ['original-pending-work'])
        scope = dict(require=legacy.require, same_scope_path=legacy.same_scope_path,
            env={}, source=source, user=USER, UPSTREAM=legacy.UPSTREAM,
            previous_operations=set(), registered=dict(ok=True, user=USER, addr=legacy.UPSTREAM),
            self=SimpleNamespace(registration_receipt=Mock(return_value=receipt)),
            account=dict(confirmed=True), session=vault, candidate=vault, rows=lambda: before, before=before)
        exec(checks, dict(scope))  # Terminal summary has no transaction receipt fields.
        canonical = legacy.canonical_scope_path(source)
        exec(checks, dict(scope, self=SimpleNamespace(registration_receipt=Mock(
            return_value=dict(receipt, source_profile=canonical, target_profile=canonical)))))
        recovery = next(node for node in ast.walk(historical) if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name) and node.func.id == 'require'
            and len(node.args) == 2 and isinstance(node.args[1], ast.Constant)
            and node.args[1].value == 'historical_recovery_not_committed_in_place')
        recovery_check = compile(ast.Expression(recovery), '<recovery-scope-contract>', 'eval')
        recovered_scope = dict(scope, recovered=dict(outcome='committed', source_profile=canonical, operation_id='owned'),
            operation='owned', candidate=dict(user='', addr=''))
        eval(recovery_check, recovered_scope)
        for wrong in (str(source.parent), None, 'source', str(source / 'missing')):
            for field in ('source_profile', 'target_profile'):
                with self.assertRaisesRegex(RuntimeError, '^historical_registration_' + field.split('_')[0] + '_changed$'):
                    exec(checks, dict(scope, self=SimpleNamespace(registration_receipt=Mock(
                        return_value=dict(receipt, **{field: wrong})))))
            with self.assertRaisesRegex(RuntimeError, '^historical_recovery_not_committed_in_place$'):
                eval(recovery_check, dict(recovered_scope, recovered=dict(recovered_scope['recovered'], source_profile=wrong)))
        for changed, error in ((dict(registered={}), 'terminal_identity_mismatch'),
                (dict(self=SimpleNamespace(registration_receipt=Mock(return_value=dict(receipt, outcome='unknown')))),
                    'receipt_not_committed'),
                (dict(self=SimpleNamespace(registration_receipt=Mock(return_value=dict(receipt, source_profile='other')))),
                    'source_changed'),
                (dict(self=SimpleNamespace(registration_receipt=Mock(return_value=dict(receipt, target_profile='other')))),
                    'target_changed'),
                (dict(account=dict(confirmed=False)), 'account_unconfirmed'),
                (dict(session=dict(vault, wrapped_urk='foreign')), 'wrap_changed'),
                (dict(rows=lambda: ([], before[1])), 'rows_changed'),
                (dict(rows=lambda: (before[0], [])), 'outbox_changed')):
            with self.assertRaisesRegex(RuntimeError, '^historical_registration_' + error + '$') as failure:
                exec(checks, dict(scope, **changed))
            for secret in (SUPER, ALIAS, USER):
                self.assertNotIn(secret, str(failure.exception))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            folder = root / 'runtime/autostart/account-results'
            folder.mkdir(parents=True)
            operation = '55555555-5555-4555-8555-555555555555'
            binding = dict(schema=1, operation_id=operation, root=str(root), source=str(root),
                user=USER, addr=legacy.UPSTREAM, register=True, sent=True)
            path = folder / (operation + '.json')
            path.write_text(json.dumps(binding), encoding='utf-8')
            smoke = object.__new__(legacy.Smoke)
            smoke.cli = Mock(return_value={'summary': dict(receipt, operation_id=operation)})
            env = dict(RSRS_DATA_DIR=str(root))
            self.assertEqual(smoke.registration_receipt(env, root, USER, set())['operation_id'], operation)
            smoke.cli.assert_called_once_with(env, 'runtime', 'account-result', '--operation-id', operation,
                '--source-profile', str(root), '--register')
            smoke.cli.reset_mock()
            canonical = legacy.canonical_scope_path(root)
            if sys.platform == 'win32':
                self.assertTrue(canonical.startswith('\\\\?\\'))
                self.assertNotEqual(str(root), canonical)
            path.write_text(json.dumps(dict(binding, root=canonical, source=canonical)), encoding='utf-8')
            smoke.registration_receipt(env, root, USER, set())
            smoke.cli.assert_called_once_with(env, 'runtime', 'account-result', '--operation-id', operation,
                '--source-profile', str(root), '--register')
            smoke.cli.reset_mock()
            other = root / 'other'
            other.mkdir()
            for field in ('root', 'source'):
                path.write_text(json.dumps(dict(binding, **{field: legacy.canonical_scope_path(other)})), encoding='utf-8')
                with self.assertRaisesRegex(RuntimeError, 'operation_binding_mismatch'):
                    smoke.registration_receipt(env, root, USER, set())
            smoke.cli.assert_not_called()
            smoke.args = SimpleNamespace(binary=Path('synthetic-cli'))
            smoke.root = root
            expected = dict(password='', super_password=SUPER, account_secret='', totp=None,
                material=dict(keyring_account=ALIAS, keyring_backend='native', reset=None, challenge=None))
            verified = dict(verified=True, operation_id=operation, installation_root=canonical,
                            source_profile=canonical)
            output = SimpleNamespace(returncode=0, stdout=json.dumps(dict(status='ok', summary=verified)), stderr=b'')
            with patch.object(legacy.subprocess, 'run', return_value=output) as run:
                smoke.verify_recovery_control(env, root, operation, expected)
                self.assertEqual(json.loads(run.call_args.kwargs['input']), expected)
                self.assertNotIn(SUPER, str(run.call_args.args) + str(run.call_args.kwargs['env']))
                for field, foreign in (('verified', False), ('operation_id', 'foreign'),
                                       ('installation_root', str(other)), ('source_profile', str(other))):
                    output.stdout = json.dumps(dict(status='ok', summary=dict(verified, **{field: foreign})))
                    with self.assertRaisesRegex(RuntimeError, 'native_receipt_mismatch'):
                        smoke.verify_recovery_control(env, root, operation, expected)
            path.write_text(json.dumps(dict(binding, user='foreign')), encoding='utf-8')
            with self.assertRaisesRegex(RuntimeError, 'operation_binding_mismatch'):
                smoke.registration_receipt(env, root, USER, set())
            with self.assertRaisesRegex(RuntimeError, 'operation_not_unique'):
                smoke.registration_receipt(env, root, USER, {path.name})
            smoke.cli.assert_not_called()

    def test_native_only_proofs_and_independent_tracking_remain_in_suite(self):
        legacy_text = '\n'.join((HERE / name).read_text(encoding='utf-8') for name in
            ('dev-legacy-vault-smoke.py', 'legacy_vault_scenarios.py'))
        profile_text = '\n'.join((HERE / name).read_text(encoding='utf-8') for name in
            ('dev-profile-migration-smoke.py', 'profile_migration_scenarios.py'))
        for code in ('headless_v1_changed_original_factors', 'headless_v1_changed_urk',
                     'new_device_ciphertext_differs', 'migration_changed_urk', 'original_source_changed',
                     'full_migration_not_reencrypted', 'old_authentication_hash_still_accepted'):
            self.assertIn(code, legacy_text)
        self.assertIn('authorization_fallback_and_absent_native_store_not_tested', legacy_text)
        database = next(node for node in ast.walk(ast.parse(legacy_text))
            if isinstance(node, ast.FunctionDef) and node.name == 'database_sync')
        tombstone = next(node for node in ast.walk(database) if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name) and node.func.id == 'require'
            and len(node.args) == 2 and isinstance(node.args[1], ast.Constant)
            and node.args[1].value == 'new_device_tombstone_not_propagated')
        assertion = compile(ast.Expression(tombstone), '<remote-tombstone-contract>', 'eval')
        head = {'id': 'deleted-fixture', 'deleted': True, 'ciphertext': '', 'nonce': ''}
        scope = dict(require=legacy.require, deleted='deleted-fixture',
            observed={'deleted-fixture': ('deleted-fixture', '', '', 1)}, heads={'deleted-fixture': head})
        eval(assertion, scope)
        for wrong in ({}, dict(head, deleted=False), dict(head, id='unrelated'),
                      dict(head, ciphertext='stale'), dict(head, nonce='stale')):
            with self.subTest(tombstone=wrong):
                with self.assertRaisesRegex(RuntimeError, '^new_device_tombstone_not_propagated$'):
                    eval(assertion, dict(scope, heads={'deleted-fixture': wrong}))
        with self.assertRaisesRegex(RuntimeError, '^new_device_tombstone_not_propagated$'):
            eval(assertion, dict(scope, observed={}))
        tree = ast.parse(profile_text)
        case = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef) and node.name == 'case')
        body = ast.unparse(case)
        self.assertLess(body.index('self.keys.track_created_login(independent,'), body.index("self.cli(remote, 'sync')"))
        setup = next(node for node in ast.walk(case) if isinstance(node, ast.Assign)
                     and isinstance(node.value, ast.Constant) and node.value.value == 'independent_current_login')
        login = next(node for node in ast.walk(case) if isinstance(node, ast.Expr)
                     and isinstance(node.value, ast.Call) and ast.unparse(node.value.func) == 'self.cli'
                     and node.value.args and ast.unparse(node.value.args[0]) == 'remote')
        statements = sorted((node for node in ast.walk(case) if isinstance(node, ast.stmt)
                             and setup.lineno <= node.lineno < login.lineno), key=lambda node: node.lineno)
        with tempfile.TemporaryDirectory() as temporary:
            smoke = object.__new__(profile.Smoke)
            smoke.root, smoke.model = Path(temporary), Path(temporary) / 'models'
            smoke.args, smoke.base_env, smoke.keys = SimpleNamespace(binary=Path(temporary) / 'rsrs'), {}, None
            smoke.report, smoke.save = {}, Mock()
            fixture = {'code': SUPER}
            scope = dict(self=smoke, user=USER, fixture=fixture, Path=Path, require=profile.require)
            with patch.object(profile.support, 'install_published_runtime') as install:
                exec(compile(ast.Module(body=statements, type_ignores=[]), '<actual-independent-profile>', 'exec'), scope)
            remote = scope['remote']
            session = Path(remote['RSRS_DATA_DIR']) / 'session.json'
            self.assertFalse(session.exists())
            self.assertFalse((session.parent / 'rsrs.db').exists())
            self.assertTrue(session.parent.is_relative_to(smoke.root))
            self.assertEqual(remote['RSRS_SUPER'], SUPER)
            self.assertEqual(smoke.report['fixture_operation'], 'independent_current_login')
            install.assert_called_once()
            self.assertEqual(fixture, {'code': SUPER})
            # A partial named session is an existing recovery target, not a fresh login.
            session.write_text(json.dumps({'user': USER}), encoding='utf-8')
            guard = next(node.value for node in statements if isinstance(node, ast.Expr)
                         and isinstance(node.value, ast.Call) and ast.unparse(node.value.func) == 'require')
            with self.assertRaisesRegex(RuntimeError, '^independent_profile_not_empty$'):
                eval(compile(ast.Expression(guard), '<independent-profile-guard>', 'eval'), scope)
        for code in ('existing_target_data_overwritten', 'existing_target_session_overwritten'):
            self.assertIn(code, body)
        self.assertIn('self.read_entry(current_env,', body)
        seed = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef) and node.name == 'seed')
        seed_text = ast.unparse(seed)
        for removed in ('seed_legacy_native_credentials', 'remove_current_native_credentials',
                        'rewrite_legacy_endpoint', 'https://api.1memory.ai', 'https://api.memocap.ai',
                        'https://api.respire.ai', '/api/self/vault', 'self.keys.put(', 'self.keys.remove('):
            self.assertNotIn(removed, seed_text)
        self.assertIn('source_alias', seed_text)
        self.assertIn("dict(default, RSRS_SUPER=fixture['code'])", body)
        self.assertLess(body.index("self.cli(user_env, 'login', '--interactive'"),
                        body.index('self.read_entry(user_env,'))
        shared = load('db_only_shared_gate', 'dev-smoke-gate.py')
        native_gate = load('db_only_secret_service_gate', 'dev-secret-service-migration-gate.py')
        self.assertEqual(set(profile.REQUIRED), shared.MIGRATION)
        self.assertEqual(native_gate.REQUIRED, set(profile.REQUIRED) |
                         {'secret_service_current_credentials_for_legacy_db'})
        workflow = (HERE.parent / '.github/workflows/migration-keyring-check.yml').read_text()
        self.assertIn("module.REQUIRED-{'secret_service_current_credentials_for_legacy_db'}", workflow)
        self.assertNotIn('secret_service_legacy_fields_and_aliases', workflow)
        self.assertTrue({'legacy_keys_migrated_without_super_override', 'legacy_api_defaults_rewritten'}
                        .isdisjoint(shared.MIGRATION))
        for retained in ('migration_wrong_key_rejected', 'migration_incompatible_primary_keys_rejected',
                         'migration_interrupted_restart_recovered', 'migration_repeated_start_idempotent',
                         'migrated_outbox_sync_and_independent_decrypt'):
            self.assertIn(retained, shared.MIGRATION)
        negative = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)
                        and node.name == 'negative_cases')
        rejection = next(node for node in ast.walk(negative) if isinstance(node, ast.Call)
                         and isinstance(node.func, ast.Name) and node.func.id == 'require'
                         and len(node.args) == 2 and isinstance(node.args[1], ast.Constant)
                         and node.args[1].value == 'wrong_key_not_rejected_by_runtime')
        check = compile(ast.Expression(rejection), '<wrong-key-rejection-guard>', 'eval')
        accepted_rejection = dict(status='fail', errors=[
            'migration_stage=exchange; migration_exchange=runtime_migration_outcome_unknown'])
        eval(check, dict(require=profile.require, result=SimpleNamespace(returncode=1),
                         rejected=accepted_rejection))
        for exit_code, envelope in ((0, accepted_rejection), (1, dict(status='ok')),
                (1, dict(status='fail', errors=['migration_stage=host_candidates'])),
                *((1, dict(status='fail', errors=['migration_stage=exchange; migration_exchange=' + reason]))
                  for reason in ('channel', 'deadline', 'spawn', 'frame', 'unclassified',
                                 'runtime_scope_mismatch', 'runtime_unauthorized')),
                (1, dict(status='fail', errors=['migration_stage=exchange'])),
                (1, dict(status='fail', errors=['unrelated CLI failure']))):
            with self.subTest(exit_code=exit_code, envelope=envelope), \
                    self.assertRaisesRegex(RuntimeError, '^wrong_key_not_rejected_by_runtime$'):
                eval(check, dict(require=profile.require, result=SimpleNamespace(returncode=exit_code),
                                 rejected=envelope))
