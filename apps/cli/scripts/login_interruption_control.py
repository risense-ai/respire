"""Report and validate the owned login interruption and recovery operations."""
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import uuid

from legacy_vault_fixture import require


# Only fixed fixture assertions may become diagnostic strings. Never serialize
# arbitrary exception text, subprocess output, paths or account material.
FAILURE_CODES = frozenset(['cli_expectation_failed', 'API_config_changed', 'DEV4_recovery_failed', 'SIGINT_fixture_requires_linux_pidfd', 'binary_hash_mismatch', 'binary_version_mismatch', 'ciphertext_fixture_missing', 'cleanup_control_helper_still_running', 'cleanup_process_identity_changed', 'cleanup_process_lifetime_unproven', 'cleanup_untracked_group_member', 'commit_barrier_not_reached', 'credential_journal_outside_fixture', 'credential_not_bound_to_fixture_transaction', 'data_key_changed', 'durable_recovery_missing', 'explicit_retry_login_count_changed', 'fresh_fixture_required', 'fresh_login_replaced_maintenance_owner', 'fresh_login_replaced_original_operation', 'fresh_login_replayed_authentication', 'full_library_namespace_not_migrated', 'inconsistent_factors_modified_state', 'inconsistent_profile_overwritten', 'interrupted_control_helper_still_running', 'interrupted_group_still_running', 'interrupted_native_credential_not_deleted', 'interrupted_native_password_not_deleted', 'interrupted_operation_advanced', 'interrupted_phase_not_proven', 'interruption_already_completed', 'interruption_child_not_owned', 'interruption_control_helper_not_bound', 'interruption_group_changed', 'interruption_group_has_unbound_member', 'interruption_session_not_owned', 'invalid_publication_barrier', 'invalid_recovery_action', 'failed_login_requires_clean_journal', 'failed_login_interrupted_credential_not_deleted', 'failed_login_original_ciphertexts_changed', 'failed_login_original_profile_not_restored', 'failed_login_recovery_binding_invalid', 'migrated_fixture_witness_invalid', 'migration_effects_not_exactly_once', 'native_alias_missing', 'native_backend_not_used', 'native_credential_not_written', 'native_fixture_cleanup_failed', 'operation_journal_schema_missing', 'operation_recovery_not_retired', 'original_account_identity_changed', 'original_candidate_changed_during_completion', 'original_ciphertexts_changed', 'original_factors_not_recovered', 'original_login_password_changed', 'original_operation_binding_changed', 'original_operation_invalid', 'original_operation_missing', 'original_operation_not_committed', 'original_operation_not_unique', 'original_password_not_written', 'original_plaintext_not_recovered', 'original_profile_not_discovered', 'original_profile_not_migrated', 'original_profile_not_restored', 'original_profile_selection_failed', 'original_recovery_image_changed', 'original_result_not_repeatable', 'original_rollback_not_confirmed', 'original_source_changed', 'original_terminal_receipt_missing', 'original_vault_factors_changed', 'owned_child_not_interrupted', 'owned_runtime_cleanup_failed', 'password_update_not_authorized', 'publication_checkpoint_not_proven', 'publication_window_not_proven', 'repeated_result_changed_material', 'result_replayed_authentication', 'rollback_receipt_not_retained', 'rollback_replayed_authentication', 'server_transfer_changed_source', 'server_transfer_copy_missing', 'server_transfer_count_failed', 'server_transfer_not_reencrypted', 'server_transfer_remote_duplicated', 'server_transfer_retry_duplicated', 'server_transfer_reused_source', 'server_transfer_sync_warning_missing', 'transfer_push_not_authorized', 'unexpected_private_credential', 'unknown_result_changed_material', 'unknown_result_released_maintenance', 'unknown_result_replayed_effect', 'unknown_rollback_changed_material', 'unknown_rollback_deleted_credentials', 'unowned_fixture_alias', 'unowned_fixture_operation_alias', 'wrong_recovery_modified_state'])


FAILURE_CODES = FAILURE_CODES | frozenset({
    "login_continuation_identity_mismatch", "login_continuation_inputs_mismatch",
    "login_continuation_transport_mismatch", "login_continuation_envelope_mismatch",
    "login_continuation_query_failed", "login_continuation_admission_effects_mismatch",
    "login_continuation_maintenance_not_finished",
    "original_operation_binding_changed_idempotency_key", "original_operation_binding_changed_profile",
    "original_operation_binding_changed_root", "original_operation_binding_changed_alias",
    "login_continuation_target_escaped", "unowned_fixture_login_custody",
    "key_continuation_identity_mismatch", "key_continuation_inputs_mismatch",
    "key_continuation_effects_mismatch", "key_continuation_material_changed",
    "key_continuation_unlock_failed", "key_continuation_memory_failed",
    "key_continuation_credential_not_owned", "key_continuation_query_failed",
    "server_transfer_fault_not_observed",
    "fixture_secret_service_daemon_exited", "fixture_secret_service_owner_mismatch",
})


