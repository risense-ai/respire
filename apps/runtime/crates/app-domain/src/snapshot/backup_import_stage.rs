//! Own the exact private staging inode; this is not an offline restore permit.
use super::{AuthenticatedBackupHeader, Manifest};
use anyhow::{ensure, Result};
use respire_spawn::owner_credential::{MigrationCopy, MigrationSource};
use std::{io::{self, Read}, path::{Path, PathBuf}};

pub struct BackupImportStage {
    file: MigrationCopy,
    root: PathBuf,
    root_identity: [u64; 2],
    leaf: String,
    file_identity: [u64; 2],
    manifest: Manifest,
}

impl AuthenticatedBackupHeader {
    /// Only after verified selection of the actual local store. Neither path
    /// may come from a transport request. The caller must check the full
    /// current scope (including the selected store) in the callback.
    pub fn copy_into_owner_stage(
        self,
        input: impl Read,
        stage_root: &Path,
        selected_database: &Path,
        mut gate: impl FnMut() -> Result<()>,
    ) -> Result<BackupImportStage> {
        gate()?;
        let manifest = self.manifest;
        ensure!(
            manifest.source_library == super::super::fingerprint_path(selected_database)?,
            "backup does not belong to the selected physical library"
        );
        let root = MigrationSource::open(stage_root, false)?;
        let root_identity = root.identity()?;
        let leaf = format!(".backup-import-{}.pending", uuid::Uuid::new_v4().simple());
        let mut stage = MigrationCopy::create(stage_root, &leaf)?;
        let file_identity = stage.source_identity(&leaf)?;
        stage.write_exact_stage(manifest.bytes, |writer| {
            super::copy_checked(input, writer, &manifest, &mut gate).map_err(io::Error::other)
        })?;
        let stage = BackupImportStage {
            file: stage, root: stage_root.to_path_buf(), root_identity,
            leaf, file_identity, manifest,
        };
        stage.checked_path()?;
        gate()?;
        Ok(stage)
    }
}

impl BackupImportStage {
    /// For inspection with the stage retained; never substitute a caller
    /// path for this witness. Restore must perform its own fresh validation.
    pub fn checked_path(&self) -> Result<&Path> {
        let root = MigrationSource::open(&self.root, false)?;
        ensure!(
            root.identity()? == self.root_identity
                && root.file_identity(&self.leaf)? == self.file_identity
                && self.file.source_identity(&self.leaf)? == self.file_identity,
            "backup stage is no longer named"
        );
        Ok(self.file.path())
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}
