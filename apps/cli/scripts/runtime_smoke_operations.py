"""Model recovery, space commands and update checks against the fixture Runtime."""
import hashlib
import json
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess
import time
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
import threading
from native_fixture_process import install_published_runtime
from native_fixture_process import verify_runtime_health
from native_fixture_process import scoped_rpc_request
from native_fixture_process import verify_rpc_receipt
from runtime_smoke_failures import Failure
from runtime_smoke_failures import require


class RuntimeOperations:
    def doctor_model_recovery(self):
        """Exercise a real failed download and retry against the exact artifact."""
        require(self.child is None, 'doctor_fixture_requires_stopped_runtime')
        model = Path(self.env['RSRS_M3_DIR'])
        cache = self.root / 'doctor-model-cache'
        self.start()
        candidates = self.rpc(['recall', 'Runtime fixture persisted memory.', '--titles'])['details']
        candidate_ids = [row['id'] for row in candidates]
        require(bool(candidate_ids), 'doctor_baseline_candidates_missing')
        self.stop()
        model.rename(cache)
        database = self.library / 'rsrs.db'

        def asset_digests(directory):
            values = {}
            for name in ('tokenizer.json', 'onnx/model_quantized.onnx'):
                with (directory / name).open('rb') as stream:
                    values[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
            return values

        expected_assets = asset_digests(cache)

        def snapshot():
            with sqlite3.connect(database) as db:
                return (db.execute('SELECT id,user,ciphertext,nonce,dirty,deleted,created_at,updated_at FROM memories ORDER BY id').fetchall(),
                        db.execute('SELECT * FROM sync_outbox ORDER BY seq').fetchall())

        before = snapshot()
        session = (self.library / 'session.json').read_bytes()
        with sqlite3.connect(database) as db:
            db.execute('DELETE FROM core_artifacts')
            db.commit()
        allowed, started, release = threading.Event(), threading.Event(), threading.Event()
        requests, rejected_requests = [], []
        repair = None
        repair_deadline = None
        prefix = '/Xenova/bge-m3/resolve/4de13258303883538bd53b696b452bf8099f0858/'

        class ModelSource(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                name = self.path.removeprefix(prefix)
                if not self.path.startswith(prefix) or name not in ('tokenizer.json', 'onnx/model_quantized.onnx'):
                    self.send_response(404)
                    self.end_headers()
                    return
                if not allowed.is_set():
                    rejected_requests.append(name)
                    self.send_response(503)
                    self.end_headers()
                    return
                requests.append((name, self.headers.get('Range')))
                path = cache / name
                self.send_response(200)
                self.send_header('Content-Length', str(path.stat().st_size))
                self.end_headers()
                try:
                    with path.open('rb') as stream:
                        first = True
                        while chunk := stream.read(256 * 1024):
                            self.wfile.write(chunk)
                            self.wfile.flush()
                            if first and name.startswith('onnx/') and self.headers.get('Range') is None:
                                started.set()
                                if not release.wait(45):
                                    return
                            first = False
                except (BrokenPipeError, ConnectionResetError):
                    return

        server = ThreadingHTTPServer(('127.0.0.1', 0), ModelSource)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        previous_mirror = self.env.get('RSRS_MIRROR')
        self.env['RSRS_MIRROR'] = 'http://127.0.0.1:' + str(server.server_port)

        def doctor(fix=False):
            argv = ['--json', 'doctor', *(['--fix'] if fix else [])]
            body = {'v': 1, 'id': secrets.token_hex(8), 'method': 'cli.exec', 'args': argv}
            if self.args.runtime_binary and fix:
                # Repair and asset installation belong to the host CLI.
                result = subprocess.run([str(self.args.binary), *argv], env=self.env,
                                        cwd=self.root, capture_output=True, timeout=90)
                require(result.returncode in (0, 1, 2), 'host_doctor_repair_failed')
                reply = {'exit': result.returncode, 'envelope': json.loads(result.stdout)}
                code = 200
            else:
                if self.args.runtime_binary:
                    code, health, _ = self.http(self.url, 'GET', '/api/health')
                    require(code == 200, 'doctor_scope_health_failed')
                    verify_runtime_health(self.args, health, self.root, self.library, self.child.pid)
                    body = scoped_rpc_request(health, argv)
                code, reply, _ = self.http(self.url, 'POST', '/api/rpc', body, self.token)
                if self.args.runtime_binary:
                    verify_rpc_receipt(body, reply)
            envelope = reply.get('envelope', {})
            require(code == 200 and envelope.get('command') == 'doctor'
                    and reply.get('exit') in (0, 1, 2), 'doctor_repair_envelope_invalid')
            rows = {row['name']: row for row in envelope.get('items', [])
                    if row.get('name') in ('embedder', 'model index')}
            require(len(rows) == 2, 'doctor_model_rows_missing')
            return rows

        def wait_index(want, seconds=90):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if want == 'ready':
                    # Exercise the existing repair while TUI-style reads continue.
                    # These reads must not restart model verification or indexing.
                    for command in (['account', 'list'], ['config'], ['agent-config']):
                        if self.args.runtime_binary:
                            self.direct(command)
                        else:
                            self.rpc(command)
                index = self.rpc(['status'])['summary']['retrieval_index']
                states = ('idle', 'scheduled', 'running', 'waiting_model', 'ready', 'missing', 'failed', 'paused', 'stopped')
                self.report['doctor_last_index'] = {
                    'expected': want, 'state': index.get('state') if index.get('state') in states else 'unknown',
                    **{key: index.get(key) if type(index.get(key)) is bool else None
                       for key in ('scheduled', 'running', 'worker_active')},
                }
                if index.get('state') == want:
                    return index
                time.sleep(.2)
            raise Failure('doctor_index_' + want + '_timeout')

        def host_result(result):
            require(result.returncode in (0, 1, 2), 'host_doctor_repair_failed')
            try:
                envelope = json.loads(result.stdout.strip().splitlines()[-1])
            except (ValueError, IndexError):
                raise Failure('host_doctor_repair_envelope_invalid') from None
            require(isinstance(envelope, dict), 'host_doctor_repair_envelope_invalid')
            return envelope

        try:
            self.start()
            if self.args.runtime_binary:
                # Runtime verifies assets; the host owns downloads and Paused recovery.
                wait_index('missing')
                missing = doctor()
                require(missing['embedder']['status'] == 'fail'
                        and missing['model index']['status'] == 'warn'
                        and not rejected_requests and not requests, 'doctor_missing_assets_not_visible')
                command = [str(self.args.binary), '--json', 'doctor', '--fix']
                failed = subprocess.run(command, env=self.env, cwd=self.root, capture_output=True, timeout=90)
                failure = host_result(failed)
                marker = self.root / 'runtime/autostart/maintenance.json'
                require(failed.returncode != 0 and failure.get('status') == 'fail'
                        and '503' in json.dumps(failure) and bool(rejected_requests)
                        and not requests and all(not (model / name).exists() for name in expected_assets),
                        'doctor_download_error_hidden')
                require(marker.is_file() and not marker.is_symlink()
                        and json.loads(marker.read_bytes()).get('phase') == 'Paused', 'doctor_failure_not_paused')
                self.stop()
                require(snapshot() == before and (self.library / 'session.json').read_bytes() == session,
                        'doctor_failed_repair_changed_source_or_session')
                self.passed('doctor_model_failed_download_visible', http_status=503, maintenance='Paused')
                # Recover the failed transaction explicitly before starting another write.
                rejected_count = len(rejected_requests)
                self.direct(['runtime', 'restart'])
                require(not marker.exists() and len(rejected_requests) == rejected_count
                        and not requests, 'doctor_paused_recovery_failed')
                self.start()
                wait_index('missing')
                allowed.set()
                repair_deadline = time.monotonic() + 90
                repair = subprocess.Popen(command, env=self.env, cwd=self.root,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                require(started.wait(45) and repair.poll() is None, 'doctor_retry_did_not_download')
                self.passed('doctor_model_failed_download_retry', synchronous=True, paused_recovered=True)
                paused = marker.read_bytes()
                require(json.loads(paused).get('phase') == 'Paused', 'doctor_active_repair_not_paused')
                busy = subprocess.run(command, env=self.env, cwd=self.root, capture_output=True,
                                      timeout=max(.1, repair_deadline - time.monotonic()))
                busy_result = host_result(busy)
                require(busy.returncode != 0 and busy_result.get('status') == 'fail'
                        and any(word in json.dumps(busy_result).lower() for word in ('maintenance', 'paused'))
                        and marker.read_bytes() == paused and repair.poll() is None,
                        'doctor_competing_repair_not_fenced')
            else:
                wait_index('failed')
                failed = doctor()
                require(all(row['status'] == 'fail' for row in failed.values())
                        and '503' in str(failed['model index'].get('value')), 'doctor_download_error_hidden')
                self.passed('doctor_model_failed_download_visible', http_status=503)
                allowed.set()
                scheduled = doctor(True)
                require(all(row['status'] == 'warn' for row in scheduled.values()), 'doctor_retry_not_scheduled')
                require(started.wait(45), 'doctor_retry_did_not_download')
                self.passed('doctor_model_failed_download_retry')
                busy = doctor(True)
                require(all(row['status'] == 'warn' for row in busy.values()), 'doctor_competing_installer_started')
            # One ignored Range probe and one blocked transfer, with no competing installer.
            model_requests = [range_header for name, range_header in requests
                              if name == 'onnx/model_quantized.onnx']
            require(model_requests == ['bytes=0-0', None], 'doctor_competing_installer_started')
            release.set()
            if repair is not None:
                stdout, _ = repair.communicate(timeout=max(.1, repair_deadline - time.monotonic()))
                repaired = host_result(subprocess.CompletedProcess(command, repair.returncode, stdout))
                require(repair.returncode in (0, 2) and repaired.get('command') == 'doctor'
                        and repaired.get('status') != 'fail' and not marker.exists(), 'host_doctor_repair_not_completed')
                # Host maintenance replaces the owner; resume the fixture's verified child.
                self.stop()
                self.start()
            wait_index('ready', 240)
            ready = doctor()
            require(all(row['status'] == 'ok' for row in ready.values()), 'doctor_ready_model_not_pass')
            require([header for name, header in requests if name == 'onnx/model_quantized.onnx']
                    == model_requests, 'doctor_competing_installer_started')
            self.passed('doctor_model_active_download_no_duplicate', model_downloads=1, range_probes=1)
            require(asset_digests(model) == expected_assets, 'doctor_repaired_asset_digest_mismatch')
            self.passed('doctor_model_repair_ready', asset_digests=expected_assets)
            require(snapshot() == before and (self.library / 'session.json').read_bytes() == session,
                    'doctor_repair_changed_source_or_session')
            recovered = self.rpc(['recall', 'Runtime fixture persisted memory.', '--titles'])['details']
            require([row['id'] for row in recovered] == candidate_ids, 'doctor_repair_changed_candidates')
            self.passed('doctor_model_repair_preserves_source', candidates_unchanged=True)
        finally:
            release.set()
            self.report['doctor_repair_cleanup'] = {'passed': False}
            try:
                try:
                    if repair is not None and repair.poll() is None:
                        try:
                            repair.communicate(timeout=max(.1, repair_deadline - time.monotonic()))
                        except subprocess.TimeoutExpired:
                            repair.kill()
                            repair.communicate(timeout=5)
                            raise Failure('doctor_owned_repair_cleanup_timeout') from None
                finally:
                    self.stop()
                    if self.args.runtime_binary:
                        self.direct(['--runtime-internal', '--stop'])
                self.report['doctor_repair_cleanup']['passed'] = True
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
                if previous_mirror is None:
                    self.env.pop('RSRS_MIRROR', None)
                else:
                    self.env['RSRS_MIRROR'] = previous_mirror


    def validation_errors(self):
        entry = self.direct(['remember', 'Owned validation fixture.', '--title', 'Validation leaf',
                             '--force', '--importance', 'important'])['summary']['id']
        commands = [
            ['resort', '--spec', '{"ops":[{"id":"aaaaaaaa","parent":"aaaaaaaa"}]}'],
            ['resort', '--spec', '{'], ['resort', '--spec', '{"ops":[]}'],
            ['split', entry, '--go', '--spec', '{"items":[]}'],
            ['split', entry, '--go', '--spec', '{'], ['tree-deepen', '--root', entry],
        ]
        for command in commands:
            result = subprocess.run([str(self.args.binary), '--direct', *command, '--json'],
                                    env=self.env, cwd=self.root, capture_output=True, text=True, timeout=90)
            require(result.returncode == 2, 'direct_validation_exit_not_user_error')
            value = json.loads(result.stdout.strip())
            require(value.get('status') == 'fail', 'direct_validation_status_not_fail')
            require(value['summary'].get('reason') == 'invalid_input'
                    and value['details'].get('error_type') == 'user', 'direct_validation_error_not_typed')
        self.start()
        try:
            for command in commands:
                value = self.rpc(command, expected=2)
                require(value['summary'].get('reason') == 'invalid_input'
                        and value['details'].get('error_type') == 'user', 'rpc_validation_error_not_typed')
        finally:
            self.stop()
        self.passed('runtime_validation_errors_typed', direct_cases=len(commands), rpc_cases=len(commands))

    def host_space_commands(self):
        home = self.root / 'space-command-home'
        library = home / '.rsrs'
        home.mkdir(mode=0o700)
        library.mkdir(mode=0o700)
        config = library / 'client.json'
        config.write_text(json.dumps({'data_dir': str(library), 'api_base': 'https://fixture.invalid',
                                      'custom': 'preserve'}), encoding='utf-8')
        env = self.env.copy()
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        url = 'http://127.0.0.1:' + str(port)
        env.update(HOME=str(home), USERPROFILE=str(home), RSRS_DATA_DIR=str(library),
                   RSRS_RPC_PORT=str(port), XDG_RUNTIME_DIR=str(home / 'runtime'))
        install_published_runtime(self.args.binary, env)
        def cli(args, expected=0):
            result = subprocess.run([str(self.args.binary), '--json', *args], env=env, cwd=home,
                                    capture_output=True, text=True, timeout=90)
            require(result.returncode == expected, 'space_host_cli_exit_mismatch')
            value = json.loads(result.stdout.strip())
            require(value['status'] == ('ok' if expected == 0 else 'fail'), 'space_host_cli_status_mismatch')
            return value
        def health(profile):
            code, actual, _ = self.http(url, 'GET', '/api/health')
            if self.args.runtime_binary:
                require(code == 200, 'space_host_health_failed')
                verify_runtime_health(self.args, actual, library, profile)
            else:
                require(code == 200 and actual['bin'] == self.args.version
                    and hashlib.sha256(Path(actual['exe']).read_bytes()).hexdigest() == self.args.binary_sha256
                    and Path(actual['data_dir']).resolve() == profile.resolve(), 'space_host_runtime_readback_failed')
            return actual['pid']
        try:
            cli(['space', 'list'])
            # An existing Absent target cannot prove store readiness. Use a real
            # keygen-Unbound profile, whose verified switch deliberately stays idle.
            cli(['keygen'])
            cli(['runtime', 'start'])
            old_pid = health(library)
            cli(['space', 'create', 'probe'])
            probe = library / 'accounts/probe'
            require(Path(json.loads(config.read_text(encoding='utf-8'))['data_dir']).resolve() == probe.resolve(),
                    'space_create_did_not_select_profile')
            with socket.socket() as listener:
                listener.settimeout(2)
                require(listener.connect_ex(('127.0.0.1', port)) != 0, 'space_create_did_not_stop_runtime')
            cli(['runtime', 'start'])
            require(probe.joinpath('space_owner.json').is_file() and health(probe) != old_pid,
                    'space_create_did_not_take_over_runtime')
            before = config.read_bytes()
            denied = cli(['space', 'remove', 'probe'], 2)
            require(denied['summary']['reason'] == 'invalid_input' and config.read_bytes() == before,
                    'space_remove_without_confirmation_changed_profile')
            cli(['space', 'remove', 'probe', '--yes'], 2)
            require(probe.is_dir(), 'space_removed_active_profile')
            health(probe)
            cli(['space', 'use', 'main'])
            with socket.socket() as listener:
                listener.settimeout(2)
                require(listener.connect_ex(('127.0.0.1', port)) != 0,
                        'space_unbound_switch_started_runtime')
            cli(['runtime', 'start'])
            health(library)
            before = config.read_bytes()
            cli(['space', 'use', 'missing'], 2)
            require(config.read_bytes() == before, 'space_failed_switch_changed_configuration')
            health(library)
            cli(['space', 'remove', 'probe', '--yes'])
            require(not probe.exists(), 'space_remove_did_not_remove_profile')
            health(library)
            preserved = json.loads(config.read_text(encoding='utf-8'))
            require(preserved['api_base'] == 'https://fixture.invalid' and preserved['custom'] == 'preserve',
                    'space_host_command_lost_configuration')
        finally:
            cli(['--runtime-internal', '--stop'])
            with socket.socket() as listener:
                listener.settimeout(2)
                require(listener.connect_ex(('127.0.0.1', port)) != 0, 'space_fixture_runtime_not_closed')
        self.passed('host_space_lifecycle', runtime_cleanup=True, configuration_preserved=True)

    def update_check_comparisons(self):
        previous = self.env.get('RSRS_UPDATE_CHECK')
        self.env['RSRS_UPDATE_CHECK'] = '1'
        cache = self.root / 'update_check.json'
        try:
            for latest, ahead, outdated in [('0.0.1', True, False), (self.args.version, False, False),
                                             ('99.0.0', False, True)]:
                cache.write_text(json.dumps({'checked_at': int(time.time()), 'latest': latest, 'ok': True}),
                                 encoding='utf-8')
                result = subprocess.run([str(self.args.binary), '--direct', '--json', 'update-check'],
                                        env=self.env, cwd=self.root, capture_output=True, text=True, timeout=30)
                require(result.returncode == (2 if outdated else 0), 'update_check_exit_mismatch')
                value = json.loads(result.stdout.strip())
                require(value['summary'].get('ahead') is ahead and value['summary'].get('outdated') is outdated
                        and value['summary'].get('cached') is True, 'update_comparison_state_wrong')
                display = value['items'][0]['value']
                require((' -> ' in display) is outdated and ('ahead of published latest' in display) is ahead
                        and bool(value['actions']) is outdated, 'update_comparison_offers_downgrade')
        finally:
            cache.unlink(missing_ok=True)
            if previous is None:
                self.env.pop('RSRS_UPDATE_CHECK', None)
            else:
                self.env['RSRS_UPDATE_CHECK'] = previous
        self.passed('update_check_comparison', states=3)