@contextmanager
def fixture_report(args, report):
    report["stage"] = "bootstrap"
    try:
        yield
    except BaseException as error:
        report["passed"] = False
        code = str(error) if type(error) is RuntimeError and str(error) in FAILURE_CODES else {
            subprocess.TimeoutExpired: "subprocess_timeout", FileNotFoundError: "required_file_missing",
            PermissionError: "permission_denied", json.JSONDecodeError: "invalid_json",
            KeyboardInterrupt: "interrupted", SystemExit: "early_exit"}.get(type(error), "unclassified_failure")
        report["failure_code"] = code
        raise
    finally:
        # Outermost write also runs when setup or an owned cleanup check raises.
        (args.root / "interruption-coverage.json").write_text(json.dumps(report, indent=2) + "\n")


def verify_credential_provider(keys, report):
    # A crashed provider can be replaced by D-Bus activation. Never
    # enumerate or delete from a collection whose owner is unverified.
    provider = report["credential_provider"] = {"exit_code": keys.daemon.poll(), "owner_verified": False}
    keys.verify_owner()
    provider["owner_verified"] = True


def safe_cli_diagnostic(stdout, stderr):
    """Keep fixed classifications only; subprocess output can contain account material."""
    try:
        value = json.loads(stdout)
    except (ValueError, UnicodeError, RecursionError):
        value = None
    summary = value.get("summary") if isinstance(value, dict) else None
    reason = summary.get("reason") if isinstance(summary, dict) else None
    reasons = ("account_outcome_unknown", "control_outcome_unknown", "auth_outcome_unknown",
               "request_outcome_unknown", "runtime_error", "invalid_input",
               "account_handoff_pending", "identity_write_recovery_required")
    errors = value.get("errors") if isinstance(value, dict) else None
    text = "\n".join(error for error in errors if isinstance(error, str)) if isinstance(errors, list) else ""
    text += "\n" + stderr.decode("utf-8", errors="replace")
    categories = (
        ("key_source_inline_with_journal", "source factors and an existing login journal require explicit recovery"),
        ("key_source_inline", "this inline source format has no host canonicalization path"),
        ("runtime_unlock_rejected", "Runtime V2 unlock rejected"),
        ("runtime_unlock_transport", "Runtime V2 unlock transport failed"),
        ("runtime_startup_timeout", "host runtime did not become ready"),
        ("runtime_startup_exit", "host runtime exited before readiness"),
        ("runtime_startup_identity", "runtime owner changed during startup"),
        ("config_target_start", "config stage=target_start"),
        ("config_target_owner", "config stage=target_owner_verification"),
        ("config_private_validation", "Runtime did not confirm private config validation"),
        ("config_profile_binding", "candidate selected a different target profile"),
        ("config_source_binding", "config source profile changed since review"),
        ("config_store_not_ready", "configured target Runtime did not confirm store readiness"),
        ("runtime_rpc_transport", "runtime_rpc_transport_failed"),
        ("runtime_account_rejected", "Runtime rejected account control (account_outcome_unknown)"),
        ("control_spawn", "cannot start owned host control Runtime"),
        ("control_response_channel", "host control response channel failure"),
        ("control_deadline", "host control deadline exceeded"),
        ("control_exit_wait", "cannot wait for host control Runtime exit"),
        ("control_exit_mismatch", "host control Runtime exit does not match its response"),
        ("control_response_identity", "account control response identity mismatch"),
        ("control_profile_identity", "account response launch profile mismatch"),
        ("control_parent_or_channel", "parent authorization ended or control channel was interrupted"),
        ("control_parent_or_channel", "auth_diagnostic=auth_parent_revoked_or_channel_interrupted"),
        ("control_authorization", "account launch or parent authorization failed"),
        ("control_binding", "account control binding changed or does not match"),
        ("control_ownership", "account library ownership is unavailable"),
        ("auth_recovery_pending", "authentication recovery requires the original operation"),
        ("host_maintenance_pending", "runtime maintenance is incomplete; use the host runtime restart or the indicated snapshot/login recovery before accessing the library"),
        ("discovery_symlink", "discovery path contains a symlink: "),
        ("discovery_reparse", "discovery path contains a reparse point: "),
        ("discovery_parent_traversal", "discovery path contains parent traversal"),
        ("discovery_ancestor", "discovery ancestor is not a directory"),
        ("discovery_profile_root", "profile root is not a directory"),
        ("migration_control_unknown", "migration_outcome_unknown operation_id="),
        ("login_vault_unlock", "login_kind=vault_unlock_rejected"),
        ("login_account_material", "login_kind=account_material_rejected"),
        ("owned_cleanup", "cleanup_failed owned_pid="),
        ("key_owner_unhealthy", "key control has a recorded owner without verifiable health"),
        ("key_owner_identity", "key owner differs from the installed Runtime, account, profile or registration"),
        ("key_owner_identity", "key owner changed during verification"),
        ("key_selection", "key recovery belongs to another selection"),
        ("key_selection", "key control launch selection changed"),
        ("key_selection", "key operation installation, profile, image or identity changed"),
        ("key_maintenance", "key control requires an actual drained Paused binding"),
        ("key_maintenance", "key control requires owned maintenance"),
        ("key_maintenance", "key control requires this installation's takeover ownership"),
        ("key_maintenance", "key control must own maintenance"),
        ("key_recovery_binding", "invalid key recovery binding"),
        ("key_recovery_binding", "key recovery registration or coordinator changed"),
        ("key_recovery_binding", "key recovery does not own the original maintenance marker"),
        ("key_source_session", "super-reset source session missing"),
        ("key_source_session", "source credential account missing"),
        ("key_source_session", "source credential backend missing"),
        ("key_source_credential", "host session credential read failed"),
        ("key_source_credential", "Runtime session key is unavailable; library remains locked"),
        ("key_credential", "host key credential unavailable"),
        ("key_credential", "host key credential read failed"),
        ("key_credential", "host key credential write failed"),
        ("key_credential", "host key credential readback failed"),
        ("key_credential", "host key credential conflict; original retained"),
        ("key_custody", "host key custody missing; retain the original operation"),
        ("key_custody", "host key candidate credential changed; retain the original operation"),
        ("key_private_exchange", "private key exchange failed; query the original operation"),
        ("key_private_receipt", "invalid private key receipt"),
        ("key_private_receipt", "key receipt changed its original intent or source"),
        ('key_maintenance', 'account and key control must own maintenance independently'),
        ('key_maintenance', 'host maintenance already has an active controller'),
        ('key_maintenance', "maintenance requires this installation's takeover ownership"),
        ('key_maintenance', 'interrupted host maintenance; query the original operation before starting another write'),
        ('key_maintenance', 'maintenance belongs to another coordinator or operation family'),
        ('key_maintenance', 'maintenance still belongs to a live controller'),
        ('key_maintenance', 'maintenance installation or selected profile changed'),
        ('key_maintenance', 'maintenance owner, phase, process birth or exact marker digest changed'),
        ('key_runtime_image', 'Runtime image is not a regular file'),
        ('key_runtime_image', 'Runtime image differs from the selected manifest'),
        ('key_runtime_image', 'Runtime image path changed'),
        ('key_runtime_image', 'account startup requires the registered installed Runtime'),
        ('key_runtime_image', 'account startup cannot select an override instead of the installed Runtime'),
        ('key_source_session', 'RSRS_DATA_DIR must be a nonempty UTF-8 path'),
        ('key_recovery_binding', 'key recovery path is a symlink'),
        ('key_recovery_binding', 'key recovery path is a reparse point'),
        ('key_recovery_binding', 'invalid key recovery file'),
        ('key_recovery_binding', 'invalid key recovery metadata'),
        ('key_recovery_binding', 'key recovery context exceeds its bound'),
        ('key_recovery_binding', 'original key recovery metadata is missing; no new operation was sent'),
        ('key_recovery_binding', 'key operation was not sent; explicit queries cannot admit it'),
        ('key_recovery_binding', 'key source identity changed before admission'),
        ('key_recovery_binding', 'key recovery metadata changed before cleanup'),
        ('key_private_receipt', 'private key receipt missing'),
        ('key_private_receipt', 'invalid key response envelope'),
        ('key_private_receipt', 'invalid private key receipt shape'),
        ('key_private_receipt', 'key receipt does not match the selected session'),
        ('key_private_receipt', 'invalid pending key response'),
        ('key_private_receipt', 'invalid terminal key response'),
        ('key_private_receipt', 'super-reset requires an original bound account'),
        ('key_private_receipt', 'super-reset changed its profile'),
    )
    diagnostic = {"reason": reason if reason in reasons else None,
            "category": next((name for name, marker in categories if marker in text), "unclassified"),
            "stdout_json": isinstance(value, dict), "stderr_present": bool(stderr)}
    # The host migration error deliberately hides its underlying error chain.
    # Preserve only its existing fixed stage, never adjacent operation parameters.
    stages = ("paused_grant", "owner_check", "drain_owner", "drained_check",
        "selection_before_send", "save_sent", "exchange", "encode_unknown", "selection_after_receipt",
        "restore_owner_start", "restore_owner_probe", "restore_owner_verify", "restore_store_ready",
        "selection_before_finish", "finish_maintenance", "save_terminal", "paused_publication")
    stage = re.search(r"\bmigration_stage=([a-z_]+)(?=$|[;\s])", text)
    diagnostic["migration_stage"] = stage[1] if stage and stage[1] in stages else None
    return diagnostic


