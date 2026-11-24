//! Durable host maintenance ownership shared by the CLI and native Runtime.
//!
//! The embedding performs real service/process operations. This module owns the
//! marker, phase transitions, drain-before-auth proof and live target checks.
use anyhow::{Context, Result};
use respire_client_host::lock::LibraryLock;
use respire_commands::net_rpc::Health;
use respire_protocol::host_control::{HostAuthAccountOutcome, HostAuthAccountResult};
use respire_protocol::{HostAuthIdentity, HostControlMaintenanceBinding};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::launch::{hash, process_birth};
use crate::VerifiedRuntime;

mod accounts;
pub(crate) use accounts::{finish_account_routing, pending_account, prepare_keygen_account, lookup_keygen_account};
pub use accounts::{KeygenAccountTransfer, KeygenAccountAdmission};
pub(crate) mod private_io;
mod takeover;
use private_io::{marker_bytes, private_write, registration_digest};
pub use takeover::takeover_lock;

pub(crate) const MAINTENANCE_MARKER_MAX_BYTES: usize = 16 * 1024;
thread_local! {
    static ACTIVE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Observed and ownership-checked registration, never a request-side claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationMode {
    Unregistered,
    Stopped { coordinator_id: String },
    Managed { coordinator_id: String },
}

impl RegistrationMode {
    fn coordinator_id(&self, root: &Path) -> String {
        match self {
            Self::Unregistered => format!("host-{}", hash(root.display().to_string().as_bytes())),
            Self::Stopped { coordinator_id } | Self::Managed { coordinator_id } => {
                coordinator_id.clone()
            }
        }
    }

    fn managed_id(&self) -> Option<&str> {
        match self {
            Self::Managed { coordinator_id } => Some(coordinator_id),
            _ => None,
        }
    }
}

/// Host-only operations, implemented against the embedding's actual lifecycle.
/// A Runtime without service-manager support must reject an existing registration
/// from `inspect_registration`; reporting it as unregistered is not permitted.
pub trait HostLifecycle {
    fn require_account_write(&self) -> Result<()>;
    fn verified_runtime(&self) -> Result<VerifiedRuntime>;
    fn issuer_version(&self) -> &str;
    fn inspect_registration(&self, root: &Path) -> Result<RegistrationMode>;
    /// Persist the checked existing managed registration only. Never register it.
    fn set_maintenance(&self, root: &Path, enabled: bool) -> Result<()>;
    fn probe_runtime(&self) -> Result<Option<Health>>;
    fn endpoint_path(&self) -> PathBuf;
    /// Verify exact ownership, wait for final drain receipt and process exit,
    /// acquire the released library lock, remove only its locator, then stop and
    /// re-inspect a managed supervisor. Never kill an unverified owner.
    fn drain_runtime(&self, health: &Health) -> Result<()>;
    /// Launch the checked installed image with the inherited serving grant.
    /// Return only after independently observed readiness, or an error. No
    /// installer, registration creation or unverified detached fallback belongs here.
    fn start_account_runtime(&self, launch: &LaunchContext) -> Result<()>;
    /// Called only after independent commit verification and durable maintenance
    /// completion, while this coordinator still holds installation takeover.
    /// Ordinary CLI hosts keep the verified serving instance. A legacy desktop
    /// host must drain that exact strict instance and verify its fresh host lane.
    fn finish_account_handoff(&self, _receipt: &HostAuthAccountResult) -> Result<()> {
        Ok(())
    }
    fn finish_key_handoff(
        &self,
        _profile: &Path,
        _identity: &HostAuthIdentity,
        _serving: bool,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryMode {
    Fresh,
    QueryOriginal,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Phase {
    Paused,
    Starting,
    Verified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchPurpose {
    Account,
    AccountComplete,
    Serving,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    id: String,
    token: String,
    phase: Phase,
    expected_profile: Option<PathBuf>,
    expected_version: String,
    owner_pid: u32,
    owner_birth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_config: Option<Value>,
}

/// A non-deserializable capability minted only from this controller's exact
/// marker. It never exports the durable reusable maintenance token.
pub struct OwnedMaintenanceBinding {
    root: PathBuf,
    profile: PathBuf,
    marker: Marker,
    maintenance: HostControlMaintenanceBinding,
    registration: RegistrationMode,
    registration_sha256: Option<String>,
    purpose: LaunchPurpose,
    host: Arc<dyn HostLifecycle>,
}

pub type LaunchBinding = OwnedMaintenanceBinding;

/// One attempt to launch a replacement from a real Starting owner. It cannot be
/// cloned, deserialized, or retried after an uncertain launch.
pub struct LaunchContext {
    binding: OwnedMaintenanceBinding,
    identity: HostAuthIdentity,
    claimed: Cell<bool>,
}

impl LaunchContext {
    pub fn installation_root(&self) -> &Path {
        self.binding.installation_root()
    }
    pub fn profile(&self) -> &Path {
        self.binding.profile()
    }
    pub(crate) fn claim(&self) -> Result<(&OwnedMaintenanceBinding, &HostAuthIdentity)> {
        anyhow::ensure!(
            !self.claimed.replace(true),
            "replacement launch was already attempted; query its outcome"
        );
        self.binding.revalidate()?;
        anyhow::ensure!(
            self.binding.phase() == Phase::Starting
                && self.binding.purpose() == LaunchPurpose::Serving
                && crate::identity(self.profile())? == self.identity,
            "replacement launch owner or identity changed"
        );
        ensure_no_owner(self.binding.host.as_ref())?;
        Ok((&self.binding, &self.identity))
    }
}

impl OwnedMaintenanceBinding {
    pub fn installation_root(&self) -> &Path {
        &self.root
    }
    pub fn profile(&self) -> &Path {
        &self.profile
    }
    pub fn maintenance(&self) -> &HostControlMaintenanceBinding {
        &self.maintenance
    }
    pub fn managed_id(&self) -> Option<&str> {
        self.registration.managed_id()
    }
    pub fn registration_sha256(&self) -> Option<&str> {
        self.registration_sha256.as_deref()
    }
    pub fn phase(&self) -> Phase {
        self.marker.phase
    }
    pub fn purpose(&self) -> LaunchPurpose {
        self.purpose
    }

    pub fn revalidate(&self) -> Result<()> {
        verify_selection(&self.root, &self.profile)?;
        ensure_registration(self.host.as_ref(), &self.root, &self.registration)?;
        let bytes = marker_bytes(&self.root)?.context("maintenance grant marker disappeared")?;
        let actual = parse_marker(&bytes)?;
        ensure_exact_owner(
            &actual,
            &self.marker,
            &bytes,
            &self.maintenance.marker_sha256,
        )?;
        ensure_purpose(self.purpose, actual.phase)?;
        anyhow::ensure!(
            actual.expected_profile.as_deref() == Some(self.profile.as_path())
                && actual.expected_version == self.host.issuer_version(),
            "maintenance launch profile or issuer changed"
        );
        anyhow::ensure!(
            registration_digest(&self.root, &self.registration)?.as_deref()
                == self.registration_sha256.as_deref(),
            "maintenance registration changed before launch"
        );
        if self.purpose == LaunchPurpose::Account {
            ensure_no_owner(self.host.as_ref())?;
        }
        Ok(())
    }
}

pub struct Maintenance {
    root: PathBuf,
    host: Arc<dyn HostLifecycle>,
    registration: RegistrationMode,
    marker: Marker,
    marker_digest: String,
    drained: bool,
    original_health: Option<Health>,
    account_origin: Option<accounts::AccountOrigin>,
    account_record: Option<accounts::AccountRecord>,
    completed: bool,
}

// Refusal fence only: neither presence nor a parseable pending DTO grants ownership.
fn reject_offline_keygen(root: &Path) -> Result<()> {
    let path = respire_client_host::config::normalize_control_path(
        &root.join("runtime/autostart/offline-keygen.json"))?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("offline keygen is pending; use its verified original-operation handoff"),
    }
}

fn account_owner_token(
    recovery: RecoveryMode,
    previous: Option<&Marker>,
    retained: Option<&str>,
) -> Result<String> {
    match retained {
        Some(token) => {
            let parsed = uuid::Uuid::parse_str(token)?;
            anyhow::ensure!(
                (recovery == RecoveryMode::QueryOriginal || previous.is_none())
                    && parsed.get_version_num() == 4
                    && parsed.to_string() == token
                    && previous.is_none_or(|previous| previous.token == token),
                "recovery cannot replace its original maintenance token"
            );
            Ok(token.to_owned())
        }
        None => {
            anyhow::ensure!(
                recovery == RecoveryMode::Fresh,
                "account recovery requires the verified original operation token"
            );
            Ok(uuid::Uuid::new_v4().to_string())
        }
    }
}

include!("maintenance/ownership.rs");

include!("maintenance/restore.rs");

include!("maintenance/owner_verification.rs");

#[cfg(test)]
mod tests;
