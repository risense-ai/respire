#!/usr/bin/env python3
"""Published Runtime/dashboard checks on an ephemeral GitHub-hosted Linux runner.

Uses a fresh owned HOME, models and development account. Reports never contain
credentials, command arguments, plaintext responses or raw subprocess logs.
Legacy mode is explicitly separate from standalone Runtime evidence; neither is G1.
"""
import argparse
import ctypes
import multiprocessing
import signal
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
from native_fixture_process import (install_published_runtime, add_runtime_arguments, runtime_inputs, runtime_report,
                                    verify_runtime_health, scoped_rpc_request, verify_rpc_receipt, RuntimeHealthFailure)


REQUIRED = json.loads(Path(__file__).with_name('dev-runtime-required.json').read_text(encoding='utf-8'))['required_cases']


DIRECT_CASE_IDS = frozenset({'owned_cli', 'nonloopback_unspecified_ipv4',
                             'nonloopback_documentation_ipv4', 'nonloopback_dns',
                             'account_profile_create', 'account_config_set', 'account_config_read',
                             'account_model_install', 'account_register', 'account_model_probe',
                             'runtime_prestart_stop', 'runtime_host_unlock'})
CLI_REASONS = frozenset({'invalid_input', 'runtime_error', 'request_outcome_unknown'})

UNLOCK_ERRORS = (
    ('Runtime is not ready for V2 credential delivery', 'credential_delivery_not_ready'),
    ('unlock Runtime does not own the selected profile', 'owner_profile_mismatch'),
    ('unlock Runtime process identity changed', 'owner_process_changed'),
    ('unlock Runtime image changed', 'owner_image_changed'),
    ('Runtime session credentials are unavailable; library remains locked', 'session_file_unavailable'),
    ('host session credential read failed (', 'host_session_credential_read_failed'),
    ('Runtime session key is unavailable; library remains locked', 'session_key_unavailable'),
    ('Runtime V2 unlock transport failed (', 'unlock_transport_failed'),
    ('invalid Runtime V2 unlock receipt', 'unlock_receipt_invalid'),
    ('Runtime unlock receipt schema mismatch:', 'unlock_receipt_schema_mismatch'),
    ('Runtime V2 unlock receipt reports a locked library', 'unlock_receipt_locked'),
    ('Runtime provider credential receipt mismatch:', 'provider_receipt_mismatch'),
)


def exit_diagnostic(case_id, expected, result):
    reason = 'unknown'
    unlock_error = None
    try:
        value = json.loads(result.stdout.strip().splitlines()[-1])
        candidate = value.get('summary', {}).get('reason')
        if type(candidate) is str and candidate in CLI_REASONS:
            reason = candidate
        if case_id == 'runtime_host_unlock' and isinstance(value.get('errors'), list):
            for error in value['errors']:
                if not isinstance(error, str):
                    continue
                http = re.match(r'Runtime V2 unlock rejected \(HTTP ([1-5][0-9]{2})\);', error)
                if http:
                    unlock_error = {'code': 'unlock_http_rejected', 'http_status': int(http[1])}
                    break
                code = next((code for prefix, code in UNLOCK_ERRORS if error.startswith(prefix)), None)
                if code:
                    unlock_error = {'code': code}
                    break
    except (ValueError, IndexError, AttributeError, TypeError):
        pass
    diagnostic = {'case_id': case_id if type(case_id) is str and case_id in DIRECT_CASE_IDS else 'owned_cli',
                  'expected_exit': expected if type(expected) is int else None,
                  'actual_exit': result.returncode if type(result.returncode) is int else None,
                  'structured_reason': reason}
    if case_id == 'runtime_host_unlock':
        diagnostic['first_unlock_error'] = unlock_error or {'code': 'unclassified'}
    return diagnostic


from runtime_smoke_failures import DEV, DASH, Failure, client_only_diagnostic, require


def private_fixture_bus(env):
    bus = env.get('DBUS_SESSION_BUS_ADDRESS', '')
    if (sys.platform.startswith('linux') and env.get('RSRS_SS_FIXTURE_BUS') == 'private'
            and re.fullmatch(r'unix:(?:path|abstract)=[^,;\s]+(?:,guid=[0-9a-f]{32})?', bus)):
        return bus
    return None


