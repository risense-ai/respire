#!/usr/bin/env python3
"""CI-only synthetic legacy wraps, real CLI upgrade and new-device decryption."""
from native_fixture_process import canonical_scope_path, install_published_runtime
import argparse
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import socket
import ssl
import sqlite3
from dev_witness_fixture import register_witness_functions, verify_fixture_witness
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from dev_model_fixture import share_cpu_container
from dev_fixture_tty import run_hidden
from legacy_vault_fixture import UPSTREAM, REQUIRED, DATABASE_REQUIRED, MODEL_HASHES, require, same_scope_path, readonly_database_uri, digest, hkdf, PREFIX, encrypted_bytes, v4_kek, legacy_kek, decrypt_content, NoRedirect


from legacy_vault_scenarios import LegacyVaultScenarios


class Smoke(LegacyVaultScenarios):
    def __init__(self, args):
        self.args = args
        self.root = args.root
        self.model = self.root / "models/bge-m3"
        self.root.mkdir()
        self.base_env = {k: v for k, v in os.environ.items()
            if not k.startswith(("RSRS_", "ONEMEMORY_", "RESPIRE_", "XDG_", "DS_", "JEV_"))
            and not k.endswith(("_TOKEN", "_API_KEY")) and "TEST_MODE" not in k
            and k not in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "DBUS_SESSION_BUS_ADDRESS",
                "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy")}
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self.accounts = []
        self.diagnostic_secrets = set()
        self.keys = None
        self.report = {"status": "running", "source_sha": args.source_sha,
            "workflow_sha": os.environ.get("GITHUB_SHA"), "version": args.version,
            "binary_sha256": args.binary_sha256, "target": UPSTREAM, "cases": {},
            "passed": False, "required_cases": list(("model_cpu_real", REQUIRED[2]) if getattr(args, "auth_salt_only", False) else DATABASE_REQUIRED),
            "dependency": "cryptography==46.0.3", "cloud_cleanup": {"passed": False, "events": [], "remaining_users": []}}

    def env(self, name, user=None):
        path = self.root / name
        require(not path.exists(), "profile_not_fresh")
        for folder in ("home", "config", "data", "cache", "tmp", "library", "bin"):
            (path / folder).mkdir(parents=True)
        env = dict(self.base_env)
        env.update(HOME=str(path / "home"), USERPROFILE=str(path / "home"),
            APPDATA=str(path / "config"), LOCALAPPDATA=str(path / "data"),
            XDG_CONFIG_HOME=str(path / "config"), XDG_DATA_HOME=str(path / "data"),
            XDG_CACHE_HOME=str(path / "cache"), TMPDIR=str(path / "tmp"),
            RSRS_DATA_DIR=str(path / "library"), RSRS_BIN_DIR=str(path / "bin"),
            RSRS_M3_DIR=str(self.model), RSRS_ENGINE="cpu", RSRS_NO_AUTOSYNC="1",
            DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(path / "tmp/missing-keyring.sock"))
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            env['RSRS_RPC_PORT'] = str(listener.getsockname()[1])
        if user:
            self.write_session(env, {"user": user})
        if self.keys is not None:
            self.keys.configure_env(env)
        install_published_runtime(self.args.binary, env)
        return env

    def redacted_cli_error(self, env, output):
        try:
            value = json.loads(output.stdout)
            if isinstance(value, dict) and isinstance(value.get("envelope"), dict):
                value = value["envelope"]
            errors = value.get("errors", []) if isinstance(value, dict) else []
            diagnostic = "; ".join(str(error) for error in errors)
        except (ValueError, UnicodeError):
            diagnostic = ""
        diagnostic = diagnostic or output.stderr.decode("utf-8", errors="replace")[-1500:]
        secrets_to_redact = set(self.diagnostic_secrets)
        session_path = self.profile(env) / "session.json" if env.get("RSRS_DATA_DIR") else None
        if session_path is not None and session_path.is_file():
            session = json.loads(session_path.read_text())
            secrets_to_redact.update(session.get(key) for key in
                ("pass", "pass_hash", "super", "token", "secret", "secret_key", "wrapped_urk", "urk_nonce", "kdf_salt")
                if isinstance(session.get(key), str) and session[key])
        for secret in sorted(secrets_to_redact, key=len, reverse=True):
            diagnostic = diagnostic.replace(secret, "[redacted]")
        diagnostic = re.sub(r"(?i)bearer\s+\S+", "Bearer [redacted]", diagnostic)
        diagnostic = re.sub(r"(?i)\b(?:A3-)?[a-f0-9]{24,}\b", "[redacted]", diagnostic)
        return diagnostic[:1500] or "no structured error"

    def historical_provider_state(self, when):
        if not self.report.get("stage", "").startswith("historical_"):
            return
        keys = getattr(self, "keys", None)
        if keys is None or not hasattr(keys, "daemon"):
            return
        daemon = keys.daemon
        started = time.monotonic_ns()
        code = daemon.poll() if daemon is not None else None
        stderr_path = getattr(keys, "daemon_stderr", None)
        try:
            stderr_empty = stderr_path.stat().st_size == 0 if stderr_path is not None else None
        except OSError:
            stderr_empty = None
        self.report.setdefault("provider_observation", {})[when] = {
            "original_handle_available": daemon is not None,
            "running": daemon is not None and code is None, "exit_code": code,
            "stderr_empty": stderr_empty, "stage": self.report["stage"] if self.report["stage"] in (
                "historical_same_key_recovery", "historical_existing_keys_register", "historical_source_push",
                "historical_reverse_peer_push", "historical_reverse_source_pull", "historical_reverse_source_read")
                else "historical_other",
            "sample_started_ns": started, "sample_completed_ns": time.monotonic_ns()}

    def cli(self, env, *args, timeout=180, prompts=None, host_request=None):
        business_failed = False
        try:
            self.historical_provider_state("before")
            provider = self.report.get("provider_observation", {}).get("before")
            require(provider is None or provider["running"], "historical_provider_owner_exited")
            if host_request is not None:
                output = subprocess.run([str(self.args.binary), "--json", "--host-rpc-stdio"],
                    input=json.dumps(host_request).encode(), cwd=self.root, env=env, capture_output=True, timeout=timeout)
            else:
                argv = [str(self.args.binary), "--direct", "--json", *args]
                output = run_hidden(argv, prompts=prompts, cwd=self.root, env=env, timeout=timeout) if prompts is not None else subprocess.run(
                    argv, cwd=self.root, env=env, capture_output=True, timeout=timeout)
            self.historical_provider_state("after")
            if output.returncode != 0:
                self.report["cli_failure"] = {"exit_code": output.returncode,
                    "reason": "process_exit"}
                try:
                    frame = json.loads(output.stdout)
                    envelope = frame.get("envelope", frame) if isinstance(frame, dict) else {}
                    for field, value, allowed in (
                        ("status", envelope.get("status"), ("ok", "error", "failed")),
                        ("structured_reason", envelope.get("summary", {}).get("reason"),
                         ("runtime_error", "invalid_input", "rpc_error", "request_outcome_unknown")),
                        ("error_type", envelope.get("details", {}).get("error_type"),
                         ("runtime", "input", "user", "rpc", "validation", "outcome_unknown"))):
                        if isinstance(value, str) and value in allowed:
                            self.report["cli_failure"][field] = value
                    code = envelope.get("details", {}).get("code")
                    if type(code) is int:
                        self.report["cli_failure"]["code"] = code
                except (ValueError, UnicodeError, AttributeError, TypeError):
                    self.report["cli_failure"]["structured_error"] = "unavailable"
                raise RuntimeError("cli_failed_" + args[0])
            provider = self.report.get("provider_observation", {}).get("after")
            require(provider is None or provider["running"], "historical_provider_owner_exited")
            try:
                value = json.loads(output.stdout)
            except (ValueError, UnicodeError):
                raise RuntimeError("invalid_cli_json_" + args[0]) from None
            if host_request is not None:
                require(isinstance(value, dict) and value.get("v") == 1 and value.get("id") == host_request["id"]
                    and value.get("exit") == 0 and value.get("ok") is True
                    and isinstance(value.get("envelope"), dict), "legacy_login_rpc_transport_mismatch")
                value = value["envelope"]
                require(value.get("status") == "ok", "legacy_login_rpc_not_committed")
            require(not value.get("errors") and value.get("status") not in ("error", "failed"), "cli_error_" + args[0])
            return value
        except Exception:
            business_failed = True
            self.historical_provider_state("after")
            self.record_runtime_failure()
            raise
        finally:
            # Login and automatic indexing can start this fixture's runtime.
            # Release it before subsequent direct database consumers.
            try:
                self.historical_provider_state("stop_before")
                self.report["runtime_stop"] = {"passed": False}
                stopped = subprocess.run([str(self.args.binary), '--runtime-internal', '--stop'],
                    cwd=self.root, env=env, capture_output=True, timeout=60)
                self.report["runtime_stop"] = {"exit_code": stopped.returncode,
                    "passed": stopped.returncode in (0, 2)}
                require(stopped.returncode in (0, 2), 'owned_runtime_stop_failed')
                self.last_runtime_stop = stopped.returncode
            except Exception:
                if not business_failed:
                    raise
                # Keep the first business exception; expose cleanup failure separately.
                self.report.setdefault("runtime_stop", {"passed": False})["passed"] = False

    def record_runtime_failure(self):
        if "runtime_failure_evidence" in self.report:
            return
        stage = self.report.get("stage")
        # This fixture holds no verified Runtime owner or original process handle.
        # An endpoint/PID lookup here cannot bind the owner of the failed request.
        # The preceding pull stops Runtime before returning; a pre-show health
        # snapshot cannot identify an owner that show may subsequently start.
        self.report["runtime_failure_evidence"] = {
            "stage": stage if stage in ("historical_reverse_peer_push",
                "historical_reverse_source_pull", "historical_reverse_source_read") else "other",
            "binding": "unbound", "reason": "show_request_owner_not_captured"
                if stage == "historical_reverse_source_read" else "no_verified_original_owner",
            "request_bound": False, "owner_sampled": False,
            "identity_verified": False, "process_handle_available": False,
            "liveness": "unknown", "exit_code_available": False,
            "persistence": "saved"}
        try:
            self.save()
        except Exception:
            self.report["runtime_failure_evidence"]["persistence"] = "unavailable"
            print('{"runtime_failure_evidence":"unavailable"}', file=sys.stderr)

    def password_login(self, env, user, password, version, original_super, account_secret=None, secret_key=None):
        """Authorize through hidden TTY prompts or the existing v3 stdin host RPC."""
        prompts = [("  login method: 1 OAuth (default), 2 password/TOTP: ", "2"),
            ("  login password: ", password)]
        if version != 1:
            prompts.append(("  super password (A3-…): ", original_super))
        if account_secret is not None:
            prompts.append(("  original v1 Account Secret: ", account_secret))
        login_env = dict(env)
        if version != 1:
            # Prove the original factor through explicit TTY/stdin input,
            # without letting an environment factor satisfy the login.
            login_env.pop("RSRS_SUPER", None)
        previous = self.session(env)
        previous_alias = previous.get("keyring_account")
        if version != 3 and not previous.get("token"):
            # Runtime creates the source DB before offering a merge, even on a fresh device.
            prompts.append(("  Merge selected local memories into this server account (skip identical bodies; keep existing records)? [y/N]: ", "n"))
        completed = False
        try:
            if version == 3:
                require(isinstance(secret_key, str) and bool(secret_key), "legacy_v3_secret_key_missing")
                request = {"v": 1, "id": "legacy-login-" + uuid.uuid4().hex, "method": "auth.login",
                    "params": {"addr": UPSTREAM, "user": user, "password": password, "super": original_super,
                        "continuation": json.dumps({"secret_key": secret_key})}}
                result = self.cli(login_env, "login", host_request=request)
                receipt = result.get("summary") or {}
                operation_id = receipt.get("operation_id")
                require(isinstance(operation_id, str) and operation_id != request["id"]
                    and re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", operation_id)
                    and receipt.get("outcome") == "committed"
                    and same_scope_path(receipt.get("source_profile"), env["RSRS_DATA_DIR"]),
                    "legacy_login_rpc_transaction_mismatch")
                login = receipt.get("login") or {}
                require(login.get("user") == user and login.get("addr") == UPSTREAM
                    and login.get("required_inputs") == [], "legacy_login_rpc_identity_mismatch")
            else:
                result = self.cli(login_env, "login", "--interactive", "--addr", UPSTREAM, "--user", user, prompts=prompts)
            completed = True
        finally:
            # A committed login can still fail the TTY/JSON/owned-stop checks.
            # Claim only its new alias from this exact owned session, never an
            # existing alias or arbitrary credential-store contents. Keep the
            # original failure fatal while retaining exact cleanup ownership.
            committed = self.session(env)
            created = (bool(committed.get("token")) and bool(committed.get("keyring_account"))
                and committed.get("keyring_account") != previous_alias)
            if completed or created:
                require(created, "login_credential_fixture_new_session_missing")
                self.keys.track_created_login(committed, user,
                    None if version == 1 else original_super, password)
        return result

    def profile(self, env):
        # Compare both paths in Rust's canonical form, including Windows verbatim prefixes.
        root = Path(canonical_scope_path(env["RSRS_DATA_DIR"]))
        config_path = root / "client.json"
        config = json.loads(config_path.read_text()) if config_path.exists() else {}
        selected = config.get("data_dir", str(root))
        require(isinstance(selected, str) and bool(selected), "login_profile_selection_invalid")
        profile = Path(canonical_scope_path(selected))
        require(profile.is_relative_to(root), "login_profile_selection_outside_fixture")
        return profile

    def session(self, env):
        # A new-device login starts without a session; existing files must parse.
        path = self.profile(env) / "session.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def write_session(self, env, value):
        path = Path(env["RSRS_DATA_DIR"]) / "session.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        path.chmod(0o600)

    def request(self, account, method, path, body=None):
        require(account["confirmed"] and account["token"], "account_identity_not_confirmed")
        encoded = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(UPSTREAM + path, encoded,
            {"Authorization": "Bearer " + account["token"], "Content-Type": "application/json"}, method=method)
        try:
            response = self.opener.open(request, timeout=45)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(1024 * 1024 + 1)
            require(len(raw) <= 1024 * 1024, "api_response_too_large")
            return response.code, json.loads(raw)

    def api(self, account, method, path, body=None):
        status, value = self.request(account, method, path, body)
        require(status == 200, "dev_api_failed_" + method + "_" + path.replace("/", "_"))
        return value

    def save(self):
        (self.root / "legacy-vault-coverage.json").write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")

    def passed(self, name, **facts):
        self.report["cases"][name] = {"status": "passed", "passed": True, **facts}
        self.save()

    def read_entry(self, env, memory_id, plaintext):
        result = self.cli(env, "show", memory_id)
        require(plaintext in json.dumps(result), "old_ciphertext_not_decrypted")

    def verify_legacy_factor(self, session, secret, code):
        alias = session.get("keyring_account")
        require(isinstance(alias, str) and ("rsrs", "legacy-secret:" + alias) in self.keys.entries
            and session.get("legacy_secret_backend") == self.keys.credential_backend
            and self.keys.credential_backend in ("native", "secret_service")
            and not {"pass", "super", "secret", "secret_key"}.intersection(session), code)
        require(self.keys.read("rsrs", "legacy-secret:" + alias) == secret, code)

    def current_login(self, env, user, password, code):
        """One current host login; an unknown outcome is never replayed."""
        previous_alias = self.session(env).get("keyring_account")
        completed = False
        try:
            request = {"v": 1, "id": "database-login-" + uuid.uuid4().hex, "method": "auth.login",
                "params": {"addr": UPSTREAM, "user": user, "password": password, "super": code}}
            result = self.cli(env, "login", host_request=request)
            receipt = result.get("summary") or {}
            operation_id = receipt.get("operation_id")
            require(isinstance(operation_id, str) and operation_id != request["id"]
                and re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", operation_id)
                and receipt.get("outcome") == "committed"
                and same_scope_path(receipt.get("source_profile"), env["RSRS_DATA_DIR"]),
                "database_login_transaction_mismatch")
            login = receipt.get("login") or {}
            require(login.get("user") == user and login.get("addr") == UPSTREAM
                and login.get("required_inputs") == [], "database_login_identity_mismatch")
            completed = True
        finally:
            # Track a committed alias even if subsequent receipt validation fails.
            committed = self.session(env)
            created = (bool(committed.get("token")) and bool(committed.get("keyring_account"))
                and committed.get("keyring_account") != previous_alias)
            if completed or created:
                require(created, "login_credential_fixture_new_session_missing")
                self.keys.track_created_login(committed, user, code, password)
        return result


    def cleanup(self):
        cleanup = self.report["cloud_cleanup"]
        for account in self.accounts:
            event = {"user": account["user"], "passed": False, "stage": "identity"}
            try:
                require(account["confirmed"], "registration_outcome_unconfirmed")
                event["stage"] = "purge_request"
                value = self.api(account, "POST", "/api/self/purge", {"confirm": account["user"]})
                require(value.get("purged") is True and value.get("user") == account["user"], "self_purge_not_confirmed")
                event["stage"] = "verify_revocation"
                status, _ = self.request(account, "GET", "/api/self")
                require(status == 401, "purged_token_still_accepted")
                cleanup["remaining_users"].remove(account["user"])
                event.update(passed=True, old_token_rejected=True, stage="complete")
            except Exception as error:
                event["code"] = str(error) if isinstance(error, RuntimeError) else type(error).__name__
                if isinstance(error, urllib.error.URLError):
                    reason = error.reason
                    event["transport"] = ("timeout" if isinstance(reason, TimeoutError)
                        else "dns" if isinstance(reason, socket.gaierror)
                        else "tls" if isinstance(reason, ssl.SSLError)
                        else "os_error" if isinstance(reason, OSError)
                        else "unclassified_url_error")
                    if isinstance(reason, OSError):
                        event["errno"] = reason.errno
            cleanup["events"].append(event)
        cleanup["passed"] = not cleanup["remaining_users"]

    def verify_development_server(self):
        expected_server = os.environ.get("RSRS_DEV_SERVER_SHA", "")
        require(os.environ.get("RSRS_DEV_SERVER_ADDR") == UPSTREAM
                and re.fullmatch("[0-9a-f]{40}", expected_server), "exact_development_server_required")
        with self.opener.open(UPSTREAM + "/health", timeout=45) as health:
            raw = health.read(1024 * 1024 + 1)
            require(len(raw) <= 1024 * 1024, "deployment_response_too_large")
            value = json.loads(raw)
            require(health.status == 200 and isinstance(value, dict)
                    and value.get("source_revision") == expected_server,
                    "development_server_revision_mismatch")
        self.report["server_sha"] = expected_server

    def run(self):
        self.verify_development_server()
        import cryptography
        require(cryptography.__version__ == "46.0.3", "cryptography_version_not_pinned")
        keyring_script = ("dev-migration-secret-service.py" if sys.platform.startswith("linux")
            and os.environ.get("RSRS_SS_FIXTURE_BUS") == "private"
            and os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").startswith("unix:") else "dev-migration-keyring.py")
        spec = importlib.util.spec_from_file_location("legacy_vault_keyring", Path(__file__).with_name(keyring_script))
        backend = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(backend)
        self.keys = backend.create(self.root / "keyring-home")
        env = self.env("model-probe")
        output = subprocess.run([str(self.args.binary), "--version"], env=env, capture_output=True, timeout=20)
        require(output.returncode == 0 and re.search(r"(?<!\S)" + re.escape(self.args.version) + r"(?!\S)", output.stdout.decode()), "binary_version_mismatch")
        installed = self.cli(env, "model", "install-m3", timeout=900)
        share_cpu_container(self.root, env, self.model)
        require(all(digest(self.model / p) == h for p, h in MODEL_HASHES.items()), "model_hash_mismatch")
        probe = installed["details"]["backend_load"]["evidence"]
        require(probe.get("ready") is True and probe.get("inference_completed") is True
            and probe.get("fresh_session") is True and probe.get("dimensions") == 1024
            and str(probe.get("selected")).lower() == "cpu", "real_cpu_probe_failed")
        self.passed("model_cpu_real", dimensions=1024, model_hashes=MODEL_HASHES)
        if self.args.auth_salt_only:
            # Explicit manual authentication diagnostic, outside release acceptance.
            self.run_version(2)
        else:
            self.database_sync()
        self.report["status"] = "passed"


def main():
    require(os.environ.get("GITHUB_ACTIONS") == "true", "github_actions_required")
    require(os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted" and sys.platform.startswith("linux"), "disposable_linux_hosted_runner_required")
    parser = argparse.ArgumentParser()
    parser.add_argument("--auth-salt-only", action="store_true", help="Run the focused real DEV authentication migration case")
    for name in ("binary", "root"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("binary-sha256", "source-sha", "version"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    require(re.fullmatch(r"[0-9a-f]{64}", args.binary_sha256), "binary_sha_invalid")
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_sha) and args.source_sha == os.environ.get("CLI_SHA"), "verified_artifact_source_sha_mismatch")
    require(args.version and not any(c.isspace() for c in args.version), "version_invalid")
    require(args.binary.is_absolute() and args.binary.is_file() and digest(args.binary) == args.binary_sha256, "binary_hash_mismatch")
    temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    require(args.root.is_absolute() and not args.root.exists(), "root_not_fresh")
    args.root = args.root.resolve()
    require(args.root != temp and args.root.is_relative_to(temp), "root_outside_runner_temp")
    smoke = Smoke(args)
    try:
        smoke.run()
    except Exception as error:
        smoke.report.update(status="failed", failure=str(error) if isinstance(error, RuntimeError) else type(error).__name__)
        smoke.record_runtime_failure()
    finally:
        smoke.cleanup()
        if smoke.keys is not None:
            try:
                smoke.report["keyring_cleanup"] = smoke.keys.cleanup()
            except Exception as error:
                smoke.report["keyring_cleanup"] = {"passed": False, "failure_code": type(error).__name__}
        required = smoke.report["required_cases"]
        smoke.report["scope"] = "real_auth_salt_migration" if args.auth_salt_only else "historical_ciphertext_sync"
        smoke.report["missing_cases"] = [case for case in required if case not in smoke.report["cases"]]
        if not smoke.report["cloud_cleanup"]["passed"]:
            smoke.report["status"] = "failed"
        smoke.report["passed"] = smoke.report["status"] == "passed" and not smoke.report["missing_cases"] \
            and smoke.report["cloud_cleanup"]["passed"] and smoke.report.get("keyring_cleanup", {}).get("passed") is True
        smoke.save()
    print(json.dumps({"status": smoke.report["status"], "passed": len(smoke.report["cases"]),
        "required": len(required), "report": str(smoke.root / "legacy-vault-coverage.json")}))
    return 0 if smoke.report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
