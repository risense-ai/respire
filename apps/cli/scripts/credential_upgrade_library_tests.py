"""Library state and index integrity across credential upgrades."""
import ast
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from unittest.mock import patch
from credential_alias_test_support import HERE
from credential_alias_test_support import legacy
from credential_alias_test_support import PASSWORD
from credential_alias_test_support import SUPER
from credential_alias_test_support import MemoryKeys


class UpgradeLibraryStateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from contextlib import closing
        import hashlib
        import sqlite3
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        from native_fixture_process import same_existing_path
        tree = ast.parse((HERE / 'test-real-m3-account-switch.py').read_text(encoding='utf-8'))
        library = ast.parse((HERE / 'm3_account_library.py').read_text(encoding='utf-8'))
        functions = [node for node in (*ast.walk(tree), *library.body) if isinstance(node, ast.FunctionDef)
                     and node.name in ('recover_fixture_urk', 'library_state', 'snapshot',
                                       'database_checkpoint_state', 'assert_database_checkpoint_preserved',
                                       'assert_historical_database_seed',
                                       'legacy_custody_alias', 'migrate_fixture_custody',
                                       'custody_consumer_error', 'verify_fixture_custody')]
        namespace = dict(vault_crypto=legacy, AESGCM=AESGCM, json=json, hashlib=hashlib, sqlite3=sqlite3, closing=closing,
                         pathlib=SimpleNamespace(Path=Path), sys=SimpleNamespace(platform='darwin'),
                         canonical_scope_path=lambda path: str(path.resolve()), same_existing_path=same_existing_path)
        exec(compile(ast.Module(body=functions, type_ignores=[]), '<upgrade-library-fixture>', 'exec'), namespace)
        cls.helpers = namespace

    def test_actual_seed_setup_covers_legacy_v1_v3_and_current_v4(self):
        import secrets
        import tempfile
        import time
        from types import SimpleNamespace
        tree = ast.parse((HERE / 'm3_account_lifecycle.py').read_text(encoding='utf-8'))
        body = next(node.body for node in tree.body
                    if isinstance(node, ast.FunctionDef) and node.name == 'seed_accounts')
        start = next(index for index, node in enumerate(body) if isinstance(node, ast.Assign)
                     and any(isinstance(target, ast.Name) and target.id == 'expected_urks' for target in node.targets))
        end = next(index for index, node in enumerate(body) if isinstance(node, ast.Assign)
                   and any(isinstance(target, ast.Name) and target.id == 'seed_env' for target in node.targets))
        code = compile(ast.Module(body=body[start:end], type_ignores=[]), '<actual-upgrade-seed>', 'exec')
        for historical in (False, True):
            with self.subTest(historical=historical), tempfile.TemporaryDirectory() as directory:
                main, alternate = Path(directory) / 'main', Path(directory) / 'alternate'
                main.mkdir(); alternate.mkdir()
                namespace = dict(self.helpers, main=main, alternate=alternate, secrets=secrets, time=time,
                                 args=SimpleNamespace(seed_binary=historical), code='A3-' + '12' * 18)
                exec(code, namespace)
                self.assertEqual(namespace['seed_vault_versions'], [1, 3] if historical else [4, 4])
                for profile in (main, alternate):
                    recovered = self.helpers['recover_fixture_urk'](
                        json.loads((profile / 'session.json').read_text()), namespace['recovery_factors'][str(profile)])
                    self.assertEqual(recovered, namespace['expected_urks'][str(profile)])

        seed_command = next(node for node in ast.walk(tree)
                            if isinstance(node, ast.FunctionDef) and node.name == 'seed_command')
        version = next(node for node in ast.walk(tree) if isinstance(node, ast.Assign)
                       and any(isinstance(target, ast.Name) and target.id == 'seed_version' for target in node.targets))
        policy = next(node for node in ast.walk(tree) if isinstance(node, ast.If)
                      and isinstance(node.test, ast.Compare) and isinstance(node.test.left, ast.Name)
                      and node.test.left.id == 'seed_version')
        version_code = compile(ast.Module(body=[seed_command, version, policy], type_ignores=[]),
                               '<actual-historical-version>', 'exec')
        for released in ('1.0.11', '1.0.11-dev.1', '1.0.12', '1.0.10'):
            with self.subTest(version=released), tempfile.TemporaryDirectory() as directory:
                calls = []
                def captured_run(argv, **kwargs):
                    self.assertEqual(argv[1:3], ['--json', '--direct'])
                    self.assertEqual(kwargs['env'], {'isolated': 'fixture'})
                    calls.append(argv[3:])
                    # Clap --version is text even with --json; the v command uses the JSON envelope.
                    stdout = ('rsrs ' + released).encode() if argv[3:] == ['--version'] else \
                        json.dumps(dict(summary=dict(version=released))).encode()
                    return SimpleNamespace(returncode=0, stdout=stdout, stderr=b'')
                namespace = dict(json=json, pathlib=SimpleNamespace(Path=Path), captured_run=captured_run,
                                 args=SimpleNamespace(seed_binary='historical-cli'), seed_env={'isolated': 'fixture'},
                                 profile=Path(directory))
                exec(version_code, namespace)
                self.assertEqual(calls, [['v']] + ([['model', 'activate', 'm3']] if released == '1.0.10' else []))
                for reply, error in ((SimpleNamespace(returncode=17, stdout=b'', stderr=b''), AssertionError),
                                     (SimpleNamespace(returncode=0, stdout=b'not json', stderr=b''), json.JSONDecodeError),
                                     (SimpleNamespace(returncode=0, stdout=b'{}', stderr=b''), KeyError)):
                    with self.assertRaises(error):
                        exec(version_code, dict(namespace, captured_run=lambda *a, **kw: reply))
                if released != '1.0.10':
                    (Path(directory) / 'rsrs.db').touch()
                    with self.assertRaisesRegex(AssertionError, '^historical seed profile is not empty$'):
                        exec(version_code, namespace)

    def test_legacy_session_slot_changes_and_rebuilt_indexes_preserve_real_library_state(self):
        from contextlib import closing
        import sqlite3
        import tempfile
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        from cryptography.exceptions import InvalidTag
        for version, database_name in ((version, name) for version in (1, 3, 4)
                                       for name in ('rsrs.db', 'onememory.db')):
            with self.subTest(version=version, database=database_name), tempfile.TemporaryDirectory() as directory:
                installation = Path(directory) / '.rsrs'
                profile = installation / 'accounts/alternate' if version == 3 else installation
                profile.mkdir(parents=True)
                database = profile / database_name
                urk, salt, nonce = bytes(range(32)), 'ab' * 16, bytes.fromhex('cd' * 12)
                factors = {'pass': PASSWORD, 'super': 'A3-' + '12' * 18,
                           'secret': 'original-secret', 'secret_key': 'original-secret-key'}
                def wrap(current, key=urk):
                    kek = (legacy.v4_kek(factors['super'], salt, legacy.PREFIX if current else '')
                           if version == 4 else legacy.legacy_kek(version,
                           factors['pass'] if version == 1 else factors['super'],
                           factors['secret'] if version == 1 else factors['secret_key'], salt, current=current))
                    return (legacy.PREFIX if current else '') + AESGCM(kek).encrypt(nonce, key, None).hex()
                def encrypt(current, payload=b'{"content":"original record"}', key=urk):
                    info = b'rsrs:data:v1' if current else b'onememory:data:v1'
                    return (legacy.PREFIX if current else '') + AESGCM(legacy.hkdf(key, None, info)).encrypt(nonce, payload, None).hex()
                session = dict(vault_version=version, kdf_salt=salt, urk_nonce=nonce.hex(),
                               wrapped_urk=wrap(False), **factors)
                (profile / 'session.json').write_text(json.dumps(session), encoding='utf-8')
                (profile / 'core-index').mkdir()
                (profile / 'core-index/index').write_bytes(b'old index')
                with closing(sqlite3.connect(database)) as db, db:
                    db.executescript('CREATE TABLE memories(id TEXT,ciphertext TEXT,nonce TEXT,dirty INTEGER,deleted INTEGER);'
                        'CREATE TABLE sync_outbox(seq INTEGER,operation TEXT);'
                        'CREATE TABLE core_artifacts(memory_id TEXT,model TEXT,source TEXT);')
                    db.execute('INSERT INTO memories VALUES(?,?,?,?,?)', ('original', encrypt(False), nonce.hex(), 1, 0))
                    db.execute('INSERT INTO sync_outbox VALUES(1,?)', ('pending original write',))
                    db.execute('INSERT INTO core_artifacts VALUES(?,?,?)', ('original', 'm3', 'old index source'))
                snapshot, state = self.helpers['snapshot'], self.helpers['library_state']
                before = snapshot(profile)
                expected = state(before, factors)
                self.assertEqual(expected[2], urk)
                if version in (1, 3):
                    alias = self.helpers['legacy_custody_alias'](profile, before[2])
                    slots = {'pass' if version == 1 else 'super': factors['pass' if version == 1 else 'super'],
                             'legacy-secret': factors['secret' if version == 1 else 'secret_key']}
                    migrate = self.helpers['migrate_fixture_custody']
                    for backend in ('native', 'secret_service'):
                        keys = MemoryKeys()
                        keys.credential_backend = backend
                        for slot in slots:
                            keys.reserve('rsrs', slot + ':' + alias)
                        # Failed readback must retain the original session and owned cleanup set.
                        with patch.object(keys, 'read', return_value='wrong'), self.assertRaises(RuntimeError):
                            migrate(profile, keys, (alias, version, slots), urk)
                        self.assertEqual(snapshot(profile), before)
                        self.assertTrue(keys.cleanup()['passed'])
                        migrate(profile, keys, (alias, version, slots), urk)
                        current = snapshot(profile)
                        self.assertEqual(current[:2], before[:2])
                        self.assertEqual(state(current, factors), expected)
                        committed = json.loads(current[2])
                        self.assertEqual(committed['legacy_secret_backend'], backend)
                        self.assertEqual(committed['keyring_backend'], backend)
                        self.assertEqual(committed['vault_version'], version)
                        self.assertFalse(set(factors) & committed.keys())
                        if backend == 'native':
                            config = installation / 'client.json'
                            config.write_text(json.dumps({'data_dir': str(profile)}), encoding='utf-8')
                            config_before = config.read_bytes()
                            env = {'RSRS_DATA_DIR': str(installation), 'RSRS_BIN_DIR': str(installation / 'bin'),
                                   'RSRS_RUNTIME_BIN': str(installation / 'bin/runtime/dev23/rsrs-runtime')}
                            keys.keychain = profile / 'synthetic.keychain'
                            keys._security = Mock(return_value=SimpleNamespace(returncode=0))
                            def consume(command, *, env, timeout):
                                self.assertEqual(env['RSRS_DATA_DIR'], str(installation))
                                self.assertEqual(json.loads(config.read_bytes())['data_dir'], str(profile))
                                self.assertEqual(env, expected_env)
                                self.assertEqual(command, ['fixture-cli', '--json', 'show', 'original'])
                                return SimpleNamespace(returncode=0, stderr=b'', stdout=json.dumps({
                                    'status': 'ok', 'details': {'entry': {'content': 'original record'}}}).encode())
                            expected_env = dict(env)
                            verify = self.helpers['verify_fixture_custody']
                            with patch.dict(self.helpers, captured_run=consume):
                                receipt = verify(profile, keys, (alias, version, slots), urk, 'fixture-cli', env)
                            self.assertEqual(receipt['consumer'], 'cli_authenticated_decryption')
                            self.assertEqual(config.read_bytes(), config_before)
                            self.assertEqual(snapshot(profile), current)
                            for returncode, message in ((1, 'unlock Runtime does not own the selected profile'),
                                                        (1, 'runtime request belongs to another installation'),
                                                        (0, 'host session credential read failed'), (1, 'unknown')):
                                output = SimpleNamespace(returncode=returncode,
                                    stdout=json.dumps({'status': 'fail', 'errors': [message + ' ' + PASSWORD]}).encode(),
                                    stderr=(SUPER + ' original record').encode())
                                with patch.dict(self.helpers, captured_run=Mock(return_value=output)), \
                                        self.assertRaises(AssertionError) as failed:
                                    verify(profile, keys, (alias, version, slots), urk, 'fixture-cli', env)
                                diagnostic = str(failed.exception)
                                self.assertIn(message if message != 'unknown' else 'unclassified', diagnostic)
                                self.assertNotIn(PASSWORD, diagnostic)
                                self.assertNotIn(SUPER, diagnostic)
                                self.assertNotIn('original record', diagnostic)
                            config.write_text(json.dumps({'data_dir': str(profile / 'wrong')}), encoding='utf-8')
                            child = Mock()
                            with patch.dict(self.helpers, captured_run=child), \
                                    self.assertRaisesRegex(AssertionError, 'account is not selected'):
                                verify(profile, keys, (alias, version, slots), urk, 'fixture-cli', env)
                            child.assert_not_called()
                        self.assertTrue(keys.cleanup()['passed'])
                        self.assertFalse(keys.values)
                        (profile / 'session.json').write_bytes(before[2])
                migrated = {key: value for key, value in session.items() if key not in factors}
                migrated.update(wrapped_urk=wrap(True), keyring_account='legacy-' + '1' * 16,
                                keyring_backend='native')
                (profile / 'session.json').write_text(json.dumps(migrated), encoding='utf-8')
                (profile / 'core-index/index').write_bytes(b'rebuilt index')
                with closing(sqlite3.connect(database)) as db, db:
                    db.execute('UPDATE memories SET ciphertext=?', (encrypt(True),))
                    db.execute('UPDATE core_artifacts SET source=?', ('rebuilt source',))
                self.assertNotEqual(snapshot(profile), before)
                self.assertEqual(state(snapshot(profile), factors), expected)
                for field in ('super',) if version == 4 else ('pass', 'secret') if version == 1 else ('super', 'secret_key'):
                    with self.subTest(wrong_factor=field), self.assertRaises(InvalidTag):
                        state(snapshot(profile), {**factors, field: 'A3-' + '34' * 18 if field == 'super' and version == 4 else 'wrong'})
                for mutation, restore in (("UPDATE memories SET id='replacement'", "UPDATE memories SET id='original'"),
                        ('DELETE FROM memories', None), ('UPDATE memories SET dirty=0', 'UPDATE memories SET dirty=1'),
                        ('UPDATE memories SET deleted=1', 'UPDATE memories SET deleted=0'),
                        ('DELETE FROM sync_outbox', "INSERT INTO sync_outbox VALUES(1,'pending original write')")):
                    with closing(sqlite3.connect(database)) as db, db:
                        db.execute(mutation)
                    self.assertNotEqual(state(snapshot(profile), factors), expected)
                    with closing(sqlite3.connect(database)) as db, db:
                        if restore is None:
                            db.execute('INSERT INTO memories VALUES(?,?,?,?,?)', ('original', encrypt(True), nonce.hex(), 1, 0))
                        else:
                            db.execute(restore)
                with closing(sqlite3.connect(database)) as db, db:
                    db.execute('UPDATE memories SET ciphertext=?', (encrypt(True, b'changed record'),))
                self.assertNotEqual(state(snapshot(profile), factors), expected)
                replacement_urk = bytes(reversed(range(32)))
                migrated['wrapped_urk'] = wrap(True, replacement_urk)
                (profile / 'session.json').write_text(json.dumps(migrated), encoding='utf-8')
                with self.assertRaises(InvalidTag):
                    state(snapshot(profile), factors)
                with closing(sqlite3.connect(database)) as db, db:
                    db.execute('UPDATE memories SET ciphertext=?', (encrypt(True, key=replacement_urk),))
                self.assertEqual(state(snapshot(profile), factors)[0], expected[0])
                self.assertNotEqual(state(snapshot(profile), factors), expected)

        checkpoint = self.helpers['database_checkpoint_state']
        preserved = self.helpers['assert_database_checkpoint_preserved']
        key, nonce = bytes(range(32)), bytes.fromhex('cd' * 12)
        payload = json.dumps({'content': 'legacy association', 'supersedes': 'ancestor',
                              'superseded_by': 'successor', 'see_also': ['peer']}).encode()
        old = AESGCM(legacy.hkdf(key, None, b'onememory:data:v1')).encrypt(nonce, payload, None).hex()
        raw = ([[('entry', old, nonce.hex(), 1, 0)]], [], b'')
        seeded = ([[('entry', old, nonce.hex(), 1, 0), ('deleted', old, nonce.hex(), 1, 1)],
                   [('pending operation',)], []], [], b'')
        require_seed = self.helpers['assert_historical_database_seed']
        require_seed(seeded)
        for memories, outbox in ((seeded[0][0][:1], seeded[0][1]),
                                (seeded[0][0][1:], seeded[0][1]),
                                ([seeded[0][0][0], ('deleted', '', '', 1, 1)], seeded[0][1]),
                                ([seeded[0][0][0], ('deleted', old, nonce.hex(), 0, 1)], seeded[0][1]),
                                (seeded[0][0], [])):
            with self.assertRaises(AssertionError):
                require_seed(([memories, outbox, []], [], b''))
        records = [('entry', legacy.decrypt_content(key, old, nonce.hex()), 1, 0)]
        before = checkpoint(Path('.'), 'v1.0.11-dev.1', raw, records)
        self.assertTrue(before['legacy_domain'])
        after = {**before, 'legacy_domain': False, 'links': before['links'] + [('new', '', '', ())]}
        preserved(before, after)
        with self.assertRaisesRegex(AssertionError, 'relation fields changed'):
            preserved(before, {**after, 'links': [('entry', 'ancestor', '', ('peer',))]})
        with self.assertRaisesRegex(ValueError, 'unsupported database checkpoint'):
            checkpoint(Path('.'), 'v1.0.15-dev.10', raw, records)

        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            with closing(sqlite3.connect(profile / 'rsrs.db')) as db, db:
                db.executescript('CREATE TABLE witness_log(schema_version,library_id,seq,ts,action,entry_id,'
                    'previous_entry_id,before_digest,after_digest,prev,hash);'
                    'CREATE TABLE query_log(id INTEGER PRIMARY KEY,request_id TEXT);'
                    'CREATE TABLE query_feedback(query_log_id INTEGER,memory_id TEXT,good INTEGER,'
                    'source TEXT,marked_at INTEGER,revision INTEGER);')
                event = (1, 'library', 1, 'stamp', 'create', 'entry', '', None, 'digest', 'previous', 'hash')
                db.execute('INSERT INTO witness_log VALUES(?,?,?,?,?,?,?,?,?,?,?)', event)
                db.execute("INSERT INTO query_log VALUES(1,'request')")
                db.execute("INSERT INTO query_feedback VALUES(1,'entry',1,'human',1,1)")
            before = checkpoint(profile, 'v1.0.17-dev.6', raw, records)
            self.assertTrue(before['witness'] and before['requests'] and before['feedback'])
            self.assertFalse(before['recovery_table'])
            with closing(sqlite3.connect(profile / 'rsrs.db')) as db, db:
                db.execute('INSERT INTO witness_log VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                           (1, 'library', 2, 'later', 'update', 'entry', 'entry', 'digest', 'next', 'hash', 'next-hash'))
            preserved(before, checkpoint(profile, 'v1.0.17-dev.6', raw, records))
            with closing(sqlite3.connect(profile / 'rsrs.db')) as db, db:
                db.execute("UPDATE witness_log SET after_digest='changed' WHERE seq=1")
            with self.assertRaisesRegex(AssertionError, 'witness prefix changed'):
                preserved(before, checkpoint(profile, 'v1.0.17-dev.6', raw, records))
            with closing(sqlite3.connect(profile / 'rsrs.db')) as db, db:
                db.execute("UPDATE witness_log SET after_digest='digest' WHERE seq=1")
                db.execute('DELETE FROM query_feedback')
            with self.assertRaisesRegex(AssertionError, 'explicit feedback changed'):
                preserved(before, checkpoint(profile, 'v1.0.17-dev.6', raw, records))
