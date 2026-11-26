//! Offline consumer for an archive pinned by the native maintenance owner.
use super::*;
use crate::snapshot::{self, RestoreStatus, SnapshotOperation};
use respire_spawn::owner_credential::{ArchiveOperation, BackupArchiveHandoff, ReadyBackupArchive};

/// Captured while the authenticated session still owns the profile. No key
/// material is exported. The health fallback for an anonymous profile cannot bind.
pub struct BackupVaultIdentity {
    owner: String,
    profile: std::path::PathBuf,
    epoch: uuid::Uuid,
}
impl BackupVaultIdentity {
    pub fn capture(profile: &Path) -> Result<Self> {
        crate::auth::with_runtime_library(profile, |owner, epoch, _| Self {
            owner: owner.to_owned(), profile: profile.to_owned(), epoch,
        })?.context("backup requires an unlocked named Vault owner")
    }
    /// The surviving native coordinator supplies a new challenge before shutdown.
    pub fn attest(
        &self,
        owner: &respire_spawn::owner_credential::OwnerRecord,
        challenge: &[u8; 16],
    ) -> Result<(Vec<u8>, [u8; 32])> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "owner": self.owner, "profile": self.profile, "epoch": self.epoch.to_string()
        }))?;
        let tag = respire_spawn::owner_credential::backup_grant::vault_observation(
            owner, challenge, &bytes,
        )?;
        Ok((bytes, tag))
    }

    pub fn from_attestation(
        owner: &respire_spawn::owner_credential::OwnerRecord,
        challenge: &[u8; 16],
        bytes: &[u8],
        tag: &[u8; 32],
    ) -> Result<Self> {
        ensure!(bytes.len() <= 8192, "oversized Vault observation");
        respire_spawn::owner_credential::backup_grant::verify_vault_observation(
            owner, challenge, bytes, tag,
        )?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Observation {
            owner: String,
            profile: std::path::PathBuf,
            epoch: String,
        }
        let Observation { owner, profile, epoch } = serde_json::from_slice(bytes)?;
        Ok(Self { owner, profile, epoch: uuid::Uuid::parse_str(&epoch)? })
    }
    #[cfg(test)]
    pub(crate) fn fixture(owner: &str, profile: &Path, epoch: uuid::Uuid) -> Self {
        Self {owner: owner.to_owned(), profile: profile.to_owned(), epoch}
    }
    pub fn check(&self, grant: &respire_spawn::owner_credential::backup_grant::VerifiedGrant) -> Result<()> {
        grant.check_fresh()?;
        let hash = |bytes: &[u8]| -> [u8;32] { Sha256::digest(bytes).into() };
        ensure!(self.epoch.as_bytes() == &grant.vault_epoch()
            && hash(self.owner.as_bytes()) == grant.scope().account
            && hash(self.profile.as_os_str().as_encoded_bytes()) == grant.scope().profile,
            "backup grant differs from authenticated Vault owner, profile or epoch");
        Ok(())
    }
}

pub fn restore_approved_archive_locked(
    root: &Path,
    custody: &Path,
    selected: &respire_spawn::owner_credential::ArchiveTarget,
    vault: &BackupVaultIdentity,
    keys: &crate::SessionKeys,
    snapshot_id: &str,
    operation: &SnapshotOperation,
    ownership: &crate::lock::LibraryLock,
    grant: respire_spawn::owner_credential::backup_grant::VerifiedGrant,
    authorize: impl Fn() -> Result<()>,
) -> Result<Option<RestoreStatus>> {
    vault.check(&grant)?;
    let account = vault.owner.as_str();
    ensure!(ownership.protects(root)? && snapshot::valid_id(snapshot_id), "invalid archive restore target");
    authorize()?;
    ensure!(snapshot::operation_status(root, account, operation)?.is_none(), snapshot::OperationConflict);
    selected.verify()?;
    ensure!(crate::service::database_path(root)?.canonicalize()? == selected.path(), "selected database changed");
    let id = uuid::Uuid::parse_str(&operation.operation_id)?;
    grant.check_fresh()?;
    let scope = grant.scope();
    let hash = |bytes: &[u8]| -> [u8; 32] { Sha256::digest(bytes).into() };
    ensure!(scope.operation == *id.as_bytes() && scope.target_identity == selected.identity_digest()?
        && scope.target == hash(selected.path().as_os_str().as_encoded_bytes())
        && scope.profile == hash(custody.canonicalize()?.as_os_str().as_encoded_bytes())
        && operation.request_fingerprint == grant.fingerprint(), "archive confirmation scope changed");
    let intent_sha256 = grant.intent();
    let archive_sha256 = scope.archive;
    let op = ArchiveOperation::from_v4_bytes(*id.as_bytes())?;
    let handoff = BackupArchiveHandoff::from_approved_intent(op, intent_sha256);
    let ready = ReadyBackupArchive::open(custody, handoff)?.require_archive_digest(archive_sha256)?;
    let library = crate::service::database_path(root)?.canonicalize()?;
    let library = library.to_str().context("selected library must be UTF-8")?;
    let library_digest = format!("{:x}", Sha256::digest(library.as_bytes()));
    let (scratch, manifest) = ready.consume(|mut reader| {
        let decode = || -> Result<_> {
            let header = BackupArchiveHeader::read(&mut reader)?.authenticate(keys, account)?;
            ensure!(header.manifest.id == snapshot_id && header.manifest.source_library == library_digest,
                "archive differs from confirmed snapshot or selected library");
            chrono::DateTime::parse_from_rfc3339(&header.manifest.created_at)?;
            authorize()?;
            let path = root.join(format!(".backup-restore-{}.db", uuid::Uuid::new_v4().simple()));
            let file = snapshot::private_new(&path)?;
            let scratch = snapshot::PreparedRestoreCopy { path };
            let manifest = header.copy_to_stage(reader, file, &authorize)?;
            Ok((scratch, manifest))
        };
        decode().map_err(std::io::Error::other)
    })?;
    snapshot::validate_manifest_database(&scratch.path, &manifest)?;
    authorize()?;
    snapshot::restore_source_with_hook(root, account, keys, snapshot_id, ownership, Some(operation),
        Some((manifest, &scratch.path)), &|phase| {
            authorize()?;
            if matches!(phase, "after_backups" | "before_replace") { grant.check_fresh()?; selected.verify()?; }
            Ok(())
        })?;
    snapshot::operation_status(root, account, operation)
}
