"""Redact native account candidate failures while retaining diagnostic codes."""
import json
from pathlib import Path
import re
from native_account_cases import CASE
from native_account_cases import NATIVE_STOP_CASE


def safe_failure_diagnostics(captured, failed_cases):
    # Rebuild fixed fields only. Never publish arbitrary errors, paths, journal
    # values or output text, including when a diagnostic event is malformed.
    enums = {
        "reason_code": {None, "account_outcome_unknown", "runtime_error", "redacted"},
        "error_type": {None, "outcome_unknown", "runtime", "redacted"},
        "runtime_log_reason_code": {"not_captured_by_fixture"},
        "journal_reason_code": {"read", "invalid_json", "not_found", "permission_denied", "io_error"},
        "phase": {None, "admitted", "login_input", "remote_pending", "remote_unknown", "authorized",
                  "prepared", "awaiting_verification", "rolling_back", "committed", "rolled_back", "redacted"},
        "backend": {None, "native", "secret_service", "kernel", "redacted"},
    }
    booleans = {"journal_schema_v2", "active_present", "active_matches_operation",
                "root_matches_fixture", "source_matches_fixture", "candidate_present",
                "candidate_sha256_present", "authorization_present", "vault_present", "target_present"}
    nullable_booleans = {"published_vault", "vault_pending", "import_started", "sync_started",
                         "publication_pending", "publication_done", "vault_done", "password_done", "owns_config"}
    arrays = {"backends", "pass_backends", "totp_backends"}
    base = {"schema_version", "event", "reason_code", "error_type", "operation_id", "runtime_log_reason_code"}
    allowed = base | set(enums) | booleans | nullable_booleans | arrays | {"journal_os_error_code", "matching_records"}
    events, invalid, seen = [], False, 0
    for line in captured.splitlines():
        if b'"authorization_failure_evidence"' not in line:
            continue
        seen += 1
        if seen > 16:
            invalid = True
            break
        try:
            event = json.loads(line[line.index(b"{"):])
            if (not isinstance(event, dict) or not base.issubset(event) or set(event) - allowed
                    or type(event["schema_version"]) is not int or event["schema_version"] != 1
                    or event["event"] != "authorization_failure_evidence"):
                raise ValueError
            clean = {"schema_version": 1, "event": "authorization_failure_evidence"}
            for key, choices in enums.items():
                if key in event:
                    value = event[key]
                    if value is not None and not isinstance(value, str):
                        raise ValueError
                    if value not in choices:
                        raise ValueError
                    clean[key] = value
            for key in booleans | nullable_booleans:
                if key in event:
                    value = event[key]
                    if type(value) is not bool and not (key in nullable_booleans and value is None):
                        raise ValueError
                    clean[key] = value
            for key in arrays:
                if key in event:
                    value = event[key]
                    if value is not None and (not isinstance(value, list) or len(value) > 16
                            or any(item is not None and (not isinstance(item, str)
                                or item not in enums["backend"]) for item in value)):
                        raise ValueError
                    clean[key] = value
            for key in ("journal_os_error_code", "matching_records"):
                if key in event:
                    value = event[key]
                    if not (key == "journal_os_error_code" and value is None):
                        if type(value) is not int or not (-2147483648 <= value <= 2147483647):
                            raise ValueError
                        if key == "matching_records" and not 0 <= value <= 1024:
                            raise ValueError
                    clean[key] = value
            operation = event["operation_id"]
            if operation is not None and (not isinstance(operation, str) or not re.fullmatch(
                    r"[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}", operation)):
                raise ValueError
            clean["operation_id_present"] = operation is not None
            events.append(clean)
        except (ValueError, TypeError):
            invalid = True

    known = {
        b'completed account selection changed before supervisor handoff': 'account_handoff_selection_changed',
        b'compatibility handoff cannot inherit maintenance authorization': 'account_handoff_inherited_maintenance',
        b'compatibility handoff requires an unregistered host and completed durable maintenance': 'account_handoff_maintenance_incomplete',
        b'Client2 requires the same verified CLI bytes at the stable bin path': 'account_handoff_stable_cli_mismatch',
        b'compatibility startup cannot inherit account maintenance': 'account_handoff_start_inherited_maintenance',
        b'compatibility host exited before readiness': 'account_handoff_host_exited',
        b'compatibility startup is uncertain for child': 'account_handoff_start_unknown',
        b'compatibility listener is not the launched child': 'account_handoff_listener_pid_mismatch',
        b'compatibility host identity does not match this installation': 'account_handoff_host_identity_mismatch',
        b'compatibility host backend binding changed': 'account_handoff_backend_binding_changed',
        b'compatibility Runtime failed independent committed-owner handoff verification': 'account_handoff_runtime_verification_failed',
        b'account rollback did not restore the original source identity': 'account_rollback_source_identity_changed',
        b'rollback serving state differs from the actually drained owner': 'account_rollback_serving_state_changed',
        b'registered lifecycle command failed with exit code': 'owned_registered_lifecycle_failed',
        b'systemd user session unavailable': 'owned_registered_systemd_unavailable',
        b'systemd unit load state is unavailable or unexpected': 'owned_registered_systemd_load_state_invalid',
        b'systemd unit path missing': 'owned_registered_systemd_unit_path_missing',
        b'unknown systemd user unit path': 'owned_registered_systemd_unit_path_mismatch',
        b'systemd loaded working directory differs from this installation': 'owned_registered_systemd_working_directory_mismatch',
        b'systemd unit object unavailable': 'owned_registered_systemd_unit_object_missing',
        b'unexpected systemd unit object': 'owned_registered_systemd_unit_object_invalid',
        b'systemctl current-user operation failed': 'owned_registered_systemctl_failed',
        b'busctl current-user operation failed': 'owned_registered_busctl_failed',
        b'registered service could not unlock its actual persistent credential': 'owned_registered_unlock_failed',
        b'original Go Manager.Start failed': 'owned_client2_start_failed',
        b'original Go startup/RPC failed': 'owned_client2_start_rpc_failed',
        b'registered business did not reach the actual provider': 'owned_registered_provider_not_reached',
        b'actual Client2-owned SDK callback missing': 'owned_client2_provider_not_reached',
        b'library lock check failed without proving contention': 'owned_library_lock_unproved',
        b'owned Runtime released its library lock before business cleanup': 'owned_library_lock_released_early',
        b'REGISTERED_STOP_CLEANUP_FAILED': 'owned_registered_cleanup_preserved',
        b'CLIENT2_OWNED_STOP_CLEANUP_FAILED': 'owned_client2_cleanup_preserved',
        b"authorized OAuth login failed": "account_authorized_login_failed",
        b"OAuth login did not report the verified account": "account_login_result_mismatch",
        b"login changed unrelated API preferences or escaped the fixture": "account_login_preferences_changed",
        b"headless login persisted an unexpected credential": "account_login_session_mismatch",
        b"login did not retain exactly one completed new operation": "account_login_receipt_count_mismatch",
        b"login session does not match its committed credential receipt": "account_login_receipt_mismatch",
        b"could not stop the isolated OAuth runtime": "account_login_stop_failed",
        b"cloud fixture row missing": "account_cloud_fixture_missing",
        b"trailing characters at line": "account_json_trailing_data",
        b"EOF while parsing": "account_json_incomplete",
        b"expected value at line": "account_json_invalid_value",
        b"owned space command": "account_owned_space_command_failed",
        b"join never fetched": "account_join_request_not_observed",
        b"join did not authenticate its ordered GET": "account_join_authorization_mismatch",
        b"unverifiable witness prevented healthy rescue": "snapshot_unverifiable_rescue_failed",
        b"restore of witnessed library failed": "snapshot_witness_restore_failed",
        b"healthy witness snapshot rescue failed": "snapshot_witness_rescue_failed",
        b"expected witness checkpoint rejection": "snapshot_checkpoint_rejection_mismatch",
        b"expected diagnostic copy rejection": "snapshot_diagnostic_rejection_mismatch",
        b"account committed; Runtime host handoff is pending": "account_handoff_pending",
        b"operation committed and maintenance finished; Client2 compatibility handoff pending": "compatibility_handoff_pending",
        b"model owner differs from the installed Runtime, account, profile or registration": "compatibility_owner_binding_mismatch",
        b"compatibility handoff cannot inherit maintenance authorization": "compatibility_maintenance_token_present",
        b"compatibility handoff requires an unregistered host and completed durable maintenance": "compatibility_maintenance_not_finished",
        b"Client2 requires the same verified CLI bytes at the stable bin path": "compatibility_stable_cli_mismatch",
        b"compatibility handoff owner did not fully drain": "compatibility_owner_drain_incomplete",
        b"compatibility host readiness identity mismatch": "compatibility_host_identity_mismatch",
        b"compatibility host is not the verified OS listener": "compatibility_listener_mismatch",
        b"claimed compatibility host does not own the OS listener": "compatibility_listener_mismatch",
        b"compatibility loopback listener ownership is unavailable or ambiguous": "compatibility_listener_unverified",
        b"compatibility loopback listener is unavailable or ambiguous": "compatibility_listener_unverified",
        b"compatibility listener is not the stable CLI image": "compatibility_host_image_mismatch",
        b"compatibility host identity does not match this installation": "compatibility_host_identity_mismatch",
        b"compatibility host backend binding changed": "compatibility_backend_binding_mismatch",
        b"compatibility backend OS process changed": "compatibility_backend_process_mismatch",
        b"compatibility startup is uncertain for child": "compatibility_startup_uncertain",
        b"compatibility Runtime failed independent committed-owner handoff verification": "compatibility_restored_owner_mismatch",
        b"host control request frame exceeds the protocol limit": "control_request_frame_limit",
        b"cannot start owned host control Runtime": "control_helper_spawn_failed",
        b"missing host control input pipe": "control_input_pipe_missing",
        b"missing host control output pipe": "control_output_pipe_missing",
        b"host control input owner was released": "control_input_owner_released",
        b"host control request write failed": "control_request_write_failed",
        b"missing host control response frame": "control_response_frame_missing",
        b"invalid host control response frame size": "control_response_frame_size_invalid",
        b"incomplete host control response frame": "control_response_frame_incomplete",
        b"invalid host control response JSON": "control_response_json_invalid",
        b"extra host control response bytes": "control_response_extra_bytes",
        b"host control response channel failure": "control_response_channel_failed",
        b"host control deadline exceeded": "control_launch_deadline_exceeded",
        b"cannot wait for host control Runtime exit": "control_exit_wait_failed",
        # Historical CLI output remains relevant to the legacy-stable candidate lane.
        b"host control response deadline or channel failure": "control_response_channel_failed",
        b"host control response deadline exceeded": "control_response_deadline_exceeded",
        b"host control exit deadline exceeded": "control_exit_deadline_exceeded",
        b"host control Runtime exit does not match its response": "control_exit_response_mismatch",
        b"host control response identity mismatch": "control_response_identity_mismatch",
        b"host control data directory mismatch": "control_response_profile_mismatch",
        b"missing host control result envelope": "control_result_envelope_missing",
        b"host control authorization was not confirmed": "control_authorization_unconfirmed",
        b"host control I/O worker did not finish": "control_io_cleanup_incomplete",
        b"host control I/O worker panicked": "control_io_worker_panicked",
        b"auth maintenance profile differs from the selected profile": "auth_maintenance_profile_mismatch",
        b"auth maintenance installation changed": "auth_maintenance_installation_changed",
        b"auth maintenance marker is missing": "auth_maintenance_marker_missing",
        b"auth maintenance ownership is missing": "auth_maintenance_owner_missing",
        b"auth maintenance digest is missing": "auth_maintenance_digest_missing",
        b"auth maintenance owner or phase changed": "auth_maintenance_owner_phase_changed",
        b"auth control requires this installation's takeover ownership": "auth_control_takeover_mismatch",
        b"auth control image or identity changed while preparing authorization": "auth_control_grant_identity_changed",
        b"account control response identity mismatch": "account_control_response_identity_mismatch",
        b"account launch or parent authorization failed": "account_control_parent_unauthorized",
        b"account control binding changed or does not match": "account_control_binding_mismatch",
        b"account controller is unavailable": "account_control_unavailable",
        b"parent authorization ended or control channel was interrupted": "account_control_parent_revoked_or_channel_interrupted",
        b"auth_diagnostic=auth_parent_revoked_or_channel_interrupted": "auth_parent_revoked_or_channel_interrupted",
        b"account deadline or parent authorization ended": "account_control_deadline_or_parent_ended",
        b"snapshot control requires matching takeover, image and maintenance grant": "snapshot_control_grant_binding_mismatch",
        b"snapshot maintenance grant missing": "snapshot_control_grant_missing",
        b"snapshot response identity mismatch": "snapshot_control_response_identity_mismatch",
        b"snapshot response profile missing": "snapshot_control_response_profile_missing",
        b"snapshot response profile mismatch": "snapshot_control_response_profile_mismatch",
        b"snapshot Runtime did not confirm a receipt": "snapshot_remote_receipt_unconfirmed",
        b"snapshot control readiness mismatch": "snapshot_control_readiness_mismatch",
        b"snapshot result missing": "snapshot_control_result_missing",
        b"invalid snapshot receipt shape": "snapshot_control_receipt_invalid",
        b"snapshot receipt does not identify the original request": "snapshot_control_receipt_request_mismatch",
        b"snapshot witness checkpoint is unverifiable; diagnostic copy cannot be a restore target": "snapshot_witness_diagnostic_copy_rejected",
        b"snapshot witness checkpoint mismatch or missing checkpoint": "snapshot_witness_checkpoint_mismatch",
        b"snapshot witness checkpoint exists without a witness schema": "snapshot_witness_schema_missing",
        b"snapshot requires this controller's exact Paused marker": "snapshot_paused_marker_mismatch",
        b"snapshot finalization requires this controller's exact Verified marker": "snapshot_verified_marker_mismatch",
        b"snapshot maintenance marker missing": "snapshot_marker_missing",
        b"snapshot maintenance ownership missing": "snapshot_marker_ownership_missing",
        b"snapshot marker changed during drain": "snapshot_marker_changed_during_drain",
        b"snapshot operation or drained owner changed": "snapshot_drained_owner_changed",
        b"snapshot has no authenticated drain capability": "snapshot_drain_capability_missing",
        b"snapshot drain account differs from original operation": "snapshot_drain_account_mismatch",
        b"snapshot drained owner is still alive": "snapshot_drained_owner_alive",
        b"snapshot owner is alive but health is unavailable; drain proof required": "snapshot_live_owner_without_health",
        b"snapshot supervisor is still running; explicit drain is required": "snapshot_supervisor_still_running",
        b"snapshot endpoint is not a bounded regular file": "snapshot_endpoint_invalid_file",
        b"snapshot endpoint too large": "snapshot_endpoint_too_large",
        b"snapshot endpoint owner PID missing": "snapshot_endpoint_pid_missing",
        b"snapshot endpoint changed since offline fencing": "snapshot_endpoint_changed",
        b"drained runtime supervisor is still active or its endpoint changed": "runtime_supervisor_or_endpoint_changed",
        b"runtime endpoint owner changed after drain": "runtime_endpoint_changed_after_drain",
        b"snapshot restore exchange failed": "snapshot_restore_exchange_failed",
        b"snapshot restore is not Installed": "snapshot_restore_not_installed",
        b"snapshot startup/verification failed": "snapshot_startup_verification_failed",
        b"snapshot startup failed; recovery remains pending": "snapshot_startup_recovery_pending",
        b"snapshot commit outcome unknown": "snapshot_commit_outcome_unknown",
        b"snapshot commit not confirmed": "snapshot_commit_unconfirmed",
        b"compatibility request failed; outcome may be unknown; do not replay": "compatibility_proxy_outcome_unknown",
        b"runtime restart blocked: owner roots do not match this installation": "runtime_stop_root_mismatch",
        b"runtime process already exited": "runtime_stop_owner_already_exited",
        b"runtime drain stop request failed": "runtime_drain_stop_request_failed",
        b"runtime drain final receipt verification failed": "runtime_drain_final_receipt_failed",
        b"runtime drain listener did not close": "runtime_drain_listener_close_failed",
        b"runtime drain endpoint removal failed": "runtime_drain_endpoint_remove_failed",
        b"runtime stop host policy rejected": "runtime_stop_host_policy_rejected",
        b"runtime stop HTTP exchange failed": "runtime_stop_http_exchange_failed",
        b"runtime_unavailable:": "runtime_error_unavailable",
        b"runtime_token_unreadable:": "runtime_error_token_unreadable",
        b"runtime_unauthorized:": "runtime_error_unauthorized",
        b"runtime_transport:": "runtime_error_transport",
        b"runtime stop identity is incomplete": "runtime_stop_identity_incomplete",
        b"invalid stop acknowledgement": "runtime_stop_ack_invalid_json",
        b"runtime did not accept a traceable stop operation": "runtime_stop_ack_untraceable",
        b"stop acknowledgement lacks operation ID": "runtime_stop_ack_operation_missing",
        b"final runtime stop receipt is missing": "runtime_stop_final_receipt_missing",
        b"invalid runtime stop instance ID": "runtime_stop_instance_invalid",
        b"stopped runtime belongs to another installation": "runtime_stop_receipt_installation_mismatch",
        b"invalid stop receipt file": "runtime_stop_receipt_file_invalid",
        b"stop receipt exceeds 1 MiB": "runtime_stop_receipt_file_too_large",
        b"invalid final stop receipt": "runtime_stop_receipt_json_invalid",
        b"runtime stopped without a proven complete drain": "runtime_stop_drain_unproven",
        b"runtime did not exit after stop": "runtime_stop_listener_still_open",
        b"manual snapshot owner differs from the installed Runtime": "snapshot_manual_owner_identity_mismatch",
        b"manual snapshot owner exited before drain": "snapshot_manual_owner_already_exited",
        b"snapshot marker already bound; query that operation instead of draining again": "snapshot_marker_already_bound",
        b"runtime process start identity does not match health": "runtime_stop_identity_mismatch",
        b"runtime drain is still pending; no process was forced to exit": "runtime_drain_pending",
        b"stopped runtime has not released its library lock": "runtime_library_lock_retained",
        b"snapshot transaction completed; host maintenance cleanup pending": "snapshot_host_cleanup_pending",
        b"snapshot transaction completed; cleanup pending": "snapshot_cleanup_pending",
    }
    # libtest prints private failure output under exact, known case headers.
    # Ignore success output and unrelated text, which may contain expected errors.
    blocks, current, in_failures = {}, None, False
    for line in captured.splitlines():
        if line == b"failures:":
            in_failures, current = True, None
            continue
        if line == b"successes:" or line.startswith(b"test result:"):
            in_failures, current = False, None
        if in_failures and line.startswith(b"---- ") and line.endswith(b" stdout ----"):
            current = next((case for case in failed_cases
                            if line == b"---- " + case.encode() + b" stdout ----"), None)
            if current is not None:
                blocks[current] = []
        elif in_failures and current is not None:
            blocks[current].append(line)
    snapshot_stages = []
    stage_invalid = False
    allowed_stages = {"candidate_guard", "remote_create", "remote_list", "old_install", "old_start",
                      "old_create", "old_update", "old_restart", "old_restore", "old_recovery", "old_cleanup"}
    for case in failed_cases:
        for line in blocks.get(case, []):
            if not line.startswith(b"legacy-snapshot-stage "):
                continue
            try:
                if len(line) > 2048 or len(snapshot_stages) >= 64:
                    raise ValueError
                event = json.loads(line[len(b"legacy-snapshot-stage "):])
                if (not isinstance(event, dict) or set(event) != {"case", "stage"}
                        or event["case"] != case or event["stage"] not in allowed_stages):
                    raise ValueError
                snapshot_stages.append(event)
            except (ValueError, TypeError):
                stage_invalid = True
    response_reasons = {None, "auth_outcome_unknown", "identity_write_recovery_required",
                        "key_outcome_unknown", "account_handoff_pending", "account_outcome_unknown",
                        "request_outcome_unknown", "invalid_input", "runtime_error"}
    response_error_types = {None, "outcome_unknown", "identity_write_recovery_required",
                            "handoff_pending", "user", "runtime"}
    response_events, response_invalid, response_seen = [], False, 0
    response_marker = b"account-response-evidence "
    for case in failed_cases:
        for line in blocks.get(case, []):
            if response_marker not in line:
                continue
            response_seen += 1
            if response_seen > 16:
                response_invalid = True
                break
            try:
                if not line.startswith(response_marker) or len(line) > 16384:
                    raise ValueError
                response = json.loads(line[len(response_marker):])
                if (not isinstance(response, dict)
                        or set(response) != {"stage", "reason_code", "error_type"}
                        or not isinstance(response["stage"], str)
                        or response["stage"] not in {"required_input", "wrong_super", "space_command"}):
                    raise ValueError
                clean = {"case": case, "stage": response["stage"]}
                for key, choices in (("reason_code", response_reasons), ("error_type", response_error_types)):
                    value = response[key]
                    if value is not None and not isinstance(value, str):
                        raise ValueError
                    clean[key] = value if value in choices else "redacted"
                response_events.append(clean)
            except (ValueError, TypeError):
                response_invalid = True
    account_stages = {"oauth_completed", "receipt_verified", "oauth_runtime_stopped",
                      "cloud_memory_seeded", "cloud_store_opened", "cloud_fixture_staged"}
    account_checkpoints = [line[len(b"account-checkpoint "):].decode("ascii")
        for line in blocks.get(CASE, []) if line.startswith(b"account-checkpoint ")
        and line[len(b"account-checkpoint "):] in {stage.encode() for stage in account_stages}][:32]
    codes, locations = {}, {}
    for case in failed_cases:
        lines = list(blocks.get(case, []))
        # libtest prints returned Result errors outside the captured stdout block.
        if failed_cases == [CASE] and case == CASE:
            lines.extend(line for line in captured.splitlines()
                         if line.startswith(b"Error: ") and len(line) <= 65536)
        block = b"\n".join(lines)
        codes[case] = list(dict.fromkeys(code for text, code in known.items() if text in block))
        # HTTP details can contain URLs and response data. Retain only a fixed
        # status when the same line identifies this failed stop exchange.
        for line in blocks.get(case, []):
            if (b"runtime stop HTTP exchange failed" not in line
                    or b"runtime_transport:" not in line or len(line) > 65536):
                continue
            for status in (b"400", b"401", b"403", b"404", b"409", b"429", b"500", b"502", b"503", b"504"):
                if re.search(rb"(?:status code |unexpected HTTP )" + status + rb"(?![0-9])", line):
                    code = "runtime_stop_http_" + status.decode("ascii")
                    if code not in codes[case]:
                        codes[case].append(code)
        if not codes[case]:
            codes[case] = ["unknown_private_failure"]
        locations[case] = []
        for line in blocks.get(case, []):
            if b"panicked at " not in line or len(line) > 8192:
                continue
            for match in re.finditer(rb"[/\\](account\.rs|snapshot_lifecycle\.rs|diary_identity\.rs|capture_main\.rs|memory_revision\.rs):([0-9]{1,6}):([0-9]{1,4}):", line):
                number, column = int(match[2]), int(match[3])
                if not (1 <= number <= 20000 and 1 <= column <= 4096):
                    continue
                location = {"file": match[1].decode("ascii"), "line": number, "column": column}
                if location not in locations[case] and len(locations[case]) < 8:
                    locations[case].append(location)
    native_stages = {b"start", b"fixture_ready", b"memory_seeded", b"runtime_ready",
                     b"provider_received", b"active_verified", b"stop_accepted",
                     b"force_identities_rejected", b"worker_reaped", b"normal_waited",
                     b"same_key_checked", b"business_completed", b"runtime_stopped",
                     b"store_unchanged", b"restarted", b"stale_rejected", b"restart_stopped", b"complete"}
    checkpoints, checkpoint_invalid = [], False
    for line in blocks.get(NATIVE_STOP_CASE, []):
        if b"NATIVE_STOP_CHECKPOINT" not in line:
            continue
        match = re.fullmatch(rb"NATIVE_STOP_CHECKPOINT mode=(normal|forced) stage=([a-z_]{1,32})", line)
        if match is None or match[2] not in native_stages:
            checkpoint_invalid = True
            continue
        checkpoint = {"mode": match[1].decode("ascii"), "stage": match[2].decode("ascii")}
        if checkpoint not in checkpoints:
            if len(checkpoints) >= 36:
                checkpoint_invalid = True
                continue
            checkpoints.append(checkpoint)
    owned_checkpoints, owned_invalid, owned_seen = [], False, 0
    owned_stages = {"actors_bound", "actors_exited", "bundle_staged", "business_completed", "caller_wait_complete", "cleanup_complete", "cleanup_requested", "complete", "core_bound", "core_reaped", "credential_ready", "credential_requested", "drain_observed", "fixture_created", "fixture_ready", "force_requested", "health_bound", "lock_held_draining", "lock_held_initial", "memory_seeded", "provider_finished", "provider_received", "provider_release_requested", "provider_requested", "quiet_period_verified", "receipt_verified", "recovery_bound", "recovery_requested", "recovery_started", "recovery_stop_requested", "recovery_stopped", "runtime_install_requested", "runtime_installed", "start", "start_complete", "start_requested", "stop_requested", "stop_started", "write_verified"}
    marker = b"OWNED_STOP_CHECKPOINT "
    for line in blocks.get(NATIVE_STOP_CASE, []):
        if b"OWNED_STOP_CHECKPOINT" not in line:
            continue
        owned_seen += 1
        if owned_seen > 128:
            owned_invalid = True
            break
        try:
            if not line.startswith(marker) or len(line) > 2048:
                raise ValueError
            event = json.loads(line[len(marker):])
            if (not isinstance(event, dict) or set(event) != {"startup_mode", "case", "stage"}
                    or not all(isinstance(value, str) for value in event.values())
                    or event["startup_mode"] not in {"registered", "client2-owned"}
                    or event["case"] not in {"normal", "caller-timeout", "explicit-force"}
                    or event["stage"] not in owned_stages):
                raise ValueError
            owned_checkpoints.append(event)
        except (ValueError, TypeError):
            owned_invalid = True
    # Report only checked-out source locations matching static error text, never captured text.
    host_error_sources = {}
    if CASE in failed_cases:
        source_root = Path(__file__).resolve().parents[1]
        block = b"\n".join(blocks.get(CASE, []))
        matches = []
        for directory in ("cli/src", "crates/host-control/src"):
            for path in sorted((source_root / directory).rglob("*.rs")):
                if path.is_symlink():
                    continue
                text = path.read_text(encoding="utf-8")
                for literal in re.finditer(r'"(?:[^"\\]|\\.)*"', text):
                    try:
                        message = json.loads(literal.group()).split("{", 1)[0].rstrip()
                    except ValueError:
                        continue
                    if len(message) >= 32 and message.encode() in block:
                        location = {"file": path.relative_to(source_root).as_posix(),
                                    "line": text.count("\n", 0, literal.start()) + 1}
                        matches.append((len(message), location))
        matches.sort(key=lambda match: -match[0])
        host_error_sources[CASE] = [location for _, location in matches[:8]]
    provider_failures, provider_failure_invalid = [], False
    marker = b"HOST_PROVIDER_CREDENTIAL_FAILURE "
    for case in failed_cases:
        for line in blocks.get(case, []):
            if not line.startswith(marker):
                continue
            try:
                failure = json.loads(line[len(marker):])
                if (not isinstance(failure, dict)
                        or set(failure) != {"schema_version", "stage", "error_type", "os_status", "io_error_code"}
                        or type(failure["schema_version"]) is not int or failure["schema_version"] != 1
                        or failure["stage"] not in {"keyring_read", "agent_read", "embedding_profiles_read"}
                        or failure["error_type"] not in {"security_framework", "platform_failure", "io"}):
                    raise ValueError
                for key in ("os_status", "io_error_code"):
                    value = failure[key]
                    if value is not None and (type(value) is not int or not -(2**31) <= value < 2**31):
                        raise ValueError
                if failure["stage"] == "keyring_read":
                    if (failure["error_type"] == "io" or failure["io_error_code"] is not None
                            or (failure["error_type"] == "security_framework") != (failure["os_status"] is not None)):
                        raise ValueError
                elif failure["error_type"] != "io" or failure["os_status"] is not None:
                    raise ValueError
                provider_failures.append(failure)
            except (ValueError, TypeError):
                provider_failure_invalid = True
    return {"authorization_events": events, "authorization_event_invalid": invalid,
            "provider_failure_events": provider_failures, "provider_failure_invalid": provider_failure_invalid,
            "failure_codes": codes, "failure_locations": locations, "host_error_sources": host_error_sources,
            "account_checkpoints": account_checkpoints,
            "legacy_snapshot_stages": snapshot_stages, "legacy_snapshot_stage_invalid": stage_invalid,
            "response_events": response_events, "response_event_invalid": response_invalid,
            "native_stop_checkpoints": checkpoints, "native_stop_checkpoint_invalid": checkpoint_invalid,
            "owned_stop_checkpoints": owned_checkpoints, "owned_stop_checkpoint_invalid": owned_invalid}
