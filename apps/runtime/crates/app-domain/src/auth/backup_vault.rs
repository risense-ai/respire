//! Typed archive operations over the verified live Vault.
use super::RuntimeLibrary;
use crate::snapshot::{
    backup_archive::{self, AuthenticatedBackupHeader, BackupArchiveHeader},
    Manifest,
};
use anyhow::Result;

impl RuntimeLibrary<'_> {
    /// Use only the selected Runtime Vault. The request cannot supply an
    /// account or obtain key material. Restore needs separate host authority.
    pub fn authenticate_backup_import_header(
        &self,
        header: BackupArchiveHeader,
    ) -> Result<AuthenticatedBackupHeader> {
        header.authenticate(self.0, self.1)
    }

    /// Seal a manifest produced by the selected local backup workflow.
    /// Does not sign arbitrary challenges or export a Vault key.
    pub fn seal_backup_manifest_header(&self, manifest: &Manifest) -> Result<Vec<u8>> {
        backup_archive::encode_header(self.0, self.1, manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionKeys;
    use sha2::{Digest, Sha256};
    use std::path::Path;

    #[test]
    fn live_backup_proof_is_bound_to_account_vault_and_exact_wire() -> Result<()> {
        let db = [7u8; 65_537];
        let manifest = Manifest {
            schema_version: 1,
            id: "123456789abcdef0123456789abcdef0".into(),
            created_at: "2026-11-25T00:00:00Z".into(),
            note: String::new(),
            database_sha256: format!("{:x}", Sha256::digest(db)),
            bytes: db.len() as u64,
            entries: 0,
            source_library: "fixture source".into(),
            account_fingerprint: format!("{:x}", Sha256::digest(b"alice")),
            sqlite_user_version: 1,
            database_schema_sha256: String::new(),
            sdk_version: "fixture".into(),
            sdk_abi: 1,
            model: String::new(),
            witness_head: None,
            witness_count: None,
            witness_intact: None,
            witness_problem: None,
        };
        let current = SessionKeys::from_urk([7; 32])?;
        let legacy = SessionKeys::from_legacy_urk([7; 32])?;
        let wrong = SessionKeys::from_urk([8; 32])?;
        let profile = Path::new("synthetic-profile");
        let source = RuntimeLibrary(&current, "alice", profile);
        let reader = RuntimeLibrary(&legacy, "alice", profile);
        let wrong_account = RuntimeLibrary(&current, "other", profile);
        let wrong_vault = RuntimeLibrary(&wrong, "alice", profile);
        assert!(wrong_account.seal_backup_manifest_header(&manifest).is_err());

        let mut wire = source.seal_backup_manifest_header(&manifest)?;
        wire.extend_from_slice(&db);
        assert!(wrong_account.authenticate_backup_import_header(BackupArchiveHeader::read(wire.as_slice())?).is_err());
        assert!(wrong_vault.authenticate_backup_import_header(BackupArchiveHeader::read(wire.as_slice())?).is_err());

        let mut input = wire.as_slice();
        let proof = reader.authenticate_backup_import_header(BackupArchiveHeader::read(&mut input)?)?;
        assert_eq!(proof.wire_length(), wire.len() as u64);
        let mut staged = Vec::new();
        let selected = proof.copy_envelope_to_stage(input, &mut staged, || Ok(()))?;
        assert_eq!(selected.database_sha256, manifest.database_sha256);
        assert_eq!(staged, wire);
        Ok(())
    }
}
