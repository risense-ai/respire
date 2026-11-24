//! Snapshot-only control client. Lifecycle/drain and maintenance grants belong
//! to the coordinator; this module never opens the business store.
use std::path::Path;
use std::time::Instant;

use super::lifecycle::MAINTENANCE_MARKER_MAX_BYTES;
use anyhow::{Context, Result};
use respire_client_host::lock::LibraryLock;
use respire_commands::output::{ResultEnvelope, Status};
use respire_protocol::maintenance_control::{
    MaintenanceAction as Action, MaintenanceControlParams as Params,
    MaintenanceControlResult as Receipt, MaintenanceState as State, MAINTENANCE_CONTROL_METHOD,
    MAINTENANCE_CONTROL_SCHEMA_V,
};
use respire_protocol::{ControlBinding, HostControlLaunchGrant, RpcRequest, RpcResponse};
use sha2::{Digest, Sha256};
use respire_protocol::maintenance_control::{LegacySnapshotRecoveryParams as LegacyParams, LegacySnapshotRecoveryResult as LegacyReceipt,
    LEGACY_SNAPSHOT_RECOVERY_METHOD};
pub(crate) use respire_protocol::maintenance_control::LegacySnapshotDescriptor;

fn restore_material(profile: &Path) -> Result<respire_protocol::host_control::HostAuthSessionMaterial> {
    use respire_client_host::host_credentials::{read_export_factors, read_stored_session_credentials};
    use respire_protocol::host_control::{HostAuthSessionMaterial, HostAuthVaultFactors as Factors};
    let path = profile.join("session.json");
    let metadata = std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink()
        && metadata.len() <= 65536, "snapshot session must be a bounded regular file");
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?.take(65537).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 65536, "snapshot session is too large");
    let session: serde_json::Value = serde_json::from_slice(&bytes)?;
    let version = match session.get("vault_version") {
        None => 1,
        Some(value) => value.as_u64().context("invalid snapshot vault version")?,
    };
    anyhow::ensure!((1..=4).contains(&version), "unsupported snapshot vault version");
    let (super_password, legacy_secret) = read_export_factors(&session)?;
    let factors = match version {
        1 => Factors::V1 {
            password: read_stored_session_credentials(&session)?.1.context("snapshot original password is unavailable")?,
            account_secret: legacy_secret.context("snapshot Account Secret is unavailable")?,
        },
        2 => Factors::V2 { super_password: super_password.context("snapshot super password is unavailable")? },
        3 => Factors::V3 {
            super_password: super_password.context("snapshot super password is unavailable")?,
            secret_key: legacy_secret.context("snapshot Secret Key is unavailable")?,
        },
        4 => Factors::V4 { super_password: super_password.context("snapshot super password is unavailable")? },
        _ => anyhow::bail!("unsupported snapshot vault version"),
    };
    anyhow::ensure!(std::fs::read(&path)? == bytes, "snapshot session changed while reading host factors");
    Ok(HostAuthSessionMaterial { session_sha256: format!("{:x}", Sha256::digest(&bytes)), factors })
}

fn digest_shape(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn validate_legacy_descriptor(descriptor: &LegacySnapshotDescriptor) -> Result<()> {
    anyhow::ensure!(digest_shape(&descriptor.journal_sha256) && digest_shape(&descriptor.account_id)
        && descriptor.stage != State::Unknown, "invalid legacy snapshot descriptor");
    Ok(())
}

pub(crate) fn legacy_fingerprint(params: &LegacyParams) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&(
        "snapshot-legacy-recovery-v1", &params.binding.installation_root,
        &params.binding.profile, &params.binding.expected_account_id,
        &params.recovery_operation_id, &params.journal_sha256))?)))
}

