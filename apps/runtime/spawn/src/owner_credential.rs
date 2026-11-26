//! A fresh, instance-scoped credential transported only to the same OS owner.
//! This secret is not an HTTP token. The caller supplies fresh CSPRNG bytes.

use std::{ffi::CString, io};

pub const OWNER_RECORD_LEN: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RemovalMetadata {
    Journal,
    CommitTemp,
    Committed,
}

/// Raw record: 32-byte key followed by a 16-byte opaque authority epoch.
/// No Debug/Display implementation; this is not an HTTP token.
pub struct OwnerRecord([u8; OWNER_RECORD_LEN]);

impl OwnerRecord {
    pub fn from_bytes(value: [u8; OWNER_RECORD_LEN]) -> Self {
        Self(value)
    }
    pub fn as_bytes(&self) -> &[u8; OWNER_RECORD_LEN] {
        &self.0
    }
}
const SECRET_FILE: &str = "secret";

fn directory_name(instance_id: &str) -> io::Result<CString> {
    if !(1..=80).contains(&instance_id.len())
        || !instance_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid owner credential instance",
        ));
    }
    CString::new(format!(".owner-credentials-{instance_id}"))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{
    create_owner_credential, read_owner_credential, same_model_file, ModelRemovalRoot, ModelStage,
    OwnerCredential,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::model_namespace::provision_model_namespace;
#[cfg(windows)]
pub use windows::model_removal::ModelRemovalRoot;
#[cfg(windows)]
pub use windows::model_stage::{same_model_file, ModelStage};
#[cfg(windows)]
pub use windows::{create_owner_credential, read_owner_credential, OwnerCredential};
#[cfg(windows)]
pub use windows::local_spaces::LocalSpaceNamespace;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::local_spaces::LocalSpaceNamespace;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn is_local_space_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// The names returned here are candidates, not authorization. Each receipt is
// opened and proved separately through its retained owner-verified directory.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn candidate_local_ids(directory: &std::path::Path) -> io::Result<Vec<String>> {
    const MAX_SCAN: usize = 512;
    let mut ids = Vec::new();
    for (index, entry) in std::fs::read_dir(directory)?.enumerate() {
        if index >= MAX_SCAN {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "local catalog scan limit exceeded"));
        }
        if let Some(id) = entry?.file_name().to_str().and_then(|name| name.strip_prefix("local-")) {
            if is_local_space_id(id) {
                ids.push(id.to_owned());
            }
        }
    }
    ids.sort_unstable();
    Ok(ids)
}


#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const LOCAL_SPACE_STAGE_FILES: &[&str] = &[
    "onememory.db", "onememory.db-wal", "onememory.db-shm", "onememory.db-journal",
    "lock.db", "lock.db-wal", "lock.db-shm", "lock.db-journal",
    "owner.receipt", ".owner-receipt.pending",
];

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const LOCAL_SPACE_STAGING_SLOTS: u8 = 16;

pub mod model_proof;
pub mod proof;

#[cfg(windows)]
pub use windows::migration_copy::MigrationCopy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::migration_copy::{MigrationCopy, MigrationSource};

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn validate_migration_leaf(name: &str) -> io::Result<()> {
    if !(1..=200).contains(&name.len()) || name == "." || name == ".."
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'.'|b'_'|b'-'))
        || name.ends_with('.')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid migration backup leaf"));
    }
    Ok(())
}

#[cfg(windows)]
pub use windows::restore_stage::PrivateRestoreStage;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod model_job;
#[cfg(windows)]
pub use windows::model_job::ModelJobRoot;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::model_job::ModelJobRoot;

#[cfg(target_os = "linux")]
pub use unix::backup_archive::{
    read_backup_terminal, ArchiveOperation, BackupArchiveHandoff, PendingBackupArchive, ReadyBackupArchive,
};
