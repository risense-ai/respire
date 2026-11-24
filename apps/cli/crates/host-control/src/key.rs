//! Fixed key control. Passwords and issued codes never enter recovery metadata.
//! Extracted from CLI 25ef33a23c87b0c00f1430c23d2c5b6c629ec13d.
//! Host ownership is shared; key execution remains in the private Runtime helper.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::maintenance::{HostLifecycle, Maintenance, RecoveryMode, RegistrationMode};
use crate::VerifiedRuntime;
use anyhow::{Context, Result};
use respire_client_host::lock::LibraryLock;
use respire_commands::output::{Item, ResultEnvelope, Status};
use respire_protocol::key_control::*;
use respire_protocol::{ControlBinding, HostAuthIdentity, RpcRequest, RpcResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
mod material;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Original {
    schema_version: u32,
    root: PathBuf,
    profile: PathBuf,
    operation_id: String,
    key: String,
    source_identity: HostAuthIdentity,
    operation: HostKeyOperation,
    token: String,
    coordinator: String,
    was_serving: bool,
    store_ready: bool,
    sent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate: Option<Value>,
    #[serde(default)]
    rolled_back: bool,
}

pub struct KeyRecoveryRequired {
    pub operation_id: String,
    pub source_profile: PathBuf,
}
impl std::fmt::Display for KeyRecoveryRequired {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "key_outcome_unknown operation_id={}; query this original operation; do not repeat the key operation", self.operation_id)
    }
}
impl std::fmt::Debug for KeyRecoveryRequired {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, out)
    }
}
impl std::error::Error for KeyRecoveryRequired {}

impl Original {
    fn recovery(&self) -> KeyRecoveryRequired {
        KeyRecoveryRequired {
            operation_id: self.operation_id.clone(),
            source_profile: self.profile.clone(),
        }
    }
    fn fingerprint(&self) -> Result<String> {
        Ok(crate::launch::hash(&serde_json::to_vec(
            &json!({"schema_version":HOST_KEY_SCHEMA_V,
            "method":"control.key", "root":self.root,"profile":self.profile,
            "source_identity":self.source_identity,"operation":self.operation}),
        )?))
    }
}

enum Action<'a> {
    Reset {
        operation: HostKeyOperation,
        input: HostKeyInput,
        resume: Option<&'a HostKeyResult>,
    },
    Query(&'a str),
    Rollback(&'a str),
}

/// Trusted host facade. Secret inputs are never journaled or passed in argv/env.
pub struct KeyCoordinator {
    host: Arc<dyn HostLifecycle>,
}

impl KeyCoordinator {
    pub fn new(host: Arc<dyn HostLifecycle>) -> Self {
        Self { host }
    }

    pub fn super_reset(&self, super_password: Option<&str>) -> Result<ResultEnvelope> {
        self.super_reset_step(
            None,
            HostKeyInput {
                password: None,
                super_password: super_password.map(str::to_owned),
                ..HostKeyInput::default()
            },
            None,
        )
    }

    pub fn super_reset_step(
        &self,
        addr: Option<&str>,
        input: HostKeyInput,
        resume: Option<&HostKeyResult>,
    ) -> Result<ResultEnvelope> {
        control_key(
            Arc::clone(&self.host),
            Action::Reset {
                operation: HostKeyOperation::SuperReset {
                    addr: addr.map(str::to_owned),
                },
                input,
                resume,
            },
        )
    }

    /// Verify archived custody locally, without replaying Runtime or the current account identity.
    pub fn verify_custody(operation_id: &str, source: &Path, expected: &HostKeyInput) -> Result<ResultEnvelope> {
        anyhow::ensure!(canonical(operation_id), "invalid custody operation ID");
        let (root, _) = respire_client_host::config::control_roots()?;
        let root = root.canonicalize()?;
        let source = respire_client_host::config::normalize_control_path(source)?.canonicalize()?;
        let takeover = crate::maintenance::takeover_lock()?;
        anyhow::ensure!(takeover.protects(&root.join("runtime/takeover"))?, "custody takeover root changed");
        verify_archived_custody(&root, &source, operation_id, expected)
    }

    /// Query only the original operation. This never admits an unsent operation.
    pub fn result(&self, operation_id: &str) -> Result<ResultEnvelope> {
        control_key(Arc::clone(&self.host), Action::Query(operation_id))
    }

    pub fn result_at(&self, operation_id: &str, source: &Path) -> Result<ResultEnvelope> {
        control_key_inner(Arc::clone(&self.host), Action::Query(operation_id), Some(source), None)
    }

    pub fn continue_input(&self, operation_id: &str, source: &Path, input: HostKeyInput) -> Result<ResultEnvelope> {
        anyhow::ensure!(input.material.is_none(), "desktop key input cannot supply private material");
        control_key_inner(Arc::clone(&self.host), Action::Query(operation_id), Some(source), Some(input))
    }

    pub fn rollback(&self, operation_id: &str) -> Result<ResultEnvelope> {
        control_key(Arc::clone(&self.host), Action::Rollback(operation_id))
    }

    pub fn fivekeys(
        &self,
        operation: HostKeyOperation,
        input: HostKeyInput,
        resume: Option<&HostKeyResult>,
    ) -> Result<ResultEnvelope> {
        anyhow::ensure!(
            matches!(operation, HostKeyOperation::Fivekeys { .. }),
            "not a fivekeys intent"
        );
        control_key(
            Arc::clone(&self.host),
            Action::Reset {
                operation,
                input,
                resume,
            },
        )
    }
}

fn execute_method(operation: &HostKeyOperation) -> &'static str {
    match operation {
        HostKeyOperation::SuperReset { .. } => HOST_KEY_SUPER_RESET_METHOD,
        HostKeyOperation::Fivekeys { .. } => HOST_KEY_FIVEKEYS_METHOD,
    }
}

/// Retain the original transaction identity alongside legacy terminal success fields.
fn committed_summary(receipt: &HostKeyResult) -> Result<Value> {
    anyhow::ensure!(receipt.outcome == HostKeyOutcome::Committed && !receipt.verification_required,
        "key success summary requires a verified committed receipt");
    let mut summary = json!({"ok":true, "operation_id":receipt.operation_id,
        "source_profile":receipt.source_profile, "outcome":receipt.outcome});
    match &receipt.operation {
        HostKeyOperation::Fivekeys { user, addr, .. } => {
            summary["user"] = json!(user);
            summary["addr"] = json!(addr);
        }
        HostKeyOperation::SuperReset { .. } => {
            summary["super"] = json!(receipt.super_password.as_deref()
                .context("verified key receipt has no issued code")?);
        }
    }
    Ok(summary)
}

fn host_completion_required(receipt: &HostKeyResult) -> bool {
    receipt.verification_required
        || matches!(receipt.outcome, HostKeyOutcome::Committed | HostKeyOutcome::RolledBack)
}

fn canonical(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

fn read_original(path: &Path) -> Result<Option<(Original, String)>> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "key recovery path is a symlink"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "key recovery path is a reparse point"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024,
        "invalid key recovery file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 16 * 1024,
        "key recovery context exceeds its bound"
    );
    let original = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid key recovery metadata"))?;
    material::validate_public(&original)?;
    Ok(Some((original, crate::launch::hash(&bytes))))
}

include!("key/operation.rs");

include!("key/custody.rs");

include!("key/exchange.rs");

#[cfg(test)]
mod tests;
