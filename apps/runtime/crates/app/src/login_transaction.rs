//! Prepare login without changing the active account; commit only after vault verification.
use std::path::{Path, PathBuf};
use anyhow::{anyhow, ensure, Context, Result};
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
use std::io::Write;
use sha2::Digest;
use respire_protocol::{HostAuthIdentity, HostAuthOutcome, HostAuthResult};
use respire_protocol::host_control::{HostAuthAccountOutcome, HostAuthAccountResult,
    HostAuthAuthorization, HostAuthLoginParams, HostAuthRegisterParams};

mod cold_keygen;
mod login;
mod identity;
pub use identity::{control_identity_write, control_identity_write_result};
#[cfg(test)]
mod keygen_existing_protocol;
mod keys;
pub use keys::{control_key, control_key_result, control_key_rollback, control_key_complete, verify_key_receipt};
mod space;
pub use space::control_space;
mod profile;
pub use profile::control_profile_remove;

mod recovery_credentials;

#[derive(Clone, Serialize, Deserialize)]
struct OriginalBackup {
    slot: String,
    backend: String,
    sha256: String,
    #[serde(default)]
    pending: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Recovery {
    directory: PathBuf,
    #[serde(default)]
    original: Option<Vec<u8>>,
    #[serde(default)]
    original_backup: Option<OriginalBackup>,
    #[serde(default)]
    original_backups: Vec<OriginalBackup>,
    #[serde(default)]
    restored: Option<Vec<u8>>,
    #[serde(default)]
    restore_alias: Option<String>,
    #[serde(default)]
    retired: bool,
    #[serde(default)]
    legacy_secret_backends: Vec<String>,
    config: Option<Vec<u8>>,
    alias: String,
    backends: Vec<String>,
    #[serde(default)]
    pass_backends: Vec<String>,
    #[serde(default)]
    totp_backends: Vec<String>,
    directory_existed: bool,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    owns_config: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    active: Option<Recovery>,
    operations: Vec<AuthRecord>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthPhase { Admitted, LoginInput, RemotePending, RemoteUnknown, Authorized, Prepared, AwaitingVerification, RollingBack, Committed, RolledBack }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountOperation {
    register: bool,
    addr: String,
    user: String,
    source_identity: HostAuthIdentity,
    target: Option<PathBuf>,
    authorization: Option<Value>,
    vault: Option<Value>,
    #[serde(default)]
    published_vault: bool,
    #[serde(default)]
    vault_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login: Option<login::LoginProgress>,
}

#[derive(Serialize, Deserialize)]
struct RestoredSession {
    profile: PathBuf,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthRecord {
    operation_id: String,
    idempotency_key: String,
    fingerprint: String,
    root: PathBuf,
    profile: PathBuf,
    original_sha256: Option<String>,
    candidate: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_backup: Option<OriginalBackup>,
    candidate_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restored: Option<RestoredSession>,
    alias: String,
    backend: Option<String>,
    phase: AuthPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<AccountOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<identity::IdentityOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_operation: Option<keys::KeyOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_operation: Option<space::SpaceOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_remove: Option<profile::ProfileRemoveOperation>,
}

fn digest(bytes: &[u8]) -> String { hex::encode(sha2::Sha256::digest(bytes)) }

fn load_journal_metadata() -> Result<Journal> {
    let path = recovery_path();
    ensure!(!std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "login recovery record must not be a symbolic link");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            return Ok(Journal { schema_version: 2, ..Journal::default() }),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_slice(&bytes).context("invalid login recovery record; retain it for recovery")?;
    let journal = if value.get("schema_version").is_none() {
        Journal { schema_version: 2, active: Some(serde_json::from_value(value)?), operations: Vec::new() }
    } else { serde_json::from_value(value)? };
    ensure!(journal.schema_version == 2, "unsupported login recovery schema");
    Ok(journal)
}

fn load_journal() -> Result<Journal> {
    let mut journal = load_journal_metadata()?;
    recovery_credentials::hydrate(&mut journal)?;
    Ok(journal)
}

fn save_journal(journal: &mut Journal) -> Result<()> {
    recovery_credentials::prepare_candidates(journal)?;
    recovery_credentials::prepare(journal)?;
    recovery_credentials::write(journal)
}

fn retire_active(journal: &mut Journal) -> Result<()> {
    if let Some(active) = &journal.active {
        ensure!(active.backends.is_empty() && active.pass_backends.is_empty()
            && active.totp_backends.is_empty() && active.legacy_secret_backends.is_empty(),
            "host_recovery_required: historical credential cleanup metadata retained");
    }
    if let Some(active) = journal.active.as_mut() {
        active.retired = true;
        save_journal(journal)?;
        let active = journal.active.as_ref().context("retiring recovery is missing")?;
        recovery_credentials::remove(active)?;
    }
    journal.active = None;
    if journal.operations.is_empty() {
        std::fs::remove_file(recovery_path())?;
        #[cfg(unix)] {
            std::fs::File::open(recovery_path().parent().context("recovery root is missing")?)?.sync_all()?;
        }
        Ok(())
    } else { save_journal(journal) }
}

#[cfg(test)]
mod tests;
include!("login_transaction/journal_recovery.rs");
include!("login_transaction/ownership_keygen.rs");
include!("login_transaction/account_admission.rs");
include!("login_transaction/account_receipts.rs");
include!("login_transaction/prepared_login.rs");