class RuntimeProviderKeys:
    """Exact absent-before-write provider slots on a dedicated wrapper-owned bus.

    Login aliases stay alive for resume_session_real and are cleaned by the
    owning wrapper. Never enumerate/delete host credentials or read secrets.
    """
    def __init__(self, env):
        require(private_fixture_bus(env) is not None
                and private_fixture_bus(env) == private_fixture_bus(os.environ),
                'private_runtime_fixture_bus_required')
        import secretstorage
        from jeepney import DBusAddress, new_method_call
        self.connection = secretstorage.dbus_init()
        self.address = DBusAddress('/org/freedesktop/DBus', 'org.freedesktop.DBus',
                                   'org.freedesktop.DBus')
        self.new_method_call = new_method_call
        self.owner = self.current_owner()
        self.collection = secretstorage.get_collection_by_alias(self.connection, 'default')
        require(not self.collection.is_locked() and not list(self.collection.get_all_items()),
                'runtime_fixture_collection_not_empty_or_locked')
        self.slots = set()

    def current_owner(self):
        return self.connection.send_and_get_reply(self.new_method_call(
            self.address, 'GetNameOwner', 's', ('org.freedesktop.secrets',))).body

    def owner_at_failure(self):
        try:
            check = self.new_method_call(self.address, 'NameHasOwner', 's', ('org.freedesktop.secrets',))
            if self.connection.send_and_get_reply(check, timeout=2).body == (False,):
                return 'missing'
            lookup = self.new_method_call(self.address, 'GetNameOwner', 's', ('org.freedesktop.secrets',))
            owner = self.connection.send_and_get_reply(lookup, timeout=2).body
            return 'same' if owner == self.owner else 'changed'
        except Exception:
            return 'unavailable'

    def verify_owner(self):
        require(self.current_owner() == self.owner, 'runtime_fixture_owner_changed')

    @staticmethod
    def attributes(slot):
        return {'service': 'rsrs', 'target': 'default', 'username': slot,
                'application': 'rust-keyring'}

    def items(self, slot):
        self.verify_owner()
        attributes = self.attributes(slot)
        items = list(self.collection.search_items({k: attributes[k]
                     for k in ('service', 'target', 'username')}))
        require(len(items) <= 1, 'runtime_provider_slot_ambiguous')
        for item in items:
            require(all(item.get_attributes().get(k) == v for k, v in attributes.items()),
                    'runtime_provider_slot_attributes_mismatch')
        return items

    def reserve(self, slot):
        require(slot in ('classify:last-backend', 'classify:ds-last-base')
                or re.fullmatch(r'classify:ds(?:-model)?@127\.0\.0\.1:[0-9]{1,5}', slot),
                'runtime_provider_slot_not_owned')
        require(slot not in self.slots and not self.items(slot), 'runtime_provider_slot_exists')
        self.slots.add(slot)

    def cleanup(self):
        passed = True
        for slot in sorted(self.slots):
            try:
                for item in self.items(slot):
                    item.delete()
                require(not self.items(slot), 'runtime_provider_slot_not_removed')
            except Exception:
                passed = False
        return passed


from runtime_rejection_process import rejection_children, preflight_rejection_pidfd, kill_rejection_child, reap_rejection_children, rejection_supervisor, contained_rejection_run
        # Never forcibly kill a still-cleaning subreaper and abandon its children.




class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


from runtime_smoke_operations import RuntimeOperations


from runtime_smoke_business import RuntimeBusiness


