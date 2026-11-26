//! The typed HTTP backup envelope. It is intentionally separate from the
//! legacy two-file snapshot format and its offline restoration authority.
use super::Manifest;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"RSRSBKP\x01";
const PURPOSE: &str = "rsrs:backup-archive:manifest:v1:";
const MAX_HEADER: usize = 64 * 1024;
pub const MAX_DATABASE: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    bytes: u64,
    nonce: String,
    sealed_manifest_digest: String,
    manifest: Manifest,
}

fn manifest_challenge(manifest: &Manifest) -> Result<String> {
    let bytes = serde_json::to_vec(manifest)?;
    Ok(format!("{PURPOSE}{:x}", Sha256::digest(bytes)))
}

fn check_manifest(manifest: &Manifest, account: &str, bytes: u64) -> Result<()> {
    ensure!(!account.is_empty(), "backup account is unbound");
    let account_hash = format!("{:x}", Sha256::digest(account.as_bytes()));
    ensure!(
        manifest.schema_version == 1 && manifest.account_fingerprint == account_hash,
        "backup manifest belongs to a different account or version"
    );
    ensure!(
        bytes != 0 && bytes <= MAX_DATABASE && manifest.bytes == bytes,
        "backup database size is invalid"
    );
    ensure!(
        manifest.database_sha256.len() == 64
            && manifest.database_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "backup database digest is invalid"
    );
    Ok(())
}

fn copy_checked(
    mut input: impl Read,
    mut output: impl Write,
    manifest: &Manifest,
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut remaining = manifest.bytes;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    while remaining != 0 {
        check()?;
        let len = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..len])?;
        digest.update(&buffer[..len]);
        output.write_all(&buffer[..len])?;
        remaining -= len as u64;
    }
    check()?;
    let mut trailing = [0; 1];
    ensure!(input.read(&mut trailing)? == 0, "trailing bytes in backup");
    ensure!(
        format!("{:x}", digest.finalize()) == manifest.database_sha256,
        "backup database digest mismatch"
    );
    check()?;
    Ok(())
}

/// Parsed wire header only. No lock, Vault key, or file authority is needed.
pub struct BackupArchiveHeader {
    value: Header,
    prefix: Vec<u8>,
}

/// A consumed, account-bound header proof, not a restore authorization.
pub struct AuthenticatedBackupHeader {
    manifest: Manifest,
    prefix: Vec<u8>,
}

impl BackupArchiveHeader {
    pub fn read(mut archive: impl Read) -> Result<Self> {
        let mut magic = [0; 8];
        archive.read_exact(&mut magic)?;
        ensure!(&magic == MAGIC, "unsupported backup archive");
        let mut size = [0; 4];
        archive.read_exact(&mut size)?;
        let length = u32::from_be_bytes(size) as usize;
        ensure!((1..=MAX_HEADER).contains(&length), "invalid backup header length");
        let mut prefix = Vec::with_capacity(MAGIC.len() + size.len() + length);
        prefix.extend_from_slice(MAGIC);
        prefix.extend_from_slice(&size);
        let start = prefix.len();
        prefix.resize(start + length, 0);
        archive.read_exact(&mut prefix[start..])?;
        Ok(Self { value: serde_json::from_slice(&prefix[start..])?, prefix })
    }

    pub(crate) fn authenticate(
        self,
        keys: &crate::SessionKeys,
        account: &str,
    ) -> Result<AuthenticatedBackupHeader> {
        let header = self.value;
        check_manifest(&header.manifest, account, header.bytes)?;
        let proof = keys.decrypt_content(&header.sealed_manifest_digest, &header.nonce)
            .context("backup manifest authentication failed")?;
        ensure!(proof == manifest_challenge(&header.manifest)?, "backup manifest authentication failed");
        Ok(AuthenticatedBackupHeader { manifest: header.manifest, prefix: self.prefix })
    }
}

impl AuthenticatedBackupHeader {
    pub fn wire_length(&self) -> u64 {
        self.manifest.bytes + self.prefix.len() as u64
    }

