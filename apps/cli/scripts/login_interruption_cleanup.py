"""Reap owned processes and clean credentials from the interruption fixture."""
import json
from pathlib import Path
import re
import subprocess
import uuid

from legacy_vault_fixture import require
from login_interruption_control import (
    process_group_members,
    safe_cli_diagnostic,
    stop_owned_control,
    verify_credential_provider,
)


def cleanup_fixture(args, report, root, child, state, release, env, keys,
                    migration_keys, native_aliases, server):
    if not report["passed"]:
        report["failure_stage"] = report["stage"]
    report["stage"] = "owned_child_cleanup"
    if child is not None:
        child_running = child.poll() is None
        members = process_group_members(child.pid)
        report["cleanup_snapshot"] = {"child_running": child_running,
            "members": len(members), "helpers": sum(row["control_helper"] for row in members.values()),
            "zombies": sum(row["state"] == "Z" for row in members.values())}
        if child_running:
            stop_owned_control(child)
        try:
            stdout, stderr = child.communicate(timeout=15)
        except subprocess.TimeoutExpired as error:
            stdout = error.output or b""
            stderr = error.stderr or b""
        except OSError:
            stdout = b""
            stderr = b""
        report["migration_child"] = {"exit_code": child.returncode,
            "migration_stage": None, "migration_exchange": None, "register_kind": None,
            "diagnostic": safe_cli_diagnostic(stdout, stderr), "operation_phase": None}
        report["fixture_requests"] = {name: state.get(name, 0)
            for name in ("gets", "posts", "logins", "password_posts")}
        try:
            value = json.loads(stdout)
        except (ValueError, UnicodeError, RecursionError):
            value = None
        recovery_path = root / ".login-recovery.json"
        # Observe only this response's owned journal; never recover or replay it.
        try:
            details = value.get("details") if isinstance(value, dict) else None
            operation_id = details.get("operation_id") if isinstance(details, dict) else None
            source = details.get("source_profile") if isinstance(details, dict) else None
            if (isinstance(operation_id, str) and str(uuid.UUID(operation_id)) == operation_id
                    and uuid.UUID(operation_id).version == 4 and isinstance(source, str)
                    and Path(source).resolve() == root.resolve()
                    and not recovery_path.is_symlink()
                    and recovery_path.resolve(strict=True) == root.resolve() / ".login-recovery.json"):
                journal = json.loads(recovery_path.read_bytes())
                records = [row for row in journal.get("operations", [])
                    if isinstance(row, dict) and row.get("operation_id") == operation_id]
                if (journal.get("schema_version") == 2 and len(records) == 1
                        and (journal.get("active") or {}).get("operation_id") == operation_id):
                    record = records[0]
                    phase = record.get("phase")
                    if (record.get("idempotency_key") == operation_id
                            and (journal.get("active") or {}).get("alias") == record.get("alias")
                            and re.fullmatch(r"login-[a-f0-9]{32}", record.get("alias", "")) is not None
                            and record.get("root") == str(root.resolve())
                            and record.get("profile") == str(root.resolve())
                            and phase in ("admitted", "login_input", "remote_pending", "remote_unknown",
                                "authorized", "prepared", "awaiting_verification", "rolling_back",
                                "committed", "rolled_back")):
                        report["migration_child"]["operation_phase"] = phase
        except (OSError, ValueError, TypeError, AttributeError, RecursionError):
            pass  # Diagnostic absence cannot replace the original fixture failure.
        errors = value.get("errors") if isinstance(value, dict) else None
        text = "\n".join(error for error in errors if isinstance(error, str)) if isinstance(errors, list) else ""
        migration_stages = ("paused_grant", "owner_check", "drain_owner", "drained_check",
            "selection_before_send", "save_sent", "exchange", "encode_unknown", "selection_after_receipt",
            "restore_owner_start", "restore_owner_probe", "restore_owner_verify", "restore_store_ready",
            "selection_before_finish", "finish_maintenance", "save_terminal", "paused_publication")
        exchange_categories = ("spawn", "channel", "frame", "encoding", "deadline", "child_exit",
            "response_binding", "response_profile", "response_envelope", "response_readiness", "result_identity",
            "unknown_receipt", "terminal_receipt", "terminal_details", "unclassified", "runtime_invalid_request",
            "runtime_protocol_version_mismatch", "runtime_control_unavailable", "runtime_auth_recovery_pending",
            "runtime_unauthorized", "runtime_auth_ownership_unavailable", "runtime_scope_mismatch",
            "runtime_migration_outcome_unknown", "runtime_control_outcome_unknown")
        register_kinds = ("register_remote_unknown", "register_native_credentials", "register_state_conflict",
            "register_material_invalid", "register_local_compatibility", "register_unclassified")
        for name, allowed in (("migration_stage", migration_stages), ("migration_exchange", exchange_categories),
                              ("register_kind", register_kinds)):
            token = re.search(r"\b" + name + r"=([a-z_]+)(?=$|[;\s])", text)
            if token and token[1] in allowed and (name != "migration_exchange"
                    or report["migration_child"]["migration_stage"] == "exchange"):
                report["migration_child"][name] = token[1]
        if not child_running:
            stop_owned_control(child)
    # Never unblock a request while a surviving private helper can publish it.
    release.set()
    report["stage"] = "runtime_cleanup"
    stopped = subprocess.run([str(args.binary), "--runtime-internal", "--stop"], env=env, capture_output=True, timeout=60)
    report["runtime_cleanup"] = {"passed": stopped.returncode in (0, 2)}
    report["stage"] = "credential_cleanup"
    if keys:
        verify_credential_provider(keys, report)
        # Discover interrupted aliases only from our synthetic files, including
        # failures before a barrier. Never claim arbitrary credential-store items.
        for path in args.root.rglob("*.json"):
            if path.name.startswith("account-login-"):
                custody = json.loads(path.read_bytes())
                operation_id = custody["operation_id"]
                parsed = uuid.UUID(operation_id)
                require(parsed.version == 4 and str(parsed) == operation_id
                        and path == Path(custody["root"]) / "runtime/autostart" / ("account-login-" + operation_id + ".json")
                        and all(Path(custody[key]).resolve().is_relative_to(args.root.resolve())
                                for key in ("root", "source"))
                        and custody["account"] == "login-" + parsed.hex
                        and custody["backend"] == "secret_service", "unowned_fixture_login_custody")
                native_aliases.add(custody["account"])
                continue
            if path.name not in ("session.json", ".login-recovery.json"):
                continue
            value = json.loads(path.read_bytes())
            if path.name == ".login-recovery.json" and value.get("schema_version") == 2:
                for operation in value["operations"]:
                    require(all(Path(operation[key]).resolve().is_relative_to(args.root.resolve())
                                for key in ("root", "profile")), "credential_journal_outside_fixture")
                    recorded_alias = operation["alias"]
                    require(re.fullmatch(r"login-[a-f0-9]{32}", recorded_alias) is not None,
                            "unowned_fixture_operation_alias")
                    native_aliases.add(recorded_alias)
            alias = value.get("keyring_account") if path.name == "session.json" else (
                (value.get("active") or {}).get("alias") if "schema_version" in value else value.get("alias"))
            if alias is not None:
                require(re.fullmatch(r"(?:login-[a-f0-9]{32}|legacy-[a-f0-9]{16})", alias) is not None,
                        "unowned_fixture_alias")
                native_aliases.add(alias)
        # The authenticated private collection started empty. Require an exact
        # alias from these owned transactions as well as the credential schema.
        for item in keys.collection.get_all_items():
            attributes = item.get_attributes()
            require(attributes.get("service") == "rsrs" and re.fullmatch(r"(?:(?:super|pass):(?:login-[a-f0-9]{32}|legacy-[a-f0-9]{16})|(?:login-control|key-control):login-[a-f0-9]{32})", attributes.get("username", "")), "unexpected_private_credential")
            require(attributes["username"].split(":", 1)[1] in native_aliases,
                    "credential_not_bound_to_fixture_transaction")
            item.delete()
        require(not list(keys.collection.get_all_items()), "native_fixture_cleanup_failed")
        report["native_cleanup"] = keys.cleanup()
    elif migration_keys:
        report["native_cleanup"] = migration_keys.cleanup()
    report["stage"] = "server_cleanup"
    server.shutdown()
    server.server_close()
    report["server_cleanup"] = {"passed": True, "scope": "loopback_fixture"}