/// One fixed exchange. A failed response never starts another recovery operation.
pub(crate) fn dispatch_legacy_snapshot(
    takeover: &LibraryLock,
    grant: &HostControlLaunchGrant,
    params: &LegacyParams,
) -> Result<LegacyReceipt> {
    crate::runtime_policy::require_host("legacy snapshot recovery")?;
    let operation = uuid::Uuid::parse_str(&params.recovery_operation_id)?;
    anyhow::ensure!(params.schema_version == MAINTENANCE_CONTROL_SCHEMA_V
        && operation.get_version_num() == 4 && operation.to_string() == params.recovery_operation_id
        && digest_shape(&params.journal_sha256)
        && params.request_fingerprint == legacy_fingerprint(params)?
        && params.binding.installation_root == grant.installation_root
        && params.binding.profile == grant.profile
        && params.binding.expected_account_id.as_deref() == Some(grant.account_id.as_str())
        && params.binding.launch_id == grant.instance_id
        && takeover.protects(&grant.installation_root.join("runtime/takeover"))?,
        "legacy snapshot request binding mismatch");
    let path = grant.installation_root.join("runtime/autostart/maintenance.json");
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAINTENANCE_MARKER_MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    let marker: serde_json::Value = serde_json::from_slice(&bytes)?;
    let capability = grant.maintenance.as_ref().context("legacy snapshot maintenance grant missing")?;
    let bound = &marker["snapshot"]["legacy_recovery"];
    anyhow::ensure!(bytes.len() <= MAINTENANCE_MARKER_MAX_BYTES
        && format!("{:x}", Sha256::digest(&bytes)) == capability.marker_sha256
        && marker["phase"] == "Paused" && marker["id"] == capability.coordinator_id
        && marker["owner_pid"] == capability.controller_pid
        && marker["owner_birth"] == capability.controller_birth
        && marker["expected_version"] == capability.issuer_cli_version
        && marker["expected_profile"].as_str().map(Path::new) == Some(grant.profile.as_path())
        && capability.controller_pid == std::process::id()
        && capability.controller_pid == grant.supervisor_pid
        && capability.controller_birth == grant.supervisor_birth
        && bound["recovery_operation_id"] == params.recovery_operation_id
        && bound["request_fingerprint"] == params.request_fingerprint
        && bound["journal_sha256"] == params.journal_sha256
        && bound["account_id"] == grant.account_id
        && marker["snapshot"]["verified_runtime"].is_null(),
        "legacy snapshot helper requires its exact Paused recovery marker");
    let request = RpcRequest { v: respire_commands::rpc_client::PROTOCOL_V,
        id: uuid::Uuid::new_v4().to_string(), method: LEGACY_SNAPSHOT_RECOVERY_METHOD.to_owned(),
        args: vec![serde_json::to_string(params)?], scope: None,
        operation_id: Some(params.recovery_operation_id.clone()),
        idempotency_key: Some(params.recovery_operation_id.clone()), parent_operation_id: None,
        queue_wait_ms: None, lifecycle: None };
    let result = super::supervise::exchange_snapshot_control(&grant.image_path, grant, &request,
        Instant::now() + std::time::Duration::from_secs(60), |response, pid| {
        anyhow::ensure!(response.v == request.v && response.id == request.id
            && response.bin == "rsrs-runtime" && response.pid == Some(pid)
            && response.operation_id == request.operation_id && response.idempotency_key == request.idempotency_key
            && response.scope.is_none() && response.web_url.is_none()
            && response.data_dir.as_deref().map(Path::new) == Some(grant.profile.as_path()),
            "legacy snapshot response identity mismatch");
        if !response.ok || response.exit != 0 || response.code.is_some() || response.error.is_some() {
            return Err(SnapshotRemoteFailure { code: response.code, exit: response.exit,
                validation_reason: None }.into());
        }
        let envelope = response.envelope.context("legacy snapshot receipt missing")?;
        let receipt: LegacyReceipt = serde_json::from_value(envelope.summary.clone())?;
        anyhow::ensure!(envelope.command == "snapshot"
            && receipt.recovery_operation_id == params.recovery_operation_id
            && receipt.request_fingerprint == params.request_fingerprint
            && receipt.journal_sha256 == params.journal_sha256
            && (receipt.cleanup_warning.is_none() || receipt.cleanup_pending)
            && (receipt.state != State::Unknown || (!receipt.cleanup_pending && receipt.cleanup_warning.is_none()))
            && if receipt.cleanup_pending { matches!(envelope.status, Status::Warn)
                && (envelope.errors.is_empty() || envelope.errors.as_slice()
                    == receipt.cleanup_warning.as_ref().map(std::slice::from_ref).unwrap_or_default())
            } else { matches!(envelope.status, Status::Ok) && envelope.errors.is_empty() },
            "legacy snapshot receipt mismatch");
        for (name, value, ok) in [("control_ready", "true", true), ("store_ready", "false", false)] {
            let mut items = envelope.items.iter().filter(|item| item.name == name);
            anyhow::ensure!(items.next().is_some_and(|item| item.value == value
                && if ok { matches!(item.status, Status::Ok) } else { matches!(item.status, Status::Skip) })
                && items.next().is_none(), "legacy snapshot readiness mismatch");
        }
        Ok(envelope)
    }).context("snapshot_outcome_unknown; query the same recovery operation; no recovery replayed")?;
    serde_json::from_value(result.summary).context("invalid legacy snapshot result")
}

/// Keep this original request identity across helper exits, restart and query.
/// Reconstructing it never authorizes replay; the Runtime journal decides that.
pub(crate) struct SnapshotOperation {
    original: Params,
}

#[derive(Debug)]
pub(crate) struct SnapshotOutcomeUnknown {
    pub operation_id: String,
    pub request_fingerprint: String,
    pub snapshot_id: String,
    pub sync_generation: usize,
}

impl std::fmt::Display for SnapshotOutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "snapshot_outcome_unknown operation_id={}; query the original operation before replay or compensation", self.operation_id)
    }
}

impl std::error::Error for SnapshotOutcomeUnknown {}

/// Preserve Runtime error codes for the parent's fixed rejection mapping.
/// A code alone is not permission to replay; unconfirmed writes remain unknown.
#[derive(Debug)]
pub(crate) struct SnapshotRemoteFailure {
    pub code: Option<String>,
    pub exit: i32,
    validation_reason: Option<&'static str>,
}

impl std::fmt::Display for SnapshotRemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("snapshot Runtime did not confirm a receipt")?;
        if let Some(reason) = self.validation_reason {
            write!(f, "; validation cause: {reason}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotRemoteFailure {}

include!("snapshot_control/operation.rs");

#[cfg(test)]
#[path = "control_tests/snapshot_validation.rs"]
mod validation_tests;
