#!/usr/bin/env python3
"""Hosted CI: legacy encrypted databases with current credentials and explicit DEV configuration."""
import argparse
import base64
from contextlib import closing, contextmanager
from dev_migration_diagnostics import migration_failure_diagnostics, migration_execution_state, script_failure_location
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import sqlite3
from dev_witness_fixture import register_witness_functions, verify_fixture_witness, sqlite_failure_details
import subprocess
import sys
import time
import uuid
from native_fixture_process import canonical_scope_path, scoped_rpc_request, verify_rpc_receipt

from profile_migration_fixture_context import spec, support, keyring_script, keyring_spec, keyring_backend, require, digest, REQUIRED


from profile_migration_scenarios import ProfileMigrationScenarios


class Smoke(ProfileMigrationScenarios, support.Smoke):
    def __init__(self, args):
        super().__init__(args)
        self.keys = None
        self.wal_connections = []
        self.report.update(required_cases=list(REQUIRED), cases={}, passed=False,
            scope="Legacy encrypted databases; current host credentials and explicit DEV configuration; synthetic data only.",
            platform=sys.platform, keyring_cleanup={"passed": False, "remaining_entries": []})

    def env(self, name, user=None):
        env = super().env(name, user)
        if self.keys is not None:
            self.keys.configure_env(env)
        return env

    def reserve_account(self, user):
        for slot in ("super:", "pass:"):
            self.keys.reserve("rsrs", slot + user)

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

    def registration_diagnostics(self, env, text):
        result = {"journal_status": "not_found"}
        try:
            root = Path(env["RSRS_DATA_DIR"]).resolve(strict=True)
            require(root.is_relative_to(self.root.resolve(strict=True)), "registration_root_outside_fixture")
            path = root / ".login-recovery.json"
            if path.is_symlink():
                return {"journal_status": "link_rejected"}
            if not path.exists():
                return result
            operation = re.search(r"\baccount_outcome_unknown operation_id=([0-9a-f-]{36})\b", text)
            if operation is None:
                return {"journal_status": "operation_binding_missing"}
            journal = json.loads(path.read_text(encoding="utf-8"))
            records = [record for record in journal["operations"]
                if record["operation_id"] == operation[1]]
            if len(records) != 1:
                return {"journal_status": "operation_not_unique"}
            record = records[0]
            phases = ("admitted", "login_input", "remote_pending", "remote_unknown", "authorized",
                "prepared", "awaiting_verification", "rolling_back", "committed", "rolled_back")
            if record["phase"] not in phases or record["account"]["register"] is not True:
                return {"journal_status": "registration_record_invalid"}
            account = record["account"]
            result.update(journal_status="matched", phase=record["phase"],
                root_matches=Path(record["root"]).resolve(strict=True) == root,
                source_selected=Path(record["profile"]).resolve(strict=True) == self.profile(env),
                authorization_recorded=account.get("authorization") is not None,
                target_recorded=account.get("target") is not None,
                vault_pending=account.get("vault_pending") is True,
                published_vault=account.get("published_vault") is True)
        except (OSError, ValueError, TypeError, KeyError, RuntimeError):
            # Keep the original failure fatal; never copy journal values or exception text.
            result = {"journal_status": "diagnostic_unavailable"}
        return result

    def cli(self, env, *args, timeout=180):
        started = time.monotonic()
        try:
            output = subprocess.run([str(self.args.binary), "--direct", "--json", *args],
                cwd=self.root, env=env, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            self.report["cli_failure"] = {"command": args[0], "timeout_seconds": timeout,
                "error_class": "timeout", "has_source": "--source" in args, "has_account": "--account" in args}
            self.save()
            raise
        finally:
            elapsed_ms = round((time.monotonic() - started) * 1000)
            stopped = subprocess.run([str(self.args.binary), '--runtime-internal', '--stop'],
                cwd=self.root, env=env, capture_output=True, timeout=60)
            require(stopped.returncode in (0, 2), 'owned_runtime_stop_failed')
        value = None
        try:
            value = json.loads(output.stdout)
        except (ValueError, UnicodeError):
            pass
        # An empty destination has no key material; Runtime reports this as warn/2.
        locked_status = (args == ("status",) and output.returncode == 2
            and isinstance(value, dict) and value.get("command") == "status"
            and value.get("status") == "warn" and not value.get("errors")
            and isinstance(value.get("summary"), dict)
            and value["summary"].get("unlocked") is False
            and value["summary"].get("store_ready") is False
            and value["summary"].get("credential_state") == "required"
            and isinstance(value["summary"].get("data_dir"), str)
            and keyring_backend.canonical_identity(value["summary"]["data_dir"])
                == keyring_backend.canonical_identity(env.get("RSRS_DATA_DIR") or Path(env["HOME"]) / ".rsrs"))
        failed = (output.returncode != 0 and not locked_status) or not isinstance(value, dict) \
            or value.get("errors") or value.get("status") in ("error", "failed")
        if failed:
            text = (output.stdout + output.stderr).decode("utf-8", errors="replace").lower()
            categories = (
                ("argument_contract", ("unexpected argument", "required arguments were not provided", "a value is required")),
                ("host_required", ("client_only: explicit legacy migration is host-managed",)),
                ("account_control_unknown", ("control_outcome_unknown",)),
                ("host_control", ("host control deadline exceeded", "missing host control response frame", "migration_outcome_unknown", "migration response binding mismatch", "migration response profile mismatch")),
                ("profile_discovery", ("migration discovery did not return profiles", "legacy profile has no name", "profile root is not a directory", "invalid session metadata file")),
                ("discovery_path", ("discovery path contains parent traversal", "discovery path contains a symlink", "discovery path contains a reparse point", "discovery ancestor is not a directory")),
                ("rate_limited", ("too many requests", "rate limit", "http 429", "status: 429")),
                ("native_credentials", ("keyring", "keychain", "credential manager", "osstatus")),
                ("vault_unlock", ("failed to decrypt", "vault could not be unlocked", "super password required", "secret key required")),
                ("entry_not_found", ("not found #", "memory not found")),
                ("http_transport", ("connection refused", "connection timed out", "network is unreachable", "tls")),
                ("legacy_vault_login", ("this account uses a legacy vault",)),
                ("vault_fetch", ("could not fetch the account vault", "vault missing")),
                ("vault_unlock", ("super password does not unlock this account",)),
                ("local_key_mismatch", ("local library uses different key material", "local library key compatibility could not be verified")),
                ("login_destination", ("target account session", "destination belongs to another account", "destination must not be a symbolic link")),
                ("runtime_readiness", ("did not become ready", "exited before readiness", "runtime owner changed", "runtime is not running")),
                ("runtime_account_verification", ("runtime account verification failed", "runtime account list missing accounts", "runtime did not report a selected account", "runtime account or directory differs from the authorized account")),
                ("library_lock", ("library is locked", "database is locked")),
                ("server_auth", ("http 401", "status: 401", "unauthorized", "invalid password", "invalid credentials")),
                ("native_credential_cleanup", ("owned_runtime_stop_failed",)),
            )
            category = next((name for name, terms in categories if any(term in text for term in terms)), "unclassified")
            self.report["cli_failure"] = {"command": args[0], "exit_code": output.returncode,
                "error_class": category, "json_envelope": isinstance(value, dict),
                "has_source": "--source" in args, "has_account": "--account" in args,
                "error": self.redacted_cli_error(env, output), "elapsed_ms": elapsed_ms}
            if args[0] == "register" and category == "account_control_unknown":
                self.report["cli_failure"]["account_control"] = self.registration_diagnostics(env, text)
            self.report["cli_failure"].update(migration_failure_diagnostics(value))
            migration_stages = ("paused_grant", "owner_check", "drain_owner", "drained_check",
                "selection_before_send", "save_sent", "exchange", "encode_unknown", "selection_after_receipt",
                "restore_owner_start", "restore_owner_probe", "restore_owner_verify", "restore_store_ready",
                "selection_before_finish", "finish_maintenance", "save_terminal", "paused_publication")
            errors = value.get("errors") if isinstance(value, dict) else None
            stage = re.search(r"\bmigration_stage=([a-z_]+)(?=$|[;\s])", "\n".join(
                error for error in errors if isinstance(error, str)) if isinstance(errors, list) else "")
            self.report["cli_failure"]["migration_stage"] = stage[1] if stage and stage[1] in migration_stages else None
            exchange_categories = ("spawn", "channel", "frame", "encoding", "deadline", "child_exit",
                "response_binding", "response_profile", "response_envelope", "response_readiness", "result_identity",
                "unknown_receipt", "terminal_receipt", "terminal_details", "unclassified", "runtime_invalid_request",
                "runtime_protocol_version_mismatch", "runtime_control_unavailable", "runtime_auth_recovery_pending",
                "runtime_unauthorized", "runtime_auth_ownership_unavailable", "runtime_scope_mismatch",
                "runtime_migration_outcome_unknown", "runtime_control_outcome_unknown")
            exchange = re.search(r"\bmigration_exchange=([a-z_]+)(?=$|[;\s])", "\n".join(
                error for error in errors if isinstance(error, str)) if isinstance(errors, list) else "")
            self.report["cli_failure"]["migration_exchange"] = exchange[1] if stage and stage[1] == "exchange" \
                and exchange and exchange[1] in exchange_categories else None
            if args[0] == "migrate" and self.report["cli_failure"]["migration_stage"] == "exchange":
                self.report["cli_failure"]["execution_state"] = migration_execution_state(self.root, env, args, value)
            self.report["cli_failure"]["runtime_signals"] = [name for name, term in (
                ("child_exited", "host runtime exited before readiness"),
                ("startup_identity_changed", "runtime owner changed during startup"),
                ("readiness_timeout", "host runtime did not become ready"),
                ("old_runtime_restart_failed", "original runtime restart failed"),
                ("spawn_failed", "failed to start host runtime"),
            ) if term in text]
            if args[0] == "sync" and isinstance(value, dict):
                summary = value.get("summary", {})
                self.report["cli_failure"]["sync_counts"] = {
                    key: summary[key] for key in ("local_total", "local_alive", "remote_total", "remote_alive",
                        "pending", "conflicts", "undecodable", "protocol", "total_matched", "converged")
                    if key in summary and type(summary[key]) in (int, bool)}
            self.save()
            reason = "cli_failed_" if output.returncode != 0 else \
                "invalid_cli_json_" if not isinstance(value, dict) else "cli_error_"
            raise RuntimeError(reason + args[0])
        return value

    def verify_retained_current_credentials(self, session, fixture):
        # Only the original, already tracked login alias owns these credentials.
        alias = session.get("keyring_account")
        require(session.get("user") == fixture["account"]["user"]
            and isinstance(alias, str) and re.fullmatch(r"login-[a-f0-9]{32}", alias)
            and alias == fixture["alias"], "existing_target_credential_identity_changed")
        require(self.keys.read("rsrs", "super:" + alias) == fixture["code"]
            and self.keys.read("rsrs", "pass:" + alias) == fixture["password"],
            "existing_target_native_credentials_overwritten")

    def reserve_alias(self, source):
        alias = "legacy-" + keyring_backend.canonical_identity(source)[:16]
        for slot in ("super:", "pass:", "legacy-secret:"):
            self.keys.reserve("rsrs", slot + alias)
        return alias

    def verify_mac_native_credentials(self, env, fixture):
        """Consume owned native credentials through the unchanged CLI creator.

        Verify the current native key and original ciphertext without synthesizing
        an old session, performing automatic recovery, or changing item ACLs.
        """
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        clean_env = dict(env)
        for key in ("RSRS_SUPER", "RSRS_PASS", "RSRS_SECRET",
                    "RSRS_KDF_SALT", "RSRS_WRAPPED_URK", "RSRS_URK_NONCE"):
            clean_env.pop(key, None)
        profile = Path(clean_env["RSRS_DATA_DIR"])
        keyring_backend.owned_path(profile)
        session_path = profile / "session.json"
        require(session_path.is_file() and not session_path.is_symlink(), "native_consumer_session_missing")
        original = session_path.read_bytes()
        session = json.loads(original)
        require(session.get("user") == fixture["account"]["user"]
            and session.get("vault_version") == 4
            and all(not session.get(key) for key in ("pass", "super", "secret", "secret_key"))
            and "legacy_secret_backend" not in session,
            "native_consumer_session_has_override")
        alias = session.get("keyring_account")
        require(isinstance(alias, str) and ("rsrs", "pass:" + alias) in self.keys.entries,
                "native_consumer_credential_alias_not_owned")
        note = self.root / ("native-recovery-" + secrets.token_hex(8) + ".txt")
        require(not note.exists(), "native_consumer_export_not_fresh")
        try:
            self.report["fixture_operation"] = "native_consumer_super_export"
            self.save()
            self.cli(clean_env, "keys-export", "--out", str(note))
            require(note.is_file() and not note.is_symlink(), "native_consumer_export_missing")
            exported = re.findall(r"^super password: (.+)$", note.read_text(encoding="utf-8"), re.MULTILINE)
            require(exported == [fixture["code"]], "native_consumer_super_changed")
            urk = AESGCM(support.v4_kek(fixture["code"], session["kdf_salt"], session["wrapped_urk"])).decrypt(
                bytes.fromhex(session["urk_nonce"]), support.encrypted_bytes(session["wrapped_urk"]), None)
            require(len(urk) == 32, "native_consumer_urk_invalid")
            self.report["fixture_operation"] = "native_consumer_current_show"
            self.save()
            shown = self.cli(clean_env, "show", fixture["id"])
            require(shown.get("details", {}).get("entry", {}).get("content") == fixture["text"],
                    "native_consumer_original_ciphertext_not_decrypted")
            require(session_path.read_bytes() == original, "native_consumer_session_changed")
        finally:
            try:
                session_path.write_bytes(original)
            finally:
                note.unlink(missing_ok=True)
        database = profile / ("rsrs.db" if (profile / "rsrs.db").exists() else "onememory.db")
        with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)) as db:
            row = db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (fixture["id"],)).fetchone()
            require(support.decrypt_content(urk, *row) == support.decrypt_content(urk, *fixture["cipher"]),
                    "native_consumer_content_changed")
        proof = self.report.setdefault("native_credential_consumer", {
            "platform": "darwin", "super": "cli_keys_export", "ciphertext": "cli_show",
            "consumer_changes_product_acl": False, "automatic_server_reauthentication_tested": False,
            "verified_profiles": 0})
        proof["verified_profiles"] += 1
        self.save()

    @contextmanager
    def selected_profile(self, default, fixture):
        profile = fixture["destination"]
        config_path = profile / "client.json"
        original = config_path.read_bytes() if config_path.exists() else None
        # A migrated config can select a sibling account; explicitly select this
        # profile for its isolated consumer check, then restore the original bytes.
        config = json.loads(original) if original is not None else {}
        root = Path(canonical_scope_path(Path(default["HOME"]) / ".rsrs"))
        selected = config.get("data_dir", str(profile))
        require(isinstance(selected, str) and bool(selected), "login_profile_selection_invalid")
        require(Path(canonical_scope_path(profile)).is_relative_to(root)
            and Path(canonical_scope_path(selected)).is_relative_to(root),
            "login_profile_selection_outside_fixture")
        config["data_dir"] = str(profile)
        try:
            config_path.write_text(json.dumps(config), encoding="utf-8")
            env = dict(default, RSRS_DATA_DIR=str(profile))
            support.install_published_runtime(self.args.binary, env)
            yield env
        finally:
            if original is None:
                config_path.unlink()
            else:
                config_path.write_bytes(original)
            require((config_path.read_bytes() if config_path.exists() else None) == original,
                "fixture_profile_selection_restore_failed")

    def save(self):
        (self.root / "migration-coverage.json").write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")

    def cleanup(self):
        recoveries = []
        for account in self.accounts:
            if account["confirmed"]:
                continue
            event = {"user": account["user"], "confirmed": False}
            try:
                # Authenticate with this fixture's generated credential; never
                # delete an account by prefix or an unconfirmed username.
                request = support.urllib.request.Request(support.UPSTREAM + "/login",
                    json.dumps({"user": account["user"], "pass_hash": account["pass_hash"],
                        "device_name": "migration-cleanup"}).encode(),
                    {"Content-Type": "application/json"}, method="POST")
                with self.opener.open(request, timeout=45) as response:
                    require(response.status == 200, "cleanup_login_not_confirmed")
                    raw = response.read(1024 * 1024 + 1)
                    require(len(raw) <= 1024 * 1024, "cleanup_login_response_too_large")
                    token = json.loads(raw).get("token")
                require(isinstance(token, str) and bool(token), "cleanup_login_token_missing")
                candidate = dict(account, confirmed=True, token=token)
                identity = self.api(candidate, "GET", "/api/self")
                require(identity.get("user") == account["user"], "cleanup_login_identity_mismatch")
                account.update(confirmed=True, token=token)
                event["confirmed"] = True
            except Exception as error:
                event["failure_code"] = str(error) if isinstance(error, RuntimeError) else type(error).__name__
            recoveries.append(event)
        self.report["cloud_cleanup"]["recovered_session_checks"] = recoveries
        super().cleanup()

    def default_env(self, env):
        result = dict(env)
        result.pop("RSRS_DATA_DIR", None)
        result.pop("RSRS_SUPER", None)
        # Switching to HOME/.rsrs also changes the managed Runtime owner.
        result.pop('RSRS_RUNTIME_BIN', None)
        return result

    def seed(self, env, path, service, label):
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        path.mkdir(parents=True, exist_ok=True)
        env = dict(env, RSRS_DATA_DIR=str(path))
        support.install_published_runtime(self.args.binary, env)
        user, password = "ci-migrate-" + secrets.token_hex(8), "ci-" + secrets.token_urlsafe(32)
        self.reserve_account(user)
        self.diagnostic_secrets.add(password)
        account = {"user": user, "token": None, "confirmed": False,
            "pass_hash": None}
        self.accounts.append(account)
        self.report["cloud_cleanup"]["remaining_users"].append(user)
        self.report["fixture_operation"] = "register"
        self.save()
        value = self.cli(env, "register", "--addr", support.UPSTREAM, "--user", user, "--pass", password)
        session = self.session(env)
        require(value["summary"].get("ok") is True and value["summary"].get("user") == user
            and session.get("user") == user and session.get("addr") == support.UPSTREAM
            and bool(session.get("token")), "migration_registered_identity_mismatch")
        account.update(token=session["token"], confirmed=True)
        code = value["summary"].get("super")
        require(isinstance(code, str) and bool(code), "migration_super_missing")
        self.diagnostic_secrets.update((code, session["token"]))
        self.report["fixture_operation"] = "verify_registered_native_credentials"
        self.save()
        self.keys.track_created_login(session, user, code, password)
        if self.keys.name == "linux-secret-service":
            self.keys.register_current_target(session, user)
        auth_salt = bytes.fromhex(self.api(account, "GET", "/auth/salt?user=" + support.urllib.parse.quote(user))["salt"].removeprefix(support.PREFIX))
        account["pass_hash"] = hashlib.pbkdf2_hmac("sha256", password.encode(), auth_salt, 100_000, 32).hex()
        self.diagnostic_secrets.add(account["pass_hash"])
        env["RSRS_SUPER"] = code
        self.report["fixture_operation"] = "seed_local_content"
        self.save()
        self.cli(env, "config", "--addr", support.UPSTREAM, "--autosync", "false")
        text = "Synthetic unsynchronized migration content " + label
        created = self.cli(env, "remember", text, "--title", label, "--force", "--importance", "important")
        memory_id = created["summary"]["id"]
        urk = AESGCM(support.v4_kek(code, session["kdf_salt"], session["wrapped_urk"])).decrypt(
            bytes.fromhex(session["urk_nonce"]), support.encrypted_bytes(session["wrapped_urk"]), None)
        db = path / "rsrs.db"
        if service != "rsrs":
            # Produce a genuine old-format fixture from this disposable account.
            db.rename(path / "onememory.db")
            db = path / "onememory.db"
            wrap_nonce = secrets.token_bytes(12)
            session["wrapped_urk"] = AESGCM(support.v4_kek(code, session["kdf_salt"])).encrypt(wrap_nonce, urk, None).hex()
            session["urk_nonce"] = wrap_nonce.hex()
            session.pop("crypto_namespace", None)
        connection = sqlite3.connect(db)
        register_witness_functions(connection)
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("PRAGMA wal_autocheckpoint=0")
        connection.execute("CREATE TABLE IF NOT EXISTS migration_wal_fixture(marker TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO migration_wal_fixture VALUES(?)", (label,))
        row = connection.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (memory_id,)).fetchone()
        require(row is not None, "migration_seed_memory_missing")
        payload = support.decrypt_content(urk, *row)
        if service != "rsrs":
            item_nonce = secrets.token_bytes(12)
            ciphertext = AESGCM(support.hkdf(urk, None, b"onememory:data:v1")).encrypt(item_nonce, payload, None).hex()
            connection.execute("UPDATE memories SET ciphertext=?,nonce=? WHERE id=?", (ciphertext, item_nonce.hex(), memory_id))
            connection.execute("UPDATE core_artifacts SET source=? WHERE memory_id=?", (ciphertext, memory_id))
            connection.execute("UPDATE sync_outbox SET ciphertext=?,nonce=? WHERE id=? AND state='pending'", (ciphertext, item_nonce.hex(), memory_id))
        tombstones = []
        if service != "rsrs":
            tombstone_time = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
            # All deleted payloads must also decrypt and be reencrypted.
            for _ in range(5):
                item_nonce = secrets.token_bytes(12)
                tombstone = (str(uuid.uuid4()), user,
                    AESGCM(support.hkdf(urk, None, b"onememory:data:v1")).encrypt(item_nonce, payload, None).hex(),
                    item_nonce.hex(), 1)
                connection.execute(
                    "INSERT INTO memories (id,user,ciphertext,nonce,deleted,dirty,created_at,updated_at) "
                    "VALUES (?,?,?,?,?,0,?,?)", tombstone + (tombstone_time, tombstone_time))
                tombstones.append(tombstone)
        connection.commit()
        verify_fixture_witness(connection)
        require(Path(str(db) + "-wal").stat().st_size > 0, "migration_wal_fixture_empty")
        row = connection.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (memory_id,)).fetchone()
        require(row and connection.execute("SELECT COUNT(*) FROM sync_outbox WHERE state='pending'").fetchone()[0] > 0,
            "migration_outbox_fixture_not_pending")
        self.wal_connections.append(connection)
        if sys.platform == "darwin" and service == "rsrs":
            self.verify_mac_native_credentials(env, {"account": account, "code": code, "password": password,
                "id": memory_id, "text": text, "cipher": row})
        if service == "rsrs":
            # This is the genuine current target, not a legacy namespace fixture.
            # Keep the CLI-created credentials and endpoint exactly as registered.
            self.report["fixture_operation"] = "retained_current_credentials"
            self.save()
            return {"account": account, "password": password, "code": code, "service": service,
                "source": path, "id": memory_id, "text": text, "label": label, "cipher": row, "alias": session["keyring_account"]}
        # Only the database uses the old namespace; keep the current host's
        # login alias and explicit endpoint, and supply its known data key at import.
        self.report["fixture_operation"] = "prepare_current_host_for_legacy_db"
        self.save()
        self.write_session(env, session)
        alias = self.reserve_alias(path)
        return {"account": account, "password": password, "code": code, "service": service,
            "source": path, "id": memory_id, "text": text, "label": label, "cipher": row, "alias": alias,
            "source_alias": session["keyring_account"],
            "legacy_tombstones": tombstones, "urk": urk, "payload": payload}

    def migrated(self, home, fixtures):
        root = home / ".rsrs"
        require(root.is_dir(), "migration_target_missing")
        sessions = {}
        for path in root.rglob("session.json"):
            value = json.loads(path.read_text())
            if value.get("user") in {f["account"]["user"] for f in fixtures}:
                require(value["user"] not in sessions, "migration_profile_duplicate")
                sessions[value["user"]] = (path, value)
        require(len(sessions) == len(fixtures), "migration_profiles_missing")
        for fixture in fixtures:
            user = fixture["account"]["user"]
            session_path, value = sessions[user]
            profile = session_path.parent
            require(value.get("addr") == support.UPSTREAM, "explicit_dev_endpoint_changed")
            receipt = json.loads((profile / ".rsrs-migration.json").read_text())
            require(receipt.get("namespace_migrated") is True, "full_migration_receipt_missing")
            require(value["wrapped_urk"].startswith(support.PREFIX), "migration_wrap_namespace_missing")
            source_session = json.loads((fixture["source"] / "session.json").read_text())
            require(source_session.get("keyring_account") == fixture["source_alias"]
                and source_session.get("addr") == support.UPSTREAM, "current_source_host_changed")
            if sys.platform != "darwin":
                require(self.keys.read("rsrs", "super:" + fixture["source_alias"]) == fixture["code"]
                    and self.keys.read("rsrs", "pass:" + fixture["source_alias"]) == fixture["password"],
                    "current_source_credentials_changed")
            with sqlite3.connect((fixture["source"] / "onememory.db").as_uri() + "?mode=ro", uri=True) as source_db:
                require(source_db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (fixture["id"],)).fetchone() == fixture["cipher"],
                    "legacy_source_ciphertext_changed")
                for tombstone in fixture["legacy_tombstones"]:
                    require(source_db.execute("SELECT id,user,ciphertext,nonce,deleted FROM memories WHERE id=?",
                        (tombstone[0],)).fetchone() == tombstone, "legacy_source_foreign_tombstone_changed")
            with sqlite3.connect((profile / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
                require(db.execute("PRAGMA integrity_check").fetchone()[0] == "ok", "migrated_database_invalid")
                converted = db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (fixture["id"],)).fetchone()
                require(converted != fixture["cipher"] and converted[0].startswith(support.PREFIX)
                    and support.decrypt_content(fixture["urk"], *converted) == fixture["payload"], "migration_reencryption_failed")
                require(db.execute("SELECT marker FROM migration_wal_fixture").fetchone()[0] == fixture["label"], "migration_lost_committed_wal")
                require(db.execute("SELECT COUNT(*) FROM sync_outbox WHERE state='pending'").fetchone()[0] > 0, "migration_lost_outbox")
                for tombstone in fixture["legacy_tombstones"]:
                    converted = db.execute("SELECT id,user,ciphertext,nonce,deleted FROM memories WHERE id=?", (tombstone[0],)).fetchone()
                    require(converted[:2] == tombstone[:2] and converted[4] == 1
                        and converted[2].startswith(support.PREFIX)
                        and support.decrypt_content(fixture["urk"], converted[2], converted[3]) == fixture["payload"],
                        "migration_deleted_payload_not_reencrypted")
            fixture["destination"] = profile
        return sessions



def main():
    require(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and sys.platform in ("linux", "darwin", "win32"), "disposable_native_hosted_runner_required")
    parser = argparse.ArgumentParser()
    for name in ("binary", "root"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("binary-sha256", "source-sha", "version"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    require(re.fullmatch(r"[0-9a-f]{64}", args.binary_sha256) and args.binary.is_absolute()
        and args.binary.is_file() and digest(args.binary) == args.binary_sha256, "binary_hash_mismatch")
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_sha) and args.source_sha == os.environ.get("CLI_SHA"), "verified_artifact_source_sha_mismatch")
    require(args.version and not any(c.isspace() for c in args.version), "version_invalid")
    temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    require(args.root.is_absolute() and not args.root.exists(), "root_not_fresh")
    args.root = args.root.resolve()
    require(args.root != temp and args.root.is_relative_to(temp), "root_outside_runner_temp")
    smoke = Smoke(args)
    try:
        smoke.run()
    except Exception as error:
        smoke.report.update(status="failed", failure=str(error) if isinstance(error, RuntimeError) else type(error).__name__)
        smoke.report["failure_location"] = script_failure_location(error, __file__)
        if isinstance(error, support.urllib.error.HTTPError):
            smoke.report["failure_http_status"] = error.code
        sqlite_failure = sqlite_failure_details(error)
        if sqlite_failure is not None:
            smoke.report["sqlite_failure"] = sqlite_failure
    finally:
        for connection in smoke.wal_connections:
            connection.close()
        smoke.cleanup()
        if smoke.keys is not None:
            try:
                smoke.report["keyring_cleanup"] = smoke.keys.cleanup()
            except Exception as error:
                smoke.report["keyring_cleanup"] = {"passed": False, "failure_code": type(error).__name__,
                    "remaining_entries": ["cleanup_not_verified"]}
        smoke.report["missing_cases"] = [case for case in REQUIRED if case not in smoke.report["cases"]]
        smoke.report["passed"] = smoke.report["status"] == "passed" and not smoke.report["missing_cases"] \
            and smoke.report["cloud_cleanup"]["passed"] and smoke.report["keyring_cleanup"]["passed"]
        smoke.save()
    print(json.dumps({"passed": smoke.report["passed"], "cases": len(smoke.report["cases"]),
        "report": str(smoke.root / "migration-coverage.json")}))
    return 0 if smoke.report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
