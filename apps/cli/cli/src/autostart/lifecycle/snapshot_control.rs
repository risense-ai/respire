//! Snapshot capability publication follows the existing authenticated drain.
use super::super::snapshot_control::{SnapshotOperation, dispatch_legacy_snapshot, legacy_fingerprint, validate_legacy_descriptor};
use respire_protocol::maintenance_control::{LegacySnapshotRecoveryAction as LegacyAction,
    LegacySnapshotRecoveryParams as LegacyParams, LegacySnapshotRecoveryResult as LegacyReceipt};
use respire_protocol::ControlBinding;
use super::*;
use respire_client_host::lock::LibraryLock;
use respire_commands::output::{ResultEnvelope, Status};
use respire_protocol::maintenance_control::{MaintenanceAction, MaintenanceControlResult};
use respire_protocol::maintenance_control::{
    MaintenanceControlParams, MaintenanceState, MAINTENANCE_CONTROL_SCHEMA_V,
};
use respire_protocol::{HostControlLaunchGrant, RuntimeStopReceipt};
use std::io::Read;

/// Optional field `snapshot` in the existing maintenance marker, not a ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotMarkerBinding {
    pub operation_id: String,
    pub request_fingerprint: String,
    pub sync_generation: usize,
    pub snapshot_id: String,
    pub account_id: String,
    pub drained_birth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain: Option<RuntimeStopReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline: Option<SnapshotOfflineProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_runtime: Option<SnapshotVerifiedRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_recovery: Option<LegacySnapshotRecoveryBinding>,
}

pub(crate) use respire_protocol::maintenance_control::LegacySnapshotRecoveryBinding;

/// Absence evidence is not a drain receipt. Installation and library locks
/// serialize its publication; Runtime reacquires both before offline execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotOfflineProof {
    pub endpoint_sha256: Option<String>,
    pub endpoint_pid: Option<u32>,
    pub prior_runtime: Option<SnapshotVerifiedRuntime>,
}

/// Frozen only after the coordinator independently verifies the live target.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotVerifiedRuntime {
    pub scope: respire_protocol::RuntimeRequestScope,
    pub pid: u32,
    pub process_start_id: String,
    pub image_path: PathBuf,
    pub image_sha256: String,
    pub runtime_version: String,
}

impl SnapshotOfflineProof {
    fn capture(root: &Path, prior_runtime: Option<SnapshotVerifiedRuntime>) -> Result<Self> {
        let path = root.join("runtime/endpoint.json");
        let (endpoint_sha256, endpoint_pid) = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() <= 16 * 1024,
                    "snapshot endpoint is not a bounded regular file"
                );
                let mut bytes = Vec::new();
                fs::File::open(&path)?
                    .take(16 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= 16 * 1024, "snapshot endpoint too large");
                let endpoint: Value = serde_json::from_slice(&bytes)?;
                let pid = endpoint["pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                    .filter(|pid| *pid > 0)
                    .context("snapshot endpoint owner PID missing")?;
                // A legacy locator has no birth. Any live/reused PID is ambiguous.
                anyhow::ensure!(
                    manager::process_birth(pid)?.is_none(),
                    "snapshot owner is alive but health is unavailable; drain proof required"
                );
                (Some(hash(&bytes)), Some(pid))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(error) => return Err(error.into()),
        };
        if let Some(prior) = prior_runtime.as_ref() {
            anyhow::ensure!(
                prior.pid > 0 && !prior.process_start_id.is_empty(),
                "snapshot previous Runtime identity incomplete"
            );
            let alive = manager::process_birth(prior.pid)?.is_some_and(|birth| {
                prior.process_start_id == format!("{}{birth}", snapshot_birth_prefix())
            });
            anyhow::ensure!(
                !alive,
                "snapshot previous Runtime is alive without health; no offline recovery"
            );
        }
        Ok(Self {
            endpoint_sha256,
            endpoint_pid,
            prior_runtime,
        })
    }

    fn verify(&self, root: &Path) -> Result<()> {
        let current = Self::capture(root, self.prior_runtime.clone())?;
        anyhow::ensure!(
            current.endpoint_sha256 == self.endpoint_sha256
                && current.endpoint_pid == self.endpoint_pid,
            "snapshot endpoint changed since offline fencing"
        );
        Ok(())
    }
}

fn normal_marker(operation: &SnapshotOperation) -> Result<SnapshotMarkerBinding> {
    let original = operation.original();
    Ok(SnapshotMarkerBinding {
        operation_id: original.operation_id.clone(),
        request_fingerprint: original.request_fingerprint.clone(),
        sync_generation: original.sync_generation,
        snapshot_id: original.snapshot_id.clone(),
        account_id: original.binding.expected_account_id.clone().context("snapshot account missing")?,
        drained_birth: String::new(), drain: None, offline: None, verified_runtime: None,
        legacy_recovery: None,
    })
}

fn same_snapshot_intent(left: &SnapshotMarkerBinding, right: &SnapshotMarkerBinding) -> bool {
    left.operation_id == right.operation_id && left.request_fingerprint == right.request_fingerprint
        && left.sync_generation == right.sync_generation && left.snapshot_id == right.snapshot_id
        && left.account_id == right.account_id && left.legacy_recovery == right.legacy_recovery
}

include!("snapshot_control/authority.rs");

include!("snapshot_control/verification.rs");

include!("snapshot_control/legacy_recovery.rs");

include!("snapshot_control/recovery.rs");

include!("snapshot_control/commands.rs");

#[cfg(test)]
#[path = "../control_tests/snapshot_offline_proof.rs"]
mod offline_proof_tests;
