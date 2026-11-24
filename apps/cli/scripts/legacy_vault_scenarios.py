"""Legacy Vault login, historical database recovery, and independent sync checks."""
from native_fixture_process import install_published_runtime
import hashlib
import json
from pathlib import Path
import re
import secrets
import sqlite3
from dev_witness_fixture import register_witness_functions
from dev_witness_fixture import verify_fixture_witness
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from legacy_vault_fixture import UPSTREAM
from legacy_vault_fixture import REQUIRED
from legacy_vault_fixture import require
from legacy_vault_fixture import same_scope_path
from legacy_vault_fixture import readonly_database_uri
from legacy_vault_fixture import digest
from legacy_vault_fixture import hkdf
from legacy_vault_fixture import PREFIX
from legacy_vault_fixture import encrypted_bytes
from legacy_vault_fixture import v4_kek
from legacy_vault_fixture import legacy_kek
from legacy_vault_fixture import decrypt_content


class LegacyVaultScenarios:
    def database_sync(self):
        """Keep legacy data crypto independent of historical host authentication."""
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        env = self.env("database-original")
        user, password = "ci-migrate-" + secrets.token_hex(8), "ci-" + secrets.token_urlsafe(32)
        self.diagnostic_secrets.add(password)
        for slot in ("super:", "pass:"):
            self.keys.reserve("rsrs", slot + user)
        account = {"user": user, "token": None, "confirmed": False}
        self.accounts.append(account)
        self.report["cloud_cleanup"]["remaining_users"].append(user)
        self.save()
        registered = self.cli(env, "register", "--addr", UPSTREAM, "--user", user,
            prompts=[("  login password (for the server; can be reset): ", password)])
        session = self.session(env)
        require(registered["summary"].get("ok") is True and session.get("user") == user
            and session.get("addr") == UPSTREAM and bool(session.get("token")), "registered_identity_mismatch")
        account.update(token=session["token"], confirmed=True)
        code = registered["summary"].get("super")
        require(isinstance(code, str) and bool(code), "registered_super_missing")
        self.keys.track_created_login(session, user, code, password)
        self.diagnostic_secrets.update((code, session["token"]))
        env["RSRS_SUPER"] = code
        vault = self.api(account, "GET", "/api/self/vault")
        urk = AESGCM(v4_kek(code, vault["kdf_salt"], vault["wrapped_urk"])).decrypt(
            bytes.fromhex(vault["urk_nonce"]), encrypted_bytes(vault["wrapped_urk"]), None)
        require(len(urk) == 32, "original_urk_invalid")
        self.diagnostic_secrets.add(urk.hex())
        plaintext = "Synthetic historical ciphertext survives migration and independent-device synchronization."
        active = self.cli(env, "remember", plaintext, "--title", "CI legacy data", "--force", "--importance", "important")["summary"]["id"]
        deleted = self.cli(env, "remember", "Historical encrypted tombstone", "--title", "CI legacy deleted", "--force", "--importance", "important")["summary"]["id"]
        self.cli(env, "forget", deleted)
        # cli() already drained this exact owner and waited for its final receipt
        # and library lock release. A warning/exit 2 is not permission to edit a DB.
        require(self.last_runtime_stop == 0, "database_owner_exit_not_confirmed")
        original = self.profile(env)
        payloads = {}
        with sqlite3.connect(original / "rsrs.db") as db:
            register_witness_functions(db)
            rows = db.execute("SELECT id,ciphertext,nonce,deleted,dirty FROM memories WHERE id IN (?,?) ORDER BY id", (active, deleted)).fetchall()
            require(len(rows) == 2 and {row[3] for row in rows} == {0, 1}
                and all(row[4] == 1 for row in rows), "legacy_database_pending_seed_missing")
            for memory_id, ciphertext, nonce, tombstone, _ in rows:
                payload = decrypt_content(urk, ciphertext, nonce)
                payloads[memory_id] = (payload, tombstone)
                old_nonce = secrets.token_bytes(12).hex()
                old_ciphertext = AESGCM(hkdf(urk, None, b"onememory:data:v1")).encrypt(bytes.fromhex(old_nonce), payload, None).hex()
                db.execute("UPDATE memories SET ciphertext=?,nonce=? WHERE id=?", (old_ciphertext, old_nonce, memory_id))
                db.execute("UPDATE core_artifacts SET source=? WHERE memory_id=?", (old_ciphertext, memory_id))
                db.execute("UPDATE sync_outbox SET ciphertext=?,nonce=? WHERE id=? AND state='pending'", (old_ciphertext, old_nonce, memory_id))
            verify_fixture_witness(db)
        db.close()
        source = Path(env["HOME"]) / ".onememory"
        original.rename(source)
        (source / "rsrs.db").rename(source / "onememory.db")
        # Match the existing profile-migration seed: only local crypto is old;
        # keep the current host alias/endpoint and the server's current vault.
        session = json.loads((source / "session.json").read_text())
        wrap_nonce = secrets.token_bytes(12)
        session["wrapped_urk"] = AESGCM(v4_kek(code, session["kdf_salt"])).encrypt(wrap_nonce, urk, None).hex()
        session["urk_nonce"] = wrap_nonce.hex()
        session.pop("crypto_namespace", None)
        (source / "session.json").write_text(json.dumps(session), encoding="utf-8")
        source_config = source / "client.json"
        config = json.loads(source_config.read_text()) if source_config.exists() else {}
        config["data_dir"] = str(source)
        source_config.write_text(json.dumps(config), encoding="utf-8")
        original_session = (source / "session.json").read_bytes()
        original_database = digest(source / "onememory.db")
        alias = "legacy-" + hashlib.sha256(str(source.resolve()).encode()).hexdigest()[:16]
        for slot in ("super:", "pass:", "legacy-secret:"):
            self.keys.reserve("rsrs", slot + alias)
        migration_env = dict(env)
        for key in ("RSRS_DATA_DIR", "RSRS_RUNTIME_BIN", "RSRS_SUPER"):
            migration_env.pop(key, None)
        install_published_runtime(self.args.binary, migration_env)
        discovery = self.cli(migration_env, "migrate")["details"]
        self.report["migration_discovery"] = discovery
        self.save()
        candidates = discovery["profiles"]
        require(isinstance(candidates, list) and all(isinstance(row, dict)
            and isinstance(row.get("source"), str) and Path(row["source"]).is_absolute()
            and isinstance(row.get("source_id"), str) and re.fullmatch(r"[0-9a-f]{64}", row["source_id"])
            and isinstance(row.get("account"), str) for row in candidates), "legacy_discovery_schema_invalid")
        candidates = [row for row in candidates if Path(row["source"]).resolve() == source.resolve()]
        require(len(candidates) == 1, "legacy_source_not_unique")
        result = self.cli(dict(migration_env, RSRS_SUPER=code), "migrate", "--source", candidates[0]["source_id"], "--account", candidates[0]["account"])
        require(result["summary"].get("state") == "migrated", "full_migration_not_published")
        destination = Path(result["summary"]["dir"])
        upgraded_env = dict(env, RSRS_DATA_DIR=str(destination))
        install_published_runtime(self.args.binary, upgraded_env)
        self.read_entry(upgraded_env, active, plaintext)
        with sqlite3.connect(readonly_database_uri(destination / "rsrs.db"), uri=True) as db:
            converted = db.execute("SELECT id,ciphertext,nonce,deleted FROM memories WHERE id IN (?,?) ORDER BY id", (active, deleted)).fetchall()
        require(len(converted) == 2 and all(row[1].startswith(PREFIX)
            and (decrypt_content(urk, row[1], row[2]), row[3]) == payloads[row[0]] for row in converted),
            "legacy_database_content_not_preserved")
        self.report["stage"] = "migrated_sync"
        self.save()
        require(self.cli(upgraded_env, "sync").get("status") == "ok", "migrated_sync_not_completed")
        final_env = self.env("database-new-device")
        self.current_login(final_env, user, password, code)
        recovered = self.session(final_env)
        require(recovered.get("user") == user and bool(recovered.get("token")), "fresh_login_identity_mismatch")
        account["token"] = recovered["token"]
        self.report["stage"] = "fresh_device_sync"
        self.save()
        require(self.cli(final_env, "sync").get("status") == "ok", "fresh_device_sync_not_completed")
        self.read_entry(final_env, active, plaintext)
        with sqlite3.connect(readonly_database_uri(self.profile(final_env) / "rsrs.db"), uri=True) as db:
            remote = db.execute("SELECT id,ciphertext,nonce,deleted FROM memories WHERE id IN (?,?) ORDER BY id", (active, deleted)).fetchall()
        expected = {row[0]: row for row in converted}
        observed = {row[0]: row for row in remote}
        with sqlite3.connect(readonly_database_uri(self.profile(final_env) / "rsrs.db"), uri=True) as db:
            heads = {memory_id: json.loads(wire) for memory_id, wire in db.execute(
                "SELECT id,wire FROM sync_remote_heads WHERE id IN (?,?)", (active, deleted))}
        readback = {}
        for label, memory_id in (("active", active), ("deleted", deleted)):
            row = observed.get(memory_id)
            head = heads.get(memory_id)
            readback[label] = {"present": row is not None, "head_present": head is not None}
            if row is not None:
                readback[label].update(ciphertext_matches=row[1] == expected[memory_id][1],
                    nonce_matches=row[2] == expected[memory_id][2], deleted_matches=row[3] == expected[memory_id][3],
                    ciphertext_present=bool(row[1]), nonce_present=bool(row[2]),
                    plaintext_matches=bool(row[1] and row[2]) and decrypt_content(urk, row[1], row[2]) == payloads[memory_id][0])
            if head is not None:
                readback[label].update(head_ciphertext_matches=head.get("ciphertext") == expected[memory_id][1],
                    head_nonce_matches=head.get("nonce") == expected[memory_id][2],
                    head_deleted_matches=head.get("deleted") == bool(expected[memory_id][3]))
        self.report["database_sync_readback"] = readback
        self.save()
        require(observed.get(active) == expected[active]
            and readback["active"].get("plaintext_matches") is True
            and readback["active"].get("head_ciphertext_matches") is True
            and readback["active"].get("head_nonce_matches") is True
            and readback["active"].get("head_deleted_matches") is True,
            "new_device_active_ciphertext_differs")
        # The current server propagates a content-free deletion head. The old
        # encrypted tombstone was checked above before any remote synchronization.
        require(observed.get(deleted) == (deleted, "", "", 1)
            and heads.get(deleted, {}).get("id") == deleted
            and heads[deleted].get("deleted") is True
            and heads[deleted].get("ciphertext") == "" and heads[deleted].get("nonce") == "",
            "new_device_tombstone_not_propagated")
        require(self.api(account, "GET", "/api/self/vault") == vault, "database_sync_changed_current_vault")
        require((source / "session.json").read_bytes() == original_session
            and digest(source / "onememory.db") == original_database, "original_source_changed")
        self.passed("legacy_ciphertext_sync", urk_preserved=True, original_source_preserved=True,
            old_content_decrypted=True, ciphertext_reencrypted=True, encrypted_tombstone_preserved=True,
            migrated_push=True, independent_device_pull_decrypted=True, current_host_login=True,
            remote_tombstone_propagated=True, remote_tombstone_content="canonical_empty",
            historical_authentication_tested=False)

    def verify_recovery_control(self, env, source, operation, expected):
        output = subprocess.run([str(self.args.binary), "--json", "runtime", "key-result",
            "--operation-id", operation, "--source-profile", str(source), "--verify-custody"],
            input=json.dumps(expected).encode("utf-8"), cwd=self.root, env=env,
            capture_output=True, timeout=180)
        require(output.returncode == 0, "recovery_control_native_verification_failed")
        try:
            envelope = json.loads(output.stdout)
        except (ValueError, UnicodeError):
            raise RuntimeError("recovery_control_native_receipt_invalid") from None
        receipt = envelope.get("summary", {})
        require(envelope.get("status") == "ok" and not envelope.get("errors")
            and receipt.get("verified") is True and receipt.get("operation_id") == operation
            and same_scope_path(receipt.get("installation_root"), Path(env["RSRS_DATA_DIR"]))
            and same_scope_path(receipt.get("source_profile"), source),
            "recovery_control_native_receipt_mismatch")

    def registration_receipt(self, env, source, user, previous_operations):
        root = Path(env["RSRS_DATA_DIR"]).resolve()
        folder = root / "runtime/autostart/account-results"
        created = [path for path in folder.glob("*.json") if path.name not in previous_operations
            and re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", path.stem)]
        require(len(created) == 1 and not created[0].is_symlink(), "historical_registration_operation_not_unique")
        path, = created
        binding = json.loads(path.read_text(encoding="utf-8"))
        require(binding.get("schema") == 1 and binding.get("operation_id") == path.stem
            and same_scope_path(binding.get("root"), root) and same_scope_path(binding.get("source"), source)
            and binding.get("user") == user and binding.get("addr") == UPSTREAM
            and binding.get("register") is True and binding.get("sent") is True,
            "historical_registration_operation_binding_mismatch")
        # Query the original successful registration; never admit another auth operation.
        receipt = self.cli(env, "runtime", "account-result", "--operation-id", path.stem,
            "--source-profile", str(source), "--register")["summary"]
        require(receipt.get("operation_id") == path.stem, "historical_registration_receipt_operation_mismatch")
        return receipt

    def historical_database_sync(self, env, expected_urk, code, expected_records):
        """Continue a real upgraded historical DB; never replace its file or data key."""
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        self.verify_development_server()
        source = self.profile(env).resolve()
        databases = [source / name for name in ("rsrs.db", "onememory.db") if (source / name).is_file()]
        require(len(databases) == 1, "historical_database_file_not_unique")
        database, = databases
        identity = (database.stat().st_dev, database.stat().st_ino)
        def rows():
            require(self.profile(env).resolve() == source
                and (database.stat().st_dev, database.stat().st_ino) == identity, "historical_database_replaced")
            with sqlite3.connect(readonly_database_uri(database), uri=True) as db:
                require(db.execute("PRAGMA integrity_check").fetchall() == [("ok",)], "historical_database_integrity_failed")
                return (db.execute("SELECT id,ciphertext,nonce,dirty,deleted FROM memories ORDER BY id").fetchall(),
                        db.execute("SELECT * FROM sync_outbox ORDER BY seq").fetchall())
        before = rows()
        expected = {record[0]: record for record in expected_records}
        require(expected and any(record[3] for record in expected.values())
            and any(not record[3] for record in expected.values()) and before[1], "historical_sync_seed_empty")
        for memory_id, ciphertext, nonce, dirty, deleted in before[0]:
            require((memory_id, decrypt_content(expected_urk, ciphertext, nonce), dirty, deleted) == expected[memory_id],
                    "historical_sync_original_key_mismatch")
        self.diagnostic_secrets.update((code, expected_urk.hex()))
        # Current public recovery establishes a valid unbound identity around the known original URK.
        # It is preparation by the current host, not legacy authentication compatibility evidence.
        self.report["identity_preparation"] = {"actor": "current-host", "method": "auth.recover",
            "synthetic_current_v4_wrap": True, "legacy_authentication_compatibility": False}
        self.report["stage"] = "historical_same_key_recovery"
        self.save()
        self.cli(env, "logout", "--full")
        require(not (source / "session.json").exists() and rows() == before, "historical_logout_changed_database")
        salt, nonce = secrets.token_hex(16), secrets.token_bytes(12)
        wrapped = PREFIX + AESGCM(v4_kek(code, salt, PREFIX)).encrypt(nonce, expected_urk, None).hex()
        self.diagnostic_secrets.update((salt, nonce.hex(), wrapped))
        request = {"v": 1, "id": "historical-recover-" + uuid.uuid4().hex, "method": "auth.recover",
            "params": {"addr": "", "user": "", "super": code,
                "kdf_salt": salt, "wrapped_urk": wrapped, "urk_nonce": nonce.hex()}}
        try:
            recovered = self.cli(env, "fivekeys", host_request=request)["summary"]
        finally:
            # A public recovery may commit before its output/stop check fails.
            candidate = self.session(env)
            operation = candidate.get("auth_operation_id")
            if all(candidate.get(field) == value for field, value in
                    (("wrapped_urk", wrapped), ("kdf_salt", salt), ("urk_nonce", nonce.hex()))) \
                    and isinstance(operation, str):
                native_verifier = ({"recovery_verifier": lambda expected:
                    self.verify_recovery_control(env, source, operation, expected)} if sys.platform == "darwin" else {})
                self.keys.track_created_login(candidate, "", code, None, recovery_operation=operation, **native_verifier)
        require(recovered.get("outcome") == "committed" and same_scope_path(recovered.get("source_profile"), source)
            and recovered.get("operation_id") == operation and candidate.get("user") == ""
            and candidate.get("addr") == "" and rows() == before, "historical_recovery_not_committed_in_place")
        require(AESGCM(v4_kek(code, salt, wrapped)).decrypt(nonce, encrypted_bytes(candidate["wrapped_urk"]), None)
            == expected_urk, "historical_recovery_changed_urk")
        user, password = "ci-migrate-" + secrets.token_hex(8), "ci-" + secrets.token_urlsafe(32)
        self.diagnostic_secrets.add(password)
        account = {"user": user, "token": None, "confirmed": False}
        self.accounts.append(account)
        self.report["cloud_cleanup"]["remaining_users"].append(user)
        self.report["stage"] = "historical_existing_keys_register"
        self.save()
        register = {"v": 1, "id": "historical-register-" + uuid.uuid4().hex, "method": "auth.register",
            "params": {"addr": UPSTREAM, "user": user, "password": password}}
        previous_operations = {path.name for path in
            (Path(env["RSRS_DATA_DIR"]) / "runtime/autostart/account-results").glob("*.json")}
        try:
            registered = self.cli(env, "register", host_request=register)["summary"]
        finally:
            session = self.session(env)
            if session.get("user") == user and session.get("addr") == UPSTREAM and session.get("token"):
                account.update(token=session["token"], confirmed=True)
                self.diagnostic_secrets.add(session["token"])
                self.keys.track_created_login(session, user, code, password, recovery_operation=operation)
        require(registered.get("ok") is True and registered.get("user") == user
            and registered.get("addr") == UPSTREAM, "historical_registration_terminal_identity_mismatch")
        receipt = self.registration_receipt(env, source, user, previous_operations)
        require(receipt.get("outcome") == "committed", "historical_registration_receipt_not_committed")
        require(same_scope_path(receipt.get("source_profile"), source), "historical_registration_source_changed")
        require(same_scope_path(receipt.get("target_profile"), source), "historical_registration_target_changed")
        require(account["confirmed"], "historical_registration_account_unconfirmed")
        require(all(session.get(field) == candidate.get(field) for field in
            ("wrapped_urk", "kdf_salt", "urk_nonce", "keyring_account", "keyring_backend")),
            "historical_registration_wrap_changed")
        after_registration = rows()
        require(after_registration[0] == before[0], "historical_registration_rows_changed")
        require(after_registration[1] == before[1], "historical_registration_outbox_changed")
        self.cli(env, "config", "--addr", UPSTREAM, "--autosync", "false")
        require(rows() == before, "historical_endpoint_configuration_changed_database")
        vault = self.api(account, "GET", "/api/self/vault")
        require(vault.get("version") == 4 and all(vault.get(field) == session[field] for field in
            ("wrapped_urk", "kdf_salt", "urk_nonce")), "historical_server_vault_changed_original_key")
        self.report["stage"] = "historical_source_push"
        self.save()
        require(self.cli(env, "sync").get("status") == "ok", "historical_source_sync_not_completed")
        peer = self.env("historical-independent-device")
        self.current_login(peer, user, password, code)
        peer_session = self.session(peer)
        account["token"] = peer_session["token"]
        require(AESGCM(v4_kek(code, peer_session["kdf_salt"], peer_session["wrapped_urk"])).decrypt(
            bytes.fromhex(peer_session["urk_nonce"]), encrypted_bytes(peer_session["wrapped_urk"]), None) == expected_urk,
            "historical_independent_device_changed_urk")
        require(self.cli(peer, "sync").get("status") == "ok", "historical_independent_pull_not_completed")
        for memory_id, plaintext, _, deleted in expected.values():
            if not deleted:
                self.read_entry(peer, memory_id, json.loads(plaintext)["content"])
        with sqlite3.connect(readonly_database_uri(self.profile(peer) / "rsrs.db"), uri=True) as db:
            observed = {row[0]: row for row in db.execute("SELECT id,ciphertext,nonce,deleted FROM memories")}
            heads = {memory_id: json.loads(wire) for memory_id, wire in db.execute("SELECT id,wire FROM sync_remote_heads")}
        for memory_id, plaintext, _, deleted in expected.values():
            row, head = observed.get(memory_id), heads.get(memory_id, {})
            require(row is not None and row[3] == deleted and head.get("deleted") == bool(deleted),
                    "historical_remote_record_or_head_missing")
            if deleted:
                require(row[1:3] == ("", "") and head.get("ciphertext") == head.get("nonce") == "",
                        "historical_remote_tombstone_not_propagated")
            else:
                require(decrypt_content(expected_urk, row[1], row[2]) == plaintext
                    and head.get("ciphertext") == row[1] and head.get("nonce") == row[2], "historical_independent_plaintext_differs")
        marker_text = "Historical reverse synchronization " + secrets.token_hex(16)
        marker = self.cli(peer, "remember", marker_text, "--title", "historical-reverse-marker",
            "--importance", "important", "--force")["summary"]["id"]
        self.report["stage"] = "historical_reverse_peer_push"
        self.save()
        require(self.cli(peer, "sync").get("status") == "ok", "historical_reverse_push_not_completed")
        self.report["stage"] = "historical_reverse_source_pull"
        self.save()
        require(self.cli(env, "sync").get("status") == "ok", "historical_source_pull_not_completed")
        self.report["stage"] = "historical_reverse_source_read"
        self.save()
        self.read_entry(env, marker, marker_text)
        after, _ = rows()
        current = {row[0]: row for row in after}
        for memory_id, plaintext, _, deleted in expected.values():
            row = current.get(memory_id)
            require(row is not None and row[4] == deleted, "historical_sync_lost_original_record")
            if deleted:
                require(row[1:3] == ("", ""), "historical_source_tombstone_not_canonical")
            else:
                require(decrypt_content(expected_urk, row[1], row[2]) == plaintext, "historical_source_pull_changed_plaintext")
        with sqlite3.connect(readonly_database_uri(database), uri=True) as db:
            require(db.execute("SELECT count(*) FROM sync_outbox WHERE state='pending'").fetchone()[0] == 0
                and db.execute("SELECT count(*) FROM memories WHERE dirty<>0").fetchone()[0] == 0,
                "historical_sync_unacknowledged_work")
        require(self.api(account, "GET", "/api/self/vault") == vault, "historical_sync_changed_server_vault")
        final_session = self.session(env)
        require(all(final_session.get(field) == session.get(field) for field in
            ("wrapped_urk", "kdf_salt", "urk_nonce", "keyring_account", "keyring_backend")),
            "historical_sync_changed_source_wrap")
        return {"profile": str(source), "original_urk_preserved": True, "recovery_operation": operation,
            "existing_keys_registered": True, "source_push": True, "independent_pull_decrypted": True,
            "reverse_marker_source_pull": True, "original_active_preserved": True,
            "tombstones_propagated": True, "pending_work_acknowledged": True}

    def run_version(self, version):
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        env = self.env(f"v{version}-original")
        user, password = "ci-migrate-" + secrets.token_hex(8), "ci-" + secrets.token_urlsafe(32)
        self.diagnostic_secrets.add(password)
        for slot in ("super:", "pass:"):
            self.keys.reserve("rsrs", slot + user)
        account = {"user": user, "token": None, "confirmed": False}
        self.accounts.append(account)
        self.report["cloud_cleanup"]["remaining_users"].append(user)
        self.save()
        registered = self.cli(env, "register", "--addr", UPSTREAM, "--user", user,
            prompts=[("  login password (for the server; can be reset): ", password)])
        session = self.session(env)
        require(registered["summary"].get("ok") is True and registered["summary"].get("user") == user
            and session.get("user") == user and session.get("addr") == UPSTREAM and bool(session.get("token")),
            "registered_identity_mismatch")
        account.update(token=session["token"], confirmed=True)
        code = registered["summary"].get("super")
        require(isinstance(code, str) and bool(code), "registered_super_missing")
        self.keys.track_created_login(session, user, code, password)
        env["RSRS_SUPER"] = code
        self.diagnostic_secrets.update((code, session["token"]))
        vault = self.api(account, "GET", "/api/self/vault")
        urk = AESGCM(v4_kek(code, vault["kdf_salt"], vault["wrapped_urk"])).decrypt(
            bytes.fromhex(vault["urk_nonce"]), encrypted_bytes(vault["wrapped_urk"]), None)
        require(len(urk) == 32, "original_urk_invalid")
        self.diagnostic_secrets.add(urk.hex())
        plaintext = f"Synthetic legacy version {version} preserves this original encrypted content."
        title = f"CI legacy vault {version}"
        self.cli(env, "remember", plaintext, "--title", title, "--force", "--importance", "important")
        self.report["stage"] = f"v{version}_initial_sync"
        self.save()
        self.cli(env, "sync", timeout=180)
        database = Path(env["RSRS_DATA_DIR"]) / "rsrs.db"
        with sqlite3.connect(database) as db:
            register_witness_functions(db)
            entries = db.execute("SELECT id,ciphertext,nonce FROM memories WHERE title=? AND deleted=0", (title,)).fetchall()
            require(len(entries) == 1, "legacy_entry_not_unique")
            memory_id, ciphertext, item_nonce = entries[0]
            payload = decrypt_content(urk, ciphertext, item_nonce)
            item_nonce = secrets.token_bytes(12).hex()
            ciphertext = AESGCM(hkdf(urk, None, b"onememory:data:v1")).encrypt(bytes.fromhex(item_nonce), payload, None).hex()
            db.execute("UPDATE memories SET ciphertext=?,nonce=? WHERE id=?", (ciphertext, item_nonce, memory_id))
            db.execute("UPDATE core_artifacts SET source=? WHERE memory_id=?", (ciphertext, memory_id))
            verify_fixture_witness(db)
            db.execute("PRAGMA wal_checkpoint(TRUNCATE)") if not db.in_transaction else None
        # Close SQLite before relocating this owned old-format source.
        db.close()
        source = Path(env["HOME"]) / ".onememory"
        Path(env["RSRS_DATA_DIR"]).rename(source)
        (source / "rsrs.db").rename(source / "onememory.db")
        env["RSRS_DATA_DIR"] = str(source)
        salt, nonce = secrets.token_bytes(16).hex(), secrets.token_bytes(12)
        secret = secrets.token_hex(32)
        original_super = secret if version == 1 else secrets.token_urlsafe(32)
        self.diagnostic_secrets.update((original_super, secret))
        wrapped = AESGCM(legacy_kek(version, password if version == 1 else original_super, secret, salt)).encrypt(nonce, urk, None).hex()
        old_vault = {"version": version, "kdf_salt": salt, "wrapped_urk": wrapped, "urk_nonce": nonce.hex()}
        require(self.api(account, "POST", "/api/self/vault", old_vault).get("ok") is True, "legacy_vault_not_written")
        require(all(self.api(account, "GET", "/api/self/vault").get(k) == v for k, v in old_vault.items()),
            "legacy_vault_not_read_back")
        session.update({"vault_version": version, "kdf_salt": salt, "wrapped_urk": wrapped, "urk_nonce": nonce.hex(),
            "pass": password, "super": original_super})
        session.pop("crypto_namespace", None)
        session.pop("keyring_account", None)
        if version == 1:
            session["secret"] = secret
            session.pop("super", None)
        elif version == 3:
            session["secret_key"] = secret
        self.write_session(env, session)
        # Store a genuine legacy authentication hash on the real DEV server.
        # This is independent of the encrypted-vault format and must not be
        # inferred from a successful registration by the current CLI.
        old_auth_salt = hkdf(user.lower().encode(), None, b"onememory:auth-salt:v1", 16).hex()
        old_pass_hash = hashlib.pbkdf2_hmac("sha256", password.encode(), bytes.fromhex(old_auth_salt), 100000).hex()
        self.diagnostic_secrets.add(old_pass_hash)
        require(self.api(account, "POST", "/api/self/password", {"salt": old_auth_salt, "pass_hash": old_pass_hash}).get("updated") is True,
            "legacy_authentication_not_stored")
        old_login_env = self.env(f"v{version}-old-auth-login", None if version in (1, 3) else user)
        self.report["stage"] = f"v{version}_old_auth_login"
        self.save()
        self.password_login(old_login_env, user, password, version, original_super,
            account_secret=secret if version == 1 else None, secret_key=secret if version == 3 else None)
        old_login = self.session(old_login_env)
        self.diagnostic_secrets.add(old_login.get("token", ""))
        require(old_login.get("user") == user and bool(old_login.get("token"))
            and self.api(dict(account, token=old_login["token"]), "GET", "/api/self").get("user") == user,
            "real_legacy_auth_login_failed")
        require(self.api(account, "GET", "/auth/salt?user=" + urllib.parse.quote(user))["salt"] == old_auth_salt,
            "ordinary_login_changed_auth_salt")
        source_config_path = source / "client.json"
        source_config = json.loads(source_config_path.read_text()) if source_config_path.exists() else {}
        source_config["data_dir"] = str(source)
        source_config_path.write_text(json.dumps(source_config), encoding="utf-8")
        original_session = (source / "session.json").read_bytes()
        original_database = digest(source / "onememory.db")
        alias = "legacy-" + hashlib.sha256(str(source.resolve()).encode()).hexdigest()[:16]
        for slot in ("super:", "pass:", "legacy-secret:"):
            self.keys.reserve("rsrs", slot + alias)
        migration_env = dict(env, RSRS_SUPER=original_super)
        if version == 1:
            migration_env.pop("RSRS_SUPER", None)
        migration_env.pop("RSRS_DATA_DIR")
        install_published_runtime(self.args.binary, migration_env)
        candidates = self.cli(migration_env, "migrate")["details"]["profiles"]
        rows = [row for row in candidates if Path(row["source"]).resolve() == source.resolve()]
        require(len(rows) == 1, "legacy_source_not_unique")
        result = self.cli(migration_env, "migrate", "--source", rows[0]["source_id"], "--account", rows[0]["account"])
        require(result["summary"].get("state") == "migrated", "full_migration_not_published")
        destination = Path(result["summary"]["dir"])
        upgraded_env = dict(env, RSRS_DATA_DIR=str(destination), RSRS_SUPER=original_super)
        install_published_runtime(self.args.binary, upgraded_env)
        if version == 1:
            upgraded_env.pop("RSRS_SUPER", None)
        upgraded = self.session(upgraded_env)
        require(upgraded["vault_version"] == version and upgraded["wrapped_urk"].startswith(PREFIX),
            "migration_changed_original_factors")
        self.read_entry(upgraded_env, memory_id, plaintext)
        with sqlite3.connect(readonly_database_uri(destination / "rsrs.db"), uri=True) as db:
            converted = db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (memory_id,)).fetchone()
        require(converted[0].startswith(PREFIX) and converted != (ciphertext, item_nonce)
            and decrypt_content(urk, *converted) == payload, "full_migration_not_reencrypted")
        require(self.keys.read("rsrs", "pass:" + alias) == password, "original_password_not_retained")
        if version != 1:
            require(self.keys.read("rsrs", "super:" + alias) == original_super, "original_super_not_retained")
        if version in (1, 3):
            self.verify_legacy_factor(upgraded, secret, "original_account_secret_not_retained")
        # Recovery factors already exist in this fixture's verified migrated
        # session/isolated keyring; only the password needs terminal input.
        result = self.cli(upgraded_env, "migrate", "--vault", "--addr", UPSTREAM, "--user", user,
            prompts=[("  login password: ", password)])
        committed = self.session(upgraded_env)
        self.keys.track_created_login(committed, user, original_super if version != 1 else None, password)
        account["token"] = committed["token"]
        require(result["summary"].get("super_issued") is None, "migration_generated_replacement_super")
        new_auth_salt = PREFIX + hkdf(user.lower().encode(), None, b"rsrs:auth-salt:v1", 16).hex()
        require(self.api(account, "GET", "/auth/salt?user=" + urllib.parse.quote(user))["salt"] == new_auth_salt
            and committed.get("auth_salt") == new_auth_salt, "authentication_salt_not_migrated")
        rejected, _ = self.request(account, "POST", "/login", {"user": user, "pass_hash": old_pass_hash,
            "device_name": "legacy-auth-regression"})
        require(rejected == 401, "old_authentication_hash_still_accepted")
        if version in (1, 3):
            self.verify_legacy_factor(committed, secret, "cloud_migration_changed_account_secret")
        published = self.api(account, "GET", "/api/self/vault")
        require(published["version"] == version and published["wrapped_urk"].startswith(PREFIX), "cloud_factors_changed")
        recovered = AESGCM(legacy_kek(version, password if version == 1 else original_super, secret, published["kdf_salt"], current=True)).decrypt(
            bytes.fromhex(published["urk_nonce"]), encrypted_bytes(published["wrapped_urk"]), None)
        require(recovered == urk, "migration_changed_urk")
        self.report["stage"] = f"v{version}_migrated_sync"
        self.save()
        self.cli(upgraded_env, "sync")
        final_env = self.env(f"v{version}-new-device", None if version in (1, 3) else user)
        if version != 1:
            final_env["RSRS_SUPER"] = original_super
        self.password_login(final_env, user, password, version, original_super,
            account_secret=secret if version == 1 else None, secret_key=secret if version == 3 else None)
        recovered_session = self.session(final_env)
        require(recovered_session.get("user") == user and bool(recovered_session.get("token"))
            and self.api(dict(account, token=recovered_session["token"]), "GET", "/api/self").get("user") == user
            and self.api(account, "GET", "/auth/salt?user=" + urllib.parse.quote(user))["salt"] == new_auth_salt,
            "fresh_login_did_not_use_new_auth_salt")
        account["token"] = recovered_session["token"]
        if version in (1, 3):
            self.verify_legacy_factor(recovered_session, secret, "new_device_changed_account_secret")
        self.report["stage"] = f"v{version}_fresh_device_sync"
        self.save()
        self.cli(final_env, "sync")
        self.read_entry(final_env, memory_id, plaintext)
        if version == 2:
            new_title = "Post authentication migration write"
            new_plaintext = "Original credentials still permit new encrypted memory writes after authentication salt migration."
            self.cli(final_env, "remember", new_plaintext, "--title", new_title, "--force", "--importance", "important")
            self.cli(final_env, "sync")
            with sqlite3.connect(readonly_database_uri(self.profile(final_env) / "rsrs.db"), uri=True) as db:
                written = db.execute("SELECT id,ciphertext,nonce FROM memories WHERE title=? AND deleted=0", (new_title,)).fetchall()
            require(len(written) == 1 and written[0][1].startswith(PREFIX), "post_migration_write_missing")
            self.read_entry(final_env, written[0][0], new_plaintext)
            require(new_plaintext.encode() in decrypt_content(urk, written[0][1], written[0][2]), "post_migration_write_wrong_key")
        with sqlite3.connect(readonly_database_uri(self.profile(final_env) / "rsrs.db"), uri=True) as db:
            require(db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (memory_id,)).fetchone() == converted,
                "new_device_ciphertext_differs")
        if version == 1:
            # Hidden terminal input authorizes login; matching RSRS_PASS exercises the headless
            # commit and follow-up read contract, not authorization fallback.
            # Owned native passwords remain available, so this does not prove
            # recovery on a host without any native credential store.
            headless_env = dict(final_env, RSRS_PASS=password)
            self.password_login(headless_env, user, password, version, original_super)
            headless = self.session(headless_env)
            account["token"] = headless["token"]
            # Host-managed auth commits a durable operation-owned alias even
            # with matching RSRS_PASS. Only the non-operation legacy commit
            # path can omit native credentials. password_login verified and
            # tracked the exact newly committed password alias above.
            require("pass" not in headless
                and headless.get("keyring_backend") in ("native", "secret_service")
                and headless_env.get("RSRS_PASS") == password,
                "headless_v1_persistent_password_commit_invalid")
            require(headless.get("vault_version") == 1, "headless_v1_changed_original_factors")
            self.verify_legacy_factor(headless, secret, "headless_v1_changed_original_factors")
            headless_urk = AESGCM(legacy_kek(1, password, secret, headless["kdf_salt"], current=True)).decrypt(
                bytes.fromhex(headless["urk_nonce"]), encrypted_bytes(headless["wrapped_urk"]), None)
            require(headless_urk == urk, "headless_v1_changed_urk")
            self.report["stage"] = "v1_headless_sync"
            self.save()
            self.cli(headless_env, "sync")
            self.read_entry(headless_env, memory_id, plaintext)
        require((source / "session.json").read_bytes() == original_session
            and digest(source / "onememory.db") == original_database, "original_source_changed")
        self.passed(REQUIRED[version], local_legacy_version=version, resulting_version=version,
            urk_preserved=True, original_super_preserved=True if version != 1 else None, original_source_preserved=True,
            ciphertext_reencrypted=True, old_content_decrypted=True, new_device_decrypted=True,
            secret_key_recovery=version == 3, cloud_v1_recovery_supported=version == 1,
            original_account_secret_preserved=version == 1,
            headless_password_login_verified=version == 1,
            headless_password_login_scope="matching_environment_persistent_host_commit_and_followup_read; authorization_fallback_and_absent_native_store_not_tested" if version == 1 else None,
            recovery_factor="login_password_and_account_secret" if version == 1 else "original_super")
        self.report["cases"][REQUIRED[version]]["authentication"] = {
            "real_server_legacy_salt_login": True, "ordinary_login_preserved_legacy_salt": True,
            "explicit_migration_changed_to_rsrs_salt": True, "old_hash_rejected": True,
            "fresh_cli_login_original_password": True, "fresh_cli_login_new_salt": True}
        if version == 2:
            self.report["cases"][REQUIRED[version]]["authentication"]["new_memory_write_sync_restart_read"] = True
        self.save()
