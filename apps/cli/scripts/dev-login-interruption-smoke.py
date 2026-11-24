#!/usr/bin/env python3
"""Real Linux CLI SIGINT recovery; only synthetic accounts and loopback HTTP."""
from native_fixture_process import install_published_runtime
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import re
import socket
import sqlite3
from dev_witness_fixture import register_witness_functions, verify_fixture_witness
import subprocess
import threading
import time
import uuid
import select
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from login_interruption_account import account_fixture_handler, gui_recovery_server
from login_interruption_cleanup import cleanup_fixture
from login_interruption_continuations import host_key_continuation, host_login_continuation
from login_interruption_control import (
    FAILURE_CODES,
    account_recovery_flags,
    fixture_report,
    fixture_runtime_owners,
    interrupt_owned_control,
    key_continuation_receipt,
    login_continuation_receipt,
    login_terminal_operation,
    original_operation,
    process_group_members,
    remove_fixture_super,
    runtime_stop_receipt,
    safe_cli_diagnostic,
    stop_owned_control,
    terminal_operation,
    verify_credential_provider,
)
from login_interruption_transfer import verify_server_transfer

if "--gui-recovery-server" not in sys.argv:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    spec = importlib.util.spec_from_file_location("legacy", Path(__file__).with_name("dev-legacy-vault-smoke.py"))
    legacy = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(legacy)
    require = legacy.require


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "root", "model-dir"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("binary-sha256", "source-sha", "version"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--native-credentials", action="store_true")
    args = parser.parse_args()
    require(os.name == "posix" and Path("/proc/self/stat").is_file() and hasattr(os, "pidfd_open"),
            "SIGINT_fixture_requires_linux_pidfd")
    require(legacy.digest(args.binary) == args.binary_sha256, "binary_hash_mismatch")
    require(not args.root.exists(), "fresh_fixture_required")
    args.root.mkdir(mode=0o700)
    report = {"source_sha": args.source_sha, "version": args.version,
              "binary_sha256": args.binary_sha256, "passed": False, "cases": {},
              "credential_mode": "secret_service" if args.native_credentials else "headless"}
    with fixture_report(args, report):
        code = "A3-" + "-".join(secrets.token_hex(3).upper() for _ in range(6))
        urk = secrets.token_bytes(32)
        salt, nonce = secrets.token_hex(16), secrets.token_bytes(12)
        v4 = {"version": 4, "kdf_salt": salt, "urk_nonce": nonce.hex(),
              "wrapped_urk": legacy.PREFIX + AESGCM(legacy.v4_kek(code, salt, legacy.PREFIX)).encrypt(nonce, urk, None).hex()}
        salt, nonce = secrets.token_hex(16), secrets.token_bytes(12)
        old_super = "synthetic-legacy-super"
        v2 = {"version": 2, "kdf_salt": salt, "urk_nonce": nonce.hex(),
              "wrapped_urk": AESGCM(legacy.legacy_kek(2, old_super, "", salt)).encrypt(nonce, urk, None).hex()}
        migrated_v2 = dict(v2, wrapped_urk=legacy.PREFIX + v2["wrapped_urk"])
        old_auth_salt = legacy.hkdf(b"synthetic", None, b"onememory:auth-salt:v1", length=16).hex()
        state = {"vault": v4, "auth_salt": old_auth_salt, "gets": 0, "posts": 0, "logins": 0, "password_posts": 0, "barrier": None}
        reached, release = threading.Event(), threading.Event()

        Handler = account_fixture_handler(state, reached, release, v2)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        addr = f"http://127.0.0.1:{server.server_port}"
        root = args.root / "library"
        root.mkdir(mode=0o700)
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("RSRS_", "ONEMEMORY_", "RESPIRE_", "XDG_"))
               and k not in ("GH_TOKEN", "GITHUB_TOKEN")}
        home = args.root / "home"
        home.mkdir(mode=0o700)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        env.update(HOME=str(home), RSRS_DATA_DIR=str(root), RSRS_SUPER=code,
                   RSRS_RPC_PORT=str(port), RSRS_M3_DIR=str(args.model_dir),
                   RSRS_NO_AUTOSYNC="1", RSRS_UPDATE_CHECK="0", RSRS_ENGINE="cpu")
        keys = None
        if args.native_credentials:
            native_root = args.root / "native-store"
            native_root.mkdir(mode=0o700)
            key_spec = importlib.util.spec_from_file_location("private_credentials", Path(__file__).with_name("dev-migration-secret-service.py"))
            key_module = importlib.util.module_from_spec(key_spec)
            key_spec.loader.exec_module(key_module)
            keys = key_module.SecretServiceKeys(native_root)
            keys.configure_env(env)
        report["stage"] = "runtime_install"
        install_published_runtime(args.binary, env)
        report["stage"] = "profile_setup"
        session = root / "session.json"
        config = root / "client.json"
        config_bytes = json.dumps({"data_dir": str(root), "api_base": "https://previous.invalid", "custom": "preserve"}).encode()
        config.write_bytes(config_bytes)

        def write_session(vault):
            value = {"user": "synthetic", "addr": addr, "token": "synthetic-token", "vault_version": vault["version"],
                     **{k: v for k, v in vault.items() if k != "version"}}
            if vault["wrapped_urk"].startswith(legacy.PREFIX):
                value["crypto_namespace"] = legacy.PREFIX
            if keys:
                alias = "legacy-" + secrets.token_hex(8)
                keys.put("rsrs", "super:" + alias, code if vault["version"] == 4 else old_super)
                keys.put("rsrs", "pass:" + alias, "synthetic-password")
                value.update(keyring_account=alias, keyring_backend=keys.credential_backend)
                native_aliases.add(alias)
            session.write_text(json.dumps(value))
            session.chmod(0o600)
            return session.read_bytes()

        def cli(*flags, success=True):
            command_env = dict(env)
            # A profile switch must unlock with that session's native credential,
            # never the unrelated initial v4 fixture's global super override.
            if keys:
                command_env.pop("RSRS_SUPER", None)
            elif "migrate" in flags:
                command_env["RSRS_SUPER"] = old_super
            result = subprocess.run([str(args.binary), *flags], env=command_env, capture_output=True, timeout=180)
            if (result.returncode == 0) != success:
                command = next((name for name in ("account-rollback", "account-result", "login", "remember",
                    "migrate", "--runtime-internal", "--version") if name in flags), "other")
                report["failure"] = {"exit_code": result.returncode, "code": "cli_expectation_failed",
                                     "command": command,
                                     "diagnostic": safe_cli_diagnostic(result.stdout, result.stderr)}
                raise RuntimeError("cli_expectation_failed")
            return result

        migration = ["--direct", "--json", "migrate", "--vault", "--addr", addr, "--user", "synthetic",
                     "--pass=synthetic-password", "--super", old_super, "--new-super", old_super]
        child = None
        migration_keys = None
        native_aliases = set()
        try:
            report["stage"] = "host_login_continue"
            report["cases"]["host_login_continue"] = {"status": "not-evaluated-private-secret-service-required"}
            report["cases"]["host_key_continue"] = {"status": "not-evaluated-private-secret-service-required"}
            if keys:
                report["cases"]["host_login_continue"] = {}
                report["cases"]["host_key_continue"] = {}
                host_login_continuation(args.binary, args.root / "host-login-continue", env, state, addr, code,
                                        report["cases"]["host_login_continue"], keys, report["cases"]["host_key_continue"])
                state["vault"] = v4
            report["stage"] = "initial_authentication"
            version = cli("--version").stdout.decode()
            require(args.version in version, "binary_version_mismatch")
            initial_session = write_session(v4)
            cli("--direct", "--json", "remember", "Synthetic interruption ciphertext", "--title", "Interruption fixture", "--force", "--importance", "important")
            database = root / "rsrs.db"

            def ciphertexts():
                with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
                    return db.execute("SELECT id,ciphertext,nonce FROM memories ORDER BY id").fetchall()

            original_rows = ciphertexts()
            require(len(original_rows) == 1, "ciphertext_fixture_missing")
            cli("--runtime-internal", "--stop")
            # Check rollback from the current host state; historical compatibility is database-only.
            initial_session, config_bytes = session.read_bytes(), config.read_bytes()
            recovery_path = root / ".login-recovery.json"
            require(not recovery_path.exists(), "failed_login_requires_clean_journal")
            rejected = json.loads(cli("--json", "login", "--interactive", "--addr", addr, "--user", "synthetic",
                "--pass=synthetic-password", "--super=wrong", success=False).stdout)
            require(session.read_bytes() == initial_session and config.read_bytes() == config_bytes,
                    "failed_login_original_profile_not_restored")
            # Resolve only this failed login's original operation; do not replay authentication.
            report["failed_login_diagnostic"] = safe_cli_diagnostic(json.dumps(rejected).encode(), b"")
            require(recovery_path.is_file(), "failed_login_recovery_binding_invalid")
            journal = json.loads(recovery_path.read_bytes())
            active = journal.get("active") or {}
            operation_id = rejected.get("details", {}).get("operation_id")
            require(rejected.get("summary", {}).get("reason") == "account_outcome_unknown"
                    and any(error.endswith("; login_kind=vault_unlock_rejected") for error in rejected.get("errors", []))
                    and active.get("operation_id") == operation_id
                    and re.fullmatch(r"login-[a-f0-9]{32}", active.get("alias") or ""),
                    "failed_login_recovery_binding_invalid")
            rejected_alias = active["alias"]
            native_aliases.add(rejected_alias)
            records = [row for row in journal["operations"] if row["operation_id"] == operation_id]
            require(len(records) == 1 and records[0]["profile"] == str(root.resolve())
                    and records[0]["account"]["user"] == "synthetic" and records[0]["account"]["addr"] == addr,
                    "original_operation_binding_changed")
            rejected_record = records[0]
            logins_before = state["logins"]
            rollback = cli(*account_recovery_flags("account-rollback", rejected_record), success=False)
            rolled_back = json.loads(rollback.stdout)
            receipt = rolled_back["summary"]
            require(rollback.returncode == 1 and rolled_back.get("status") == "fail"
                    and receipt["operation_id"] == operation_id
                    and receipt["outcome"] == "rolled_back"
                    and state["logins"] == logins_before, "original_rollback_not_confirmed")
            journal = json.loads(recovery_path.read_bytes())
            require(len(journal["operations"]) == 1, "original_operation_not_unique")
            terminal_operation(journal, rejected_record, "rolled_back")
            require(session.read_bytes() == initial_session and config.read_bytes() == config_bytes,
                    "failed_login_original_profile_not_restored")
            if keys:
                for slot in ("super", "pass"):
                    require(not list(keys.collection.search_items(keys.attributes("rsrs", slot + ":" + rejected_alias))),
                            "failed_login_interrupted_credential_not_deleted")
            require(ciphertexts() == original_rows, "failed_login_original_ciphertexts_changed")
            cli("--runtime-internal", "--stop")
            report["cases"]["failed_login_explicit_rollback"] = {
                "passed": True, "current_journal_schema": True, "original_profile_restored": True,
                "ciphertext_preserved": True, "credential_mode": "secret_service" if keys else "headless"}
            # Recovery must unlock the legacy candidate, not the initial v4 fixture.
            env["RSRS_SUPER"] = old_super
            for barrier in ("before_publish", "after_publish"):
                report["stage"] = barrier
                state.update(vault=dict(v2), auth_salt=old_auth_salt, gets=0, posts=0,
                             logins=0, password_posts=0, barrier=barrier)
                # Transaction barriers start after full library conversion, which is
                # exercised independently by the real profile migration fixture.
                legacy_bytes = write_session(migrated_v2)
                config.write_bytes(config_bytes)
                reached.clear()
                release.clear()
                command_env = dict(env)
                if keys:
                    command_env.pop("RSRS_SUPER")
                else:
                    command_env["RSRS_SUPER"] = old_super
                child = subprocess.Popen([str(args.binary), *migration], env=command_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
                require(reached.wait(120), "commit_barrier_not_reached")
                # Native credential names belong to the host custody record, not Runtime's recovery alias.
                active = json.loads(recovery_path.read_bytes())["active"]
                custody = json.loads((root / "runtime/autostart" / ("account-login-" + active["operation_id"] + ".json")).read_bytes())
                alias = custody["account"]
                require(custody["operation_id"] == active["operation_id"]
                        and alias == "login-" + uuid.UUID(active["operation_id"]).hex
                        and Path(custody["root"]).resolve() == root.resolve()
                        and Path(custody["source"]).resolve() == root.resolve(),
                        "unowned_fixture_login_custody")
                if keys:
                    require(custody["backend"] == "secret_service", "native_backend_not_used")
                    require(re.fullmatch(r"login-[a-f0-9]{32}", alias or "") is not None, "native_alias_missing")
                    native_aliases.add(alias)
                require(json.loads(session.read_bytes())["wrapped_urk"].startswith(legacy.PREFIX)
                    and json.loads(session.read_bytes())["vault_version"] == 2, "original_vault_factors_changed")
                require((root / ".login-recovery.json").is_file(), "durable_recovery_missing")
                if keys:
                    require(json.loads(session.read_bytes()).get("keyring_backend") == "secret_service", "native_backend_not_used")
                    attributes = {"service": "rsrs", "username": "super:" + alias}
                    items = list(keys.collection.search_items(attributes))
                    require(len(items) == 1 and items[0].get_secret().decode() == old_super, "native_credential_not_written")
                    password_attributes = {"service": "rsrs", "username": "pass:" + alias}
                    passwords = list(keys.collection.search_items(password_attributes))
                    require(len(passwords) == 1 and passwords[0].get_secret().decode() == "synthetic-password", "original_password_not_written")
                marker = root / "runtime/autostart/maintenance.json"
                owner = json.loads(marker.read_bytes())["token"]
                journal_bytes = recovery_path.read_bytes()
                record = original_operation(json.loads(journal_bytes), root, barrier, addr)
                require(bytes(json.loads(journal_bytes)["active"]["original"]) == legacy_bytes
                        and bytes(json.loads(journal_bytes)["active"]["config"]) == config_bytes,
                        "original_recovery_image_changed")
                report["cases"][barrier + "_interrupted"] = {
                    "phase": record["phase"], "publication_pending": barrier == "after_publish",
                    "operation_id": record["operation_id"], "passed": False}
                # The CLI delegates private effects to control children. Interrupt the
                # whole freshly owned session, not just the CLI and leave an orphan POST.
                report["cases"][barrier + "_interrupted"]["process_exit"] = interrupt_owned_control(child)
                child = None
                release.set()
                state["barrier"] = None
                require(state["posts"] == (0 if barrier == "before_publish" else 1), "publication_window_not_proven")
                interrupted_session, interrupted_config = session.read_bytes(), config.read_bytes()
                require(recovery_path.read_bytes() == journal_bytes, "interrupted_operation_advanced")
                effect_counts = state["logins"], state["posts"], state["password_posts"]
                # OperationPending is NOT legacy recovery. A fresh rejected login must
                # preserve the candidate, journal and maintenance owner without replay.
                cli("login", "--interactive", "--addr", addr, "--user", "synthetic", "--pass=synthetic-password", "--super=wrong", success=False)
                require((session.read_bytes(), config.read_bytes(), recovery_path.read_bytes())
                        == (interrupted_session, interrupted_config, journal_bytes), "fresh_login_replaced_original_operation")
                require((state["logins"], state["posts"], state["password_posts"]) == effect_counts,
                        "fresh_login_replayed_authentication")
                require(json.loads(marker.read_bytes())["token"] == owner,
                        "fresh_login_replaced_maintenance_owner")
                if barrier == "before_publish":
                    # Querying AwaitingVerification would complete it. Explicit rollback
                    # is permitted by the original receipt before any publication starts.
                    rollback = cli(*account_recovery_flags("account-rollback", record), success=False)
                    rolled_back = json.loads(rollback.stdout)
                    receipt = rolled_back["summary"]
                    require(rollback.returncode == 1 and rolled_back.get("status") == "fail"
                            and receipt["operation_id"] == record["operation_id"]
                            and receipt["outcome"] == "rolled_back",
                            "original_rollback_not_confirmed")
                    terminal_operation(json.loads(recovery_path.read_bytes()), record, "rolled_back")
                    require(session.read_bytes() == legacy_bytes and config.read_bytes() == config_bytes,
                            "original_profile_not_restored")
                    if keys:
                        require(not list(keys.collection.search_items(attributes)), "interrupted_native_credential_not_deleted")
                        require(not list(keys.collection.search_items(password_attributes)), "interrupted_native_password_not_deleted")
                    require((state["logins"], state["posts"], state["password_posts"]) == effect_counts,
                            "rollback_replayed_authentication")
                    queried = cli(*account_recovery_flags("account-result", record), success=False)
                    confirmed = json.loads(queried.stdout)
                    require(queried.returncode == 1 and confirmed.get("status") == "fail"
                            and confirmed["summary"]["outcome"] == "rolled_back",
                            "rollback_receipt_not_retained")
                    # A fresh migration is safe only after that original terminal rollback.
                    report["stage"] = "before_publish_retry_migration"
                    cli(*migration)
                    require(state["logins"] == effect_counts[0] + 1, "explicit_retry_login_count_changed")
                else:
                    # The POST took effect but its response was lost. Compensation must
                    # fail and retain keys. Only the original result can reconcile it.
                    cli(*account_recovery_flags("account-rollback", record), success=False)
                    require((session.read_bytes(), config.read_bytes(), recovery_path.read_bytes())
                            == (interrupted_session, interrupted_config, journal_bytes), "unknown_rollback_changed_material")
                    if keys:
                        require(len(list(keys.collection.search_items(attributes))) == 1
                                and len(list(keys.collection.search_items(password_attributes))) == 1,
                                "unknown_rollback_deleted_credentials")
                    # An unconfirmed observation is not success or permission to retry.
                    # Model a stale server read while retaining the actual POST effect.
                    state["stale_vault_read"] = True
                    cli(*account_recovery_flags("account-result", record), success=False)
                    require((session.read_bytes(), config.read_bytes(), recovery_path.read_bytes())
                            == (interrupted_session, interrupted_config, journal_bytes), "unknown_result_changed_material")
                    require(json.loads(marker.read_bytes())["token"] == owner,
                            "unknown_result_released_maintenance")
                    require((state["logins"], state["posts"], state["password_posts"]) == effect_counts,
                            "unknown_result_replayed_effect")
                    state["stale_vault_read"] = False
                    report["cases"][barrier + "_interrupted"]["unknown_retained_maintenance"] = True
                    receipt = json.loads(cli(*account_recovery_flags("account-result", record)).stdout)["summary"]
                    require(receipt["operation_id"] == record["operation_id"] and receipt["outcome"] == "committed",
                            "original_operation_not_committed")
                    terminal_operation(json.loads(recovery_path.read_bytes()), record, "committed")
                    require((session.read_bytes(), config.read_bytes()) == (interrupted_session, interrupted_config),
                            "original_candidate_changed_during_completion")
                    require(state["logins"] == effect_counts[0], "result_replayed_authentication")
                    committed = session.read_bytes(), config.read_bytes(), recovery_path.read_bytes()
                    repeated = json.loads(cli(*account_recovery_flags("account-result", record)).stdout)["summary"]
                    require(repeated["operation_id"] == record["operation_id"] and repeated["outcome"] == "committed",
                            "original_result_not_repeatable")
                    require((session.read_bytes(), config.read_bytes(), recovery_path.read_bytes()) == committed,
                            "repeated_result_changed_material")
                require(state["vault"]["version"] == 2 and state["posts"] == 1
                        and state["password_posts"] == 1, "migration_effects_not_exactly_once")
                vault = state["vault"]
                require(AESGCM(legacy.legacy_kek(2, old_super, "", vault["kdf_salt"], current=True)).decrypt(
                    bytes.fromhex(vault["urk_nonce"]), legacy.encrypted_bytes(vault["wrapped_urk"]), None) == urk, "data_key_changed")
                require(ciphertexts() == original_rows, "original_ciphertexts_changed")
                require(json.loads(config.read_bytes()) == json.loads(config_bytes), "API_config_changed")
                cli("--runtime-internal", "--stop")
                report["cases"][barrier + "_interrupted"].update({
                    "passed": True, "ciphertext_preserved": True,
                    "original_profile_restored": barrier == "before_publish",
                    "original_operation_completed": barrier == "after_publish",
                    "no_authentication_replay": True, "vault_posts": state["posts"],
                    "password_posts": state["password_posts"],
                    "credential_mode": "secret_service" if keys else "headless"})
            env["RSRS_SUPER"] = code
            report["stage"] = "legacy_recovery"
            # DEV.4 has no journal. Preserve its inconsistent profile and recover by
            # importing the actual original library through the public migration API.
            state.update(vault=dict(v2), gets=0, posts=0, barrier=None)
            write_session(v4)
            before = session.read_bytes()
            wrong = list(migration)
            wrong[-1] = "A3-000000-000000-000000-000000-000000-000000"
            def reject_legacy_migration(flags):
                previous_ids = {row["operation_id"] for row in json.loads(recovery_path.read_bytes())["operations"]}
                rejected = json.loads(cli(*flags, success=False).stdout)
                operation_id = rejected.get("details", {}).get("operation_id")
                require(isinstance(operation_id, str) and str(uuid.UUID(operation_id)) == operation_id
                        and uuid.UUID(operation_id).version == 4 and operation_id not in previous_ids
                        and rejected.get("summary", {}).get("reason") == "account_outcome_unknown"
                        and rejected["details"].get("source_profile") == str(root.resolve()),
                        "original_operation_binding_changed")
                journal = json.loads(recovery_path.read_bytes())
                rows = [row for row in journal["operations"] if row["operation_id"] == operation_id]
                require(journal.get("schema_version") == 2 and len(rows) == 1,
                        "original_operation_not_unique")
                record = rows[0]
                active = journal.get("active") or {}
                account = record.get("account") or {}
                login = account.get("login") or {}
                require(record["idempotency_key"] == operation_id and record["root"] == str(root.resolve())
                        and record["profile"] == str(root.resolve()) and active.get("operation_id") == operation_id
                        and active.get("alias") == record["alias"]
                        and re.fullmatch(r"login-[a-f0-9]{32}", record["alias"]) is not None
                        and account.get("register") is False and account.get("user") == "synthetic"
                        and account.get("addr") == addr and login.get("migrate_vault") is True
                        and all(login.get(key) is False for key in
                            ("publication_pending", "publication_done", "vault_done", "password_done")),
                        "original_operation_binding_changed")
                native_aliases.add(record["alias"])
                effects = state["logins"], state["posts"], state["password_posts"]
                # Each negative case retains its own operation; retire it through the public rollback API.
                rolled_back = json.loads(cli(*account_recovery_flags("account-rollback", record), success=False).stdout)
                require(rolled_back.get("status") == "fail" and rolled_back["summary"]["operation_id"] == operation_id
                        and rolled_back["summary"]["outcome"] == "rolled_back",
                        "original_rollback_not_confirmed")
                terminal_operation(json.loads(recovery_path.read_bytes()), record, "rolled_back")
                require((state["logins"], state["posts"], state["password_posts"]) == effects,
                        "rollback_replayed_authentication")

            report["stage"] = "legacy_recovery_reject_wrong_super"
            reject_legacy_migration(wrong)
            require(session.read_bytes() == before and state["posts"] == 0, "wrong_recovery_modified_state")
            report["stage"] = "legacy_recovery_reject_inconsistent_material"
            reject_legacy_migration(migration)
            require(session.read_bytes() == before and state["posts"] == 0, "inconsistent_factors_modified_state")
            if keys is None:
                key_spec = importlib.util.spec_from_file_location("migration_keys", Path(__file__).with_name("dev-migration-keyring.py"))
                key_module = importlib.util.module_from_spec(key_spec)
                key_spec.loader.exec_module(key_module)
                migration_keys = key_module.create(args.root / "original-profile-keyring")
                migration_keys.configure_env(env)
                env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(args.root / "missing-secret-service.sock")
            else:
                migration_keys = keys
            source = home / ".onememory"
            source.mkdir(mode=0o700)
            source_database = source / "onememory.db"
            with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as original, sqlite3.connect(source_database) as backup:
                original.backup(backup)
                register_witness_functions(backup)
                for memory_id, ciphertext, old_nonce in original_rows:
                    plaintext = legacy.decrypt_content(urk, ciphertext, old_nonce)
                    old_nonce = secrets.token_bytes(12)
                    old_ciphertext = AESGCM(legacy.hkdf(urk, None, b"onememory:data:v1")).encrypt(old_nonce, plaintext, None).hex()
                    backup.execute("UPDATE memories SET ciphertext=?,nonce=?,embedding_enc='' WHERE id=?", (old_ciphertext, old_nonce.hex(), memory_id))
                    backup.execute("UPDATE core_artifacts SET source=? WHERE memory_id=?", (old_ciphertext, memory_id))
                fixture_witness = verify_fixture_witness(backup)
            backup.close()
            original.close()
            original_source_session = {"user": "synthetic", "addr": addr, "token": "synthetic-token", "vault_version": 2,
                                       **{k: v for k, v in v2.items() if k != "version"},
                                       "super": old_super, "pass": "synthetic-password"}
            source_session = source / "session.json"
            source_session.write_text(json.dumps(original_source_session), encoding="utf-8")
            source_session.chmod(0o600)
            (source / "client.json").write_text(json.dumps({"data_dir": str(source)}), encoding="utf-8")
            if (root / "core-index").is_dir():
                shutil.copytree(root / "core-index", source / "core-index")
            source_before = legacy.digest(source_database), source_session.read_bytes()
            alias = "legacy-" + hashlib.sha256(str(source.resolve()).replace("\\", "/").encode()).hexdigest()[:16]
            native_aliases.add(alias)
            for slot in ("super", "pass"):
                migration_keys.reserve("rsrs", slot + ":" + alias)
            report["stage"] = "legacy_recovery_discover_profiles"
            discovered = json.loads(cli("--json", "migrate").stdout)["details"]["profiles"]
            candidates = [row for row in discovered if Path(row["source"]).resolve() == source.resolve()]
            require(len(candidates) == 1, "original_profile_not_discovered")
            report["stage"] = "legacy_recovery_import_profile"
            result = json.loads(cli("--json", "migrate", "--source", candidates[0]["source_id"], "--account", "recovered-original").stdout)["summary"]
            require(result["state"] == "migrated", "original_profile_not_migrated")
            restored_root = Path(result["dir"])
            require((legacy.digest(source_database), source_session.read_bytes()) == source_before, "original_source_changed")
            require(session.read_bytes() == before, "inconsistent_profile_overwritten")
            restored_session = json.loads((restored_root / "session.json").read_text())
            require(restored_session["vault_version"] == 2 and restored_session["wrapped_urk"].startswith(legacy.PREFIX), "original_factors_not_recovered")
            with sqlite3.connect(restored_root / "rsrs.db") as restored:
                restored_rows = restored.execute("SELECT id,ciphertext,nonce FROM memories ORDER BY id").fetchall()
            require(len(restored_rows) == len(original_rows) and all(ciphertext.startswith(legacy.PREFIX) for _, ciphertext, _ in restored_rows), "full_library_namespace_not_migrated")
            require([legacy.decrypt_content(urk, ciphertext, nonce) for _, ciphertext, nonce in restored_rows]
                    == [legacy.decrypt_content(urk, ciphertext, nonce) for _, ciphertext, nonce in original_rows], "original_plaintext_not_recovered")
            require(AESGCM(legacy.legacy_kek(2, old_super, "", restored_session["kdf_salt"], current=True)).decrypt(
                bytes.fromhex(restored_session["urk_nonce"]), legacy.encrypted_bytes(restored_session["wrapped_urk"]), None) == urk,
                "data_key_changed")
            report["stage"] = "legacy_recovery_select_profile"
            report["profile_selection_probe"] = {
                "installation_matches_fixture": Path(env["RSRS_DATA_DIR"]).resolve() == root.resolve(),
                "runtime_inside_installation": Path(env["RSRS_RUNTIME_BIN"]).resolve().is_relative_to(root.resolve()),
                "selection_before_switch": Path(json.loads(config.read_bytes())["data_dir"]).resolve() == root.resolve(),
                "target_inside_installation": restored_root.resolve().is_relative_to(root.resolve()),
                "target_alias_matches_migration": restored_session.get("keyring_account") == alias,
                "target_backend": restored_session.get("keyring_backend") in ("native", "secret_service", "kernel"),
                "target_inline_factors": any(key in restored_session for key in ("super", "pass", "secret", "secret_key")),
                "target_primary_credential": migration_keys._read("rsrs", "super:" + alias) is not None,
            }
            selected = json.loads(cli("--json", "account", "use", "recovered-original").stdout)["summary"]
            require(Path(selected["dir"]).resolve() == restored_root.resolve(), "original_profile_selection_failed")
            profiles = json.loads(cli("--json", "account", "list").stdout)["details"]["accounts"]
            selected_profiles = [row for row in profiles if row["current"]]
            require(len(selected_profiles) == 1 and selected_profiles[0]["name"] == "recovered-original"
                    and Path(selected_profiles[0]["dir"]).resolve() == restored_root.resolve(),
                    "original_profile_selection_failed")
            report["stage"] = "legacy_recovery_migrate_vault"
            cli(*migration)
            if fixture_witness is not None:
                report["stage"] = "legacy_recovery_verify_witness"
                chain = json.loads(cli("--direct", "--json", "witness", "--verify").stdout)
                require(chain["status"] == "ok" and chain["summary"]["intact"] is True
                        and chain["summary"]["count"] > fixture_witness["count"], "migrated_fixture_witness_invalid")
                report["fixture_witness"] = {"python_pre_migration": fixture_witness,
                                             "rust_post_migration": chain["summary"]}
            require(state["vault"]["version"] == 2 and ciphertexts() == original_rows, "DEV4_recovery_failed")
            report["cases"]["pre_journal_DEV4_recovery"] = {"passed": True, "wrong_code_rejected": True,
                "inconsistent_factors_rejected": True, "public_original_profile_migration": True,
                "full_library_reencrypted": True, "original_source_preserved": True,
                "original_factors_preserved": True, "requires_original_source": True}
            # Transfer to a different server identity and URK without plaintext exports.
            verify_server_transfer(cli, report, state, server, code, urk, original_rows)
            report["passed"] = True
        except BaseException as error:
            report["operation_failure_code"] = (str(error) if type(error) is RuntimeError
                and str(error) in FAILURE_CODES else "unclassified_failure")
            raise
        finally:
            cleanup_fixture(args, report, root, child, state, release, env, keys,
                            migration_keys, native_aliases, server)
        require(report["runtime_cleanup"]["passed"], "owned_runtime_cleanup_failed")
        if migration_keys or keys:
            require(report["native_cleanup"]["passed"], "native_fixture_cleanup_failed")
        report["stage"] = "complete"
        print(json.dumps(report))


if __name__ == "__main__":
    try:
        if "--gui-recovery-server" in sys.argv:
            gui_recovery_server()
        else:
            main()
    except Exception:
        # Details are available only as bounded codes in the owned report.
        raise SystemExit("login_interruption_fixture_failed") from None
