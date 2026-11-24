"""Runtime identity and retained credential checks."""
import ast
from contextlib import closing
from contextlib import nullcontext
import json
import sqlite3
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from unittest.mock import patch
from credential_alias_test_support import HERE
from credential_alias_test_support import native
from credential_alias_test_support import profile
from credential_alias_test_support import legacy
from credential_alias_test_support import USER
from credential_alias_test_support import ALIAS
from credential_alias_test_support import OTHER


class KeylessRuntimeIdentityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.profile = Path(temporary.name)
        self.runtime = self.profile / 'rsrs-runtime.exe'
        self.runtime.touch()
        self.other = self.profile / 'other'
        self.other.mkdir()
        (self.other / self.runtime.name).touch()
        self.metadata = dict(runtime_version='2.0.0-dev.22', binary_sha256='a' * 64)
        self.health = dict(v=1, pid=123, runtime_version=self.metadata['runtime_version'],
            image_sha256=self.metadata['binary_sha256'], image_path=str(self.runtime),
            installation_root=str(self.profile), data_dir=str(self.profile))
        tree = ast.parse((HERE / 'profile_migration_scenarios.py').read_text(encoding='utf-8'))
        assertion = next(node for node in ast.walk(tree) if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name) and node.func.id == 'require'
            and len(node.args) == 2 and isinstance(node.args[1], ast.Constant)
            and node.args[1].value == 'keyless_runtime_identity_mismatch')
        self.assertion = compile(ast.Expression(assertion), '<keyless-runtime-identity>', 'eval')

    def verify(self):
        eval(self.assertion, dict(require=profile.require, health=self.health,
            child=SimpleNamespace(pid=123), metadata=self.metadata, runtime=self.runtime,
            profile=self.profile.resolve(), keyring_backend=native, sys=sys, Path=Path))

    def test_exact_identity_passes(self):
        self.verify()
        smoke = object.__new__(profile.Smoke)
        smoke.args = SimpleNamespace(binary=self.runtime)
        installation = self.profile / '.rsrs'
        destination = installation / 'accounts' / 'first'
        sibling = installation / 'accounts' / 'second'
        destination.mkdir(parents=True)
        sibling.mkdir()
        session = dict(user=USER, keyring_account=ALIAS, token='fixture-session')
        (destination / 'session.json').write_text(json.dumps(session), encoding='utf-8')
        (sibling / 'session.json').write_text(json.dumps(dict(session, user='other')), encoding='utf-8')
        config = destination / 'client.json'
        original = (json.dumps({'data_dir': str(sibling), 'addr': legacy.UPSTREAM}) + '\n').encode()
        default = {'HOME': str(self.profile)}
        for contents in (original, None):
            for fails in (False, True):
                with self.subTest(config_present=contents is not None, fails=fails):
                    if contents is not None:
                        config.write_bytes(contents)
                    with patch.object(legacy, 'install_published_runtime'):
                        with self.assertRaisesRegex(RuntimeError, '^fixture-check-failed$') if fails else nullcontext():
                            with smoke.selected_profile(default, {'destination': destination}) as env:
                                self.assertEqual(smoke.session(env), session)
                                self.assertTrue(smoke.profile(env).samefile(destination))
                                if fails:
                                    raise RuntimeError('fixture-check-failed')
                    self.assertEqual(config.read_bytes() if config.exists() else None, contents)
                    config.unlink(missing_ok=True)
        outside = json.dumps({'data_dir': str(self.other)}).encode()
        config.write_bytes(outside)
        with patch.object(legacy, 'install_published_runtime') as install:
            with self.assertRaisesRegex(RuntimeError, '^login_profile_selection_outside_fixture$'):
                with smoke.selected_profile(default, {'destination': destination}):
                    self.fail('external selection accepted')
            install.assert_not_called()
        self.assertEqual(config.read_bytes(), outside)

    @unittest.skipUnless(sys.platform == 'win32', 'Windows final-path identity')
    def test_windows_extended_path_and_case_use_full_file_identity(self):
        base_smoke = object.__new__(legacy.Smoke)
        for path in ('\\\\?\\' + str(self.runtime),
                     ('\\\\?\\' + str(self.runtime)).upper().replace('\\', '/')):
            with self.subTest(path_form=path[:7]):
                self.health['image_path'] = path
                self.verify()

        smoke = object.__new__(profile.Smoke)
        root = self.profile / 'owned'
        child = root / 'accounts' / 'fixture-account'
        child.mkdir(parents=True)
        outside = self.profile / 'owned-other'
        outside.mkdir()
        env = {'RSRS_DATA_DIR': str(root)}
        config = root / 'client.json'
        session = dict(user=USER, token='synthetic-session')
        for target in (root, child):
            (target / 'session.json').write_text(json.dumps(session), encoding='utf-8')
            database = target / 'read only # %.db'
            with closing(sqlite3.connect(database)) as db:
                db.execute('CREATE TABLE marker(value INTEGER)')
                db.commit()
            for selected in (str(target), '\\\\?\\' + str(target),
                             ('\\\\?\\' + str(target)).upper().replace('\\', '/')):
                with self.subTest(selected=selected):
                    config.write_text(json.dumps({'data_dir': selected}), encoding='utf-8')
                    self.assertTrue(smoke.profile(env).samefile(target))
                    self.assertEqual(smoke.session(env), session)
                    self.assertTrue(base_smoke.profile(env).samefile(target))
                    self.assertEqual(base_smoke.session(env), session)
                    uri = legacy.readonly_database_uri(base_smoke.profile(env) / database.name)
                    with closing(sqlite3.connect(uri, uri=True)) as db:
                        self.assertEqual(db.execute("SELECT name FROM sqlite_master WHERE name='marker'").fetchone(), ('marker',))
                        with self.assertRaisesRegex(sqlite3.OperationalError, 'readonly'):
                            db.execute('CREATE TABLE forbidden(value INTEGER)')
        for target in (self.profile, outside):
            for selected in (str(target), '\\\\?\\' + str(target)):
                with self.subTest(outside=selected):
                    config.write_text(json.dumps({'data_dir': selected}), encoding='utf-8')
                    with self.assertRaisesRegex(RuntimeError, '^login_profile_selection_outside_fixture$'):
                        smoke.session(env)
                    with self.assertRaisesRegex(RuntimeError, '^login_profile_selection_outside_fixture$'):
                        base_smoke.session(env)
        for selected in ('', None):
            config.write_text(json.dumps({'data_dir': selected}), encoding='utf-8')
            with self.assertRaisesRegex(RuntimeError, '^login_profile_selection_invalid$'):
                smoke.session(env)
        config.write_text('{', encoding='utf-8')
        with self.assertRaises(json.JSONDecodeError):
            smoke.session(env)

    def test_every_identity_field_remains_strict(self):
        for field, value in dict(v=2, pid=456, runtime_version='2.0.0-dev.21',
                image_sha256='b' * 64, image_path=str(self.other / self.runtime.name),
                installation_root=str(self.other), data_dir=str(self.other)).items():
            with self.subTest(field=field):
                original = self.health[field]
                self.health[field] = value
                with self.assertRaisesRegex(RuntimeError, '^keyless_runtime_identity_mismatch$'):
                    self.verify()
                self.health[field] = original


