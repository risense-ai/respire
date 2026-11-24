"""Verify that cross-server login copies data once and preserves the source."""
import json
from pathlib import Path
import secrets
import sqlite3

import legacy_vault_fixture as legacy
from legacy_vault_fixture import require
from login_interruption_control import safe_cli_diagnostic


def verify_server_transfer(cli, report, state, server, code, urk, original_rows):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    report["stage"] = "server_transfer_list_accounts"
    source_profile = json.loads(cli("--json", "account", "list").stdout)["details"]["accounts"]
    source_account = next(row for row in source_profile if row["current"])
    source_name = source_account["name"]
    source_path = Path(source_account["dir"])
    source_session = source_path / "session.json"
    report["stage"] = "legacy_recovery_stop_runtime"
    cli("--runtime-internal", "--stop")
    source_value = json.loads(source_session.read_bytes())
    source_value["addr"] = "https://previous.invalid"
    # Keep the current source's native custody; inline factors plus the
    # existing login journal are rejected before a transfer is admitted.
    require(all(field not in source_value for field in ("super", "pass", "secret", "secret_key")),
            "server_transfer_changed_source")
    source_session.write_text(json.dumps(source_value), encoding="utf-8")
    with sqlite3.connect((source_path / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
        source_before = source_session.read_bytes(), db.execute("SELECT id,ciphertext,nonce FROM memories ORDER BY id").fetchall()
    cloud_urk = secrets.token_bytes(32)
    cloud_salt, cloud_nonce = secrets.token_hex(16), secrets.token_bytes(12)
    state["vault"] = {"version": 4, "kdf_salt": cloud_salt, "urk_nonce": cloud_nonce.hex(),
        "wrapped_urk": legacy.PREFIX + AESGCM(legacy.v4_kek(code, cloud_salt, legacy.PREFIX)).encrypt(cloud_nonce, cloud_urk, None).hex()}
    report["transfer_http"] = []
    state.update(sync_blobs={}, sync_pulls=0, transfer_stage="initial_login",
                 transfer_http=report["transfer_http"])
    # Keep the fresh cloud vault separate from main's existing remote identity.
    transfer_addr = f"http://localhost:{server.server_port}"
    transfer = ("--json", "login", "--interactive", "--addr", transfer_addr, "--user", "synthetic",
                "--pass=synthetic-password", "--super", code, "--migrate-local")
    report["stage"] = "server_transfer_initial_login"
    first_result = cli(*transfer, success=False)
    report["transfer_initial_response"] = {"exit_code": first_result.returncode,
        "diagnostic": safe_cli_diagnostic(first_result.stdout, first_result.stderr),
        "sync_pulls": state["sync_pulls"], "synced_records": len(state["sync_blobs"])}
    require(any(event == {"stage": "initial_login", "method": "POST",
                "route": "/push/batch", "status": 503} for event in state["transfer_http"]),
            "server_transfer_fault_not_observed")
    require(first_result.returncode == 2, "server_transfer_sync_warning_missing")
    first = json.loads(first_result.stdout)["summary"]
    require(first["sync"]["status"] == "pending"
            and first["sync"]["reason"] == "post_import_sync_failed",
            "server_transfer_sync_warning_missing")
    require(first["import"]["imported"] == 1 and first["import"]["duplicates"] == 0,
            "server_transfer_count_failed")
    target = Path(first["dir"])
    require(target.resolve() != source_path.resolve(), "server_transfer_reused_source")
    with sqlite3.connect((target / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
        copied = db.execute("SELECT ciphertext,nonce FROM memories WHERE deleted=0").fetchall()
    require(len(copied) == 1, "server_transfer_copy_missing")
    copied_payload = json.loads(legacy.decrypt_content(cloud_urk, *copied[0]))
    original_payload = json.loads(legacy.decrypt_content(urk, original_rows[0][1], original_rows[0][2]))
    require(all(copied_payload[field] == original_payload[field] for field in ("content", "title")),
            "server_transfer_not_reencrypted")
    report["stage"] = "server_transfer_select_source"
    state["transfer_stage"] = "select_source"
    cli("--json", "account", "use", source_name)
    report["stage"] = "server_transfer_retry_login"
    state["transfer_stage"] = "retry_login"
    retry_result = cli(*transfer)
    retry = json.loads(retry_result.stdout)["summary"]
    require(retry["import"]["imported"] == 0 and retry["import"]["duplicates"] == 1,
            "server_transfer_retry_duplicated")
    require(len(state["sync_blobs"]) == 1, "server_transfer_remote_duplicated")
    with sqlite3.connect((source_path / "rsrs.db").as_uri() + "?mode=ro", uri=True) as db:
        require((source_session.read_bytes(), db.execute("SELECT id,ciphertext,nonce FROM memories ORDER BY id").fetchall())
                == source_before, "server_transfer_changed_source")
    report["cases"]["different_server_transfer_retry"] = {"passed": True,
        "source_preserved": True, "different_urk": True, "duplicates_skipped": True}
