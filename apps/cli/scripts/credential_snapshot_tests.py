"""Offline checks for Item, Collection, SnapshotTests."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
from unittest.mock import patch
from credential_alias_test_support import native
from credential_alias_test_support import secret_service
from credential_alias_test_support import legacy
from credential_alias_test_support import USER
from credential_alias_test_support import ALIAS
from credential_alias_test_support import OTHER
from credential_alias_test_support import PASSWORD
from credential_alias_test_support import SUPER


class Item:
    def __init__(self, collection, attributes, value):
        self.collection, self.attributes, self.value = collection, attributes, value
    def get_attributes(self): return dict(self.attributes)
    def get_secret(self): return self.value.encode()
    def is_locked(self): return False
    def delete(self): self.collection.items.remove(self)


class Collection:
    def __init__(self): self.items = []
    def search_items(self, query):
        return [item for item in self.items if all(item.attributes.get(k) == v for k, v in query.items())]
    def get_all_items(self): return list(self.items)


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.keys = object.__new__(secret_service.SecretServiceKeys)
        native.TrackedKeys.__init__(self.keys, Path('unused'))
        self.keys.attribute_snapshots = {}
        self.keys.current_target_entries = set()
        self.keys.recovered_super_entries = set()
        self.keys.generic_schema_observed = set()
        self.keys.normalized_current_entries = set()
        self.keys.collection = Collection()
        self.keys.connection = self.keys.daemon = None
        self.keys.daemon_stderr = None
        self.keys.bus_mode = 'private'
        self.session = dict(user=USER, keyring_account=ALIAS)
        for prefix, value in (('super:', SUPER), ('pass:', PASSWORD)):
            self.keys.collection.items.append(Item(self.keys.collection, self.keys.attributes('rsrs', prefix + ALIAS), value))
        self.keys.track_created_login(self.session, USER, SUPER, PASSWORD)

    def test_exact_alias_snapshot_and_narrow_normalization(self):
        import secrets
        registered = set()
        for _ in range(2):
            user = 'ci-migrate-' + secrets.token_hex(8)
            alias = 'login-' + secrets.token_hex(16)
            session = dict(user=user, keyring_account=alias)
            entries = {('rsrs', prefix + alias) for prefix in ('super:', 'pass:')}
            for prefix, value in (('super:', SUPER), ('pass:', PASSWORD)):
                self.keys.collection.items.append(Item(self.keys.collection,
                    self.keys.attributes('rsrs', prefix + alias), value))
            self.keys.track_created_login(session, user, SUPER, PASSWORD)
            before = {k: dict(v) for k, v in self.keys.attribute_snapshots.items()}
            # A previous registration must neither block a new pair nor admit it unchecked.
            for invalid_session, invalid_user in ((dict(session, user=USER), user),
                    (dict(session, keyring_account=OTHER), user), (session, 'foreign')):
                with self.assertRaises(RuntimeError):
                    self.keys.register_current_target(invalid_session, invalid_user)
                self.assertEqual(self.keys.current_target_entries, registered)
            entry = ('rsrs', 'pass:' + alias)
            snapshot = self.keys.attribute_snapshots.pop(entry)
            with self.assertRaisesRegex(RuntimeError, 'not_verified_owned_entry'):
                self.keys.register_current_target(session, user)
            self.keys.attribute_snapshots[entry] = snapshot
            self.keys.entries.remove(entry)
            with self.assertRaisesRegex(RuntimeError, 'not_verified_owned_entry'):
                self.keys.register_current_target(session, user)
            self.keys.entries.add(entry)
            item = self.keys.collection.items.pop()
            with self.assertRaisesRegex(RuntimeError, 'not_verified_owned_entry'):
                self.keys.register_current_target(session, user)
            self.keys.collection.items.append(item)
            item.attributes['extra'] = 'changed'
            with self.assertRaisesRegex(RuntimeError, 'attributes_changed'):
                self.keys.register_current_target(session, user)
            del item.attributes['extra']
            self.assertEqual(self.keys.current_target_entries, registered)
            self.keys.register_current_target(session, user)
            registered.update(entries)
            self.assertEqual(self.keys.current_target_entries, registered)
            with self.assertRaisesRegex(RuntimeError, 'registration_invalid'):
                self.keys.register_current_target(session, user)
            for current in self.keys.collection.items:
                if ('rsrs', current.attributes['username']) in entries:
                    current.attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
            for prefix, value in (('super:', SUPER), ('pass:', PASSWORD)):
                self.assertEqual(self.keys.read('rsrs', prefix + alias), value)
            self.assertEqual(self.keys.attribute_snapshots, before)
            del item.attributes['xdg:schema']
            with self.assertRaisesRegex(RuntimeError, 'schema_reverted'):
                self.keys.read('rsrs', 'pass:' + alias)
            item.attributes['xdg:schema'] = 'foreign'
            with self.assertRaisesRegex(RuntimeError, 'schema_not_generic'):
                self.keys.read('rsrs', 'pass:' + alias)
            item.attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
            item.attributes['application'] = 'foreign'
            with self.assertRaisesRegex(RuntimeError, 'attributes_mismatch'):
                self.keys.read('rsrs', 'pass:' + alias)
            item.attributes['application'] = 'rust-keyring'
        # Tracked but unregistered credentials retain strict raw schema snapshots.
        self.keys.collection.items[1].attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
        with self.assertRaisesRegex(RuntimeError, 'attributes_changed'):
            self.keys.read('rsrs', 'pass:' + ALIAS)
        report = self.keys.cleanup()
        self.assertTrue(report['passed'])
        self.assertEqual(report['provider_daemon'],
                         {'started': False, 'exit_code_at_cleanup_start': None,
                          'abort_type': 'unknown', 'abort_location': 'unknown'})
        baseline = report['provider_schema_baseline']
        self.assertEqual(baseline['scope'],
                         'verified_registration_pairs_and_verified_recovered_super_only')
        self.assertEqual(baseline['registered_entries'], 4)
        self.assertEqual(baseline['observed_normalizations'], 4)
        self.assertFalse(baseline['initial_raw_snapshots_overwritten'])
        private = (USER + ' ' + PASSWORD + ' ' + SUPER).encode()
        libc_assertion = (b'gnome-keyring-daemon: /private/' + private
                          + b'/egg-secure-memory.c:560: sec_free: Assertion `' + private + b"' failed.\n")
        for stderr, code, expected in (
                (b'Bail out! Gkm:ERROR:/private/' + private + b'.c:42:function: assertion failed: (' + private + b')', -6, 'glib_assertion'),
                (libc_assertion, -6, 'libc_assertion'),
                (libc_assertion, -5, 'unknown'),
                (libc_assertion.replace(b'egg-secure-memory.c', b'unknown.c'), -6, 'unknown'),
                (b'free(): double free detected ' + private, -6, 'allocator_double_free'),
                (b'double free or corruption ' + private, -6, 'allocator_corruption'),
                (b'malloc(): corrupted ' + private, -6, 'allocator_corruption'),
                (b'munmap_chunk(): invalid pointer ' + private, -6, 'allocator_invalid_pointer'),
                (b'*** stack smashing detected *** ' + private, -6, 'stack_smashing'),
                (b'(gnome-keyring-daemon:123): GLib-GIO-ERROR **: ' + private, -5, 'glib_fatal_error'),
                (b'** (gnome-keyring-daemon:123): Gkm-CRITICAL **: ' + private, -5, 'glib_fatal_critical'),
                (private, -6, 'unknown'), (b'ERROR:file.c:42:fn: assertion failed', None, 'unknown'),
                (b'ERROR:file.c:42:fn: assertion failed', 0, 'unknown')):
            self.assertEqual(secret_service.provider_abort_type(stderr, code), expected)
        location = secret_service.provider_abort_location(libc_assertion, -6)
        self.assertEqual(location, dict(file='egg-secure-memory.c', line=560, function='sec_free'))
        for value in (USER, PASSWORD, SUPER, '/private/', 'Assertion'):
            self.assertNotIn(value, json.dumps(location))
        self.assertEqual(secret_service.provider_abort_location(libc_assertion, -5), 'unknown')
        for filename in (b'gkd-secret-service.c', b'gkm-secret-item.c', b'egg-secure-memory.c', b'gmain.c'):
            header = b'Bail out! Gkm:ERROR:/private/' + private + b'/' + filename + b':42:some_function: assertion failed: (' + private + b')'
            location = secret_service.provider_abort_location(header, -6)
            self.assertEqual(location, dict(file=filename.decode(), line=42, function='some_function'))
            self.assertEqual(secret_service.provider_abort_location(header, -5), location)
            for value in (USER, PASSWORD, SUPER, '/private/', 'assertion failed'):
                self.assertNotIn(value, json.dumps(location))
            self.assertEqual(secret_service.provider_abort_location(header, None), 'unknown')
            self.assertEqual(secret_service.provider_abort_location(header, 0), 'unknown')
        for header in (
                libc_assertion.replace(b'egg-secure-memory.c', b'unknown.c'),
                b'ERROR:/private/' + private + b'.c:42:some_function: assertion failed: (private)',
                b'ERROR:/private/unknown.c:42:some_function: assertion failed: (private)',
                b'ERROR:/private/gkm-secret-item.c:0:some_function: assertion failed: (private)',
                b'ERROR:/private/gkm-secret-item.c:-1:some_function: assertion failed: (private)',
                b'ERROR:/private/gkm-secret-item.c:42:function-with-value: assertion failed: (private)',
                b'ERROR:/private/gkm-secret-item.c:42:some_function(' + private + b'): assertion failed: (private)',
                b'ERROR:/private/gkm-secret-item.c:42:some_function: private assertion failed',
                b'private ERROR:/private/gkm-secret-item.c:42:some_function: assertion failed: (private)',
                private):
            self.assertEqual(secret_service.provider_abort_location(header, -6), 'unknown')
        if secret_service.os.name == 'posix':
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(secret_service, 'owned_path', lambda path: path):
                root = Path(directory)
                root.chmod(0o700)
                with secret_service.private_provider_stderr(root) as stream:
                    self.assertEqual(secret_service.stat.S_IMODE(root.stat().st_mode), 0o700)
                    self.assertEqual(secret_service.stat.S_IMODE((root / 'provider.stderr').stat().st_mode), 0o600)
                    stream.write(private)
                self.assertTrue((root / 'provider.stderr').read_bytes() == private)
                with self.assertRaises(FileExistsError):
                    secret_service.private_provider_stderr(root)
                root.chmod(0o755)
                with self.assertRaisesRegex(RuntimeError, '^fixture_provider_root_not_private$'):
                    secret_service.private_provider_stderr(root)
                root.chmod(0o700)

    def test_foreign_user_alias_missing_untracked_rejected(self):
        for session in (dict(user='foreign', keyring_account=ALIAS), dict(user=USER, keyring_account=OTHER), dict(user=USER)):
            with self.assertRaises(RuntimeError):
                self.keys.register_current_target(session, USER)
            self.assertFalse(self.keys.current_target_entries)

    def test_missing_snapshot_or_credential_rejected(self):
        self.keys.attribute_snapshots.clear()
        with self.assertRaisesRegex(RuntimeError, 'not_verified_owned_entry'):
            self.keys.register_current_target(self.session, USER)
        self.assertFalse(self.keys.current_target_entries)

    def test_cleanup_never_deletes_untracked_private_item(self):
        foreign = Item(self.keys.collection, self.keys.attributes('rsrs', 'pass:' + OTHER), 'foreign')
        self.keys.collection.items.append(foreign)
        report = self.keys.cleanup()
        self.assertFalse(report['passed'])
        self.assertEqual(self.keys.collection.items, [foreign])
        self.assertIn({'code': 'fixture_collection_not_empty'}, report['remaining_entries'])
        self.assertEqual(report['collection_cleanup'],
                         {'passed': False, 'stage': 'empty_check', 'remaining_entries': 1})
        for forbidden in (PASSWORD, SUPER, USER, ALIAS, OTHER, 'foreign'):
            self.assertNotIn(forbidden, json.dumps(report))
        with tempfile.TemporaryDirectory() as directory:
            stderr = self.keys.daemon_stderr = Path(directory) / 'provider.stderr'
            stderr.write_bytes(b'Bail out! Gkm:ERROR:/private/gkm-secret-item.c:42:gkm_secret_item_get: assertion failed: ('
                               + (USER + PASSWORD + SUPER).encode() + b')')
            daemon = Mock()
            daemon.poll.return_value = -6
            self.keys.daemon = daemon
            # A dead provider cannot prove collection cleanup; null must stay unknown.
            with patch.object(self.keys.collection, 'get_all_items', side_effect=RuntimeError('private')):
                report = self.keys.cleanup()
            self.assertFalse(report['passed'])
            self.assertEqual(report['provider_daemon'], dict(started=True,
                exit_code_at_cleanup_start=-6, abort_type='glib_assertion',
                abort_location=dict(file='gkm-secret-item.c', line=42, function='gkm_secret_item_get')))
            self.assertEqual(report['collection_cleanup'], dict(passed=False,
                stage='enumeration', remaining_entries=None))
            self.assertIn({'code': 'fixture_collection_enumeration_failed'}, report['remaining_entries'])
            daemon.terminate.assert_not_called()
            daemon.kill.assert_not_called()
            for forbidden in (PASSWORD, SUPER, USER, ALIAS, OTHER, '/private/', 'assertion failed'):
                self.assertNotIn(forbidden, json.dumps(report))
            stderr.unlink()
            report = self.keys.cleanup()
            self.assertFalse(report['passed'])
            self.assertEqual(report['provider_daemon']['abort_type'], 'unknown')
            self.assertEqual(report['provider_daemon']['abort_location'], 'unknown')
            self.assertIn({'code': 'fixture_provider_diagnostic_read_failed'}, report['remaining_entries'])

    def test_business_cleanup_accepts_factor_slots_but_never_deletes_foreign_entries(self):
        self.keys.verify_owner = lambda: None
        for alias in (ALIAS, 'legacy-' + 'a' * 16, 'legacy-' + 'b' * 64):
            for prefix in ('pass:', 'super:', 'legacy-secret:'):
                self.keys.collection.items.append(Item(self.keys.collection,
                    self.keys.attributes('rsrs', prefix + alias), 'synthetic-factor'))
        foreign = Item(self.keys.collection,
            self.keys.attributes('rsrs', 'legacy-secret:' + USER), 'foreign')
        self.keys.collection.items.append(foreign)
        before = list(self.keys.collection.items)
        with self.assertRaisesRegex(RuntimeError, '^unexpected_private_credential$'):
            self.keys.cleanup_business_entries()
        self.assertEqual(self.keys.collection.items, before)
        self.assertEqual(self.keys.business_cleanup['deleted_entries'], 0)
        self.keys.collection.items.remove(foreign)
        self.keys.cleanup_business_entries()
        self.assertFalse(self.keys.collection.items)
        self.assertTrue(self.keys.business_cleanup['passed'])
        self.assertEqual(self.keys.business_classification['known_prefix_counts']['legacy-secret:'], 3)

    def test_all_tracked_cleanup_is_empty(self):
        self.keys.register_current_target(self.session, USER)
        operation = '44444444-4444-4444-8444-444444444444'
        alias = 'login-' + operation.replace('-', '')
        candidate = dict(user='', addr='', vault_version=4, auth_operation_id=operation,
            keyring_account=alias, keyring_backend='secret_service')
        payload = json.dumps(dict(password='', super_password=SUPER, account_secret='', totp=None,
            material=dict(keyring_account=alias, keyring_backend='secret_service', reset=None, challenge=None)))
        for prefix, value in (('super:', SUPER), ('key-control:', payload)):
            self.keys.collection.items.append(Item(self.keys.collection,
                self.keys.attributes('rsrs', prefix + alias), value))
        private_control = self.keys.collection.items[-1]
        private_control.value = payload.replace(SUPER, 'foreign')
        with self.assertRaisesRegex(RuntimeError, 'recovery_control_material_mismatch'):
            self.keys.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
        self.assertFalse(self.keys.recovered_super_entries)
        self.assertNotIn(('rsrs', 'key-control:' + alias), self.keys.entries)
        private_control.value = payload
        self.keys.track_created_login(candidate, '', SUPER, None, recovery_operation=operation)
        recovery_super = next(item for item in self.keys.collection.items
            if item.attributes['username'] == 'super:' + alias)
        original_attributes = dict(recovery_super.attributes)
        recovery_super.attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
        self.keys.collection.items.append(Item(self.keys.collection,
            self.keys.attributes('rsrs', 'pass:' + alias), PASSWORD))
        self.keys.track_created_login(dict(candidate, user=USER, addr=legacy.UPSTREAM), USER, SUPER, PASSWORD,
            recovery_operation=operation)
        self.assertEqual(self.keys.attribute_snapshots['rsrs', 'super:' + alias], original_attributes)
        control = next(item for item in self.keys.collection.items
            if item.attributes['username'] == 'key-control:' + alias)
        control.attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
        with self.assertRaisesRegex(RuntimeError, 'attributes_changed'):
            self.keys.read('rsrs', 'key-control:' + alias)
        del control.attributes['xdg:schema']
        del recovery_super.attributes['xdg:schema']
        with self.assertRaisesRegex(RuntimeError, 'schema_reverted'):
            self.keys.read('rsrs', 'super:' + alias)
        recovery_super.attributes['xdg:schema'] = 'foreign'
        with self.assertRaisesRegex(RuntimeError, 'schema_not_generic'):
            self.keys.read('rsrs', 'super:' + alias)
        recovery_super.attributes['xdg:schema'] = 'org.freedesktop.Secret.Generic'
        recovery_super.attributes['application'] = 'foreign'
        with self.assertRaisesRegex(RuntimeError, 'attributes_mismatch'):
            self.keys.read('rsrs', 'super:' + alias)
        recovery_super.attributes['application'] = 'rust-keyring'
        control.value = 'third-party-private-material'
        rejected = self.keys.cleanup()
        self.assertFalse(rejected['passed'])
        self.assertEqual(self.keys.collection.items, [control])
        self.assertIn({'code': 'fixture_collection_not_empty'}, rejected['remaining_entries'])
        self.assertNotIn(control.value, json.dumps(rejected))
        control.value = payload
        report = self.keys.cleanup()
        self.assertTrue(report['passed'])
        self.assertEqual(report['provider_schema_baseline']['registered_entries'], 2)
        self.assertEqual(report['provider_schema_baseline']['recovered_super_entries'], 1)
        self.assertEqual(report['provider_schema_baseline']['observed_normalizations'], 1)
        self.assertFalse(self.keys.collection.items)
