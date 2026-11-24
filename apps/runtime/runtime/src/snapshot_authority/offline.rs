//! Offline snapshot proof and verified Runtime finalization.
use super::*;

pub(super) fn offline_endpoint_matches(proof: &OfflineProof, bytes: Option<&[u8]>) -> Result<bool> {
    match (&proof.endpoint_sha256, proof.endpoint_pid, bytes) {
        (None, None, None) => Ok(true),
        (Some(digest), Some(pid), Some(bytes)) if pid > 0 && bytes.len() <= 16 * 1024 => {
            let value: serde_json::Value = serde_json::from_slice(bytes)?;
            Ok(digest == &hash(bytes) && value["pid"].as_u64() == Some(u64::from(pid)))
        }
        _ => Ok(false),
    }
}

pub(super) fn verify_offline_proof(
    grant: &HostControlLaunchGrant,
    proof: &OfflineProof,
    current: bool,
) -> Result<()> {
    anyhow::ensure!(
        match (&proof.endpoint_sha256, proof.endpoint_pid) {
            (None, None) => true,
            (Some(digest), Some(pid)) =>
                pid > 0
                    && digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            _ => false,
        },
        "snapshot offline endpoint proof malformed"
    );
    if let Some(prior) = proof.prior_runtime.as_ref() {
        anyhow::ensure!(
            prior.pid > 0 && !prior.process_start_id.is_empty(),
            "snapshot offline prior process identity missing"
        );
    }
    // A Verified target legitimately replaces the endpoint. Its strict callback
    // fences that live identity instead; absence is an Offline capability only.
    if !current {
        return Ok(());
    }
    let bytes = crate::startup_identity::read_private_file(
        &grant.installation_root.join("runtime/endpoint.json"),
    )?;
    anyhow::ensure!(
        offline_endpoint_matches(proof, bytes.as_deref())?,
        "snapshot offline endpoint changed"
    );
    if let Some(pid) = proof.endpoint_pid {
        anyhow::ensure!(
            crate::startup_identity::process_birth(pid)?.is_none(),
            "snapshot recorded owner is alive/unobservable; no offline authorization"
        );
    }
    if let Some(prior) = proof.prior_runtime.as_ref() {
        let prefix = if cfg!(windows) {
            "windows-filetime-"
        } else if cfg!(target_os = "linux") {
            "linux-starttime-"
        } else {
            "darwin-lstart-"
        };
        anyhow::ensure!(
            !crate::startup_identity::process_birth(prior.pid)?
                .is_some_and(|birth| prior.process_start_id == format!("{prefix}{birth}")),
            "snapshot previous Runtime still lives; real drain required"
        );
    }
    Ok(())
}

/// Rechecked before lock acquisition, before replacement and after the action
/// by maintenance_control. Never delegates to read-only verify_control_binding.
pub(crate) fn authorize_offline(params: &Params, access: Access) -> Result<()> {
    anyhow::ensure!(
        matches!(access, Access::Offline | Access::StartRestore)
            && !matches!(params.action, Action::Commit | Action::Release),
        "snapshot offline capability cannot finalize a serving store"
    );
    let grant = crate::startup_identity::consumed_control_grant()?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    // Reuse current image/account/registration and controller fencing, with the
    // parent's narrowly scoped Paused snapshot hook in startup validation.
    crate::startup_identity::verify_managed_boot()?;
    let bytes = crate::startup_identity::read_private_file(&marker_path(grant))?
        .context("snapshot maintenance marker disappeared")?;
    let snapshot = validate_marker(grant, &bytes, false)?;
    anyhow::ensure!(
        params.schema_version == MAINTENANCE_CONTROL_SCHEMA_V
            && params.binding.installation_root == grant.installation_root
            && params.binding.profile == grant.profile
            && params.binding.launch_id == grant.instance_id
            && params.binding.expected_account_id.as_deref() == Some(snapshot.account_id.as_str())
            && params.operation_id == snapshot.operation_id
            && params.request_fingerprint == snapshot.request_fingerprint
            && params.sync_generation == snapshot.sync_generation
            && params.snapshot_id == snapshot.snapshot_id,
        "snapshot request differs from the consumed marker capability"
    );
    // grant.sync_generation describes this fresh process, not the original O.
    Ok(())
}

