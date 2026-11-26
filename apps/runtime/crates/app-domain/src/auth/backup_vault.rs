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

    /// Copy, strip local grants from, and seal one selected database into `stage_path`.
    pub fn seal_portable_backup_export(
        &self,
        live_database: &std::path::Path,
        stage_path: &std::path::Path,
    ) -> Result<Vec<u8>> {
        backup_archive::seal_database_header(self.0, self.1, live_database, stage_path)
    }

    /// Re-verify a staged archive against the active Vault and atomically apply it to `store`.
    pub fn restore_staged_backup(
        &self,
        live_database: &std::path::Path,
        staged: &std::path::Path,
        manifest: &Manifest,
        store: &crate::transport::local::LocalStore,
    ) -> Result<()> {
        backup_archive::restore_verified_stage(self.0, self.1, live_database, staged, manifest, store)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl RuntimeLibrary<'_> {
    /// Seal the historical SQLite key check only for an identified detached
    /// backup draft rather than offering arbitrary challenge signing.
    pub fn prove_backup_snapshot(
        &self,
        draft: &backup_archive::BackupExportDraft,
    ) -> Result<backup_archive::BackupSnapshotProof> {
        draft.prove(self.0, self.1)
    }

    /// Enforce the same Vault lineage between detached processing and wire
    /// finalization, without opening files or lending a Vault key.
    pub fn seal_verified_backup_export(
        &self,
        ready: &backup_archive::BackupExportReady,
    ) -> Result<backup_archive::SealedBackupExportHeader> {
        ready.verify_lineage(self.0)?;
        let manifest = ready.manifest();
        backup_archive::SealedBackupExportHeader::for_manifest(
            manifest,
            self.seal_backup_manifest_header(manifest)?,
        )
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn detached_backup_only_seals_matching_live_vault_and_exact_stage() -> Result<()> {
        use crate::transport::local::LocalStore;
        use std::io::Read;

        let root = tempfile::tempdir()?;
        let profile = root.path().canonicalize()?;
        let database = profile.join("rsrs.db");
        let store = LocalStore::open(&database)?;
        let db = rusqlite::Connection::open(&database)?;
        db.execute_batch("INSERT INTO meta(key,value) VALUES ('backup-fixture','selected');")?;
        let keys = SessionKeys::from_urk([7; 32])?;
        let other = SessionKeys::from_urk([8; 32])?;
        let authority = RuntimeLibrary(&keys, "alice", &profile);
        let wrong_key = RuntimeLibrary(&other, "alice", &profile);
        let wrong_account = RuntimeLibrary(&keys, "bob", &profile);
        let make_ready = || -> Result<_> {
            let copy = store.prepare_backup_export(&profile)?.copy(|| Ok(()))?;
            let draft = backup_archive::BackupExportDraft::new(copy)?;
            let proof = authority.prove_backup_snapshot(&draft)?;
            draft.prepare(proof, || Ok(()))
        };
        let ready = make_ready()?;
        let another = make_ready()?;
        assert!(wrong_key.seal_verified_backup_export(&ready).is_err());
        assert!(wrong_account.seal_verified_backup_export(&ready).is_err());
        assert!(another.into_reader(authority.seal_verified_backup_export(&ready)?).is_err());

        let header = authority.seal_verified_backup_export(&ready)?;
        let mut wire = Vec::new();
        ready.into_reader(header)?.write_verified(&mut wire, || Ok(()))?;
        let output = profile.join("verified-fixture.db");
        let mut body = wire.as_slice();
        let receipt = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
        let mut db_out = std::fs::File::create(&output)?;
        receipt.copy_to_stage(&mut body, &mut db_out, || Ok(()))?;
        let mut extra = [0; 1];
        assert_eq!(body.read(&mut extra)?, 0);
        let value: String = rusqlite::Connection::open(&output)?.query_row(
            "SELECT value FROM meta WHERE key='backup-fixture'", [], |row| row.get(0),
        )?;
        assert_eq!(value, "selected");
        Ok(())
    }

}