class RestartRetainedCredentialTests(unittest.TestCase):
    def test_both_restart_checks_use_platform_consumer_and_preserve_alias(self):
        tree = ast.parse((HERE / 'profile_migration_scenarios.py').read_text(encoding='utf-8'))
        restart = next(node for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name == 'restart_key_handoff')
        checks = [node for node in restart.body if isinstance(node, ast.If)
            and ast.unparse(node.test) == "sys.platform == 'darwin'"]
        self.assertEqual(len(checks), 2)
        env, fixture = {'RSRS_DATA_DIR': 'owned-profile'}, {'alias': ALIAS}
        for index, check in enumerate(checks):
            for platform in ('darwin', 'win32', 'linux'):
                for alias in (ALIAS, OTHER):
                    with self.subTest(check=index, platform=platform, original_alias=alias == ALIAS):
                        session = {'keyring_account': alias}
                        smoke = SimpleNamespace(session=lambda env: session,
                            verify_mac_native_credentials=Mock(), verify_retained_current_credentials=Mock())
                        code = compile(ast.Module(body=[check], type_ignores=[]), '<restart-retained>', 'exec')
                        namespace = dict(self=smoke, env=env, fixture=fixture,
                            require=profile.require, sys=SimpleNamespace(platform=platform))
                        if platform == 'darwin' and alias != ALIAS:
                            with self.assertRaisesRegex(RuntimeError, '^existing_target_credential_identity_changed$'):
                                exec(code, namespace)
                            smoke.verify_mac_native_credentials.assert_not_called()
                            smoke.verify_retained_current_credentials.assert_not_called()
                        else:
                            exec(code, namespace)
                            if platform == 'darwin':
                                smoke.verify_mac_native_credentials.assert_called_once_with(env, fixture)
                                smoke.verify_retained_current_credentials.assert_not_called()
                            else:
                                smoke.verify_retained_current_credentials.assert_called_once_with(session, fixture)
                                smoke.verify_mac_native_credentials.assert_not_called()