def process_group_members(group):
    members = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            raw = (entry / "stat").read_text()
            fields = raw[raw.rfind(")") + 2:].split()
            if int(fields[2]) != group:
                continue
            argv = (entry / "cmdline").read_bytes().split(b"\0")
            members[int(entry.name)] = {"parent": int(fields[1]), "group": int(fields[2]),
                "session": int(fields[3]), "start_ticks": int(fields[19]), "state": fields[0],
                "control_helper": b"--host-control-internal" in argv}
        except (FileNotFoundError, ProcessLookupError):
            continue
    return members


def interrupt_owned_control(child):
    """Bind the fresh session and exact private helper before signalling; wait both."""
    members = process_group_members(child.pid)
    require(child.poll() is None and child.pid in members and os.getpgid(child.pid) == child.pid
            and os.getsid(child.pid) == child.pid, "interruption_session_not_owned")
    require(members[child.pid]["parent"] == os.getpid(), "interruption_child_not_owned")
    helpers = [pid for pid, row in members.items() if row["control_helper"]]
    require(len(helpers) == 1 and members[helpers[0]]["parent"] == child.pid,
            "interruption_control_helper_not_bound")
    require(all(row["session"] == child.pid and (pid == child.pid or row["parent"] == child.pid)
                for pid, row in members.items()), "interruption_group_has_unbound_member")
    handles = []
    completed = False
    child._fixture_pidfds = handles
    try:
        for pid in members:
            handles.append(os.pidfd_open(pid))
        require({pid: {k: v for k, v in row.items() if k != "state"}
                 for pid, row in process_group_members(child.pid).items()}
                == {pid: {k: v for k, v in row.items() if k != "state"}
                    for pid, row in members.items()}, "interruption_group_changed")
        os.killpg(child.pid, signal.SIGINT)
        child.communicate(timeout=15)  # Reap our direct CLI child.
        require(child.returncode == -signal.SIGINT, "owned_child_not_interrupted")
        # Control helpers are grandchildren, not waitpid children. pidfd readiness
        # proves their exit even if the system reaper has not collected the zombie.
        poller = select.poll()
        for handle in handles:
            poller.register(handle, select.POLLIN)
        exited = {handle for handle, events in poller.poll(15000) if events & select.POLLIN}
        # The CLI may report first; individually wait each still-unobserved helper.
        for handle in set(handles) - exited:
            waiter = select.poll()
            waiter.register(handle, select.POLLIN)
            require(any(events & select.POLLIN for _, events in waiter.poll(15000)),
                    "interrupted_control_helper_still_running")
        require(all(row["state"] == "Z" for row in process_group_members(child.pid).values()),
                "interrupted_group_still_running")
        completed = True
        return {"group": child.pid, "members": members, "cli_reaped": True, "helper_exit_confirmed": True}
    finally:
        if completed:
            for handle in handles:
                os.close(handle)
            child._fixture_pidfds = []


