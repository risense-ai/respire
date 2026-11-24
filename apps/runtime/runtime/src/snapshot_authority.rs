//! Snapshot maintenance authority, separate from fresh worker launch generation.
use anyhow::{Context, Result};
use respire_protocol::maintenance_control::{
    MaintenanceAction as Action, MaintenanceControlParams as Params,
    MaintenanceControlResult as Receipt, MAINTENANCE_CONTROL_METHOD, MAINTENANCE_CONTROL_SCHEMA_V,
};
use respire_protocol::{
    HostControlLaunchGrant, RpcRequest, RuntimeStopOutcome, RuntimeStopReceipt,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::maintenance_control::{Access, RestoreDispatch};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotBinding {
    operation_id: String,
    request_fingerprint: String,
    sync_generation: usize,
    snapshot_id: String,
    account_id: String,
    drained_birth: String,
    #[serde(default)]
    drain: Option<RuntimeStopReceipt>,
    #[serde(default)]
    offline: Option<OfflineProof>,
    #[serde(default)]
    verified_runtime: Option<VerifiedRuntime>,
    #[serde(default)]
    legacy_recovery: Option<respire_protocol::maintenance_control::LegacySnapshotRecoveryBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfflineProof {
    endpoint_sha256: Option<String>,
    endpoint_pid: Option<u32>,
    prior_runtime: Option<VerifiedRuntime>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedRuntime {
    scope: respire_protocol::RuntimeRequestScope,
    pid: u32,
    process_start_id: String,
    image_path: PathBuf,
    image_sha256: String,
    runtime_version: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn legacy_request(
    request: &RpcRequest,
) -> Result<respire_protocol::maintenance_control::LegacySnapshotRecoveryParams> {
    use respire_protocol::maintenance_control::LEGACY_SNAPSHOT_RECOVERY_METHOD;
    anyhow::ensure!(
        request.v == respire_commands::rpc_worker::PROTOCOL_V
            && request.method == LEGACY_SNAPSHOT_RECOVERY_METHOD
            && request.args.len() == 1
            && canonical_operation(&request.id)
            && request.scope.is_none()
            && request.parent_operation_id.is_none()
            && request.queue_wait_ms.is_none()
            && request.lifecycle.is_none(),
        "invalid legacy snapshot recovery request shape"
    );
    let params: respire_protocol::maintenance_control::LegacySnapshotRecoveryParams =
        serde_json::from_str(&request.args[0])?;
    anyhow::ensure!(
        params.schema_version == MAINTENANCE_CONTROL_SCHEMA_V
            && canonical_operation(&params.recovery_operation_id)
            && request.operation_id.as_deref() == Some(params.recovery_operation_id.as_str())
            && request.idempotency_key == request.operation_id
            && digest_shape(&params.journal_sha256)
            && digest_shape(&params.request_fingerprint)
            && legacy_fingerprint(&params)? == params.request_fingerprint,
        "invalid legacy snapshot recovery identity"
    );
    Ok(params)
}

fn canonical_operation(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

fn digest_shape(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn legacy_fingerprint(
    params: &respire_protocol::maintenance_control::LegacySnapshotRecoveryParams,
) -> Result<String> {
    Ok(hash(&serde_json::to_vec(&(
        "snapshot-legacy-recovery-v1",
        &params.binding.installation_root,
        &params.binding.profile,
        &params.binding.expected_account_id,
        &params.recovery_operation_id,
        &params.journal_sha256,
    ))?))
}

pub(crate) fn validate_legacy_launch(
    grant: &HostControlLaunchGrant,
    bytes: &[u8],
    params: &respire_protocol::maintenance_control::LegacySnapshotRecoveryParams,
) -> Result<()> {
    let snapshot = validate_marker_ownership(grant, bytes, false)?;
    let recovery = snapshot.legacy_recovery.as_ref()
        .context("legacy recovery requires its own marker binding")?;
    anyhow::ensure!(
        snapshot.operation_id.is_empty() && snapshot.request_fingerprint.is_empty()
            && snapshot.sync_generation == 0 && snapshot.snapshot_id.is_empty()
            && params.schema_version == MAINTENANCE_CONTROL_SCHEMA_V
            && canonical_operation(&params.recovery_operation_id)
            && digest_shape(&params.journal_sha256)
            && legacy_fingerprint(params)? == params.request_fingerprint
            && params.binding.installation_root == grant.installation_root
            && params.binding.profile == grant.profile
            && params.binding.launch_id == grant.instance_id
            && params.binding.expected_account_id.as_deref() == Some(grant.account_id.as_str())
            && recovery.account_id == grant.account_id
            && recovery.recovery_operation_id == params.recovery_operation_id
            && recovery.request_fingerprint == params.request_fingerprint
            && recovery.journal_sha256 == params.journal_sha256,
        "legacy recovery differs from its exact capability or impersonates an original operation"
    );
    Ok(())
}

pub(crate) fn authorize_legacy_recovery(
    params: &respire_protocol::maintenance_control::LegacySnapshotRecoveryParams,
) -> Result<()> {
    let grant = crate::startup_identity::consumed_control_grant()?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    crate::startup_identity::verify_managed_boot()?;
    let bytes = crate::startup_identity::read_private_file(&marker_path(grant))?
        .context("legacy snapshot maintenance marker disappeared")?;
    validate_legacy_launch(grant, &bytes, params)
}

pub(crate) fn dispatch_legacy_recovery(
    params: &respire_protocol::maintenance_control::LegacySnapshotRecoveryParams,
    account: &str,
) -> Result<respire_protocol::maintenance_control::LegacySnapshotRecoveryResult> {
    use respire_protocol::maintenance_control::{LegacySnapshotRecoveryAction, LegacySnapshotRecoveryResult};
    use respire_protocol::maintenance_control::MaintenanceState;
    authorize_legacy_recovery(params)?;
    let grant = crate::startup_identity::consumed_control_grant()?;
    anyhow::ensure!(!account.is_empty() && hash(account.as_bytes()) == grant.account_id,
        "legacy snapshot account differs from the authorized grant");
    // Keep the same installation-before-profile lock order as original snapshot maintenance.
    let _owner = respire_app::lock::LibraryLock::acquire(
        &grant.installation_root.join("runtime"), std::time::Duration::ZERO)?;
    let ownership = respire_app::lock::LibraryLock::acquire(
        &grant.profile, std::time::Duration::from_secs(15))?;
    authorize_legacy_recovery(params)?;
    let operation = respire_app::snapshot::SnapshotOperation {
        operation_id: params.recovery_operation_id.clone(),
        request_fingerprint: params.request_fingerprint.clone(),
    };
    let mut status = respire_app::snapshot::legacy_recovery_status(
        &grant.profile, account, &operation, &params.journal_sha256)?;
    // A recorded recovery is query-only even when the caller repeats Recover.
    if status.is_none() && params.action == LegacySnapshotRecoveryAction::Recover {
        let description = respire_app::snapshot::describe_legacy_recovery(&grant.profile, account)?;
        anyhow::ensure!(description.journal_sha256 == params.journal_sha256
            && description.account_fingerprint == grant.account_id,
            "legacy journal differs from the approved recovery description");
        respire_app::service::ensure_writable()?;
        authorize_legacy_recovery(params)?;
        status = Some(respire_app::snapshot::recover_legacy_locked(
            &grant.profile, account, &description, &operation, &ownership)?);
    }
    authorize_legacy_recovery(params)?;
    let (state, cleanup_pending, cleanup_warning) = match status {
        Some(status) => (legacy_stage(&status.phase)?, status.cleanup_pending, status.cleanup_warning),
        None => (MaintenanceState::Unknown, false, None),
    };
    Ok(LegacySnapshotRecoveryResult {
        recovery_operation_id: params.recovery_operation_id.clone(),
        request_fingerprint: params.request_fingerprint.clone(),
        journal_sha256: params.journal_sha256.clone(),
        state,
        cleanup_pending,
        cleanup_warning,
    })
}

fn legacy_stage(phase: &str) -> Result<respire_protocol::maintenance_control::MaintenanceState> {
    use respire_protocol::maintenance_control::MaintenanceState;
    match phase {
        "prepared" => Ok(MaintenanceState::Prepared),
        "installed" => Ok(MaintenanceState::Installed),
        "verified" => Ok(MaintenanceState::Verified),
        "released" => Ok(MaintenanceState::Released),
        "rolled_back" => Ok(MaintenanceState::RolledBack),
        _ => anyhow::bail!("unsupported legacy snapshot journal stage"),
    }
}

fn marker_path(grant: &HostControlLaunchGrant) -> PathBuf {
    grant
        .installation_root
        .join("runtime/autostart/maintenance.json")
}

/// Called by startup's existing maintenance check for the Paused snapshot case
/// only. Normal grant image/registration/context checks must still run there.
pub(crate) fn validate_offline_launch(grant: &HostControlLaunchGrant, bytes: &[u8]) -> Result<()> {
    validate_marker(grant, bytes, false).map(|_| ())
}

/// Distinct startup role: metadata finalization, never offline DB replacement.
pub(crate) fn validate_finalize_launch(grant: &HostControlLaunchGrant, bytes: &[u8]) -> Result<()> {
    let snapshot = validate_marker(grant, bytes, true)?;
    verify_runtime(grant, &snapshot)
}

fn validate_marker(
    grant: &HostControlLaunchGrant,
    bytes: &[u8],
    finalize: bool,
) -> Result<SnapshotBinding> {
    let snapshot = validate_marker_ownership(grant, bytes, finalize)?;
    anyhow::ensure!(snapshot.legacy_recovery.is_none(),
        "legacy recovery cannot impersonate an original snapshot operation");
    let params = Params {
        schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
        binding: respire_protocol::ControlBinding {
            installation_root: grant.installation_root.clone(),
            profile: grant.profile.clone(),
            launch_id: grant.instance_id.clone(),
            expected_account_id: Some(snapshot.account_id.clone()),
        },
        operation_id: snapshot.operation_id.clone(),
        request_fingerprint: snapshot.request_fingerprint.clone(),
        sync_generation: snapshot.sync_generation,
        snapshot_id: snapshot.snapshot_id.clone(),
        action: Action::Query,
        material: None,
    };
    anyhow::ensure!(
        uuid::Uuid::parse_str(&snapshot.operation_id)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == snapshot.operation_id)
            && snapshot.snapshot_id.len() == 32
            && snapshot.snapshot_id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && crate::maintenance_control::fingerprint(&params)? == snapshot.request_fingerprint,
        "invalid snapshot marker operation identity"
    );
    Ok(snapshot)
}

fn validate_marker_ownership(
    grant: &HostControlLaunchGrant,
    bytes: &[u8],
    finalize: bool,
) -> Result<SnapshotBinding> {
    anyhow::ensure!(
        bytes.len() <= 16 * 1024,
        "snapshot maintenance marker too large"
    );
    let marker: serde_json::Value = serde_json::from_slice(bytes)?;
    let binding = grant
        .maintenance
        .as_ref()
        .context("snapshot maintenance grant missing")?;
    anyhow::ensure!(
        hash(bytes) == binding.marker_sha256
            && marker["schema"] == 1
            && marker["phase"] == if finalize { "Verified" } else { "Paused" }
            && marker["id"] == binding.coordinator_id
            && marker["owner_pid"] == binding.controller_pid
            && marker["owner_birth"] == binding.controller_birth
            && marker["expected_version"] == binding.issuer_cli_version
            && marker["expected_profile"].as_str().map(Path::new) == Some(grant.profile.as_path())
            && marker["token"]
                .as_str()
                .is_some_and(|token| uuid::Uuid::parse_str(token).is_ok())
            && binding.controller_pid == grant.supervisor_pid
            && binding.controller_birth == grant.supervisor_birth
            && binding.controller_pid > 0
            && !binding.controller_birth.is_empty()
            && !binding.issuer_cli_version.is_empty()
            && crate::startup_identity::process_birth(binding.controller_pid)?.as_deref()
                == Some(binding.controller_birth.as_str()),
        "snapshot requires the exact live controller's role-specific marker"
    );
    let snapshot: SnapshotBinding = serde_json::from_value(marker["snapshot"].clone())?;
    anyhow::ensure!(
        if finalize {
            snapshot.verified_runtime.is_some()
        } else {
            snapshot.verified_runtime.is_none()
        },
        "snapshot marker role mismatch"
    );
    anyhow::ensure!(
        snapshot.account_id == grant.account_id
            && matches!(
                (&snapshot.drain, &snapshot.offline),
                (Some(_), None) | (None, Some(_))
            ),
        "snapshot requires exactly one drain or offline ownership proof"
    );
    if let Some(proof) = snapshot.offline.as_ref() {
        anyhow::ensure!(
            snapshot.drained_birth.is_empty(),
            "offline proof cannot impersonate drain"
        );
        verify_offline_proof(grant, proof, !finalize)?;
    }
    if let Some(receipt) = snapshot.drain.as_ref() {
        let prefix = if cfg!(windows) {
            "windows-filetime-"
        } else if cfg!(target_os = "linux") {
            "linux-starttime-"
        } else {
            "darwin-lstart-"
        };
        anyhow::ensure!(
            receipt.schema_version == 1
                && receipt.protocol_version == grant.protocol_version
                && receipt.state == RuntimeStopOutcome::Drained
                && receipt.unfinished.is_empty()
                && receipt.pid > 0
                && receipt.pid != grant.supervisor_pid
                && receipt.pid != std::process::id()
                && receipt.generation > 0
                && receipt.installation_id == grant.installation_id
                && receipt.account_id == grant.account_id
                && snapshot.account_id == grant.account_id
                && !receipt.operation_id.is_empty()
                && !receipt.image_path.is_empty()
                && !receipt.image_sha256.is_empty()
                && !receipt.runtime_version.is_empty()
                && !snapshot.drained_birth.is_empty()
                && receipt.process_start_id == format!("{prefix}{}", snapshot.drained_birth)
                && !receipt.instance_id.is_empty()
                && receipt.instance_id.len() <= 128
                && receipt
                    .instance_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && crate::startup_identity::process_birth(receipt.pid)?.as_deref()
                    != Some(snapshot.drained_birth.as_str()),
            "snapshot has no complete authenticated dead-owner drain"
        );
        let receipt_path = grant
            .installation_root
            .join("runtime")
            .join(format!("stop-{}.json", receipt.instance_id));
        let stored = crate::startup_identity::read_private_file(&receipt_path)?
            .context("snapshot final stop receipt missing")?;
        let stored: RuntimeStopReceipt = serde_json::from_slice(&stored)?;
        anyhow::ensure!(
            stored == *receipt,
            "snapshot stop receipt differs from published drain"
        );
    }
    Ok(snapshot)
}

mod offline;
use offline::{verify_offline_proof, verify_runtime};
#[cfg(test)]
use offline::offline_endpoint_matches;
pub(crate) use offline::{
    authorize_offline,
    dispatch_offline,
    authorize_finalize,
    dispatch_finalize,
};

#[cfg(test)]
mod tests;
