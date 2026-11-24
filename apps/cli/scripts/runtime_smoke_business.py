"""Dashboard, accounts, scoped transport and Runtime settings smoke checks."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
import threading
from native_fixture_process import verify_runtime_health
from native_fixture_process import scoped_rpc_request
from native_fixture_process import verify_rpc_receipt
from runtime_smoke_failures import DEV
from runtime_smoke_failures import DASH
from runtime_smoke_failures import Failure
from runtime_smoke_failures import client_only_diagnostic
from runtime_smoke_failures import require


class RuntimeBusiness:
    def dashboard(self):
        endpoint = self.root / 'runtime' / 'endpoint.json'
        require(not endpoint.exists(), 'runtime_existed_before_dashboard')
        result = self.direct(['web', '--no-open'])
        require(result['summary'] == {'url': DASH, 'opened': False} and not endpoint.exists(),
                'dashboard_started_runtime_or_wrong_url')
        self.passed('dashboard_url_without_runtime')
        browser = self.root / 'bin' / 'xdg-open'
        capture = self.root / 'browser-url.txt'
        browser.write_text('#!' + sys.executable + '\nimport pathlib,sys\n'
                           + 'pathlib.Path(' + repr(str(capture)) + ').write_text(sys.argv[1],encoding="utf-8")\n', encoding='utf-8')
        browser.chmod(0o700)
        self.env['PATH'] = str(browser.parent) + os.pathsep + self.env.get('PATH', '')
        opened = self.direct(['web'])
        deadline = time.monotonic() + 5
        while not capture.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        require(opened['summary'] == {'url': DASH, 'opened': True}
                and capture.is_file() and capture.read_text(encoding='utf-8') == DASH and not endpoint.exists(),
                'dashboard_browser_launch_failed')
        self.passed('dashboard_browser_launch_verified', method='actual_cli_with_owned_os_browser_handler')
        for flag in ('--status', '--stop', '--internal', '--port', '--host'):
            result = subprocess.run([str(self.args.binary), 'web', flag], env=self.env, cwd=self.root,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
            require(result.returncode != 0 and not endpoint.exists(), 'legacy_web_flag_was_accepted')
        self.passed('dashboard_legacy_flags_rejected')
        for host, case_id in (('0.0.0.0', 'nonloopback_unspecified_ipv4'),
                              ('192.0.2.1', 'nonloopback_documentation_ipv4'),
                              ('example.invalid', 'nonloopback_dns')):
            self.nonloopback(host, case_id)
        self.passed('runtime_nonloopback_rejected')
        client_home = self.root / 'client-only-home'
        old = client_home / '.onememory'
        old.mkdir(parents=True, mode=0o700)
        marker = old / 'client.json'
        marker.write_text('{"addr":"https://api-dev.rsrs.rs"}\n', encoding='utf-8')
        before = {str(path.relative_to(client_home)): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in client_home.rglob('*') if path.is_file()}
        client_env = dict(self.env, HOME=str(client_home), USERPROFILE=str(client_home),
                          RSRS_CLIENT_ONLY='1')
        client_env.pop('RSRS_DATA_DIR', None)
        client_env.pop('RSRS_DEFAULT_DATA_DIR', None)
        result = subprocess.run([str(self.args.binary), '--client-only', '--json', 'status'],
                                env=client_env, cwd=self.root, capture_output=True, timeout=25)
        after = {str(path.relative_to(client_home)): hashlib.sha256(path.read_bytes()).hexdigest()
                 for path in client_home.rglob('*') if path.is_file()}
        diagnostic = client_only_diagnostic(result, before, after,
                                            (client_home / '.rsrs').exists(), endpoint.exists())
        if diagnostic['failed_checks']:
            self.report['failure_diagnostic'] = diagnostic
            raise Failure('client_only_migrated_or_created_profile')
        self.passed('client_only_no_profile_writes')

    def account(self):
        expected = os.environ.get('RSRS_DEV_SERVER_SHA', '')
        require(os.environ.get('RSRS_DEV_SERVER_ADDR') == DEV and re.fullmatch('[0-9a-f]{40}', expected),
                'exact_development_server_required')
        code, health, _ = self.http(DEV, 'GET', '/health')
        require(code == 200 and isinstance(health, dict) and health.get('source_revision') == expected,
                'development_server_revision_mismatch')
        self.report['server_sha'] = expected
        if self.args.runtime_binary:
            # Only the host's creation transaction can select a fresh empty profile without a store.
            self.library = self.root / 'accounts/runtime-smoke'
            created = self.direct(['space', 'create', 'runtime-smoke'], case_id='account_profile_create')
            require(Path(created.get('details', {}).get('dir', '')).resolve() == self.library,
                    'owned_account_profile_mismatch')
        self.direct(['config', '--data-dir', str(self.library), '--addr', DEV, '--autosync', 'false', '--cure-auto', 'false'],
                    case_id='account_config_set')
        config = self.direct(['config'], case_id='account_config_read')['summary']
        require(Path(config.get('data_dir', '')).resolve() == self.library and config.get('addr') == DEV,
                'data_dir_configuration_failed')
        self.passed('data_dir_persisted')
        installed = self.direct(['model', 'install-m3'], timeout=1200, case_id='account_model_install')
        probe = installed['details']['backend_load']['evidence']
        require(probe.get('ready') is True and probe.get('inference_completed') is True
                and probe.get('fresh_session') is True and probe.get('dimensions') == 1024
                and str(probe.get('selected', '')).lower() == 'cpu', 'real_bge_cpu_probe_failed')
        self.attempted = True
        registered = self.direct(['register', '--user', self.user, '--pass=' + self.password, '--addr', DEV],
                                 case_id='account_register')
        require(registered['summary'].get('user') == self.user, 'owned_account_registration_failed')
        probe = self.direct(['model', 'probe', '--model', 'm3'], timeout=180, case_id='account_model_probe')['summary']
        require(probe.get('ready') is True and probe.get('dimensions') == 1024
                and str(probe.get('selected', '')).lower() == 'cpu', 'real_bge_cpu_probe_failed')
        self.start()

    def transport(self):
        self.passed('hidden_runtime_health_identity')
        status = self.direct(['--runtime-internal', '--status'])['summary']
        require(status.get('state') == 'up' and status.get('pid') == self.child.pid, 'hidden_status_identity_failed')
        self.passed('hidden_runtime_status_up')
        for token, name in ((None, 'runtime_loopback_without_token'), ('ci-invalid-token', 'runtime_loopback_ignores_token')):
            code, value, _ = self.http(self.url, 'GET', '/api/health', token=token)
            if self.args.runtime_binary:
                require(code == 200, 'runtime_loopback_health_status_failed')
                verify_runtime_health(self.args, value, self.root, self.library, self.child.pid)
            else:
                require(code == 200 and value.get('v') == 1 and value.get('bin') == self.args.version
                    and value.get('pid') == self.child.pid
                    and Path(value.get('exe', '')).resolve() == self.args.binary
                    and Path(value.get('data_dir', '')).resolve() == self.library
                    and not (self.root / 'runtime' / 'token').exists(), 'runtime_loopback_health_identity_failed')
            self.passed(name)
        code, value, _ = self.http(self.url, 'POST', '/api/rpc', {}, self.token, {'Origin': 'https://example.invalid'})
        require(code == 403 and value.get('error') == 'forbidden origin', 'runtime_foreign_origin_not_rejected')
        self.passed('runtime_origin_rejected')
        code, value, _ = self.http(self.url, 'POST', '/api/rpc', {'v': 999, 'id': 'bad-version', 'method': 'runtime.status', 'args': []}, self.token)
        require(code == 200 and value.get('ok') is False and value.get('code') == 'protocol_version_mismatch', 'rpc_version_not_rejected')
        self.passed('runtime_rpc_version_rejected')
        if self.args.runtime_binary:
            rejected = self.rpc(['config'], expected=2)
            require(rejected['details'].get('error_type') == 'host_boundary', 'host_config_not_fenced')
            config = self.direct(['config'])['summary']
        else:
            config = self.rpc(['config'])['summary']
        require(Path(config.get('data_dir', '')).resolve() == self.library and config.get('addr') == DEV, 'rpc_config_mismatch')
        self.passed('runtime_rpc_config_verified', host_boundary_rejected=bool(self.args.runtime_binary),
                    config_owner='host_cli' if self.args.runtime_binary else 'legacy_shim')
        require(self.rpc(['status'])['summary'].get('unlocked') is True, 'runtime_not_unlocked')
        self.passed('runtime_rpc_status_unlocked')
        for method, ident, name in (('initialize', 1, 'runtime_mcp_initialized'), ('tools/list', 2, 'runtime_mcp_tools_list')):
            body = {'jsonrpc': '2.0', 'id': ident, 'method': method}
            if method == 'initialize':
                body['params'] = {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'runtime-ci', 'version': '1'}}
            code, value, _ = self.http(self.url, 'POST', '/mcp', body, self.token)
            require(code == 200 and value.get('id') == ident and 'error' not in value, 'mcp_response_failed')
            if method == 'initialize':
                require(value.get('result', {}).get('protocolVersion') == '2025-03-26', 'mcp_protocol_mismatch')
            else:
                tools = value.get('result', {}).get('tools', [])
                require(isinstance(tools, list) and any(tool.get('name') == 'memory_status' for tool in tools), 'mcp_tools_missing')
            self.passed(name)
        for path in ('/', '/index.html', '/favicon.ico', '/fonts/example.ttf', '/api/invoke', '/api/task?id=old'):
            code, value, _ = self.http(self.url, 'POST' if path == '/api/invoke' else 'GET', path,
                                      {} if path == '/api/invoke' else None, self.token)
            require(code == 404 and value.get('error') == 'unknown runtime endpoint', 'removed_ui_route_still_served')
        self.passed('removed_ui_routes_not_served', routes=6)

    def agent_config(self, args):
        if not self.args.runtime_binary:
            return self.rpc(['agent-config', *args])
        # This is a host-owned profile change, not a fallback after an RPC failure.
        self.stop()
        value = self.direct(['agent-config', *args])
        mode = self.direct(['agent-config'])['summary']['memory_off']
        require(type(mode) is bool, 'host_memory_off_state_invalid')
        self.start(unlock=not mode)
        return value

    def settings(self):
        for mode in ('verbose', 'concise'):
            self.agent_config(['--set', 'diary_mode=' + mode])
            require(self.agent_config([])['summary'].get('diary_mode') == mode, 'diary_mode_not_persisted')
        self.passed('diary_mode_persisted')
        value = self.rpc(['remember', 'Runtime fixture persisted memory.', '--title', 'Runtime fixture', '--importance', 'important', '--force'])
        memory_id = value['summary'].get('id')
        require(isinstance(memory_id, str) and memory_id, 'owned_memory_not_created')
        self.agent_config(['--set', 'readonly=true'])
        rejected = self.rpc(['remember', 'Readonly write must fail.', '--importance', 'important', '--force'], expected=1)
        require('read-only' in ' '.join(rejected.get('errors', [])), 'readonly_write_guard_missing')
        require(self.rpc(['show', memory_id])['details'].get('entry', {}).get('id') == memory_id, 'readonly_read_failed')
        self.agent_config(['--set', 'readonly=false'])
        self.agent_config(['--set', 'memory_off=true'])
        if self.args.runtime_binary:
            code, health, _ = self.http(self.url, 'GET', '/api/health')
            require(code == 200, 'off_runtime_health_failed')
            verify_runtime_health(self.args, health, self.root, self.library, self.child.pid)
            require(health.get('store_ready') is False, 'off_runtime_store_unlocked')
            request = scoped_rpc_request(health, ['--json', 'show', memory_id])
            code, reply, _ = self.http(self.url, 'POST', '/api/rpc', request, self.token)
            verify_rpc_receipt(request, reply)
            require(code == 200 and reply.get('ok') is False and reply.get('exit') == 1
                    and reply.get('code') == 'runtime_locked', 'off_runtime_read_not_locked')
            skipped = self.direct(['show', memory_id])
        else:
            skipped = self.rpc(['show', memory_id])
        require(skipped.get('summary') == {'mode': 'off', 'skipped': True, 'count': 0}
                and skipped.get('items') == [] and skipped.get('details') == []
                and skipped.get('errors') == [], 'off_read_empty_response_missing')
        self.agent_config(['--set', 'memory_off=false'])
        normal = self.agent_config([])['summary']
        require(normal.get('readonly') is False and normal.get('memory_off') is False, 'normal_mode_not_restored')
        require(self.rpc(['show', memory_id])['details']['entry']['content'] == 'Runtime fixture persisted memory.', 'normal_read_not_restored')
        self.passed('workspace_three_states_persisted')
        self.stop()
        self.start()
        require(self.rpc(['status'])['summary'].get('unlocked') is True
                and self.rpc(['show', memory_id])['details']['entry']['content'] == 'Runtime fixture persisted memory.',
                'stored_session_not_resumed')
        self.passed('resume_session_real', method='fresh_runtime_reuses_owned_session_and_keyring_no_password_input')
        # Remember persists before deriving its index. Exercise recall and wait
        # for the owned worker before stopping it for strict offline diagnosis.
        require(self.rpc(['recall', 'Runtime fixture persisted memory.', '--titles']).get('items'),
                'resumed_pending_source_not_recalled')
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            code, health, _ = self.http(self.url, 'GET', '/api/health', token=self.token)
            require(code == 200, 'owned_index_health_unavailable')
            index = health.get('retrieval_index', {})
            require(index.get('state') != 'failed', 'owned_index_preparation_failed')
            if index.get('state') == 'ready' and not index.get('running') and not index.get('scheduled'):
                break
            time.sleep(.1)
        else:
            raise Failure('owned_index_not_ready_before_offline_doctor')
        self.stop()
        retired = subprocess.run([str(self.args.binary), '--direct', '--json', 'model', 'install-rerank'], env=self.env,
                                 cwd=self.root, capture_output=True, text=True, timeout=30)
        require(retired.returncode == 2, 'retired_reranker_command_was_accepted')
        agent_file = self.root / 'home/.codex/AGENTS.md'
        agent_file.parent.mkdir()
        original = '# Disposable runtime fixture\nKeep this text.\n'
        agent_file.write_text(original, encoding='utf-8')
        self.direct(['inject', '--id', 'codex'])
        installed = agent_file.read_text(encoding='utf-8')
        require(installed != original and original.strip() in installed,
                'owned_agent_injection_not_applied')
        self.direct(['inject', '--id', 'generic'])
        # Keep doctor strict: a fresh supported agent must be actually injected.
        result = subprocess.run([str(self.args.binary), '--direct', '--json', 'doctor'], env=self.env,
                                cwd=self.root, capture_output=True, text=True, timeout=180)
        self.report['doctor'] = {'exit': result.returncode, 'items': []}
        try:
            diagnostic = json.loads(result.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            diagnostic = None
        if isinstance(diagnostic, dict) and isinstance(diagnostic.get('items'), list):
            names = {'store', 'data dir', 'mcp bin', 'mcp http', 'session', 'embedder',
                     'lock', 'remote', 'inject', 'memory status',
                     'tidy counter', 'CLI version', 'Runtime version', 'model index'}
            statuses = {'ok', 'warn', 'fail', 'skip', 'pending'}
            self.report['doctor']['items'] = [
                {'name': item['name'], 'status': item['status']}
                for item in diagnostic['items']
                if isinstance(item, dict) and isinstance(item.get('name'), str)
                and item['name'] in names and isinstance(item.get('status'), str)
                and item['status'] in statuses
            ]
        require(result.returncode in (0, 2), 'm3_doctor_execution_failed')
        require(isinstance(diagnostic, dict) and isinstance(diagnostic.get('items'), list),
                'doctor_envelope_invalid')
        require(any(item.get('name') == 'inject' and item.get('status') == 'ok'
                    for item in diagnostic.get('items', [])), 'owned_agent_doctor_not_fresh')
        require(not any(item.get('name') == 'reranker' for item in diagnostic.get('items', [])), 'retired_reranker_doctor_entry_present')
        require(any(item.get('name') == 'embedder' and item.get('status') == 'ok' for item in diagnostic.get('items', [])), 'm3_doctor_not_ready')
        self.passed('m3_status_real', retired_command_exit=retired.returncode)
        self.provider_keys.reserve('classify:last-backend')
        self.direct(['classify-config', '--set', '--backend', 'jev'])
        require(self.direct(['classify-config'])['summary'].get('backend') == 'jev', 'backend_not_persisted')
        self.passed('classify_config_persisted')
        observed = []
        class Trap(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                observed.append(True)
                self.send_error(503)
            do_GET = do_POST
        trap = ThreadingHTTPServer(('127.0.0.1', 0), Trap)
        thread = threading.Thread(target=trap.serve_forever, daemon=True)
        thread.start()
        try:
            base = 'http://127.0.0.1:' + str(trap.server_port) + '/v1'
            for slot in ('classify:ds-last-base', f'classify:ds@127.0.0.1:{trap.server_port}',
                         f'classify:ds-model@127.0.0.1:{trap.server_port}'):
                self.provider_keys.reserve(slot)
            value = self.direct(['classify-config', '--set', '--backend', 'ds', '--api-base', base,
                                 '--model', 'ci-unused-model', '--key-stdin'], stdin='ci-unused-runtime-key\n')['summary']
            readback = self.direct(['classify-config'])['summary']
            require(value.get('has_key') is True and readback.get('backend') == 'ds'
                    and readback.get('api_base') == base and readback.get('model') == 'ci-unused-model'
                    and not observed, 'provider_key_config_or_network_guard_failed')
            self.passed('ds_key_saved_without_network', provider_requests=0)
        finally:
            trap.shutdown()
            trap.server_close()
            thread.join(timeout=5)
        self.start()
        self.stop()
        require(self.child is None, 'runtime_not_stopped')
        self.passed('hidden_runtime_stop_clean')

    def cleanup(self):
        try:
            self.stop()
        except Exception:
            self.cloud_clean = False
        try:
            if self.child is not None and self.child.poll() is None:
                self.child.terminate()
                try:
                    self.child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.child.kill()
                    self.child.wait(timeout=5)
        except Exception:
            # Preserve teardown failure, but still attempt exact credential and
            # cloud cleanup. The outer wrapper also owns process containment.
            self.cloud_clean = False
        if self.attempted:
            try:
                session = json.loads((self.library / 'session.json').read_text(encoding='utf-8'))
                require(session.get('user') == self.user and session.get('addr') == DEV, 'cleanup_identity_mismatch')
                token = session['token']
                code, result, _ = self.http(DEV, 'POST', '/api/self/purge', {'confirm': self.user}, token)
                require(code == 200 and result.get('purged') is True, 'cloud_purge_failed')
                code, _, _ = self.http(DEV, 'GET', '/api/self', token=token)
                require(code == 401, 'old_account_token_not_revoked')
            except Exception:
                self.cloud_clean = False
        if self.provider_keys is not None:
            try:
                self.provider_clean = self.provider_keys.cleanup()
            except Exception:
                self.provider_clean = False
        self.report['provider_credential_cleanup'] = {'passed': self.provider_clean}
        self.report['cloud_cleanup'] = {'passed': self.cloud_clean,
                                        'remaining_users': [] if self.cloud_clean else [self.user] if self.attempted else []}