def stop_owned_control(child):
    """Failure teardown uses lifetime-bound pidfds, never a recycled numeric group."""
    handles = getattr(child, "_fixture_pidfds", [])
    if not handles:
        members = process_group_members(child.pid)
        if members:
            require(child.poll() is None and child.pid in members
                    and members[child.pid]["parent"] == os.getpid()
                    and all(row["session"] == child.pid and (pid == child.pid or row["parent"] == child.pid)
                            for pid, row in members.items()), "cleanup_process_lifetime_unproven")
            for pid, row in members.items():
                handle = os.pidfd_open(pid)
                handles.append(handle)
                observed = process_group_members(child.pid).get(pid)
                require(observed is not None and observed["start_ticks"] == row["start_ticks"],
                        "cleanup_process_identity_changed")
    try:
        for handle in handles:
            try:
                signal.pidfd_send_signal(handle, signal.SIGKILL)
            except ProcessLookupError:
                pass
        child.communicate(timeout=15)
        for handle in handles:
            waiter = select.poll()
            waiter.register(handle, select.POLLIN)
            require(any(events & select.POLLIN for _, events in waiter.poll(15000)),
                    "cleanup_control_helper_still_running")
        # A partial pidfd snapshot or a late helper spawn must fail closed. Do not
        # signal newly discovered numeric PIDs or release their publication barrier.
        require(all(row["state"] == "Z" for row in process_group_members(child.pid).values()),
                "cleanup_untracked_group_member")
    finally:
        for handle in handles:
            os.close(handle)
        child._fixture_pidfds = []