class Smoke(RuntimeBusiness, RuntimeOperations):
    def __init__(self, args):
        self.args = args
        self.root = args.root.resolve()
        self.env = None
        self.child = None
        self.token = None
        self.created = False
        self.attempted = False
        self.cloud_clean = True
        self.provider_keys = None
        self.provider_clean = True
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self.user = 'ci-runtime-' + os.environ.get('GITHUB_RUN_ID', '') + '-' + secrets.token_hex(5)
        self.password = '-' + secrets.token_urlsafe(32)
        self.report = {'source_sha': args.source_sha, 'workflow_sha': os.environ.get('GITHUB_SHA'),
                       'version': args.version, 'binary_sha256': args.binary_sha256,
                       'runtime_mode': 'explicit_cli_shim', 'standalone_runtime_evidence': False,
                       'status': 'failed', 'passed': False, 'cases': {}, 'required_cases': REQUIRED,
                       'missing_cases': REQUIRED[:], 'cloud_cleanup': {'passed': False, 'remaining_users': []}}
        self.report.update(runtime_report(args, False))

    def passed(self, name, **evidence):
        require(name in REQUIRED, 'unknown_runtime_case')
        self.report['cases'][name] = {'passed': True, **evidence}

    def direct(self, args, timeout=90, stdin=None, expected=0, case_id='owned_cli'):
        try:
            result = subprocess.run([str(self.args.binary), '--direct', '--json', *args],
                                    env=self.env, cwd=self.root, input=stdin,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired):
            raise Failure('owned_cli_execution_failed') from None
        if result.returncode != expected:
            self.report['failure_diagnostic'] = exit_diagnostic(case_id, expected, result)
            if case_id == 'runtime_host_unlock':
                self.report['failure_diagnostic']['secret_service_owner'] = self.provider_keys.owner_at_failure()
            raise Failure('owned_cli_exit_mismatch')
        try:
            value = json.loads(result.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            raise Failure('owned_cli_envelope_invalid') from None
        require(isinstance(value, dict) and all(k in value for k in
                ('command', 'status', 'summary', 'items', 'actions', 'errors', 'details')),
                'owned_cli_envelope_incomplete')
        require(value['status'] in ('ok', 'skip') if expected == 0 else value['status'] == 'fail',
                'owned_cli_status_mismatch')
        return value

    def nonloopback_no_effects(self):
        # Check before and after the Runtime rejection probe.
        with socket.socket() as check:
            check.settimeout(1)
            require(check.connect_ex(('127.0.0.1', self.port)) != 0,
                    'nonloopback_runtime_left_listener')
        require(not (self.root / 'runtime/endpoint.json').exists()
                and not (self.root / 'runtime/token').exists(),
                'nonloopback_runtime_created_endpoint_or_token')

    def nonloopback(self, host, case_id):
        cases = {
            '0.0.0.0': ('nonloopback_unspecified_ipv4', 'local runtime cannot bind a non-loopback address'),
            '192.0.2.1': ('nonloopback_documentation_ipv4', 'local runtime cannot bind a non-loopback address'),
            'example.invalid': ('nonloopback_dns', 'local runtime requires a literal loopback IP or localhost'),
        }
        require(host in cases and cases[host][0] == case_id, 'nonloopback_case_invalid')
        self.nonloopback_no_effects()
        if not self.args.runtime_binary:
            # The explicitly selected legacy shim still has its CLI envelope contract.
            self.direct(['--runtime-internal', '--host', host, '--port', str(self.port)],
                        expected=1, case_id=case_id)
            self.nonloopback_no_effects()
            return
        # Verify the installed Runtime's supported unsupervised serve entry point.
        # No boot token, grant, registration, or ownership state is manufactured.
        command = [str(self.args.runtime_binary), 'serve', '--host', host, '--port', str(self.port)]
        self.nonloopback_no_effects()
        try:
            result = contained_rejection_run(command, self.env, self.root)
        except (OSError, subprocess.TimeoutExpired):
            raise Failure('nonloopback_execution_failed') from None
        finally:
            # The supervisor has already killed/reaped its exact descendants;
            # check filesystem/listener effects even after launch/timeout errors.
            self.nonloopback_no_effects()
        # Only fixed classifications, booleans and lengths may leave this process.
        self.report['failure_diagnostic'] = {
            'case_id': case_id, 'probe_id': 'runtime_host_rejection', 'expected_exit': 2,
            'actual_exit': result.returncode if type(result.returncode) is int else None,
        }
        expected_stdout = f'runtime=starting host={host} port={self.port}\n'
        expected_stderr = 'rsrs-runtime: ' + cases[host][1] + '\n'
        known_cpuid_warning = 'onnxruntime cpuid_info warning: Unknown CPU vendor. cpuinfo_vendor value: 0\n'
        known_cpuid_warning_match = result.stderr == known_cpuid_warning + expected_stderr
        stdout_match = result.stdout == expected_stdout
        stderr_match = result.stderr == expected_stderr or known_cpuid_warning_match
        self.report['failure_diagnostic'].update(
            known_cpuid_warning_match=known_cpuid_warning_match,
            stdout_expected_match=stdout_match, stderr_expected_match=stderr_match,
            stdout_utf8_bytes=len(result.stdout.encode('utf-8')), stderr_utf8_bytes=len(result.stderr.encode('utf-8')),
            output_classification='expected_output' if stdout_match and stderr_match else 'output_mismatch')
        require(type(result.returncode) is int and result.returncode == 2,
                'nonloopback_exit_mismatch')
        require(stderr_match and stdout_match, 'nonloopback_rejection_reason_mismatch')
        self.report.pop('failure_diagnostic', None)

    def http(self, base, method, path, body=None, token=None, headers=None, raw=False):
        fields = {'Content-Type': 'application/json', **(headers or {})}
        if method == 'GET' and path == '/api/health':
            # Request backend identity, not the Client2 compatibility-host view.
            fields['X-Respire-Health'] = 'runtime-v1'
        if token is not None:
            fields['Authorization'] = 'Bearer ' + token
        req = urllib.request.Request(base + path, None if body is None else json.dumps(body).encode(),
                                     fields, method=method)
        try:
            response = self.opener.open(req, timeout=90)
        except urllib.error.HTTPError as error:
            response = error
        except (OSError, urllib.error.URLError):
            raise Failure('owned_http_transport_failed') from None
        with response:
            status, response_headers = response.code, response.headers
            data = response.read(8 * 1024 * 1024 + 1)
        require(len(data) <= 8 * 1024 * 1024, 'owned_http_response_too_large')
        if raw:
            return status, data, response_headers
        try:
            return status, json.loads(data), response_headers
        except (ValueError, UnicodeError):
            raise Failure('owned_http_response_not_json') from None

    def rpc(self, args, expected=0):
        body = {'v': 1, 'id': secrets.token_hex(8), 'method': 'cli.exec', 'args': ['--json', *args]}
        if self.args.runtime_binary:
            code, health, _ = self.http(self.url, 'GET', '/api/health')
            require(code == 200, 'runtime_scope_health_failed')
            verify_runtime_health(self.args, health, self.root, self.library, self.child.pid)
            body = scoped_rpc_request(health, ['--json', *args])
        code, reply, _ = self.http(self.url, 'POST', '/api/rpc',
                                  body, self.token)
        if self.args.runtime_binary:
            verify_rpc_receipt(body, reply)
        if code != 200 or not isinstance(reply, dict) or reply.get('ok') is not (expected == 0):
            self.report['failure_diagnostic'] = {
                'case_id': 'runtime_rpc',
                'command': args[0] if args and args[0] in
                    {'remember', 'show', 'status', 'recall', 'agent-config', 'model'} else 'other',
                'expected_exit': expected,
                'http_status': code,
                'actual_exit': reply.get('exit') if isinstance(reply, dict) and type(reply.get('exit')) is int else None,
                'rpc_code': reply.get('code') if isinstance(reply, dict) and reply.get('code') in
                    ('locked', 'runtime_locked', 'scope_mismatch', 'invalid_config', 'daemon_unavailable',
                     'runtime_maintenance', 'host_command_required') else 'unclassified',
            }
        require(code == 200 and isinstance(reply, dict) and reply.get('ok') is (expected == 0),
                'runtime_rpc_transport_failed')
        envelope = reply.get('envelope', {})
        require(reply.get('exit') == expected and envelope.get('status') in
                (('ok', 'skip') if expected == 0 else ('fail',)), 'runtime_rpc_command_failed')
        return envelope

    def stop(self):
        if self.child is None:
            return
        if self.child.poll() is None:
            if self.args.runtime_binary:
                self.direct(['--runtime-internal', '--stop'])
            else:
                code, reply, _ = self.http(self.url, 'POST', '/api/runtime/stop', {}, self.token)
                require(code == 200 and reply.get('ok') is True, 'owned_runtime_stop_rejected')
            try:
                self.child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                raise Failure('owned_runtime_stop_timeout') from None
        require(self.child.returncode == 0, 'owned_runtime_stop_exit_failed')
        self.child = None

    def start(self, unlock=True):
        require(self.child is None, 'runtime_already_owned')
        if self.args.runtime_binary:
            self.direct(['--runtime-internal', '--stop'], case_id='runtime_prestart_stop')
        token_path = self.root / 'runtime' / 'token'
        # The host may already have created its unlock token; Runtime must not change it.
        host_token = token_path.read_bytes() if token_path.exists() else None
        launch = ([str(self.args.runtime_binary), 'serve'] if self.args.runtime_binary else
                  [str(self.args.binary), '--runtime-internal'])
        self.child = subprocess.Popen([*launch, '--port', str(self.port),
                                       '--host', '127.0.0.1'], env=self.env, cwd=self.root,
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            require(self.child.poll() is None, 'owned_runtime_exited_before_ready')
            require((token_path.read_bytes() if token_path.exists() else None) == host_token,
                    'loopback_runtime_changed_host_token')
            if (self.root / 'runtime' / 'endpoint.json').is_file():
                try:
                    code, health, _ = self.http(self.url, 'GET', '/api/health')
                except Failure:
                    time.sleep(0.1)
                    continue
                if code == 200:
                    if self.args.runtime_binary:
                        verify_runtime_health(self.args, health, self.root, self.library, self.child.pid)
                        if health.get('startup_complete') is not True:
                            time.sleep(0.1)
                            continue
                        if unlock:
                            receipt = self.direct(['runtime', 'unlock'], case_id='runtime_host_unlock')
                            require(receipt.get('summary', {}).get('store_ready') is True,
                                    'runtime_host_unlock_failed')
                            code, health, _ = self.http(self.url, 'GET', '/api/health')
                            require(code == 200, 'runtime_unlocked_health_failed')
                            verify_runtime_health(self.args, health, self.root, self.library, self.child.pid)
                        require(health.get('store_ready') is unlock, 'runtime_store_readiness_mismatch')
                    else:
                        require(health.get('v') == 1 and health.get('bin') == self.args.version
                            and health.get('pid') == self.child.pid
                            and Path(health.get('exe', '')).resolve() == self.args.binary
                            and Path(health.get('data_dir', '')).resolve() == self.library,
                            'owned_runtime_identity_mismatch')
                    return
            time.sleep(0.1)
        raise Failure('owned_runtime_ready_timeout')

    def setup(self):
        temp = Path(os.environ.get('RUNNER_TEMP', '')).resolve()
        require(os.environ.get('GITHUB_ACTIONS') == 'true'
                and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
                and platform.system() == 'Linux', 'ephemeral_github_linux_runner_required')
        require(self.root != temp and self.root.is_relative_to(temp) and not self.root.exists(),
                'fresh_owned_runner_temp_required')
        require(re.fullmatch('[0-9a-f]{40}', self.args.source_sha)
                and re.fullmatch('[0-9a-f]{64}', self.args.binary_sha256)
                and hashlib.sha256(self.args.binary.read_bytes()).hexdigest() == self.args.binary_sha256,
                'exact_binary_identity_mismatch')
        self.root.mkdir(mode=0o700)
        self.created = True
        self.library = self.root / 'library'
        for name in ('home', 'config', 'data', 'cache', 'tmp', 'runtime', 'bin', 'models'):
            (self.root / name).mkdir(mode=0o700)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(('ONEMEMORY_', 'RESPIRE_', 'XDG_', 'DS_', 'JEV_', 'GNOME_'))
                    and not k.endswith(('_TOKEN', '_API_KEY')) and 'TEST_MODE' not in k
                    and k not in ('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'DBUS_SESSION_BUS_ADDRESS',
                                  'RSRS_SS_FIXTURE_BUS', 'DBUS_STARTER_ADDRESS', 'DBUS_STARTER_BUS_TYPE',
                                  'DBUS_SESSION_BUS_PID', 'SSH_AUTH_SOCK', 'RSRS_SUPER')}
        bus = private_fixture_bus(os.environ)
        if bus is not None:
            self.env.update(DBUS_SESSION_BUS_ADDRESS=bus, RSRS_SS_FIXTURE_BUS='private')
        self.env.update(HOME=str(self.root / 'home'), USERPROFILE=str(self.root / 'home'),
                        XDG_CONFIG_HOME=str(self.root / 'config'), XDG_DATA_HOME=str(self.root / 'data'),
                        XDG_CACHE_HOME=str(self.root / 'cache'), XDG_RUNTIME_DIR=str(self.root / 'runtime'),
                        TMPDIR=str(self.root / 'tmp'), RSRS_DATA_DIR=str(self.root),
                        RSRS_BIN_DIR=str(self.root / 'bin'), RSRS_ENGINE='cpu',
                        RSRS_RUNTIME_BIN=str(self.args.runtime_binary or self.args.binary.resolve()),
                        RSRS_LANG='en', RSRS_NO_AUTOSYNC='1', RSRS_UPDATE_CHECK='0',
                        RSRS_M3_DIR=str(self.root / 'models/bge-m3'),
                        )
        if self.args.runtime_binary:
            self.env['LC_ALL'] = 'C'
        for key in ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'):
            self.env.pop(key, None)
        self.provider_keys = RuntimeProviderKeys(self.env)
        with socket.socket() as port:
            port.bind(('127.0.0.1', 0))
            self.port = port.getsockname()[1]
        self.url = 'http://127.0.0.1:' + str(self.port)
        self.env['RSRS_RPC_PORT'] = str(self.port)
        installed = install_published_runtime(self.args.binary, self.env)
        if installed is not None:
            self.args.runtime_binary = installed
            if os.environ.get('RSRS_ARTIFACT_SOURCE') != 'published-assets':
                receipt = json.loads((Path(self.env['RSRS_DATA_DIR']) / 'bin/runtime.json').read_text(encoding='utf-8'))
                self.args.runtime_version, self.args.runtime_sha256 = receipt['version'], receipt['sha256']
                runtime_inputs(self.args, [self.args.binary])
        value = self.direct(['-v'])
        require(value.get('summary', {}).get('version') == self.args.version, 'exact_version_mismatch')
        self.passed('artifact_identity_verified')




def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--binary-sha256', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--report', type=Path)
    add_runtime_arguments(parser)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    runtime_inputs(args, [args.binary])
    destination = args.report.resolve() if args.report else args.root.resolve() / 'runtime-coverage.json'
    temp = Path(os.environ.get('RUNNER_TEMP', '')).resolve()
    require(destination != temp and destination.is_relative_to(temp) and not destination.exists(),
            'report_path_must_be_fresh_runner_temp_file')
    smoke = Smoke(args)
    try:
        smoke.setup()
        smoke.dashboard()
        smoke.account()
        smoke.transport()
        smoke.settings()
        smoke.doctor_model_recovery()
        smoke.validation_errors()
        smoke.host_space_commands()
        smoke.update_check_comparisons()
    except RuntimeHealthFailure as error:
        smoke.report['failure_code'] = error.code
        smoke.report['failure_diagnostic'] = error.diagnostic()
    except Exception as error:
        smoke.report['failure_code'] = str(error) if isinstance(error, Failure) else type(error).__name__
    finally:
        smoke.cleanup()
    smoke.report['missing_cases'] = [name for name in REQUIRED if smoke.report['cases'].get(name, {}).get('passed') is not True]
    smoke.report['passed'] = not smoke.report['missing_cases'] and not smoke.report.get('failure_code') and smoke.cloud_clean and smoke.provider_clean
    smoke.report['status'] = 'passed' if smoke.report['passed'] else 'failed'
    smoke.report.update(runtime_report(args, smoke.report['passed']))
    if args.runtime_binary and not smoke.report['standalone_runtime_evidence']:
        smoke.report['passed'] = False
        smoke.report['status'] = 'failed'
    if smoke.created:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(smoke.report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'passed': smoke.report['passed'], 'failure_code': smoke.report.get('failure_code'),
                      'failure_diagnostic': smoke.report.get('failure_diagnostic'),
                      'missing_cases': smoke.report['missing_cases'], 'cloud_cleanup': smoke.report['cloud_cleanup']}))
    return 0 if smoke.report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
