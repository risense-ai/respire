"""Legacy alias and native credential custody cases."""
from contextlib import closing
import json
from pathlib import Path
import tempfile
from unittest.mock import patch
from credential_alias_test_support import profile
from credential_alias_test_support import legacy
from credential_alias_test_support import USER
from credential_alias_test_support import ALIAS
from credential_alias_test_support import PASSWORD
from credential_alias_test_support import SUPER
from credential_alias_test_support import MemoryKeys


class AliasCustodyCases:
    def test_legacy_slot_names_are_bounded_to_owned_aliases_and_current_service(self):
        keys = MemoryKeys()
        for alias in (ALIAS, 'legacy-' + 'a' * 16, 'legacy-' + 'b' * 64):
            keys.reserve('rsrs', 'legacy-secret:' + alias)
        for prefix in ('super:', 'pass:'):
            keys.reserve('rsrs', prefix + 'legacy-' + 'b' * 64)
            with self.assertRaisesRegex(RuntimeError, '^credential_name_not_a_migration_fixture$'):
                keys.reserve('respire', prefix + 'legacy-' + 'b' * 64)
        for service, alias in (('1memory', ALIAS), ('rsrs', USER),
                               ('rsrs', 'legacy-' + 'a' * 17), ('rsrs', 'login-foreign'),
                               ('rsrs', 'legacy-' + 'a' * 63), ('rsrs', 'legacy-' + 'a' * 65),
                               ('rsrs', 'legacy-' + 'A' * 64)):
            with self.assertRaisesRegex(RuntimeError, '^credential_name_not_a_migration_fixture$'):
                keys.reserve(service, 'legacy-secret:' + alias)
        foreign_slot = 'legacy-secret:legacy-' + 'c' * 64
        keys.values['rsrs', foreign_slot] = 'foreign'
        with self.assertRaisesRegex(RuntimeError, '^fixture_credential_already_exists$'):
            keys.reserve('rsrs', foreign_slot)
        with self.assertRaisesRegex(RuntimeError, '^credential_not_owned_by_fixture$'):
            keys.remove('rsrs', foreign_slot)
        self.assertTrue(keys.cleanup()['passed'])
        self.assertEqual(keys.values, {('rsrs', foreign_slot): 'foreign'})

    def test_legacy_custody_and_failed_primary_readback_still_track_cleanup(self):
        for version in (1, 3):
            for backend, inline in (('native', False), ('native', True), ('kernel', False)):
                keys = MemoryKeys()
                keys.created(ALIAS, None if version == 1 else SUPER)
                keys.values['rsrs', 'legacy-secret:' + ALIAS] = 'synthetic-factor'
                session = dict(user=USER, keyring_account=ALIAS, vault_version=version,
                               legacy_secret_backend=backend)
                if inline:
                    session['secret' if version == 1 else 'secret_key'] = 'synthetic-factor'
                with self.assertRaises(RuntimeError):
                    keys.track_created_login(session, USER, None if version == 1 else SUPER, 'wrong')
                report = keys.cleanup()
                self.assertTrue(report['passed'])
                # A different backend is never claimed as this manager's slot.
                self.assertEqual(bool(keys.values), backend != 'native')
                self.assertNotIn('synthetic-factor', json.dumps(report))
        for backend in ('native', 'secret_service'):
            keys = MemoryKeys()
            keys.credential_backend = backend
            keys.values['rsrs', 'pass:' + ALIAS] = ''
            keys.values['rsrs', 'legacy-secret:' + ALIAS] = 'synthetic-factor'
            session = dict(user=USER, keyring_account=ALIAS, vault_version=1,
                           legacy_secret_backend=backend)
            keys.track_created_login(session, USER, None, '')
            self.assertEqual(keys.read('rsrs', 'pass:' + ALIAS), '')
            self.assertTrue(keys.cleanup()['passed'])
            self.assertFalse(keys.values)

    def test_mac_current_probe_preserves_session_and_keeps_failures(self):
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        import sqlite3
        for failure in (None, 'show', 'session'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                code, salt, nonce, urk = 'A3-' + 'ab' * 18, 'cd' * 16, bytes(12), bytes(range(32))
                wrapped = legacy.PREFIX + AESGCM(legacy.v4_kek(code, salt, legacy.PREFIX)).encrypt(nonce, urk, None).hex()
                session = dict(user=USER, vault_version=4, keyring_account=ALIAS,
                               keyring_backend='native', kdf_salt=salt, wrapped_urk=wrapped, urk_nonce=nonce.hex())
                session_path = root / 'session.json'
                original = json.dumps(session).encode()
                session_path.write_bytes(original)
                ciphertext = legacy.PREFIX + AESGCM(legacy.hkdf(urk, None, b'rsrs:data:v1')).encrypt(nonce, b'payload', None).hex()
                with closing(sqlite3.connect(root / 'rsrs.db')) as db, db:
                    db.execute('CREATE TABLE memories(id TEXT, ciphertext TEXT, nonce TEXT)')
                    db.execute('INSERT INTO memories VALUES(?,?,?)', ('fixture', ciphertext, nonce.hex()))
                keys = MemoryKeys()
                keys.created(ALIAS, code)
                keys.track_created_login(session, USER, code, PASSWORD)
                smoke = object.__new__(profile.Smoke)
                smoke.root, smoke.keys, smoke.report, smoke.diagnostic_secrets = root, keys, {}, set()
                smoke.save = lambda: None
                def cli(env, *args):
                    if args[0] == 'keys-export':
                        Path(args[-1]).write_text('super password: ' + code + '\n', encoding='utf-8')
                        return {}
                    probe = json.loads(session_path.read_text(encoding='utf-8'))
                    self.assertEqual(probe['vault_version'], 4)
                    self.assertEqual(probe['keyring_account'], ALIAS)
                    self.assertNotIn('legacy_secret_backend', probe)
                    self.assertNotIn('secret', probe)
                    if failure == 'session':
                        session_path.write_text(json.dumps(dict(probe, token='changed')), encoding='utf-8')
                    if failure == 'show':
                        raise RuntimeError('synthetic_show_failed')
                    return {'details': {'entry': {'content': 'text'}}}
                smoke.cli = cli
                fixture = dict(account={'user': USER}, code=code, password=PASSWORD, id='fixture', text='text',
                               cipher=(ciphertext, nonce.hex()))
                with patch.object(profile.keyring_backend, 'owned_path', lambda path: path):
                    if failure:
                        expected_error = '^native_consumer_session_changed$' if failure == 'session' else '^synthetic_show_failed$'
                        with self.assertRaisesRegex(RuntimeError, expected_error):
                            smoke.verify_mac_native_credentials({'RSRS_DATA_DIR': str(root)}, fixture)
                    else:
                        for _ in range(2):
                            smoke.verify_mac_native_credentials({'RSRS_DATA_DIR': str(root)}, fixture)
                self.assertEqual(session_path.read_bytes(), original)
                self.assertFalse(list(root.glob('native-recovery-*.txt')))
                self.assertNotIn(('rsrs', 'legacy-secret:' + ALIAS), keys.values)
                self.assertTrue(keys.cleanup()['passed'])
