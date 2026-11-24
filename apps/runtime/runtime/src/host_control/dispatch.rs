//! Private request frames and operation dispatch.
use super::*;

mod terminal;

pub(super) fn receive_and_execute(
    response: &mut RpcResponse<ResultEnvelope>,
    control_ready: &mut bool,
    progress: &Mutex<RpcResponse<ResultEnvelope>>,
    events: &mpsc::SyncSender<ControlEvent>,
) -> std::result::Result<(), ControlFailure> {
    let grant: HostControlLaunchGrant = read_frame()
        .map_err(|_| ("invalid_request", "invalid host control grant frame"))?;
    if grant.protocol_version != PROTOCOL_V {
        return Err(("protocol_version_mismatch", "protocol_version_mismatch"));
    }
    let request: RpcRequest = read_frame()
        .map_err(|_| ("invalid_request", "invalid host control RPC frame"))?;
    response.id = request.id.clone();
    use respire_protocol::host_control::{HostLoginRecoveryResult, HostLoginRecoveryState,
        HOST_LOGIN_RECOVERY_METHOD, HOST_LOGIN_RECOVERY_STATUS_METHOD};
    if [HOST_LOGIN_RECOVERY_METHOD, HOST_LOGIN_RECOVERY_STATUS_METHOD].contains(&request.method.as_str()) {
        let params = crate::auth_control::recovery_request(&request)
            .map_err(|_| ("invalid_request", "invalid fixed login recovery request"))?;
        response.envelope = Some(ResultEnvelope::new("login-recovery", Status::Fail,
            serde_json::json!({}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "login recovery parent watch failed"))?;
        let recovered = if request.method == HOST_LOGIN_RECOVERY_STATUS_METHOD {
            crate::startup_identity::verify_login_recovery_status(&grant, &params)
                .map_err(|_| ("unauthorized", "login recovery status authorization failed"))?;
            false
        } else {
            crate::startup_identity::authorize_auth_control(grant.clone())
                .map_err(|_| ("unauthorized", "login recovery maintenance authorization failed"))?;
            let ownership = crate::startup_identity::acquire_auth_ownership(&params.binding, &params.identity)
                .map_err(|_| ("auth_ownership_unavailable", "login recovery ownership unavailable"))?;
            crate::startup_identity::verify_auth_control_binding(&params.binding, &params.identity)
                .map_err(|_| ("scope_mismatch", "login recovery binding changed"))?;
            ownership.recover_legacy(&params.identity)
                .map_err(|_| ("auth_recovery_required", "login recovery incomplete; retain maintenance and query the original operation"))?
        };
        let state = respire_app::login_transaction::recovery_state()
            .map_err(|_| ("auth_recovery_required", "invalid login recovery journal; retain it for recovery"))?;
        if request.method == HOST_LOGIN_RECOVERY_STATUS_METHOD {
            crate::startup_identity::verify_login_recovery_status(&grant, &params)
                .map_err(|_| ("scope_mismatch", "login recovery status binding changed"))?;
        } else if state != HostLoginRecoveryState::Clear {
            return Err(("auth_recovery_required", "login recovery did not clear the journal"));
        }
        *control_ready = true;
        response.envelope = Some(ResultEnvelope::new("login-recovery", Status::Ok,
            serde_json::to_value(HostLoginRecoveryResult { state, recovered })
                .map_err(|_| ("auth_recovery_required", "login recovery result encoding failed"))?,
            Vec::new()));
        response.ok = true;
        response.exit = 0;
        return Ok(());
    }
    use respire_protocol::metadata_control::{HostMetadataOutcome, HOST_METADATA_ENABLE_METHOD,
        HOST_METADATA_DISABLE_METHOD, HOST_METADATA_RESULT_METHOD, HOST_METADATA_STATUS_METHOD,
        HOST_AGENT_CONFIG_READ_METHOD, HOST_SNAPSHOT_PREFLIGHT_METHOD};
    if [HOST_METADATA_STATUS_METHOD, HOST_AGENT_CONFIG_READ_METHOD, HOST_SNAPSHOT_PREFLIGHT_METHOD]
        .contains(&request.method.as_str()) {
        let read = crate::metadata_control::OfflineReadRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid fixed offline configuration read"))?;
        response.envelope = Some(ResultEnvelope::new(read.kind.command(), Status::Fail,
            serde_json::json!({}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_offline_read(grant, read.kind)
            .map_err(|_| ("unauthorized", "offline read launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "offline read parent watch failed"))?;
        crate::startup_identity::verify_offline_read_binding(read.binding(), read.kind)
            .map_err(|_| ("scope_mismatch", "offline read binding mismatch"))?;
        let envelope = read.execute().map_err(|_| {
            if read.kind == respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight {
                ("snapshot_preflight_blocked", "snapshot validation failed; inspect snapshot recovery before continuing")
            } else { ("offline_read_unavailable", "offline configuration read failed") }
        })?;
        crate::startup_identity::verify_offline_read_binding(read.binding(), read.kind)
            .map_err(|_| ("scope_mismatch", "offline read binding changed"))?;
        *control_ready = true;
        response.ok = true;
        response.exit = 0;
        response.envelope = Some(envelope);
        return Ok(());
    }
    if [HOST_METADATA_ENABLE_METHOD, HOST_METADATA_DISABLE_METHOD, HOST_METADATA_RESULT_METHOD]
        .contains(&request.method.as_str()) {
        let metadata = crate::metadata_control::MetadataChange::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private metadata request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("metadata-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_metadata_control(grant)
            .map_err(|_| ("unauthorized", "private metadata maintenance authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private metadata parent watch failed"))?;
        crate::startup_identity::verify_metadata_control_binding(metadata.binding(), metadata.identity())
            .map_err(|_| ("scope_mismatch", "private metadata binding mismatch"))?;
        let result = metadata.execute()
            .map_err(|_| ("metadata_outcome_unknown", "query the original metadata operation before retrying"))?;
        crate::startup_identity::verify_metadata_control_binding(metadata.binding(), metadata.identity())
            .map_err(|_| ("metadata_outcome_unknown", "metadata completion binding changed"))?;
        *control_ready = true;
        response.ok = result.outcome == HostMetadataOutcome::Committed;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("metadata_outcome_unknown".into()); }
        response.envelope = Some(ResultEnvelope::new("metadata-control",
            if response.ok { Status::Ok } else { Status::Fail },
            serde_json::to_value(result).map_err(|_| ("metadata_outcome_unknown", "metadata receipt encoding failed"))?,
            Vec::new()));
        return Ok(());
    }
    use respire_protocol::migration_control::{HostMigrationOutcome,
        HOST_MIGRATION_PROFILE_METHOD, HOST_MIGRATION_RESULT_METHOD};
    if [HOST_MIGRATION_PROFILE_METHOD, HOST_MIGRATION_RESULT_METHOD].contains(&request.method.as_str()) {
        let migration = crate::migration_control::MigrationRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private migration request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("migration-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        if respire_app::login_transaction::auth_recovery_pending()
            .map_err(|_| ("auth_recovery_pending", "authentication recovery state is invalid"))? {
            return Err(("auth_recovery_pending", "finish authentication recovery before migrating a legacy profile"));
        }
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private migration launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private migration parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(migration.binding(), migration.identity())
            .map_err(|_| ("auth_ownership_unavailable", "private migration ownership unavailable"))?;
        crate::startup_identity::verify_auth_control_binding(migration.binding(), migration.identity())
            .map_err(|_| ("scope_mismatch", "private migration identity mismatch"))?;
        let result = match migration.execute(&ownership) {
            Ok(result) => result,
            Err(error) if error.downcast_ref::<respire_app::migration::SchemaRejected>().is_some() => {
                crate::startup_identity::verify_auth_control_binding(migration.binding(), migration.identity())
                    .map_err(|_| ("migration_outcome_unknown", "migration rejection identity could not be verified"))?;
                *control_ready = true;
                response.ok = false;
                response.exit = 1;
                response.code = Some("migration_schema_rejected".into());
                response.data_dir = Some(migration.binding().profile.to_str()
                    .ok_or(("migration_outcome_unknown", "migration rejection profile encoding failed"))?.to_owned());
                response.envelope = Some(ResultEnvelope::new("migration-control", Status::Fail,
                    serde_json::to_value(migration.original())
                        .map_err(|_| ("migration_outcome_unknown", "migration rejection binding encoding failed"))?,
                    Vec::new()));
                return Ok(());
            }
            Err(_) => return Err(("migration_outcome_unknown", "query the original migration operation before retrying")),
        };
        crate::startup_identity::verify_auth_control_binding(migration.binding(), &result.identity)
            .map_err(|_| ("migration_outcome_unknown", "migration completion identity could not be verified"))?;
        *control_ready = true;
        response.ok = result.outcome != HostMigrationOutcome::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("migration_outcome_unknown".into()); }
        response.envelope = Some(ResultEnvelope::new("migration-control",
            if response.ok { Status::Ok } else { Status::Fail },
            serde_json::to_value(result).map_err(|_| ("migration_outcome_unknown", "migration receipt encoding failed"))?,
            Vec::new()));
        return Ok(());
    }
    use respire_protocol::key_control::{HostKeyOutcome, HOST_KEY_SUPER_RESET_METHOD,
        HOST_KEY_RESULT_METHOD, HOST_KEY_ROLLBACK_METHOD, HOST_KEY_FIVEKEYS_METHOD, HOST_KEY_COMPLETE_METHOD};
    if [HOST_KEY_SUPER_RESET_METHOD, HOST_KEY_RESULT_METHOD, HOST_KEY_ROLLBACK_METHOD,
        HOST_KEY_FIVEKEYS_METHOD, HOST_KEY_COMPLETE_METHOD]
        .contains(&request.method.as_str()) {
        let key = crate::key_control::KeyRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private key request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("key-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        if key.is_complete() { crate::startup_identity::authorize_account_completion(grant) }
        else { crate::startup_identity::authorize_auth_control(grant) }
            .map_err(|_| ("unauthorized", "private key launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private key parent watch failed"))?;
        let ownership = if key.is_complete() { None } else {
            Some(crate::startup_identity::acquire_auth_ownership(key.binding(), key.identity())
                .map_err(|_| ("auth_ownership_unavailable", "private key ownership unavailable"))?)
        };
        crate::startup_identity::verify_auth_control_binding(key.binding(), key.identity())
            .map_err(|_| ("scope_mismatch", "private key identity mismatch"))?;
        let result = if key.is_complete() {
            key.complete(&request, crate::key_control::verify_target_runtime)
        } else {
            key.execute(&request, ownership.ok_or(("auth_ownership_unavailable", "private key ownership missing"))?)
        }
            .map_err(|_| ("key_outcome_unknown", "query the original key operation before retrying"))?;
        key.verify_result(&request, &result)
            .map_err(|_| ("key_outcome_unknown", "key operation completion identity could not be verified"))?;
        *control_ready = true;
        response.ok = result.outcome != HostKeyOutcome::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("key_outcome_unknown".into()); }
        let status = match result.outcome {
            HostKeyOutcome::Committed | HostKeyOutcome::RolledBack => Status::Ok,
            HostKeyOutcome::Pending | HostKeyOutcome::AwaitingInput => Status::Pending,
            HostKeyOutcome::Unknown => Status::Fail,
        };
        // This receipt can contain the new super password: never copy it to progress.
        response.envelope = Some(ResultEnvelope::new("key-control", status,
            serde_json::to_value(result)
                .map_err(|_| ("key_outcome_unknown", "private key receipt encoding failed"))?, Vec::new()));
        return Ok(());
    }
    use respire_protocol::space_control::{HostSpaceOutcome, HOST_SPACE_INVITE_METHOD,
        HOST_SPACE_JOIN_METHOD, HOST_SPACE_KICK_METHOD, HOST_SPACE_RESULT_METHOD};
    if [HOST_SPACE_INVITE_METHOD, HOST_SPACE_JOIN_METHOD, HOST_SPACE_KICK_METHOD, HOST_SPACE_RESULT_METHOD]
        .contains(&request.method.as_str()) {
        let space = crate::space_control::SpaceRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private space request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("space-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private space launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private space parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(space.binding(),space.identity())
            .map_err(|_| ("auth_ownership_unavailable", "private space ownership unavailable"))?;
        space.verify_binding()
            .map_err(|_| ("scope_mismatch", "private space identity mismatch"))?;
        let result = space.execute(&request,&ownership)
            .map_err(|_| ("space_outcome_unknown", "query the original space operation before retrying"))?;
        space.verify_binding()
            .map_err(|_| ("space_outcome_unknown", "space source identity changed"))?;
        *control_ready = true;
        response.ok = matches!(result.outcome,HostSpaceOutcome::Committed | HostSpaceOutcome::Pending);
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some(if result.outcome == HostSpaceOutcome::Rejected {
            "space_rejected" } else { "space_outcome_unknown" }.into()); }
        let status = match result.outcome { HostSpaceOutcome::Committed => Status::Ok,
            HostSpaceOutcome::Pending => Status::Pending, HostSpaceOutcome::Unknown | HostSpaceOutcome::Rejected => Status::Fail };
        // Never publish the secret-bearing result through shared progress or logs.
        response.envelope = Some(ResultEnvelope::new("space-control",status,
            serde_json::to_value(result).map_err(|_| ("space_outcome_unknown", "space receipt encoding failed"))?,Vec::new()));
        return Ok(());
    }
    use respire_protocol::profile_maintenance::{HostProfileRemoveOutcome,
        HOST_PROFILE_REMOVE_METHOD, HOST_PROFILE_RESULT_METHOD};
    if [HOST_PROFILE_REMOVE_METHOD, HOST_PROFILE_RESULT_METHOD].contains(&request.method.as_str()) {
        let profile = crate::profile_maintenance::ProfileRemoveRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private profile removal request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("profile-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private profile launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private profile parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(profile.binding(), profile.identity())
            .map_err(|_| ("auth_ownership_unavailable", "private profile ownership unavailable"))?;
        crate::startup_identity::verify_auth_control_binding(profile.binding(), profile.identity())
            .map_err(|_| ("scope_mismatch", "private profile identity mismatch"))?;
        let result = profile.execute(&ownership)
            .map_err(|_| ("profile_remove_outcome_unknown", "query the original profile operation before retrying"))?;
        crate::startup_identity::verify_auth_control_binding(profile.binding(), profile.identity())
            .map_err(|_| ("profile_remove_outcome_unknown", "profile source identity changed"))?;
        *control_ready = true;
        response.ok = result.outcome == HostProfileRemoveOutcome::Committed;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("profile_remove_outcome_unknown".into()); }
        response.envelope = Some(ResultEnvelope::new("profile-control",
            if response.ok { Status::Ok } else { Status::Fail },
            serde_json::to_value(result)
                .map_err(|_| ("profile_remove_outcome_unknown", "profile receipt encoding failed"))?, Vec::new()));
        return Ok(());
    }
    use respire_protocol::identity_write_control::{HostIdentityWriteOutcome,
        HOST_IDENTITY_LOGOUT_METHOD, HOST_IDENTITY_SESSION_REVOKE_METHOD,
        HOST_IDENTITY_WRITE_RESULT_METHOD};
    if [HOST_IDENTITY_LOGOUT_METHOD, HOST_IDENTITY_SESSION_REVOKE_METHOD,
        HOST_IDENTITY_WRITE_RESULT_METHOD].contains(&request.method.as_str()) {
        let identity = crate::identity_write_control::IdentityWriteRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private identity write request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("identity-write", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private identity write launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private identity write parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(identity.binding(), identity.identity())
            .map_err(|_| ("auth_ownership_unavailable", "private identity write ownership unavailable"))?;
        crate::startup_identity::verify_auth_control_binding(identity.binding(), identity.identity())
            .map_err(|_| ("scope_mismatch", "private identity write identity mismatch"))?;
        let result = identity.execute(&request, &ownership)
            .map_err(|_| ("identity_write_outcome_unknown", "query the original identity operation before retrying"))?;
        crate::startup_identity::verify_identity_write_result(identity.binding(), identity.identity(), &result)
            .map_err(|_| ("identity_write_outcome_unknown", "identity write completion could not be verified"))?;
        *control_ready = true;
        response.ok = result.outcome != HostIdentityWriteOutcome::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("identity_write_outcome_unknown".into()); }
        let status = match result.outcome {
            HostIdentityWriteOutcome::Committed => Status::Ok,
            HostIdentityWriteOutcome::Pending => Status::Pending,
            HostIdentityWriteOutcome::Unknown => Status::Fail,
        };
        response.envelope = Some(ResultEnvelope::new("identity-write", status,
            serde_json::to_value(result)
                .map_err(|_| ("identity_write_outcome_unknown", "identity write receipt encoding failed"))?, Vec::new()));
        return Ok(());
    }
    if [respire_protocol::config_control::AGENT_CONFIG_SET_METHOD,
        respire_protocol::config_control::AGENT_CONFIG_RESULT_METHOD].contains(&request.method.as_str()) {
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("agent-config-control", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "agent config parent watch failed"))?;
        let envelope = crate::config_control::execute_agent_config(grant, &request)
            .map_err(|_| ("agent_config_outcome_unknown", "query the original agent config operation; do not replay"))?;
        let receipt: respire_protocol::config_control::AgentConfigReceipt = serde_json::from_value(envelope.summary.clone())
            .map_err(|_| ("agent_config_outcome_unknown", "invalid agent config receipt"))?;
        response.data_dir = Some(receipt.review.profile.to_str()
            .ok_or(("agent_config_outcome_unknown", "agent config profile is not UTF-8"))?.to_owned());
        *control_ready = true;
        response.ok = receipt.outcome != respire_protocol::config_control::AgentConfigOutcome::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("agent_config_outcome_unknown".into()); }
        response.envelope = Some(envelope);
        return Ok(());
    }
    if request.method == respire_protocol::config_control::CONFIG_VALIDATE_METHOD {
        let result = crate::config_control::execute(grant, &request)
            .map_err(|_| ("config_validation_failed", "private configuration validation failed"))?;
        response.envelope = Some(result);
        response.ok = true;
        response.exit = 0;
        *control_ready = true;
        return Ok(());
    }
    if request.method == respire_protocol::identity_control::HOST_IDENTITY_SESSION_LIST_METHOD {
        let identity = crate::identity_control::IdentityRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid private identity read request"))?;
        response.envelope = Some(ResultEnvelope::new("session", Status::Fail,
            serde_json::json!({}), Vec::new()));
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private identity read launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private identity read parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(identity.binding(), identity.identity())
            .map_err(|_| ("auth_ownership_unavailable", "private identity read ownership unavailable"))?;
        crate::startup_identity::verify_auth_control_binding(identity.binding(), identity.identity())
            .map_err(|_| ("scope_mismatch", "private identity read identity mismatch"))?;
        let result = identity.execute(&ownership)
            .map_err(|_| ("identity_read_unavailable", "private identity read failed"))?;
        crate::startup_identity::verify_auth_control_binding(identity.binding(), identity.identity())
            .map_err(|_| ("scope_mismatch", "private identity read identity changed"))?;
        let mut envelope = ResultEnvelope::new("session", Status::Ok,
            serde_json::json!({"action":"list"}), Vec::new());
        // Session details use only the inherited response, never progress or a ledger.
        envelope.details = serde_json::to_value(result)
            .map_err(|_| ("identity_read_unavailable", "private identity read encoding failed"))?;
        response.envelope = Some(envelope);
        response.ok = true;
        response.exit = 0;
        *control_ready = true;
        return Ok(());
    }
    if [respire_protocol::secret_control::HOST_SECRET_READ_METHOD,
        respire_protocol::secret_control::HOST_KEYS_EXPORT_READ_METHOD]
        .contains(&request.method.as_str()) {
        return Err(("host_owned_secret", "secret reads and exports are available only in the CLI host"));
    }
    if request.method == respire_protocol::maintenance_control::LEGACY_SNAPSHOT_RECOVERY_METHOD {
        use respire_protocol::maintenance_control::{LegacySnapshotRecoveryResult, MaintenanceState};
        let params = crate::snapshot_authority::legacy_request(&request)
            .map_err(|_| ("invalid_request", "invalid legacy snapshot recovery request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("snapshot", Status::Fail,
            serde_json::json!({}), Vec::new()));
        let marker = crate::startup_identity::read_private_file(
            &grant.installation_root.join("runtime/autostart/maintenance.json"))
            .map_err(|_| ("unauthorized", "legacy snapshot maintenance marker unavailable"))?
            .ok_or(("unauthorized", "legacy snapshot maintenance marker missing"))?;
        crate::snapshot_authority::validate_legacy_launch(&grant, &marker, &params)
            .map_err(|_| ("unauthorized", "invalid legacy snapshot recovery capability"))?;
        crate::startup_identity::authorize_snapshot_control(grant, false)
            .map_err(|_| ("unauthorized", "legacy snapshot launch authorization failed"))?;
        let verified_grant = crate::startup_identity::consumed_control_grant()
            .map_err(|_| ("unauthorized", "legacy snapshot authorized grant unavailable"))?;
        response.data_dir = Some(verified_grant.profile.to_str()
            .ok_or(("scope_mismatch", "legacy snapshot profile is not UTF-8"))?.to_owned());
        let mut result = LegacySnapshotRecoveryResult {
            recovery_operation_id: params.recovery_operation_id.clone(),
            request_fingerprint: params.request_fingerprint.clone(),
            journal_sha256: params.journal_sha256.clone(),
            state: MaintenanceState::Unknown,
            cleanup_pending: false,
            cleanup_warning: None,
        };
        response.envelope = Some(ResultEnvelope::new("snapshot", Status::Pending,
            serde_json::to_value(&result)
                .map_err(|_| ("control_unavailable", "legacy snapshot progress encoding failed"))?, Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "legacy snapshot parent watch failed"))?;
        let session = respire_app::auth::read_session_json()
            .map_err(|_| ("scope_mismatch", "legacy snapshot session is invalid"))?;
        let account = session["user"].as_str().filter(|value| !value.is_empty())
            .ok_or(("scope_mismatch", "legacy snapshot account is missing"))?;
        match crate::snapshot_authority::dispatch_legacy_recovery(&params, account) {
            Ok(receipt) => result = receipt,
            Err(_) => {
                response.code = Some("snapshot_outcome_unknown".into());
                response.error = Some("legacy snapshot recovery unconfirmed; retain maintenance and query the same recovery operation".into());
            }
        }
        *control_ready = true;
        response.ok = result.state != MaintenanceState::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("snapshot_outcome_unknown".into()); }
        let status = if !response.ok { Status::Fail }
            else if result.cleanup_pending { Status::Warn }
            else { Status::Ok };
        response.envelope = Some(ResultEnvelope::new("snapshot", status,
            serde_json::to_value(result)
                .map_err(|_| ("snapshot_outcome_unknown", "legacy snapshot receipt encoding failed"))?, Vec::new()));
        return Ok(());
    }
    if request.method == respire_protocol::maintenance_control::MAINTENANCE_CONTROL_METHOD {
        use respire_protocol::maintenance_control::{MaintenanceAction, MaintenanceControlParams, MaintenanceState};
        if request.args.len() != 1 { return Err(("invalid_request", "invalid snapshot maintenance shape")); }
        let params: MaintenanceControlParams = serde_json::from_str(&request.args[0])
            .map_err(|_| ("invalid_request", "invalid snapshot maintenance params"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("snapshot", Status::Pending, serde_json::json!({}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        let marker = crate::startup_identity::read_private_file(
            &grant.installation_root.join("runtime/autostart/maintenance.json"))
            .map_err(|_| ("unauthorized", "snapshot maintenance marker unavailable"))?
            .ok_or(("unauthorized", "snapshot maintenance marker missing"))?;
        let verified_query = params.action == MaintenanceAction::Query
            && serde_json::from_slice::<serde_json::Value>(&marker)
                .is_ok_and(|value| value["phase"] == "Verified");
        let finalize = matches!(params.action, MaintenanceAction::Commit | MaintenanceAction::Release) || verified_query;
        if finalize {
            crate::snapshot_authority::validate_finalize_launch(&grant, &marker)
        } else {
            crate::snapshot_authority::validate_offline_launch(&grant, &marker)
        }.map_err(|_| ("unauthorized", "snapshot capability or role is invalid"))?;
        crate::startup_identity::authorize_snapshot_control(grant, finalize)
            .map_err(|_| ("unauthorized", "snapshot launch authorization failed"))?;
        let verified_grant = crate::startup_identity::consumed_control_grant()
            .map_err(|_| ("unauthorized", "snapshot authorized grant unavailable"))?;
        response.data_dir = Some(verified_grant.profile.to_str()
            .ok_or(("scope_mismatch", "snapshot authorized profile is not UTF-8"))?.to_owned());
        // Preserve the authorized profile in parent-revocation responses too.
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "snapshot parent watch failed"))?;
        let session = respire_app::auth::read_session_json()
            .map_err(|_| ("scope_mismatch", "snapshot session is invalid"))?;
        let account = session["user"].as_str().filter(|value| !value.is_empty())
            .ok_or(("scope_mismatch", "snapshot account is missing"))?;
        let result = if finalize {
            crate::snapshot_authority::dispatch_finalize(&request, account)
        } else {
            crate::snapshot_authority::authorize_offline(&params, crate::maintenance_control::Access::Offline)
                .map_err(|_| ("unauthorized", "snapshot offline authorization failed"))?;
            crate::snapshot_authority::dispatch_offline(&request, account,
                if params.action == MaintenanceAction::Restore { crate::maintenance_control::RestoreDispatch::FirstAttempt }
                    else { crate::maintenance_control::RestoreDispatch::Resume })
        }.map_err(|failure| {
            response.error = Some(format!("{failure:#}"));
            ("snapshot_outcome_unknown", "query the original snapshot operation before retrying")
        })?;
        *control_ready = true;
        response.ok = result.state != MaintenanceState::Unknown;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("snapshot_outcome_unknown".into()); }
        response.envelope = Some(ResultEnvelope::new("snapshot",
            if !response.ok { Status::Fail } else if result.cleanup_pending { Status::Warn } else { Status::Ok },
            serde_json::to_value(result).map_err(|_| ("snapshot_outcome_unknown", "snapshot receipt encoding failed"))?, Vec::new()));
        return Ok(());
    }
    terminal::dispatch(grant, request, response, control_ready, progress, events)
}