/// Fixed method dispatch connects the authority directly to the existing
/// snapshot executor and journal; no generic command or alternate lock path.
pub(crate) fn dispatch_offline(
    request: &RpcRequest,
    account: &str,
    dispatch: RestoreDispatch,
) -> Result<Receipt> {
    let params = parse_request(request)?;
    authorize_offline(&params, Access::Offline)?;
    // Same installation owner as serve: held across the existing library lock,
    // journal transaction and repeated authorization. Never used by finalizers.
    let _owner = respire_app::lock::LibraryLock::acquire(
        &params.binding.installation_root.join("runtime"),
        std::time::Duration::ZERO,
    )
    .context("snapshot installation ownership unavailable; no offline DB access")?;
    crate::maintenance_control::execute_offline(&params, account, dispatch, authorize_offline)
}

fn parse_request(request: &RpcRequest) -> Result<Params> {
    anyhow::ensure!(
        request.v == respire_commands::rpc_worker::PROTOCOL_V
            && request.method == MAINTENANCE_CONTROL_METHOD
            && request.args.len() == 1
            && uuid::Uuid::parse_str(&request.id)
                .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == request.id)
            && request.scope.is_none()
            && request.parent_operation_id.is_none()
            && request.queue_wait_ms.is_none()
            && request.lifecycle.is_none(),
        "invalid snapshot maintenance request shape"
    );
    let params: Params = serde_json::from_str(&request.args[0])?;
    anyhow::ensure!(
        request.operation_id.as_deref() == Some(params.operation_id.as_str())
            && request.idempotency_key == request.operation_id,
        "snapshot request operation ID mismatch"
    );
    Ok(params)
}

/// Full target fencing is independent of the fresh helper's instance/generation.
/// Health is an actor report, not by itself evidence of process ownership.
pub(super) fn verify_runtime(grant: &HostControlLaunchGrant, snapshot: &SnapshotBinding) -> Result<()> {
    let target = snapshot
        .verified_runtime
        .as_ref()
        .context("verified snapshot target missing")?;
    anyhow::ensure!(
        target.pid > 0
            && target.pid != std::process::id()
            && target.pid != grant.supervisor_pid
            && target.scope.generation > 0
            && !target.scope.instance_id.is_empty()
            && target.scope.account_id == snapshot.account_id
            && target.image_path.is_absolute()
            && target.image_path.canonicalize()? == target.image_path
            && target.image_path == grant.image_path
            && target
                .image_sha256
                .eq_ignore_ascii_case(&grant.image_sha256)
            && !target.runtime_version.is_empty(),
        "invalid verified snapshot target identity"
    );
    let birth = crate::startup_identity::process_birth(target.pid)?
        .context("verified snapshot Runtime exited")?;
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    anyhow::ensure!(
        target.process_start_id == format!("{prefix}{birth}"),
        "verified snapshot Runtime PID was reused"
    );
    let process = verified_process(target.pid, &birth, &target.process_start_id)?;
    anyhow::ensure!(
        process == target.image_path
            && crate::startup_identity::hash_file(&target.image_path)?
                .eq_ignore_ascii_case(&target.image_sha256),
        "verified snapshot native process or image differs from marker"
    );
    let health = respire_commands::net_rpc::health()?;
    let scope: respire_protocol::RuntimeRequestScope =
        serde_json::from_value(respire_commands::net_rpc::request_scope(&health)?)?;
    anyhow::ensure!(
        scope == target.scope
            && health.pid == target.pid
            && health.v == grant.protocol_version
            && health.process_start_id == target.process_start_id
            && health.installation_id == grant.installation_id
            && Path::new(&health.installation_root).canonicalize()? == grant.installation_root
            && Path::new(&health.data_dir).canonicalize()? == grant.profile
            && Path::new(&health.image_path).canonicalize()? == target.image_path
            && Path::new(&health.exe).canonicalize()? == target.image_path
            && health
                .image_sha256
                .eq_ignore_ascii_case(&target.image_sha256)
            && health.runtime_version == target.runtime_version
            && health.store_ready
            && health.snapshot_restore_id.as_deref() == Some(snapshot.snapshot_id.as_str()),
        "verified snapshot actor/store identity differs from marker"
    );
    anyhow::ensure!(
        crate::startup_identity::process_birth(target.pid)?.as_deref() == Some(birth.as_str())
            && verified_process(target.pid, &birth, &target.process_start_id)? == process,
        "verified snapshot process changed during actor verification"
    );
    Ok(())
}