    /// Verify an upload in one pass into native pending custody. The selected
    /// database comes from the verified library dispatcher, never a request
    /// path. The resulting stage is not authority to stop or restore Runtime.
    #[cfg(target_os = "linux")]
    pub fn stage_for_restore(
        self,
        database_bytes: impl Read,
        owner_root: &Path,
        selected_database: &Path,
        operation: respire_spawn::owner_credential::ArchiveOperation,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<respire_spawn::owner_credential::PendingBackupArchive> {
        use respire_spawn::owner_credential::PendingBackupArchive;
        use std::io;

        check()?;
        let canonical = selected_database.canonicalize()?;
        ensure!(canonical == selected_database, "backup selection is not canonical");
        let selected = canonical.to_str().context("backup selection must be UTF-8")?;
        ensure!(
            self.manifest.source_library == format!("{:x}", Sha256::digest(selected.as_bytes())),
            "backup belongs to a different selected library"
        );
        let length = self.wire_length();
        Ok(PendingBackupArchive::stage_verified(
            owner_root,
            operation,
            length,
            || check().map_err(io::Error::other),
            |output, check| {
                self.copy_envelope_to_stage(database_bytes, output, || Ok(check()?))
                    .map(|_| ())
                    .map_err(io::Error::other)
            },
        )?)
    }

    /// Inspect the complete bounded input, not the live SQLite file.
    /// The caller owns and must discard any unverified partial stage.
    pub fn copy_to_stage(
        self,
        database: impl Read,
        output: impl Write,
        check: impl FnMut() -> Result<()>,
    ) -> Result<Manifest> {
        copy_checked(database, output, &self.manifest, check)?;
        Ok(self.manifest)
    }

    /// Retain the exact authenticated wire prefix for independently
    /// reauthorizing the archive under fresh offline Vault factors.
    pub fn copy_envelope_to_stage(
        self,
        database: impl Read,
        mut output: impl Write,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Manifest> {
        check()?;
        output.write_all(&self.prefix)?;
        self.copy_to_stage(database, output, check)
    }
}

pub(crate) fn encode_header(
    keys: &crate::SessionKeys,
    account: &str,
    manifest: &Manifest,
) -> Result<Vec<u8>> {
    check_manifest(manifest, account, manifest.bytes)?;
    let (nonce, sealed_manifest_digest) = keys.encrypt_content(&manifest_challenge(manifest)?)?;
    #[derive(Serialize)]
    struct EncodedHeader<'a> {
        bytes: u64,
        nonce: String,
        sealed_manifest_digest: String,
        manifest: &'a Manifest,
    }
    let wire = serde_json::to_vec(&EncodedHeader {
        bytes: manifest.bytes, nonce, sealed_manifest_digest, manifest,
    })?;
    ensure!(wire.len() <= MAX_HEADER, "backup header is too large");
    let mut prefix = Vec::with_capacity(MAGIC.len() + 4 + wire.len());
    prefix.extend_from_slice(MAGIC);
    prefix.extend_from_slice(&(wire.len() as u32).to_be_bytes());
    prefix.extend_from_slice(&wire);
    Ok(prefix)
}

/// Only for an exclusively held, disposable output. A failed call may leave
/// a partial envelope; the owner must discard it rather than publish it.
fn seal_to_stage(
    keys: &crate::SessionKeys,
    account: &str,
    manifest: Manifest,
    database: impl Read,
    mut stage: impl Write,
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    stage.write_all(&encode_header(keys, account, &manifest)?)?;
    copy_checked(database, stage, &manifest, check)
}

/// Seal a verified native snapshot, never caller-provided metadata or a
/// caller-provided source path. Keep the matching owner lock in the serving
/// caller; do not release the resulting stage until its live scope is checked.
pub fn seal_verified_snapshot(
    profile: &Path,
    ownership: &crate::lock::LibraryLock,
    keys: &crate::SessionKeys,
    account: &str,
    snapshot_id: &str,
    stage: impl Write,
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    ensure!(ownership.protects(profile)?, "backup requires library ownership");
    let manifest = super::verify_backup(profile, snapshot_id, keys)?;
    let live = crate::service::database_path(profile)?.canonicalize()?;
    let source = format!("{:x}", Sha256::digest(live.to_string_lossy().as_bytes()));
    ensure!(manifest.source_library == source, "backup source library changed");
    let snapshot = profile.join("snapshots").join(format!("{}.db", manifest.id));
    seal_to_stage(keys, account, manifest, File::open(snapshot)?, stage, check)
}

/// Authenticate before writing any DB bytes, and verify the exact final
/// digest before returning. Do not open the output as SQLite or expose it
/// unless this function and subsequent offline restore validation succeed.
pub fn open_to_stage(
    keys: &crate::SessionKeys,
    account: &str,
    mut archive: impl Read,
    stage: impl Write,
    mut check: impl FnMut() -> Result<()>,
) -> Result<Manifest> {
    check()?;
    BackupArchiveHeader::read(&mut archive)?
        .authenticate(keys, account)?
        .copy_to_stage(archive, stage, check)
}

#[cfg(test)]
#[path = "backup_archive_tests.rs"]
mod tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "backup_export_stage.rs"]
mod export;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use export::{BackupExportDraft, BackupExportReader, BackupExportReady, BackupSnapshotProof, SealedBackupExportHeader};
