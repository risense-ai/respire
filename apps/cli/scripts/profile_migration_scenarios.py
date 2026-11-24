"""Migration, interrupted restart and rejection scenarios."""
from contextlib import closing
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
from native_fixture_process import scoped_rpc_request
from native_fixture_process import verify_rpc_receipt
from profile_migration_fixture_context import support
from profile_migration_fixture_context import keyring_backend
from profile_migration_fixture_context import require
from profile_migration_fixture_context import digest


class ProfileMigrationScenarios:
    def case(self, name, sources, target=False, interrupt=False):
        self.report["stage"] = name
        env = self.env(name)
        home = Path(env["HOME"])
        fixtures = []
        for folder, service in sources:
            fixture = self.seed(env, home / folder, service, name + "-" + folder)
            fixtures.append(fixture)
        if name == "multiaccount":
            other = self.seed(env, home / ".onememory/accounts/second", "1memory", "multiaccount-secondary")
            fixtures.append(other)
            old_lock = sqlite3.connect(fixtures[0]["source"] / "lock.db")
            old_lock.execute("CREATE TABLE IF NOT EXISTS migration_lock_fixture(value TEXT)")
            old_lock.commit()
            old_lock.execute("BEGIN EXCLUSIVE")
            self.wal_connections.append(old_lock)
        if target:
            current = self.seed(env, home / ".rsrs", "rsrs", name + "-existing-rsrs")
            # A current profile must keep its existing credentials and URL untouched.
            current_env = dict(env, RSRS_DATA_DIR=str(current["source"]))
            # This retained profile owns its own installed Runtime, not the seed environment's override.
            current_env.pop("RSRS_RUNTIME_BIN", None)
            current_session = self.session(current_env)
        default = self.default_env(env)
        support.install_published_runtime(self.args.binary, default)
        self.cli(default, "status")
        root = home / ".rsrs"
        require(not list(root.rglob(".rsrs-migration.json")), "startup_automatically_migrated_profiles")
        require(not any(json.loads(p.read_text()).get("user") in {f["account"]["user"] for f in fixtures}
            for p in root.rglob("session.json")), "startup_copied_legacy_accounts")
        original_config = (root / "client.json").read_bytes() if (root / "client.json").exists() else None
        original_selection = self.cli(default, "config")["summary"]["data_dir"]
        self.passed("startup_does_not_migrate", existing_target=target)
        candidates = self.cli(default, "migrate")["details"]["profiles"]
        for fixture in fixtures:
            rows = [row for row in candidates if Path(row["source"]).resolve() == fixture["source"].resolve()]
            require(len(rows) == 1 and rows[0]["user"] == fixture["account"]["user"], "selected_migration_source_not_unique")
            fixture["source_id"] = rows[0]["source_id"]
            fixture["destination_account"] = rows[0]["account"]
            fixture["destination"] = root / "accounts" / rows[0]["account"]
        if interrupt:
            # Hold the real SQLite snapshot in Copying; a fast file copy can
            # otherwise reach non-replayable Transforming before the parent dies.
            source_db = fixtures[0]["source"] / "onememory.db"
            connection = self.wal_connections[-1]
            require(Path(connection.execute("PRAGMA database_list").fetchone()[2]).samefile(source_db),
                    "interruption_fixture_source_mismatch")
            connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            require(connection.execute("PRAGMA journal_mode=DELETE").fetchone()[0] == "delete",
                    "interruption_fixture_lock_mode_failed")
            connection.execute("BEGIN EXCLUSIVE")
            process = None
            try:
                process = subprocess.Popen([str(self.args.binary), "--direct", "--json", "migrate",
                    "--source", fixtures[0]["source_id"], "--account", fixtures[0]["destination_account"]],
                    env=dict(default, RSRS_SUPER=fixtures[0]["code"]), cwd=self.root,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                deadline = time.monotonic() + 30
                stage_seen = False
                while process.poll() is None and time.monotonic() < deadline:
                    stage = home / ".rsrs-migration-staging"
                    if stage.is_dir() and any((p / "onememory.db").is_file() for p in stage.iterdir()):
                        intents = list((home / ".rsrs-migration-intents").glob("*.json"))
                        require(len(intents) == 1, "interruption_fixture_intent_not_unique")
                        frames = intents[0].read_text(encoding="utf-8")
                        require(frames.endswith("\n"), "interruption_fixture_intent_truncated")
                        intent = json.loads(frames.splitlines()[-1])
                        require(intent["phase"] == "copying"
                                and intent["original"]["source_id"] == fixtures[0]["source_id"]
                                and intent["original"]["account"] == fixtures[0]["destination_account"],
                                "interruption_fixture_not_copying")
                        stage_seen = True
                        break
                    time.sleep(0.002)
                require(stage_seen, "interruption_fixture_did_not_reach_real_stage")
            finally:
                try:
                    if process is not None:
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=10)
                        # The control helper must release its real library lock
                        # before the fixture unblocks its source snapshot.
                        require((root / "lock.db").is_file(), "interruption_fixture_owner_lock_missing")
                        with sqlite3.connect(root / "lock.db", timeout=20) as owner:
                            owner.execute("BEGIN EXCLUSIVE")
                            owner.rollback()
                finally:
                    connection.rollback()
                    connection.execute("PRAGMA journal_mode=WAL")
            require(not fixtures[0]["destination"].exists(), "interruption_fixture_missed_prepublication_window")
        for fixture in fixtures:
            migrated = self.cli(dict(default, RSRS_SUPER=fixture["code"]), "migrate", "--source", fixture["source_id"],
                "--account", fixture["destination_account"])["summary"]
            require(migrated.get("state") == "migrated" and Path(migrated["dir"]).resolve() == fixture["destination"].resolve(),
                "selected_migration_not_published")
            require(((root / "client.json").read_bytes() if (root / "client.json").exists() else None) == original_config,
                "explicit_migration_changed_current_configuration")
        self.migrated(home, fixtures)
        selected = self.cli(default, "config")["summary"]["data_dir"]
        require(Path(selected).resolve() == Path(original_selection).resolve(), "migration_changed_selected_account")
        for fixture in fixtures:
            with self.selected_profile(default, fixture) as user_env:
                self.report["fixture_operation"] = "prepare_current_host_credentials"
                self.save()
                self.cli(user_env, "login", "--interactive", "--addr", support.UPSTREAM,
                    "--user", fixture["account"]["user"], "--pass", fixture["password"], "--super", fixture["code"])
                committed = self.session(user_env)
                require(committed.get("user") == fixture["account"]["user"]
                    and committed.get("addr") == support.UPSTREAM, "current_host_identity_mismatch")
                self.keys.track_created_login(committed, fixture["account"]["user"], fixture["code"], fixture["password"])
                fixture["account"]["token"] = committed["token"]
                fixture["alias"] = committed["keyring_account"]
                if sys.platform == "darwin":
                    self.verify_mac_native_credentials(user_env, fixture)
                self.read_entry(user_env, fixture["id"], fixture["text"])
                recalled = self.cli(user_env, "recall", fixture["text"], "--titles")
                require(fixture["id"] in json.dumps(recalled), "migrated_database_recall_failed")
        if target:
            with sqlite3.connect(current["source"] / "rsrs.db") as db:
                require(db.execute("SELECT ciphertext,nonce FROM memories WHERE id=?", (current["id"],)).fetchone() == current["cipher"],
                    "existing_target_data_overwritten")
            require(self.session(current_env) == current_session, "existing_target_session_overwritten")
            self.report["fixture_operation"] = "retained_current_credentials"
            self.save()
            if sys.platform == "darwin":
                self.verify_mac_native_credentials(current_env, current)
            else:
                self.verify_retained_current_credentials(current_session, current)
            self.read_entry(current_env, current["id"], current["text"])
            self.passed("existing_rsrs_preserved_and_legacy_imported", imported=len(fixtures))
        before = {str(p.relative_to(home)): digest(p) for p in (home / ".rsrs").rglob(".rsrs-migration.json")}
        self.cli(default, "status")
        require(before == {str(p.relative_to(home)): digest(p) for p in (home / ".rsrs").rglob(".rsrs-migration.json")},
            "repeat_start_changed_receipts")
        require(len([p for p in (home / ".rsrs").rglob("session.json") if json.loads(p.read_text()).get("user") in
            {f["account"]["user"] for f in fixtures}]) == len(fixtures), "repeat_start_duplicated_profiles")
        if name == "multiaccount":
            receipt = json.loads((fixtures[0]["destination"] / ".rsrs-migration.json").read_text())
            require(receipt.get("snapshot_only") is True and receipt.get("legacy_runtime_active") is True,
                "active_legacy_lock_snapshot_not_declared")
            selected = self.cli(default, "config")["summary"]["data_dir"]
            require(Path(selected).resolve() == Path(original_selection).resolve(), "migration_changed_selected_account")
            # A newly appearing earlier source and backup-only directory must
            # not change the identity of already copied profiles.
            backup = home / ".respire" / "bak"
            backup.mkdir(parents=True)
            shutil.copyfile(fixtures[0]["source"] / "session.json", backup / "session.json")
            rows = self.cli(default, "migrate")["details"]["profiles"]
            require({row["source_id"] for row in rows} == {f["source_id"] for f in fixtures},
                "backup_only_directory_became_migration_source")
            earlier = home / ".respire" / "accounts" / "aaa"
            earlier.mkdir(parents=True)
            shutil.copyfile(fixtures[0]["source"] / "session.json", earlier / "session.json")
            reordered = self.cli(default, "migrate")["details"]["profiles"]
            require(len(reordered) == len(fixtures) + 1, "source_order_fixture_not_observed")
            for fixture in reversed(fixtures):
                repeated = self.cli(dict(default, RSRS_SUPER=fixture["code"]), "migrate", "--source", fixture["source_id"], "--account", "different-name")["summary"]
                require(repeated.get("state") == "already_migrated"
                    and Path(repeated["dir"]).resolve() == fixture["destination"].resolve(),
                    "source_order_changed_existing_migration_identity")
            require(before == {str(p.relative_to(home)): digest(p) for p in root.rglob(".rsrs-migration.json")},
                "repeat_explicit_migration_changed_receipts")
            require(not (root / "accounts" / "different-name").exists(), "repeat_explicit_migration_duplicated_account")
            self.passed("migration_source_order_and_backup_only_idempotent", selected_sources=len(fixtures), new_earlier_source=True)
            self.passed("onememory_multiaccount_wal_migrated", profiles=len(fixtures),
                deleted_payloads_reencrypted=sum(len(f["legacy_tombstones"]) for f in fixtures))
            self.passed("legacy_db_with_explicit_current_credentials", current_source_credentials_preserved=True,
                explicit_data_key=True)
            self.passed("explicit_dev_endpoint_preserved", endpoint=support.UPSTREAM)
            self.passed("migration_repeated_start_idempotent", profiles=len(fixtures))
            for fixture in fixtures:
                with self.selected_profile(default, fixture) as profile_env:
                    self.cli(profile_env, "config", "--addr", support.UPSTREAM, "--autosync", "false")
                    created = self.cli(profile_env, "remember", "New-directory migration write",
                        "--title", "post-migration", "--force", "--importance", "important")
                    new_id = created["summary"]["id"]
                    self.cli(profile_env, "sync")
                    self.report["fixture_operation"] = "post_sync_deleted_payload_erasure"
                    self.save()
                    with sqlite3.connect((fixture["destination"] / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
                        for tombstone in fixture["legacy_tombstones"]:
                            row = db.execute("SELECT id,user,ciphertext,nonce,deleted,embedding_enc FROM memories WHERE id=?", (tombstone[0],)).fetchone()
                            # Server 1.0.1 erases deleted payloads after sync; pre-sync re-encryption is checked above.
                            require(row is not None and row[:2] == tombstone[:2] and row[4] == 1
                                and row[2] == row[3] == row[5] == "",
                                "sync_retained_deleted_payload")
                user = fixture["account"]["user"]
                self.report["fixture_operation"] = "independent_current_login"
                self.save()
                remote = self.env("independent-" + user)
                require(not (Path(remote["RSRS_DATA_DIR"]) / "session.json").exists(),
                        "independent_profile_not_empty")
                remote["RSRS_SUPER"] = fixture["code"]
                self.cli(remote, "login", "--interactive", "--addr", support.UPSTREAM, "--user", user, "--pass", fixture["password"], "--super", fixture["code"])
                independent = self.session(remote)
                self.keys.track_created_login(independent, user, fixture["code"], fixture["password"])
                fixture["account"]["token"] = independent["token"]
                self.cli(remote, "sync")
                self.read_entry(remote, fixture["id"], fixture["text"])
                self.read_entry(remote, new_id, "New-directory migration write")
                recalled = self.cli(remote, "recall", fixture["text"], "--titles")
                require(fixture["id"] in json.dumps(recalled), "independent_database_recall_failed")
                rows = self.cli(remote, "list", "--limit", "100")
                require("post-migration" in json.dumps(rows), "new_directory_write_not_synced")
            self.passed("migrated_outbox_sync_and_independent_decrypt", profiles=len(fixtures),
                deleted_payloads_erased=sum(len(f["legacy_tombstones"]) for f in fixtures))
        elif interrupt:
            self.passed("migration_interrupted_restart_recovered", real_stage_observed=True)
        elif not target:
            self.passed("respire_compatibility_migrated", profiles=len(fixtures),
                deleted_payloads_reencrypted=sum(len(f["legacy_tombstones"]) for f in fixtures))

    def restart_key_handoff(self):
        env = self.env("restart-key-handoff")
        profile = Path(env["RSRS_DATA_DIR"]).resolve()
        fixture = self.seed(env, profile, "rsrs", "restart-key-handoff")
        require(not any(name in env for name in ("RSRS_SUPER", "RSRS_PASS")),
                "restart_fixture_has_environment_credentials")
        if sys.platform == "darwin":
            require(self.session(env).get("keyring_account") == fixture["alias"],
                    "existing_target_credential_identity_changed")
            self.verify_mac_native_credentials(env, fixture)
        else:
            self.verify_retained_current_credentials(self.session(env), fixture)
        runtime = support.install_published_runtime(self.args.binary, env)
        require(runtime is not None, "restart_requires_verified_standalone_runtime")
        metadata = json.loads(json.loads((profile / "bin/runtime.json").read_bytes())["release"]["metadata_json"])
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        endpoint = "http://127.0.0.1:" + str(port)
        raw_env = dict(env, RSRS_RPC_PORT=str(port))
        output_path = profile / "keyless-runtime.log"
        with output_path.open("wb") as output:
            child = subprocess.Popen([str(runtime), "serve", "--host", "127.0.0.1", "--port", str(port),
                "--data-dir", str(profile)], cwd=runtime.parent, env=raw_env, stdin=subprocess.DEVNULL,
                stdout=output, stderr=output)
            try:
                self.report["stage"] = "keyless_restart_health"
                deadline = time.monotonic() + 90
                while True:
                    require(child.poll() is None, "keyless_runtime_exited_before_locked_probe")
                    require(time.monotonic() < deadline, "keyless_runtime_did_not_finish_startup")
                    try:
                        response = self.opener.open(endpoint + "/api/health", timeout=5)
                    except support.urllib.error.URLError as error:
                        if not isinstance(error.reason, ConnectionRefusedError):
                            raise
                        time.sleep(.1)
                        continue
                    with response:
                        require(response.status == 200, "keyless_runtime_health_http_failed")
                        health = json.load(response)
                    require(health.get("v") == 1 and health.get("pid") == child.pid
                        and health.get("runtime_version") == metadata["runtime_version"]
                        and health.get("image_sha256") == metadata["binary_sha256"]
                        and keyring_backend.canonical_identity(health.get("image_path", ""))
                            == keyring_backend.canonical_identity(runtime)
                        and keyring_backend.canonical_identity(health.get("installation_root", ""))
                            == keyring_backend.canonical_identity(profile)
                        and keyring_backend.canonical_identity(health.get("data_dir", ""))
                            == keyring_backend.canonical_identity(profile),
                        "keyless_runtime_identity_mismatch")
                    require(health.get("store_ready") is False, "runtime_read_native_credentials_without_host")
                    if health.get("startup_complete"):
                        require(health.get("state") == "locked" and health.get("credential_state") == "required",
                                "keyless_restart_did_not_remain_locked")
                        break
                    time.sleep(.1)
                self.report["stage"] = "keyless_restart_locked_read"
                request = scoped_rpc_request(health, ["--json", "show", fixture["id"]])
                read = support.urllib.request.Request(endpoint + "/api/rpc", json.dumps(request).encode(),
                    {"Content-Type": "application/json"}, method="POST")
                with self.opener.open(read, timeout=15) as response:
                    require(response.status == 200, "keyless_runtime_read_http_failed")
                    rejected = json.load(response)
                verify_rpc_receipt(request, rejected)
                require(rejected.get("ok") is False and rejected.get("code") == "runtime_locked",
                        "keyless_runtime_exposed_encrypted_memory")
                self.report["stage"] = "keyless_restart_stop"
                stop_identity = {key: health[key] for key in
                    ("installation_id", "instance_id", "account_id", "generation")}
                stop = support.urllib.request.Request(endpoint + "/api/runtime/stop", json.dumps(stop_identity).encode(),
                    {"Content-Type": "application/json"}, method="POST")
                with self.opener.open(stop, timeout=15) as response:
                    require(response.status == 200 and json.load(response).get("ok") is True,
                            "keyless_runtime_stop_rejected")
                require(child.wait(timeout=30) == 0, "keyless_runtime_stop_failed")
            finally:
                if child.poll() is None:
                    self.report["keyless_runtime_forced_cleanup"] = True
                    child.terminate()
                    child.wait(timeout=30)
        self.report["stage"] = "restart_host_unlock"
        if sys.platform == "darwin":
            require(self.session(env).get("keyring_account") == fixture["alias"],
                    "existing_target_credential_identity_changed")
            self.verify_mac_native_credentials(env, fixture)
        else:
            self.verify_retained_current_credentials(self.session(env), fixture)
        self.read_entry(env, fixture["id"], fixture["text"])
        written = self.cli(env, "remember", "Native restart host key handoff write", "--title",
            "restart-key-handoff-after", "--importance", "important", "--force")["summary"]["id"]
        self.read_entry(env, written, "Native restart host key handoff write")
        recalled = self.cli(env, "recall", fixture["text"], "--titles")
        require(fixture["id"] in json.dumps(recalled), "host_restart_recall_lost_original_memory")
        self.passed("restart_locked_with_native_credentials_then_host_unlock", native_credentials_preserved=True,
            keyless_restart_locked=True, locked_read_rejected=True, host_auto_unlock=True,
            remembered_and_read_back=True, original_memory_recalled=True)

    def run(self):
        expected_server = os.environ.get("RSRS_DEV_SERVER_SHA", "")
        require(os.environ.get("RSRS_DEV_SERVER_ADDR") == support.UPSTREAM
            and re.fullmatch("[0-9a-f]{40}", expected_server), "exact_development_server_required")
        with self.opener.open(support.UPSTREAM + "/health", timeout=45) as health:
            raw = health.read(1024 * 1024 + 1)
            require(len(raw) <= 1024 * 1024, "deployment_response_too_large")
            value = json.loads(raw)
            require(health.status == 200 and isinstance(value, dict) and value.get("source_revision") == expected_server,
                "development_server_revision_mismatch")
        self.report["server_sha"] = expected_server
        self.keys = keyring_backend.create(self.root / "keyring-home")
        self.report["keyring_backend"] = self.keys.name
        env = self.env("model-probe")
        version = subprocess.run([str(self.args.binary), "--version"], env=env, capture_output=True, timeout=20)
        require(version.returncode == 0 and re.search(r"(?<!\S)" + re.escape(self.args.version) + r"(?!\S)", version.stdout.decode()), "binary_version_mismatch")
        installed = self.cli(env, "model", "install-m3", timeout=900)
        support.share_cpu_container(self.root, env, self.model)
        require(all(digest(self.model / p) == h for p, h in support.MODEL_HASHES.items()), "model_hash_mismatch")
        # Installation already performs fresh inference without unlocking an absent library.
        probe = installed["details"]["backend_load"]["evidence"]
        require(probe.get("ready") is True and probe.get("inference_completed") is True
            and probe.get("fresh_session") is True and probe.get("dimensions") == 1024
            and str(probe.get("selected")).lower() == "cpu", "real_cpu_probe_failed")
        self.passed("model_cpu_real", dimensions=1024)
        self.restart_key_handoff()
        self.report.pop("stage", None)
        self.case("multiaccount", [(".onememory", "1memory")])
        self.case("respire", [(".respire", "respire")])
        self.case("interrupted", [(".respire", "respire")], interrupt=True)
        self.case("collision", [(".onememory", "1memory"), (".respire", "memocap")], target=True)
        self.negative_cases()
        self.report["status"] = "passed"

    def negative_cases(self):
        env = self.env("negative-source")
        fixture = self.seed(env, Path(env["HOME"]) / ".onememory", "1memory", "negative-source")
        from cryptography.exceptions import InvalidTag
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        attempt = self.env("wrong-data-key")
        source = Path(attempt["HOME"]) / ".onememory"
        source.mkdir()
        self.reserve_alias(source)
        shutil.copyfile(fixture["source"] / "session.json", source / "session.json")
        with closing(sqlite3.connect(fixture["source"] / "onememory.db")) as original, \
                closing(sqlite3.connect(source / "onememory.db")) as snapshot:
            original.backup(snapshot)
        wrong = "A3-" + secrets.token_hex(18)
        self.diagnostic_secrets.add(wrong)
        session = json.loads((source / "session.json").read_text())
        try:
            AESGCM(support.v4_kek(wrong, session["kdf_salt"], session["wrapped_urk"])).decrypt(
                bytes.fromhex(session["urk_nonce"]), support.encrypted_bytes(session["wrapped_urk"]), None)
        except InvalidTag:
            pass
        else:
            raise RuntimeError("wrong_data_key_unexpectedly_unwrapped")
        before = digest(source / "onememory.db"), digest(source / "session.json")
        rows = self.cli(self.default_env(attempt), "migrate")["details"]["profiles"]
        require(len(rows) == 1, "wrong_key_source_not_unique")
        result = subprocess.run([str(self.args.binary), "--direct", "--json", "migrate",
            "--source", rows[0]["source_id"], "--account", "wrong-key"],
            env=dict(self.default_env(attempt), RSRS_SUPER=wrong), cwd=self.root, capture_output=True, timeout=60)
        rejected = json.loads(result.stdout)
        require(result.returncode != 0 and rejected.get("status") == "fail"
            and any("migration_stage=exchange" in error
                and "migration_exchange=runtime_migration_outcome_unknown" in error
                for error in rejected.get("errors", [])),
            "wrong_key_not_rejected_by_runtime")
        intents = list((Path(attempt["HOME"]) / ".rsrs-migration-intents").glob("*.json"))
        require(len(intents) == 1, "wrong_key_runtime_intent_missing")
        intent = json.loads(intents[0].read_text(encoding="utf-8").splitlines()[-1])
        require(intent["phase"] == "copying" and intent["original"]["source_id"] == rows[0]["source_id"]
            and intent["original"]["account"] == "wrong-key"
            and not (Path(attempt["HOME"]) / ".rsrs/accounts/wrong-key").exists(),
            "wrong_key_published_or_changed_runtime_binding")
        require((digest(source / "onememory.db"), digest(source / "session.json")) == before,
                "wrong_key_changed_source")
        self.passed("migration_wrong_key_rejected", authenticated_unwrap_rejected=True,
            runtime_copying_observed=True, original_preserved=True, destination_unpublished=True)
        for label, primary in (("missing-pk", ""), ("composite-pk", ",PRIMARY KEY(id,title)")):
            attempt = self.env(label)
            source = Path(attempt["HOME"]) / ".onememory"
            source.mkdir()
            self.reserve_alias(source)
            shutil.copyfile(fixture["source"] / "session.json", source / "session.json")
            with sqlite3.connect(source / "onememory.db") as db:
                db.execute("CREATE TABLE memories(id TEXT,user TEXT,title TEXT,ciphertext TEXT,nonce TEXT,created_at TEXT,updated_at TEXT" + primary + ")")
                db.execute("INSERT INTO memories VALUES(?,?,?,?,?,?,?)", (fixture["id"],fixture["account"]["user"],label,
                    fixture["cipher"][0],fixture["cipher"][1],"2026-10-03T00:00:00Z","2026-10-03T00:00:00Z"))
            before = digest(source / "onememory.db")
            candidates = self.cli(self.default_env(attempt), "migrate")["details"]["profiles"]
            require(len(candidates) == 1, "invalid_schema_source_fixture_missing")
            result = subprocess.run([str(self.args.binary), "--direct", "--json", "migrate",
                "--source", candidates[0]["source_id"], "--account", "rejected-schema"],
                env=dict(self.default_env(attempt), RSRS_SUPER=fixture["code"]), cwd=self.root, capture_output=True, timeout=60)
            rejected = json.loads(result.stdout)
            require(result.returncode == 1 and rejected.get("status") == "fail"
                and any("incompatible pre-encrypted demo format" in error for error in rejected.get("errors", []))
                and not (Path(attempt["HOME"]) / ".rsrs/accounts/rejected-schema").exists(),
                "invalid_primary_key_migration_not_rejected")
            require(digest(source / "onememory.db") == before, "rejected_source_database_changed")
        self.passed("migration_incompatible_primary_keys_rejected", genuine_ciphertext_preserved=True, variants=2)
        link_env = self.env("symlink-root")
        link_home = Path(link_env["HOME"])
        (link_home / ".onememory").symlink_to(fixture["source"], target_is_directory=True)
        source_database = digest(fixture["source"] / "onememory.db")
        source_session = digest(fixture["source"] / "session.json")
        listed = subprocess.run([str(self.args.binary), "--direct", "--json", "migrate"],
            env=self.default_env(link_env), cwd=self.root, capture_output=True, timeout=60)
        listing = json.loads(listed.stdout)
        link_errors = ("symbolic link", "discovery path contains a symlink",
                       "discovery path contains a reparse point")
        if listed.returncode != 0:
            require(listing.get("status") == "fail" and any(term in error
                for error in listing.get("errors", []) for term in link_errors),
                "symlink_listing_failed_for_unrelated_reason")
        else:
            require(listing.get("status") == "ok", "symlink_listing_status_invalid")
            for row in listing["details"]["profiles"]:
                result = subprocess.run([str(self.args.binary), "--direct", "--json", "migrate",
                    "--source", row["source_id"], "--account", "rejected-link"],
                    env=self.default_env(link_env), cwd=self.root, capture_output=True, timeout=60)
                rejection = json.loads(result.stdout)
                require(result.returncode != 0 and rejection.get("status") == "fail"
                    and any(term in error for error in rejection.get("errors", []) for term in link_errors),
                    "symlink_root_migration_not_rejected")
        # A host rejection may write its fixed error log, but no library or Runtime state.
        destination = link_home / ".rsrs"
        require(not destination.exists() or all(not path.is_symlink()
                and str(path.relative_to(destination)).replace("\\", "/") in ("logs", "logs/cli.log")
                for path in destination.rglob("*")), "symlink_root_migration_created_destination")
        require(digest(fixture["source"] / "onememory.db") == source_database
            and digest(fixture["source"] / "session.json") == source_session,
            "rejected_symlink_source_changed")
        self.passed("migration_symlink_root_rejected", external_target_is_owned_fixture=True)
        source_session = digest(fixture["source"] / "session.json")
        candidate = self.cli(self.default_env(env), "migrate")["details"]["profiles"][0]
        destination = Path(env["HOME"]) / ".rsrs"

        def destination_snapshot():
            if not destination.exists():
                return None
            snapshot = {}
            for path in (destination, *sorted(destination.rglob("*"))):
                stat = path.lstat()
                snapshot[str(path.relative_to(destination))] = (
                    stat.st_mode, stat.st_size, stat.st_mtime_ns,
                    digest(path) if path.is_file() else None)
            return snapshot

        # The host-side cleanup above can already create runtime bookkeeping.
        # Client-only commands must preserve that baseline as well as source data.
        destination_before = destination_snapshot()
        for variable in ("RSRS_CLIENT_ONLY", "RSRS_NO_AUTOSTART"):
            guarded = self.default_env(env)
            guarded[variable] = "1"
            result = subprocess.run([str(self.args.binary), "--version"], env=guarded,
                cwd=self.root, capture_output=True, timeout=30)
            require(result.returncode == 0 and destination_snapshot() == destination_before
                and digest(fixture["source"] / "session.json") == source_session, "client_only_migration_wrote_data")
            blocked = subprocess.run([str(self.args.binary), "--json", "migrate",
                "--source", candidate["source_id"], "--account", "blocked"], env=guarded,
                cwd=self.root, capture_output=True, timeout=30)
            rejection = json.loads(blocked.stdout)
            require(blocked.returncode == 1 and rejection.get("status") == "fail"
                and any("client_only:" in error for error in rejection.get("errors", []))
                and destination_snapshot() == destination_before
                and digest(fixture["source"] / "session.json") == source_session,
                "client_only_explicit_migration_wrote_data")
        self.passed("migration_client_only_does_not_write", guards=2)
