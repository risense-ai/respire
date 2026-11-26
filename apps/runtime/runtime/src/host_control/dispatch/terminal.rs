//! Account authentication, model preparation, and memory-revision dispatch.
use super::*;

pub(super) fn dispatch(
    grant: HostControlLaunchGrant,
    request: RpcRequest,
    response: &mut RpcResponse<ResultEnvelope>,
    control_ready: &mut bool,
    progress: &Mutex<RpcResponse<ResultEnvelope>>,
    events: &mpsc::SyncSender<ControlEvent>,
) -> std::result::Result<(), ControlFailure> {
    if request.method == crate::auth_control::LOGIN_PROGRESS_METHOD {
        use respire_protocol::host_control::{HostLoginRecoveryParams, HOST_AUTH_SCHEMA_V};
        response.envelope = Some(ResultEnvelope::new("login-progress", Status::Fail,
            serde_json::json!({}), Vec::new()));
        crate::auth_control::validate_account_frame(&request)
            .map_err(|_| ("invalid_request", "invalid login progress frame"))?;
        let params: HostLoginRecoveryParams = serde_json::from_str(&request.args[0])
            .map_err(|_| ("invalid_request", "invalid login progress binding"))?;
        if params.schema_version != HOST_AUTH_SCHEMA_V {
            return Err(("invalid_request", "unsupported login progress schema"));
        }
        let id = request.operation_id.as_deref().ok_or(("invalid_request", "missing operation ID"))?;
        let key = request.idempotency_key.as_deref().ok_or(("invalid_request", "missing idempotency key"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "private login maintenance authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "private login parent watch failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(&params.binding, &params.identity)
            .map_err(|_| ("auth_ownership_unavailable", "private login ownership unavailable"))?;
        crate::startup_identity::verify_auth_control_binding(&params.binding, &params.identity)
            .map_err(|_| ("scope_mismatch", "private login binding changed"))?;
        respire_app::login_transaction::verify_login_merge_offer(&ownership, &params.identity, id, key)
            .map_err(|_| ("login_progress_unavailable", "no matching login confirmation is available"))?;
        crate::startup_identity::verify_auth_control_binding(&params.binding, &params.identity)
            .map_err(|_| ("scope_mismatch", "private login binding changed"))?;
        response.envelope = Some(ResultEnvelope::new("login-progress", Status::Pending,
            serde_json::json!({"status":"pending_confirmation"}), Vec::new()));
        response.ok = true;
        response.exit = 0;
        *control_ready = true;
        return Ok(());
    }
    use respire_protocol::host_control::{HostAuthAccountOutcome, HOST_AUTH_REGISTER_METHOD,
        HOST_AUTH_LOGIN_METHOD, HOST_AUTH_ACCOUNT_RESULT_METHOD, HOST_AUTH_ACCOUNT_COMPLETE_METHOD,
        HOST_AUTH_ACCOUNT_ROLLBACK_METHOD};
    if [HOST_AUTH_REGISTER_METHOD, HOST_AUTH_LOGIN_METHOD, HOST_AUTH_ACCOUNT_RESULT_METHOD,
        HOST_AUTH_ACCOUNT_COMPLETE_METHOD, HOST_AUTH_ACCOUNT_ROLLBACK_METHOD]
        .contains(&request.method.as_str()) {
        let account = crate::auth_control::AccountRequest::parse(&request)
            .map_err(|_| ("invalid_request", "invalid account control request"))?;
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        response.envelope = Some(ResultEnvelope::new("auth-account", Status::Fail,
            serde_json::json!({"outcome":"unknown"}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        if account.is_completion() {
            crate::startup_identity::authorize_account_completion(grant)
        } else {
            crate::startup_identity::authorize_auth_control(grant)
        }.map_err(|_| ("unauthorized", "account control launch authorization failed"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "account control parent watch failed"))?;
        let ownership = if account.is_completion() { None } else {
            Some(crate::startup_identity::acquire_auth_ownership(account.binding(), account.identity())
                .map_err(|_| ("auth_ownership_unavailable", "account control library ownership unavailable"))?)
        };
        let host_registration = account.is_host_registration(&request)
            .map_err(|_| ("scope_mismatch", "account control receipt family could not be verified"))?;
        if host_registration {
            crate::startup_identity::verify_registration_control_binding(account.binding(), account.identity())
        } else {
            crate::startup_identity::verify_auth_control_binding(account.binding(), account.identity())
        }.map_err(|_| ("scope_mismatch", "account control identity does not match the consumed grant"))?;
        *control_ready = true;
        let reply = account.execute(&request, ownership).map_err(|failure| {
            response.error = Some(format!("{failure:#}"));
            ("account_outcome_unknown", "query the same account operation before retrying")
        })?;
        let result = &reply.receipt;
        response.ok = matches!(result.outcome,
            HostAuthAccountOutcome::AwaitingInput | HostAuthAccountOutcome::AwaitingVerification
                | HostAuthAccountOutcome::Committed);
        response.exit = if response.ok { 0 } else { 2 };
        response.code = match result.outcome {
            HostAuthAccountOutcome::Unknown => Some("account_outcome_unknown".into()),
            HostAuthAccountOutcome::RolledBack => Some("account_rolled_back".into()),
            _ => None,
        };
        response.envelope = Some(ResultEnvelope::new("auth-account",
            if result.outcome == HostAuthAccountOutcome::AwaitingInput { Status::Pending }
            else if response.ok { Status::Ok } else { Status::Fail },
            (if host_registration || result.login.is_none() { serde_json::to_value(result) } else { serde_json::to_value(&reply) })
                .map_err(|_| ("account_outcome_unknown", "account receipt encoding failed"))?,
            Vec::new()));
        return Ok(());
    }
    let auth = request.method == HOST_AUTH_KEYGEN_METHOD || request.method == HOST_AUTH_RESULT_METHOD;
    if auth {
        let params = validate_auth_request(&request)?;
        if request.method == HOST_AUTH_KEYGEN_METHOD && params.material.is_none() {
            return Err(("invalid_request", "keygen mutation requires host-provided key material"));
        }
        response.operation_id = request.operation_id.clone();
        response.idempotency_key = request.idempotency_key.clone();
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        crate::startup_identity::authorize_auth_control(grant)
            .map_err(|_| ("unauthorized", "auth control launch authorization failed"))?;
        let ownership = crate::startup_identity::acquire_auth_ownership(&params.binding, &params.identity)
            .map_err(|_| ("auth_ownership_unavailable", "auth control could not acquire the authorized library"))?;
        watch_auth_parent(events.clone())
            .map_err(|_| ("unauthorized", "auth control parent watch failed"))?;
        ownership.recover_keygen(&params.identity, params.material.as_ref())
            .map_err(|_| ("auth_recovery_required", "auth transaction recovery requires attention"))?;
        crate::startup_identity::verify_keygen_control_binding(&params.binding, &params.identity)
            .map_err(|_| ("scope_mismatch", "auth control identity does not match the consumed grant"))?;
        *control_ready = true;
        let result = respire_app::login_transaction::control_keygen(&ownership, &params.identity,
            request.operation_id.as_deref().ok_or(("invalid_request", "auth operation ID is missing"))?,
            request.idempotency_key.as_deref().ok_or(("invalid_request", "auth idempotency key is missing"))?,
            params.force, request.method == HOST_AUTH_RESULT_METHOD, params.material.as_ref())
            .map_err(|failure| {
                // Diagnostics stay in this private response, never stderr or argv.
                response.error = Some(format!("{failure:#}"));
                ("auth_outcome_unknown", "query the same auth operation ID before retrying")
            })?;
        response.ok = result.outcome == HostAuthOutcome::Committed;
        response.exit = if response.ok { 0 } else { 2 };
        if !response.ok { response.code = Some("auth_not_committed".into()); }
        response.envelope = Some(ResultEnvelope::new("keygen", if response.ok { Status::Ok } else { Status::Fail },
            serde_json::to_value(result).map_err(|_| ("auth_outcome_unknown", "auth result encoding failed"))?, Vec::new()));
        return Ok(());
    }
    if request.method == offline_setting::METHOD {
        return offline_setting::dispatch(grant, &request, response, control_ready);
    }
    let (params, model_params) = validate_request(&request)?;
    let verify_binding = if request.method == HostControlOperation::MemoryRevision.method() {
        crate::startup_identity::verify_memory_revision_binding
    } else {
        crate::startup_identity::verify_control_binding
    };
    let model_plan = request.method == HostControlOperation::ModelInstallPlan.method();
    let model_verify = request.method == HostControlOperation::ModelInstallVerify.method();
    if model_plan || model_verify {
        crate::startup_identity::authorize_model_control(grant)
    } else {
        crate::startup_identity::authorize_control(grant)
    }.map_err(|_| ("unauthorized", "host control launch authorization failed"))?;
    // Read both frames before letting the watcher own stdin. No further request
    // or argv dispatch is accepted; EOF/extra bytes revoke this read-only child.
    watch_control_parent()
        .map_err(|_| ("unauthorized", "host control parent watch failed"))?;
    verify_binding(&params.binding)
        .map_err(|_| ("scope_mismatch", "host control binding does not match consumed grant"))?;
    *control_ready = true;
    if model_plan || model_verify {
        let command = if model_verify { "model-install-verify" } else { "model-install-plan" };
        response.envelope = Some(ResultEnvelope::new(command, Status::Pending, serde_json::json!({}), Vec::new()));
        *progress.lock().map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
        if model_verify {
            respire_commands::model_assets::enable_backend_probe_host(
                respire_commands::model_assets::BackendProbeHost::PrivateControl);
        }
        let model = model_params.as_ref()
            .ok_or(("invalid_request", "model preparation parameters missing"))?;
        let plan = if model_verify {
            respire_commands::model_assets::model_preparation_verify(&model.intent,
                model.selection_sha256.as_deref()
                    .ok_or(("invalid_request", "model preparation selection digest missing"))?)
        } else { respire_commands::model_assets::model_preparation_plan(&model.intent) }
            .map_err(|_| ("model_plan_unavailable", "selected model preparation status unavailable"))?;
        verify_binding(&params.binding)
            .map_err(|_| ("scope_mismatch", "host control identity changed during model planning"))?;
        let mut envelope = ResultEnvelope::new(command, Status::Ok,
            serde_json::json!({"complete":plan.assets.complete}), Vec::new());
        envelope.details = serde_json::to_value(plan)
            .map_err(|_| ("model_plan_unavailable", "model installation plan encoding failed"))?;
        response.envelope = Some(envelope);
        response.ok = true;
        response.exit = 0;
        return Ok(());
    }
    let revision = match respire_app::service::memory_revision() {
        Ok(revision) => revision,
        Err(failure) => {
            // This path reports DB/schema diagnostics, never grant or session contents.
            response.error = Some(format!("{failure:#}"));
            return Err(("revision_unavailable", "memory revision unavailable"));
        }
    };
    verify_binding(&params.binding)
        .map_err(|_| ("scope_mismatch", "host control identity changed during the read"))?;
    let actual_profile = std::path::Path::new(&revision.profile).canonicalize()
        .map_err(|_| ("scope_mismatch", "host control profile is unavailable"))?;
    if actual_profile != params.binding.profile {
        return Err(("scope_mismatch", "memory revision profile does not match control binding"));
    }
    response.envelope = Some(ResultEnvelope::new(
        "memory-revision", Status::Ok,
        serde_json::to_value(revision)
            .map_err(|_| ("revision_unavailable", "memory revision encoding failed"))?,
        Vec::new(),
    ));
    response.ok = true;
    response.exit = 0;
    Ok(())
}
