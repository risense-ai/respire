"""Verify host-owned key and login continuation against the fixture account."""
import json
from pathlib import Path
import socket
import sqlite3
import subprocess
import time
import uuid

import legacy_vault_fixture as legacy
from legacy_vault_fixture import require
from login_interruption_control import (
    account_recovery_flags,
    fixture_runtime_owners,
    key_continuation_receipt,
    login_continuation_receipt,
    login_terminal_operation,
    remove_fixture_super,
    runtime_stop_receipt,
    safe_cli_diagnostic,
)
from native_fixture_process import install_published_runtime


def host_key_continuation(binary, directory, env, state, addr, code, keys, source, rpc, observation):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    """Reach Runtime's missing-super branch without modifying its operation journal."""
    observation.update(passed=False, stage="remove_source_super")
    session_path = source / "session.json"
    original_session = session_path.read_bytes()
    vault = json.loads(original_session)
    original_urk = AESGCM(legacy.v4_kek(code, vault["kdf_salt"], vault["wrapped_urk"])).decrypt(
        bytes.fromhex(vault["urk_nonce"]), legacy.encrypted_bytes(vault["wrapped_urk"]), None)
    remove_fixture_super(keys, session_path, directory, addr, code)
    effects = state["logins"], state["posts"], state["password_posts"]
    observation["stage"] = "reset_without_super"
    pending, initial_transport = rpc("auth.reset", {}, 2)
    operation_id = pending.get("operation_id")
    require(isinstance(operation_id, str) and str(uuid.UUID(operation_id)) == operation_id
            and uuid.UUID(operation_id).version == 4 and operation_id != initial_transport,
            "key_continuation_identity_mismatch")
    key_continuation_receipt(pending, operation_id, source, "awaiting_input")
    require(session_path.read_bytes() == original_session
            and (state["logins"], state["posts"], state["password_posts"]) == effects,
            "key_continuation_effects_mismatch")
    journal_path = Path(env["RSRS_DATA_DIR"]) / ".login-recovery.json"
    pending_journal = journal_path.read_bytes()

    def query(expected_exit, outcome):
        result = subprocess.run([str(binary), "--json", "runtime", "key-result", "--operation-id", operation_id,
            "--source-profile", str(source.resolve())], env=env, capture_output=True, timeout=180)
        envelope = json.loads(result.stdout)
        require(result.returncode == expected_exit and envelope.get("status") == ("pending" if expected_exit == 2 else "ok")
                and not envelope.get("errors"), "key_continuation_query_failed")
        summary = envelope.get("summary") or {}
        key_continuation_receipt(summary, operation_id, source, outcome)
        return summary

    observation.update(operation_id=operation_id, stage="query_original_pending")
    query(2, "awaiting_input")
    require(session_path.read_bytes() == original_session and journal_path.read_bytes() == pending_journal
            and (state["logins"], state["posts"], state["password_posts"]) == effects,
            "key_continuation_effects_mismatch")
    observation["stage"] = "continue_original_key"
    params = {"operation_id": operation_id, "source_profile": str(source.resolve()),
              "input": {"super_password": code}}
    committed, continue_transport = rpc("auth.key.continue", params, 0)
    key_continuation_receipt(committed, operation_id, source, "committed")
    require(continue_transport not in (initial_transport, operation_id), "login_continuation_transport_mismatch")
    require((state["logins"], state["posts"], state["password_posts"])
            == (effects[0], effects[1] + 1, effects[2]), "key_continuation_effects_mismatch")
    effects = state["logins"], state["posts"], state["password_posts"]
    saved = session_path.read_bytes(), journal_path.read_bytes(), dict(state["vault"])
    observation["stage"] = "recover_committed_key"
    require(query(0, "committed")["super"] == committed["super"], "key_continuation_material_changed")
    repeated, repeated_transport = rpc("auth.key.continue", params, 0)
    key_continuation_receipt(repeated, operation_id, source, "committed")
    require(repeated_transport not in (initial_transport, continue_transport, operation_id),
            "login_continuation_transport_mismatch")
    require(repeated["super"] == committed["super"]
            and (session_path.read_bytes(), journal_path.read_bytes(), state["vault"]) == saved
            and (state["logins"], state["posts"], state["password_posts"]) == effects,
            "key_continuation_material_changed")
    vault = json.loads(session_path.read_bytes())
    published = {"version": vault["vault_version"], **{key: vault[key]
                 for key in ("kdf_salt", "wrapped_urk", "urk_nonce")}}
    require(published == state["vault"] and committed["super"] != code, "key_continuation_material_changed")
    new_urk = AESGCM(legacy.v4_kek(committed["super"], vault["kdf_salt"], vault["wrapped_urk"])).decrypt(
        bytes.fromhex(vault["urk_nonce"]), legacy.encrypted_bytes(vault["wrapped_urk"]), None)
    require(new_urk == original_urk, "key_continuation_unlock_failed")
    observation["stage"] = "post_reset_memory"
    plaintext, title = "Synthetic memory after key continuation", "Key continuation fixture"
    written = subprocess.run([str(binary), "--json", "remember", plaintext, "--title", title,
        "--force", "--importance", "important"], env=env, capture_output=True, timeout=180)
    require(written.returncode == 0, "key_continuation_memory_failed")
    with sqlite3.connect((source / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
        rows = db.execute("SELECT id,ciphertext,nonce FROM memories WHERE title=? AND deleted=0", (title,)).fetchall()
    require(len(rows) == 1 and plaintext.encode() in legacy.decrypt_content(new_urk, rows[0][1], rows[0][2]),
            "key_continuation_memory_failed")
    shown = subprocess.run([str(binary), "--json", "show", rows[0][0]], env=env,
        capture_output=True, timeout=180)
    require(shown.returncode == 0 and plaintext in json.dumps(json.loads(shown.stdout)),
            "key_continuation_memory_failed")
    # Recall can observe the persisted source before its derived index is ready.
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        recalled = subprocess.run([str(binary), "--json", "recall", plaintext, "--titles"], env=env,
            capture_output=True, timeout=180)
        envelope = json.loads(recalled.stdout)
        require(recalled.returncode == 0 and envelope.get("status") == "ok" and not envelope.get("errors"),
                "key_continuation_memory_failed")
        if any(row.get("id") == rows[0][0] and row.get("title") == title for row in envelope["details"]):
            break
        time.sleep(.1)
    else:
        raise RuntimeError("key_continuation_memory_failed")
    observation.update(passed=True, stage="committed", awaiting_input_observed=True,
        source_super_only_deleted=True, original_operation_committed=True, distinct_transport_ids=True,
        reset_published_once=True, recovery_without_reset_replay=True, repeated_material_unchanged=True,
        new_super_decrypts_original_urk=True, post_reset_remember_recall=True)


def host_login_continuation(binary, directory, env, state, addr, code, observation, keys, key_observation):
    """Real host admission without a super key; only the original operation may resume."""
    directory.mkdir(mode=0o700)
    source, home = directory / "library", directory / "home"
    source.mkdir(mode=0o700)
    home.mkdir(mode=0o700)
    command_env = dict(env, HOME=str(home), RSRS_DATA_DIR=str(source))
    command_env.pop("RSRS_SUPER", None)
    command_env.pop("RSRS_RUNTIME_BIN", None)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        command_env["RSRS_RPC_PORT"] = str(listener.getsockname()[1])
    observation.update(passed=False, stage="runtime_install")
    installed_runtime = install_published_runtime(binary, command_env)
    images = [binary]
    if installed_runtime is not None:
        images.append(installed_runtime)
    if command_env.get("RSRS_BIN_DIR"):
        images.append(Path(command_env["RSRS_BIN_DIR"]) / "rsrs")

    def rpc(method, params, expected_exit):
        transport_id = "host-login-" + uuid.uuid4().hex
        request = {"v": 1, "id": transport_id, "method": method, "params": params}
        result = subprocess.run([str(binary), "--json", "--host-rpc-stdio"],
            input=json.dumps(request).encode(), env=command_env, capture_output=True, timeout=180)
        reply = json.loads(result.stdout)
        envelope = reply.get("envelope") or {}
        status, outcome = envelope.get("status"), (envelope.get("summary") or {}).get("outcome")
        observation["last_rpc"] = {"method": method, "exit": result.returncode,
            "status": status if status in ("pending", "ok", "warn", "fail") else "unrecognized",
            "outcome": outcome if outcome in ("awaiting_input", "awaiting_verification", "committed",
                "rolled_back", "pending", "unknown") else "absent-or-unrecognized"}
        if result.returncode != expected_exit:
            observation["last_rpc"]["diagnostic"] = safe_cli_diagnostic(json.dumps(envelope).encode(), result.stderr)
        require(result.returncode == expected_exit and reply.get("v") == 1
                and reply.get("id") == transport_id and reply.get("exit") == expected_exit
                and reply.get("ok") is (expected_exit == 0), "login_continuation_transport_mismatch")
        require(envelope.get("status") == ("pending" if expected_exit == 2 else "ok")
                and not envelope.get("errors"), "login_continuation_envelope_mismatch")
        return envelope["summary"], transport_id

    def query(expected_exit):
        result = subprocess.run([str(binary), *account_recovery_flags("account-result", record)],
            env=command_env, capture_output=True, timeout=180)
        require(result.returncode == expected_exit, "login_continuation_query_failed")
        envelope = json.loads(result.stdout)
        require(envelope.get("status") == ("pending" if expected_exit == 2 else "ok")
                and not envelope.get("errors"), "login_continuation_envelope_mismatch")
        return envelope["summary"]

    effects = state["logins"], state["posts"], state["password_posts"]
    try:
        observation["stage"] = "admit_without_super"
        pending, initial_transport = rpc("auth.login",
            {"addr": addr, "user": "synthetic", "password": "synthetic-password"}, 2)
        operation_id = pending.get("operation_id")
        require(isinstance(operation_id, str), "original_operation_missing")
        parsed = uuid.UUID(operation_id)
        require(parsed.version == 4 and str(parsed) == operation_id
                and operation_id != initial_transport, "original_operation_invalid")
        login_continuation_receipt(pending, operation_id, source, addr, "awaiting_input")
        journal_path = source / ".login-recovery.json"
        journal = json.loads(journal_path.read_bytes())
        rows = [row for row in journal["operations"] if row["operation_id"] == operation_id]
        require(len(rows) == 1 and rows[0]["idempotency_key"] == operation_id
                and rows[0]["root"] == str(source.resolve()) and rows[0]["profile"] == str(source.resolve())
                and journal["active"]["operation_id"] == operation_id, "original_operation_binding_changed")
        record = rows[0]
        require((state["logins"], state["posts"], state["password_posts"])
                == (effects[0] + 1, effects[1], effects[2]), "login_continuation_admission_effects_mismatch")
        effects = state["logins"], state["posts"], state["password_posts"]
        observation.update(operation_id=operation_id, stage="query_original_pending")
        login_continuation_receipt(query(2), operation_id, source, addr, "awaiting_input")
        require((state["logins"], state["posts"], state["password_posts"]) == effects,
                "result_replayed_authentication")
        observation["stage"] = "continue_original"
        params = {"operation_id": operation_id, "source_profile": str(source.resolve()),
                  "continuation": {}, "super_password": code}
        committed, continue_transport = rpc("auth.login.continue", params, 0)
        require(continue_transport != initial_transport and continue_transport != operation_id,
                "login_continuation_transport_mismatch")
        login_continuation_receipt(committed, operation_id, source, addr, "committed")
        target = Path(committed["target_profile"])
        require(target.resolve().is_relative_to(source.resolve()), "login_continuation_target_escaped")
        session_path = target / "session.json"
        login_terminal_operation(json.loads(journal_path.read_bytes()), record, committed, session_path.read_bytes())
        saved = session_path.read_bytes(), journal_path.read_bytes()
        observation["stage"] = "recover_committed_original"
        login_continuation_receipt(query(0), operation_id, source, addr, "committed")
        repeated, repeated_transport = rpc("auth.login.continue", params, 0)
        require(repeated_transport not in (initial_transport, continue_transport, operation_id),
                "login_continuation_transport_mismatch")
        login_continuation_receipt(repeated, operation_id, source, addr, "committed")
        require(not (source / "runtime/autostart/maintenance.json").exists(),
                "login_continuation_maintenance_not_finished")
        require((session_path.read_bytes(), journal_path.read_bytes()) == saved,
                "repeated_result_changed_material")
        require((state["logins"], state["posts"], state["password_posts"]) == effects,
                "result_replayed_authentication")
        observation.update(stage="committed", awaiting_input_observed=True, original_operation_committed=True,
            recovery_without_authentication_replay=True, distinct_transport_ids=True, repeated_material_unchanged=True)
        host_key_continuation(binary, directory, command_env, state, addr, code, keys, target, rpc, key_observation)
    finally:
        observation["runtime_cleanup"] = {"passed": False}
        stopped = subprocess.run([str(binary), "--json", "--runtime-internal", "--stop"],
            env=command_env, capture_output=True, timeout=60)
        runtime_stop_receipt(stopped)
        require(not fixture_runtime_owners(directory, images), "owned_runtime_cleanup_failed")
        observation["runtime_cleanup"].update(passed=True, stopped_confirmed=True, no_live_owner=True)
        key_observation["runtime_cleanup"] = dict(observation["runtime_cleanup"])
    observation["passed"] = True