def original_operation(journal, root, barrier, addr):
    """Read the synthetic journal without rewriting or downgrading its ownership."""
    require(journal.get("schema_version") == 2, "operation_journal_schema_missing")
    active = journal.get("active") or {}
    operation_id = active.get("operation_id")
    require(isinstance(operation_id, str), "original_operation_missing")
    parsed = uuid.UUID(operation_id)
    require(parsed.version == 4 and str(parsed) == operation_id, "original_operation_invalid")
    records = [row for row in journal["operations"] if row["operation_id"] == operation_id]
    require(len(records) == 1, "original_operation_not_unique")
    record = records[0]
    require(record["idempotency_key"] == operation_id, "original_operation_binding_changed_idempotency_key")
    require(record["profile"] == str(root.resolve()), "original_operation_binding_changed_profile")
    require(record["root"] == str(root.resolve()), "original_operation_binding_changed_root")
    require(active["alias"] == record["alias"], "original_operation_binding_changed_alias")
    account = record["account"]
    login = account["login"]
    candidate = json.loads(bytes(record["candidate"]))
    require(account["user"] == "synthetic" and account["addr"] == addr
            and candidate["user"] == "synthetic" and candidate["addr"] == addr
            and candidate["token"] == "synthetic-token" and candidate["keyring_account"] == record["alias"],
            "original_account_identity_changed")
    require(not account["register"] and login["migrate_vault"]
            and record["phase"] == "awaiting_verification", "interrupted_phase_not_proven")
    require(not login["publication_done"] and not login["vault_done"] and not login["password_done"],
            "interruption_already_completed")
    require(barrier in ("before_publish", "after_publish"), "invalid_publication_barrier")
    require(login["publication_pending"] == (barrier == "after_publish")
            and login["publication_step"] == ("vault" if barrier == "after_publish" else None),
            "publication_checkpoint_not_proven")
    return record


def account_recovery_flags(action, record):
    require(action in ("account-result", "account-rollback"), "invalid_recovery_action")
    return ["--json", "runtime", action, "--operation-id", record["operation_id"],
            "--source-profile", record["profile"], *(["--confirm"] if action == "account-rollback" else [])]


def terminal_operation(journal, record, outcome):
    require(journal.get("schema_version") == 2 and journal.get("active") is None,
            "operation_recovery_not_retired")
    rows = [row for row in journal["operations"] if row["operation_id"] == record["operation_id"]]
    require(len(rows) == 1 and rows[0]["phase"] == outcome
            and all(rows[0][key] == record[key] for key in ("idempotency_key", "root", "profile", "alias")),
            "original_terminal_receipt_missing")


def login_terminal_operation(journal, record, summary, session_bytes):
    # Host custody replaces the admission alias with the committed candidate alias.
    session = json.loads(session_bytes)
    alias = session.get("keyring_account")
    sha256 = hashlib.sha256(session_bytes).hexdigest()
    rows = [row for row in journal["operations"] if row["operation_id"] == record["operation_id"]]
    require(isinstance(alias, str) and re.fullmatch(r"login-[a-f0-9]{32}", alias)
            and summary.get("session_sha256") == sha256 and len(rows) == 1
            and rows[0].get("candidate_sha256") == sha256
            and rows[0]["account"].get("target") == summary["target_profile"]
            and all(session.get(key) == record["account"][key] == rows[0]["account"][key]
                    for key in ("user", "addr")), "original_terminal_receipt_missing")
    terminal_operation(journal, dict(record, alias=alias), "committed")


