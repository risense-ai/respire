"""Offline outbox and Runtime process identity regressions."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import types
from unittest.mock import patch
import native_fixture_process as native
from release_diagnostic_test_support import SECRET


class ReleaseIdentityCases:
    def test_outbox_health_selectors_and_scoped_receipt_checks(self):
        import io
        from unittest.mock import Mock, MagicMock
        spec = importlib.util.spec_from_file_location('outbox_selector', Path(__file__).with_name('dev-outbox-smoke.py'))
        outbox = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(outbox)
        fixture = outbox.Fixture.__new__(outbox.Fixture)
        fixture.args = types.SimpleNamespace(binary=self.image, runtime_binary=self.image)
        fixture.a = {'RSRS_DATA_DIR': str(self.root), 'RSRS_RUNTIME_BIN': str(self.image), 'RSRS_RPC_PORT': '1234'}
        fixture.direct_failures, fixture.port = [], 1234
        self.health['startup_complete'] = True
        fixture.child = Mock(pid=123)
        (self.root / 'runtime').mkdir()
        (self.root / 'runtime' / 'endpoint.json').write_text('{}')
        requests = []
        wrong_receipt = False
        def response(request, **kwargs):
            requests.append(request)
            if request.full_url.endswith('/api/health'):
                self.assertEqual(request.get_header('X-respire-health'), 'runtime-v1')
                self.assertEqual(kwargs['timeout'], 5)
                value = self.health
            else:
                body = json.loads(request.data)
                value = {key: body[key] for key in ('v', 'id', 'operation_id', 'idempotency_key', 'scope')}
                value.update(ok=True, exit=0)
                if wrong_receipt:
                    value['scope'] = dict(value['scope'], instance_id='wrong-instance')
            stream = io.BytesIO(json.dumps(value).encode())
            stream.status = 200
            return stream
        def wait(action, seconds, code):
            self.assertTrue(action())
        fake_socket = MagicMock()
        fake_socket.__enter__.return_value.getsockname.return_value = ('127.0.0.1', 1234)
        child = Mock(pid=123)
        child.poll.return_value = None
        with patch.object(outbox.urllib.request, 'urlopen', side_effect=response), \
             patch.object(outbox, 'verify_runtime_health') as verified, \
             patch.object(outbox.subprocess, 'run', return_value=types.SimpleNamespace(returncode=0,
                 stdout=b'{"errors":[],"summary":{"store_ready":true}}')) as executed, \
             patch.object(outbox.subprocess, 'Popen', return_value=child), \
             patch.object(outbox.socket, 'socket', return_value=fake_socket), \
             patch.object(outbox, 'wait_for', side_effect=wait):
            fixture.direct(fixture.a, ['sync'])
            fixture.start()
            self.assertTrue(fixture.rpc(['status'])['ok'])
            self.assertEqual(verified.call_count, 4)  # direct, ready, start-status, explicit status
            self.assertEqual(sum(call.args[0][-2:] == ['runtime', 'unlock']
                                 for call in executed.call_args_list), 1)
            wrong_receipt = True
            with self.assertRaisesRegex(RuntimeError, '^local_rpc_failed$'):
                fixture.rpc(['status'])
            verified.side_effect = native.RuntimeHealthFailure('runtime_health_fields_mismatch', ['selected_image'])
            before = len(requests)
            with self.assertRaises(native.RuntimeHealthFailure):
                fixture.rpc(['remember', 'must-not-replay'])
            self.assertEqual(len(requests), before + 1)  # Health only; no business RPC.
        # The header neither bypasses verification nor retries a failed write.
        self.assertEqual(len([r for r in requests if r.full_url.endswith('/api/rpc')]), 3)

    def test_source_selector_contract_is_the_product_contract(self):
        root = Path(__file__).resolve().parent.parent
        host = (root / 'cli/src/client2_host.rs').read_text()
        transport = (root / 'crates/commands/src/net_rpc.rs').read_text()
        self.assertIn('Client2 launcher must connect to the independently running Runtime', host)
        self.assertIn('"startup_complete":false', host)
        self.assertNotIn('TcpListener', host)
        self.assertIn('.set("X-Respire-Health", "runtime-v1")', transport)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.profile = self.root / 'profile'
        self.profile.mkdir()
        self.image = self.root / 'image'
        self.image.write_bytes(b'fixture bytes, not an executable')
        digest = native.hashlib.sha256(self.image.read_bytes()).hexdigest()
        self.args = types.SimpleNamespace(runtime_version='fixture', runtime_binary=self.image, runtime_sha256=digest, runtime_identity_receipts=[])
        self.owner = types.SimpleNamespace(pid=123, create_time=lambda: 1, exe=lambda: str(self.image))
        install, account = native.runtime_scope_ids(self.root, self.profile, {})
        self.health = dict(v=1, pid=123, exe=str(self.image), image_path=str(self.image), process_start_id='birth', runtime_version='fixture', bin='fixture', installation_root=str(self.root), installation_id=install, data_dir=str(self.profile), account_id=account, instance_id='instance', generation=1, image_sha256=digest)
        self.enterContext(patch.dict(sys.modules, {'psutil': types.SimpleNamespace(Process=lambda pid: self.owner)}))
        self.enterContext(patch.object(native, 'process_start_id', return_value='birth'))
        self.enterContext(patch.object(native.sys, 'platform', 'win32' if os.name == 'nt' else 'fixture'))
        self.enterContext(patch.dict(os.environ, {}, clear=True))

    def verify(self):
        return native.verify_runtime_health(self.args, self.health, self.root, self.profile, 123)

    def test_success_receipts_and_cached_success(self):
        first = self.verify()
        self.assertEqual(self.verify(), first)
        self.assertEqual(len(self.args.runtime_identity_receipts), 1)
        self.assertEqual(first['image_sha256'], self.args.runtime_sha256)

    def test_all_health_fields_fail_closed(self):
        keys = dict(protocol='v', exe='exe', image_path='image_path', process_start_id='process_start_id', runtime_version='runtime_version', bin_version='bin', installation_root='installation_root', installation_id='installation_id', profile='data_dir', account_id='account_id', instance_id='instance_id', generation='generation')
        original = copy.deepcopy(self.health)
        for field, key in keys.items():
            with self.subTest(field=field):
                self.health = copy.deepcopy(original)
                self.health[key] = '' if key == 'instance_id' else SECRET
                with self.assertRaises(native.RuntimeHealthFailure) as caught:
                    self.verify()
                self.assertEqual(caught.exception.code, 'runtime_health_fields_mismatch')
                self.assertIn(field, caught.exception.fields)
                self.assertNotIn(SECRET, json.dumps(caught.exception.diagnostic()))
                self.assertEqual(self.args.runtime_identity_receipts, [])
        self.health = original
        self.owner.pid = 456
        with self.assertRaises(native.RuntimeHealthFailure) as caught:
            self.verify()
        self.assertIn('pid', caught.exception.fields)
        self.owner.pid = 123
        self.args.runtime_binary = self.root / SECRET
        with self.assertRaises(native.RuntimeHealthFailure) as caught:
            self.verify()
        self.assertIn('selected_image', caught.exception.fields)

    def test_ownership_and_profile_still_fail(self):
        if os.name == 'posix':
            self.root.chmod(0o755)
            with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_installation_not_private_owned'):
                self.verify()
            self.root.chmod(0o700)
        self.profile = self.root.parent
        with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_profile_escape'):
            self.verify()

    def test_digest_and_cached_file_change_still_fail(self):
        self.health['image_sha256'] = SECRET
        with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_digest_mismatch'):
            self.verify()
        self.health['image_sha256'] = self.args.runtime_sha256
        self.verify()
        self.image.write_bytes(b'changed')
        with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_disk_image_changed'):
            self.verify()

    def test_inspection_races_still_fail(self):
        real_stat = self.image.stat()
        fake_stat = types.SimpleNamespace(st_dev=-1, st_ino=0, st_size=0, st_mtime_ns=0, st_ctime_ns=0,
                                         st_birthtime_ns=0)
        with patch.object(native.os, 'fstat', return_value=fake_stat):
            with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_os_disk_image_mismatch'):
                self.verify()
        with patch.object(native.os, 'fstat', side_effect=[real_stat, fake_stat]):
            with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_image_changed_during_inspection'):
                self.verify()
        with patch.object(native, 'process_start_id', side_effect=['birth', SECRET]):
            with self.assertRaisesRegex(native.RuntimeHealthFailure, 'runtime_health_os_identity_changed'):
                self.verify()
        self.assertEqual(self.args.runtime_identity_receipts, [])
