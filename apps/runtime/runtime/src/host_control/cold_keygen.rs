//! Empty-library key generation over the inherited host pipe only.
use super::*;

const CAPABILITY: &str = "control.auth.cold_keygen_capability";

pub(super) fn execute(
    response: &mut RpcResponse<ResultEnvelope>,
    control_ready: &mut bool,
    progress: &Mutex<RpcResponse<ResultEnvelope>>,
    events: &mpsc::SyncSender<ControlEvent>,
) -> std::result::Result<(), ControlFailure> {
    let grant: HostControlLaunchGrant =
        read_frame().map_err(|_| ("invalid_request", "invalid cold keygen control grant"))?;
    let request: RpcRequest =
        read_frame().map_err(|_| ("invalid_request", "invalid cold keygen control request"))?;
    response.id = request.id.clone();
    if request.method == CAPABILITY {
        return capability(&grant, &request, response, control_ready);
    }
    let params = validate_auth_request(&request)?;
    if params.force || params.identity != respire_protocol::HostAuthIdentity::Absent {
        return Err((
            "invalid_request",
            "cold keygen cannot declare ownership of an existing session",
        ));
    }
    let material = params.material.as_ref().ok_or((
        "invalid_request",
        "cold keygen requires the original host factor",
    ))?;
    let verify = || {
        crate::startup_identity::verify_cold_keygen_launch(&grant, &params.binding)
            .map_err(|_| anyhow::anyhow!("cold keygen scope or parent verification failed"))
    };
    verify().map_err(|_| ("unauthorized", "cold keygen launch authorization failed"))?;
    response.idempotency_key = request.idempotency_key.clone();
    response.operation_id = request.operation_id.clone();
    response.envelope = Some(ResultEnvelope::new(
        "keygen",
        Status::Fail,
        serde_json::json!({"outcome":"unknown"}),
        Vec::new(),
    ));
    *progress
        .lock()
        .map_err(|_| ("control_unavailable", "control progress unavailable"))? = response.clone();
    watch_auth_parent(events.clone())
        .map_err(|_| ("unauthorized", "cold keygen inherited parent watch failed"))?;
    let mut admitted = false;
    let result =
        crate::startup_control::keygen_exclusion::with_configured_scope(|root, profile, owner| {
            anyhow::ensure!(
                root == params.binding.installation_root && profile == params.binding.profile,
                "cold keygen exclusion belongs to another profile"
            );
            verify()?;
            admitted = true;
            let result = owner.cold_keygen(
                request
                    .operation_id
                    .as_deref()
                    .context("cold keygen operation ID is missing")?,
                request
                    .idempotency_key
                    .as_deref()
                    .context("cold keygen idempotency key is missing")?,
                request.method == HOST_AUTH_RESULT_METHOD,
                material,
            )?;
            verify()?;
            Ok(result)
        })
        .map_err(|_| {
            if admitted {
                (
                    "auth_outcome_unknown",
                    "query the original cold keygen operation",
                )
            } else {
                (
                    "auth_ownership_unavailable",
                    "cold keygen requires the exact offline library",
                )
            }
        })?;
    *control_ready = true;
    response.ok = result.outcome == HostAuthOutcome::Committed;
    response.exit = if response.ok { 0 } else { 2 };
    if !response.ok {
        response.code = Some("auth_not_committed".into());
    }
    response.envelope = Some(ResultEnvelope::new(
        "keygen",
        if response.ok {
            Status::Ok
        } else {
            Status::Fail
        },
        serde_json::to_value(result)
            .map_err(|_| ("auth_outcome_unknown", "cold keygen receipt unavailable"))?,
        Vec::new(),
    ));
    Ok(())
}

fn capability(
    grant: &HostControlLaunchGrant,
    request: &RpcRequest,
    response: &mut RpcResponse<ResultEnvelope>,
    control_ready: &mut bool,
) -> std::result::Result<(), ControlFailure> {
    if request.v != PROTOCOL_V
        || request.args.len() != 1
        || request.scope.is_some()
        || request.parent_operation_id.is_some()
        || request.operation_id.is_some()
        || request.idempotency_key.is_some()
        || request.queue_wait_ms.is_some()
        || request.lifecycle.is_some()
        || !uuid::Uuid::parse_str(&request.id)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == request.id)
    {
        return Err(("invalid_request", "invalid cold keygen capability request"));
    }
    let params: HostControlParams = serde_json::from_str(&request.args[0])
        .map_err(|_| ("invalid_request", "invalid cold keygen capability params"))?;
    if params.schema_version != HOST_CONTROL_SCHEMA_V {
        return Err((
            "invalid_request",
            "unsupported cold keygen capability schema",
        ));
    }
    crate::startup_identity::verify_cold_keygen_launch(grant, &params.binding).map_err(|_| {
        (
            "unauthorized",
            "cold keygen capability scope verification failed",
        )
    })?;
    *control_ready = true;
    response.ok = true;
    response.exit = 0;
    response.envelope = Some(ResultEnvelope::new(
        "keygen-capability",
        Status::Ok,
        serde_json::json!({"protocol":"cold_keygen.v1"}),
        Vec::new(),
    ));
    Ok(())
}