def login_continuation_receipt(summary, operation_id, source, addr, outcome):
    require(summary.get("operation_id") == operation_id
            and summary.get("source_profile") == str(source.resolve())
            and summary.get("outcome") == outcome, "login_continuation_identity_mismatch")
    login = summary.get("login") or {}
    require(login.get("user") == "synthetic" and login.get("addr") == addr
            and login.get("required_inputs") == (["super_password"] if outcome == "awaiting_input" else []),
            "login_continuation_inputs_mismatch")


def runtime_stop_receipt(stopped):
    require(stopped.returncode == 0, "owned_runtime_cleanup_failed")
    receipt = json.loads(stopped.stdout)
    require(receipt.get("status") == "ok" and not receipt.get("errors")
            and receipt.get("summary", {}).get("state") == "stopped", "owned_runtime_cleanup_failed")


def fixture_runtime_owners(directory, images):
    """Match owned images before reading private profile scope from argv/environment."""
    images = {Path(image).resolve() for image in images}
    owners = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            if entry.stat().st_uid != os.geteuid():
                continue
            argv = (entry / "cmdline").read_bytes().split(b"\0")
            # Spawned CLI/Runtime argv[0] is its executable path; inspect only those images.
            if not argv[0] or Path(os.fsdecode(argv[0])).resolve() not in images:
                continue
            if (entry / "exe").resolve(strict=True) not in images:
                continue
            profiles = [argv[index + 1] for index, arg in enumerate(argv[:-1]) if arg == b"--data-dir"]
            profiles.extend(arg.split(b"=", 1)[1] for arg in argv if arg.startswith(b"--data-dir="))
            environment = (entry / "environ").read_bytes().split(b"\0")
            profiles.extend(item.split(b"=", 1)[1] for item in environment
                            if item.startswith(b"RSRS_DATA_DIR="))
            if not any(Path(os.fsdecode(profile)).resolve().is_relative_to(directory.resolve())
                       for profile in profiles):
                continue
            raw = (entry / "stat").read_text()
            if raw[raw.rfind(")") + 2:].split()[0] != "Z":
                owners.append(int(entry.name))
        except (FileNotFoundError, ProcessLookupError):
            continue
    return owners


def key_continuation_receipt(summary, operation_id, source, outcome):
    require(summary.get("operation_id") == operation_id
            and summary.get("source_profile") == str(source.resolve())
            and summary.get("outcome") == outcome, "key_continuation_identity_mismatch")
    if outcome == "awaiting_input":
        require(summary.get("operation", {}).get("kind") == "super_reset"
                and summary.get("required_inputs") == ["super_password"]
                and not summary.get("super_password") and not summary.get("super"),
                "key_continuation_inputs_mismatch")
    else:
        require(summary.get("ok") is True and isinstance(summary.get("super"), str)
                and bool(summary["super"]), "key_continuation_inputs_mismatch")


def remove_fixture_super(keys, session_path, directory, addr, code):
    """Delete only the exact super slot bound to this synthetic V4 login."""
    require(session_path.resolve().is_relative_to(directory.resolve()), "key_continuation_credential_not_owned")
    session = json.loads(session_path.read_bytes())
    alias = session.get("keyring_account")
    require(session.get("user") == "synthetic" and session.get("addr") == addr
            and session.get("vault_version") == 4 and session.get("keyring_backend") == "secret_service"
            and isinstance(alias, str) and re.fullmatch(r"login-[a-f0-9]{32}", alias)
            and all(field not in session for field in ("super", "pass", "secret", "secret_key")),
            "key_continuation_credential_not_owned")
    keys.verify_owner()
    items = [keys._find("rsrs", prefix + alias) for prefix in ("super:", "pass:")]
    for item, prefix in zip(items, ("super:", "pass:")):
        require(item is not None and all(item.get_attributes().get(key) == value
                for key, value in keys.attributes("rsrs", prefix + alias).items()),
                "key_continuation_credential_not_owned")
    require(items[0].get_secret().decode("utf-8") == code, "key_continuation_credential_not_owned")
    password = items[1].get_secret()
    items[0].delete()
    require(keys._find("rsrs", "super:" + alias) is None
            and keys._find("rsrs", "pass:" + alias).get_secret() == password,
            "key_continuation_credential_not_owned")