fn verified_process(pid: u32, birth: &str, process_start_id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        crate::startup_identity::process_birth(pid)?.as_deref() == Some(birth),
        "snapshot target process changed before native image inspection"
    );
    #[cfg(target_os = "linux")]
    {
        // The older commands inspector only recognizes combined-CLI argv on
        // Linux. Use its same procfs image authority, pinned to the native birth.
        let _ = process_start_id;
        let image = std::fs::read_link(format!("/proc/{pid}/exe"))?.canonicalize()?;
        anyhow::ensure!(
            crate::startup_identity::process_birth(pid)?.as_deref() == Some(birth),
            "snapshot target process changed during procfs inspection"
        );
        Ok(image)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let process = respire_commands::net_rpc::runtime_process_identity(pid)?;
        anyhow::ensure!(
            process.started_at == birth || process.started_at == process_start_id,
            "snapshot native process birth differs from marker"
        );
        Ok(Path::new(&process.executable).canonicalize()?)
    }
}

pub(crate) fn authorize_finalize(params: &Params, access: Access) -> Result<()> {
    anyhow::ensure!(
        access == Access::Finalize
            && matches!(
                params.action,
                Action::Commit | Action::Release | Action::Query
            ),
        "Verified metadata helper cannot perform offline replacement"
    );
    let grant = crate::startup_identity::consumed_control_grant()?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    crate::startup_identity::verify_managed_boot()?;
    let bytes = crate::startup_identity::read_private_file(&marker_path(grant))?
        .context("Verified snapshot marker disappeared")?;
    let snapshot = validate_marker(grant, &bytes, true)?;
    anyhow::ensure!(
        params.schema_version == MAINTENANCE_CONTROL_SCHEMA_V
            && params.binding.installation_root == grant.installation_root
            && params.binding.profile == grant.profile
            && params.binding.launch_id == grant.instance_id
            && params.binding.expected_account_id.as_deref() == Some(snapshot.account_id.as_str())
            && params.operation_id == snapshot.operation_id
            && params.request_fingerprint == snapshot.request_fingerprint
            && params.sync_generation == snapshot.sync_generation
            && params.snapshot_id == snapshot.snapshot_id,
        "snapshot finalization differs from consumed Verified capability"
    );
    verify_runtime(grant, &snapshot)?;
    let after = crate::startup_identity::read_private_file(&marker_path(grant))?
        .context("Verified snapshot marker disappeared during verification")?;
    validate_marker(grant, &after, true)?;
    Ok(())
}

/// Existing execute_verified owns journal semantics and cleanup warnings. This
/// helper never acquires a library lock, opens a store, or replaces DB files.
pub(crate) fn dispatch_finalize(request: &RpcRequest, account: &str) -> Result<Receipt> {
    let params = parse_request(request)?;
    crate::maintenance_control::execute_verified(
        &params,
        account,
        authorize_finalize,
        |binding, snapshot_id| verify_snapshot_store(&params, binding, snapshot_id),
    )
}

/// Independent strict callback at execute_verified's verify-before-commit gate.
fn verify_snapshot_store(
    params: &Params,
    binding: &respire_protocol::ControlBinding,
    snapshot_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        binding.installation_root == params.binding.installation_root
            && binding.profile == params.binding.profile
            && binding.launch_id == params.binding.launch_id
            && binding.expected_account_id == params.binding.expected_account_id
            && snapshot_id == params.snapshot_id,
        "snapshot verify callback binding mismatch"
    );
    authorize_finalize(params, Access::Finalize)
}
