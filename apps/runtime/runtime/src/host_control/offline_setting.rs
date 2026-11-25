use super::*;
use respire_app::{lock::LibraryLock, service};
use respire_protocol::{
    metadata_control::metadata_status_path, ControlBinding, HOST_CONTROL_SCHEMA_V,
};
use std::path::Path;

pub(super) const METHOD: &str = "control.agent_config.set_offline_mode";
const COMMAND: &str = "agent-config-offline-mode";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    schema_version: u32,
    binding: ControlBinding,
    key: String,
    value: String,
}

impl Params {
    fn parse(request: &RpcRequest) -> std::result::Result<Self, ControlFailure> {
        let invalid = ("invalid_request", "invalid offline agent mode request");
        if request.v != PROTOCOL_V
            || !uuid::Uuid::parse_str(&request.id)
                .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == request.id)
            || request.args.len() != 1
            || request.scope.is_some()
            || request.operation_id.is_some()
            || request.idempotency_key.is_some()
            || request.parent_operation_id.is_some()
            || request.queue_wait_ms.is_some()
            || request.lifecycle.is_some()
        {
            return Err(invalid);
        }
        let params: Self = serde_json::from_str(&request.args[0]).map_err(|_| invalid)?;
        if params.schema_version != HOST_CONTROL_SCHEMA_V
            || !matches!(
                (params.key.as_str(), params.value.as_str()),
                ("diary_mode", "concise" | "verbose") | ("recall_mode", "fast" | "quality")
            )
        {
            return Err(invalid);
        }
        Ok(params)
    }
}

pub(super) fn dispatch(
    grant: HostControlLaunchGrant,
    request: &RpcRequest,
    response: &mut RpcResponse<ResultEnvelope>,
    control_ready: &mut bool,
) -> std::result::Result<(), ControlFailure> {
    response.envelope = Some(ResultEnvelope::new(
        COMMAND,
        Status::Fail,
        serde_json::json!({}),
        Vec::new(),
    ));
    let params = Params::parse(request)?;
    let unauthorized = (
        "unauthorized",
        "offline agent mode launch authorization failed",
    );
    if grant.maintenance.is_some() {
        return Err(unauthorized);
    }
    crate::startup_identity::authorize_control(grant).map_err(|_| unauthorized)?;
    respire_spawn::watch_control_parent().map_err(|_| unauthorized)?;

    let scope_mismatch = (
        "scope_mismatch",
        "offline agent mode installation or profile changed",
    );
    let verify = || {
        crate::startup_identity::verify_control_binding(&params.binding).map_err(|_| scope_mismatch)
    };
    verify()?;
    let root = &params.binding.installation_root;
    let profile = &params.binding.profile;
    let runtime = root.join("runtime");
    let ownership_unavailable = (
        "offline_ownership_unavailable",
        "could not acquire offline installation and profile ownership",
    );
    let acquire = |directory: &Path| {
        check_lock_path(&directory.join("lock.db")).map_err(|_| ownership_unavailable)?;
        LibraryLock::acquire(directory, Duration::ZERO).map_err(|_| ownership_unavailable)
    };
    // Keep the legacy lifecycle lock outside the library and config locks.
    let _takeover = acquire(&runtime.join("takeover"))?;
    let _root = acquire(root)?;
    let _profile = if root == profile {
        None
    } else {
        Some(acquire(profile)?)
    };
    check_lock_path(&service::client_config_path().with_extension("json.lock"))
        .map_err(|_| ownership_unavailable)?;
    let _config = service::client_config_lock().map_err(|_| ownership_unavailable)?;

    let blocked = (
        "offline_setting_unavailable",
        "offline agent mode is blocked by current configuration or recovery",
    );
    for name in [
        "account-active.json",
        "key-control.json",
        "offline-keygen.json",
    ] {
        if crate::startup_identity::read_private_file(&runtime.join("autostart").join(name))
            .map_err(|_| blocked)?
            .is_some()
        {
            return Err(blocked);
        }
    }
    respire_app::snapshot::ensure_no_incomplete_restore(profile).map_err(|_| blocked)?;
    service::validate_client_config().map_err(|_| blocked)?;
    let path = profile.join("agent.json");
    let mut settings = match crate::startup_identity::read_private_file(&path)
        .map_err(|_| blocked)?
    {
        Some(bytes) => serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes)
            .map_err(|_| blocked)?,
        None => serde_json::Map::new(),
    };
    settings.insert(
        params.key.clone(),
        serde_json::Value::String(params.value.clone()),
    );
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|_| blocked)?;
    if bytes.len() > HOST_CONTROL_MAX_FRAME_BYTES {
        return Err(blocked);
    }
    verify()?;
    *control_ready = true;
    let unknown = (
        "offline_setting_outcome_unknown",
        "agent mode publication unconfirmed; read the setting before retrying",
    );
    crate::config_control::replace_reviewed_file(&path, Some(&bytes)).map_err(|_| unknown)?;
    verify().map_err(|_| unknown)?;
    response.ok = true;
    response.exit = 0;
    response.envelope = Some(ResultEnvelope::new(
        COMMAND,
        Status::Ok,
        serde_json::json!({"action":"updated", "key":params.key, "value":params.value}),
        Vec::new(),
    ));
    Ok(())
}

fn check_lock_path(path: &Path) -> Result<()> {
    metadata_status_path(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(metadata.is_file(), "offline lock path must be a file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
