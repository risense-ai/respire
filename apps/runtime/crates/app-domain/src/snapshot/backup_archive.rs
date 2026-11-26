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
pub const MAX_WIRE: u64 = MAX_DATABASE + MAGIC.len() as u64 + 4 + MAX_HEADER as u64;

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
    check_manifest(&manifest, account, manifest.bytes)?;
    let wire = authenticated_header(account, &manifest, |challenge| keys.encrypt_content(challenge))?;
    stage.write_all(&wire)?;
    copy_checked(database, stage, &manifest, check)
}

fn authenticated_header(
    account: &str,
    manifest: &Manifest,
    seal: impl FnOnce(&str) -> Result<(String, String)>,
) -> Result<Vec<u8>> {
    check_manifest(manifest, account, manifest.bytes)?;
    let (nonce, sealed_manifest_digest) = seal(&manifest_challenge(manifest)?)?;
    let wire = serde_json::to_vec(&serde_json::json!({
        "bytes": manifest.bytes,
        "nonce": nonce,
        "sealed_manifest_digest": sealed_manifest_digest,
        "manifest": manifest,
    }))?;
    ensure!(wire.len() <= MAX_HEADER, "backup header is too large");
    let mut header = Vec::with_capacity(MAGIC.len() + 4 + wire.len());
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&(wire.len() as u32).to_be_bytes());
    header.extend_from_slice(&wire);
    Ok(header)
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[path = "backup_export_stage.rs"]
mod export;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use export::{BackupExportDraft, BackupExportReady, BackupExportReader, BackupSnapshotProof};

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

/// An untrusted envelope header. Parsing is bounded and requires neither the
/// Vault nor access to a stage. The reader is positioned at the SQLite body.
pub struct BackupArchiveHeader(Header);

/// Proof of a header's account and Vault lineage. This is not a restore
/// grant: the selected library, live scope, and offline authority still
/// have to be verified. It cannot expose a manifest before the body passes
/// the exact length and digest checks.
pub struct AuthenticatedBackupHeader(Manifest);

impl BackupArchiveHeader {
    pub fn read(mut archive: impl Read) -> Result<Self> {
        let mut magic = [0; 8];
        archive.read_exact(&mut magic)?;
        ensure!(&magic == MAGIC, "unsupported backup archive");
        let mut size = [0; 4];
        archive.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        ensure!((1..=MAX_HEADER).contains(&size), "invalid backup header length");
        let mut wire = vec![0; size];
        archive.read_exact(&mut wire)?;
        Ok(Self(serde_json::from_slice(&wire)?))
    }

    fn authenticate(
        self,
        account: &str,
        open: impl FnOnce(&str, &str) -> Result<String>,
    ) -> Result<AuthenticatedBackupHeader> {
        let header = self.0;
        check_manifest(&header.manifest, account, header.bytes)?;
        let proof = open(&header.sealed_manifest_digest, &header.nonce)
            .context("backup manifest authentication failed")?;
        ensure!(
            proof == manifest_challenge(&header.manifest)?,
            "backup manifest authentication failed"
        );
        Ok(AuthenticatedBackupHeader(header.manifest))
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl crate::auth::RuntimeLibrary<'_> {
    /// The caller checks the full live Runtime scope around this short
    /// operation. No reader, writer, or SessionKeys crosses the lock boundary.
    pub fn authenticate_backup_import_header(
        &self,
        header: BackupArchiveHeader,
    ) -> Result<AuthenticatedBackupHeader> {
        header.authenticate(self.backup_account(), |ciphertext, nonce| {
            self.open_backup_challenge(ciphertext, nonce)
        })
    }
}

impl AuthenticatedBackupHeader {
    /// Only for an exclusively held, disposable owner-private stage. Failed
    /// copies can leave partial bytes; discard them, never publish them.
    /// The callback must verify cancellation and the captured scope on each
    /// 64 KiB read and before handing the result to offline maintenance.
    pub fn copy_to_stage(
        self,
        database_body: impl Read,
        stage: impl Write,
        check: impl FnMut() -> Result<()>,
    ) -> Result<Manifest> {
        copy_checked(database_body, stage, &self.0, check)?;
        Ok(self.0)
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[path = "backup_import_stage.rs"]
mod import;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use import::BackupImportStage;

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
        .authenticate(account, |ciphertext, nonce| keys.decrypt_content(ciphertext, nonce))?
        .copy_to_stage(archive, stage, check)
}

#[cfg(test)]
#[path = "backup_archive_tests.rs"]
mod tests;
