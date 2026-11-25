//! Pin the exact serving database, then copy its committed WAL generation on
//! a worker. A raw copy is private and may contain grants; it is not an export.
use super::{Connection, LocalStore, OpenFlags};
use anyhow::{ensure, Context, Result};
use respire_spawn::owner_credential::MigrationCopy;
use rusqlite::backup::{Backup, StepResult};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const COPY_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_COPY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// An exact source admitted by the serving dispatcher. Keep the original
/// serving store and owner lock alive; this type does not grant Vault access.
pub struct BackupExportSource {
    reader: Connection,
    stage: MigrationCopy,
    stage_leaf: String,
    stage_identity: [u64; 2],
    source_leaf: String,
    source_identity: [u64; 2],
}

/// Owns the native identity-checked private stage until the caller drops it.
/// Never stream this raw copy; detached grant stripping and Vault seal follow.
pub struct BackupExportCopy {
    stage: MigrationCopy,
    leaf: String,
    identity: [u64; 2],
    source: PathBuf,
    source_leaf: String,
    source_identity: [u64; 2],
}

impl LocalStore {
    /// Short admission only. Select the store from the verified owner scope
    /// before calling; no HTTP path or caller-supplied name enters this API.
    pub fn prepare_backup_export(&self) -> Result<BackupExportSource> {
        let database = Path::new(self.connection.path().context("backup requires an on-disk store")?);
        let parent = database
            .parent()
            .context("backup database has no parent")?
            .canonicalize()?;
        let source_leaf = database
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .context("backup database name is unavailable")?
            .to_owned();
        ensure!(parent.join(&source_leaf) == database, "backup store path is not canonical");
        let stage_leaf = format!(".backup-export-{}.pending", uuid::Uuid::new_v4().simple());
        let stage = MigrationCopy::create(&parent, &stage_leaf)?;
        let source_identity = stage.source_identity(&source_leaf)?;
        let stage_identity = stage.source_identity(&stage_leaf)?;
        check_companions(&stage, &source_leaf)?;
        let source = parent.join(&source_leaf);
        let reader = Connection::open_with_flags(
            &source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        reader.busy_timeout(Duration::ZERO)?;
        let pinned = BackupExportSource {
            reader, stage, stage_leaf, stage_identity, source_leaf, source_identity,
        };
        pinned.check_paths()?;
        Ok(pinned)
    }
}

impl BackupExportSource {
    fn check_paths(&self) -> Result<()> {
        ensure!(
            self.stage.source_identity(&self.source_leaf)? == self.source_identity
                && self.stage.source_identity(&self.stage_leaf)? == self.stage_identity,
            "backup source or stage changed"
        );
        check_companions(&self.stage, &self.source_leaf)
    }

    /// Run only off the owner dispatcher, under an outer resource permit.
    /// The short check must not hold a scope/Vault lock across work.
    pub fn copy(self, check: impl FnMut() -> Result<()>) -> Result<BackupExportCopy> {
        self.copy_with_limit(MAX_COPY_BYTES, check)
    }

    fn copy_with_limit(
        self,
        max_bytes: u64,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<BackupExportCopy> {
        let deadline = Instant::now() + COPY_TIMEOUT;
        let mut gate = || -> Result<()> {
            ensure!(Instant::now() < deadline, "backup copy exceeded its deadline");
            check()?;
            self.check_paths()
        };
        gate()?;
        self.reader.execute_batch("BEGIN DEFERRED")?;
        let _: u64 = self.reader.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
        let pages = page_bytes(&self.reader)?;
        ensure!(pages <= max_bytes, "backup source exceeds the stage budget");
        let mut stage = Connection::open_with_flags(
            self.stage.path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let mode: String = stage.query_row("PRAGMA journal_mode=MEMORY", [], |row| row.get(0))?;
        ensure!(mode.eq_ignore_ascii_case("memory"), "unsafe backup stage journal mode");
        {
            let backup = Backup::new(&self.reader, &mut stage)?;
            loop {
                gate()?;
                match backup.step(128)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => std::thread::sleep(Duration::from_millis(25)),
                    _ => anyhow::bail!("unexpected backup step"),
                }
            }
        }
        self.reader.execute_batch("COMMIT")?;
        gate()?;
        let mode: String = stage.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
        ensure!(mode.eq_ignore_ascii_case("delete"), "backup stage is not standalone");
        ensure!(page_bytes(&stage)? <= max_bytes, "backup stage exceeded its budget");
        let integrity: String = stage.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        ensure!(integrity == "ok", "backup stage failed SQLite quick_check");
        stage.close().map_err(|(_, error)| error)?;
        gate()?;
        let source = PathBuf::from(self.reader.path().context("backup source lost its path")?);
        Ok(BackupExportCopy {
            source,
            source_leaf: self.source_leaf,
            source_identity: self.source_identity,
            stage: self.stage, leaf: self.stage_leaf, identity: self.stage_identity,
        })
    }
}

impl BackupExportCopy {
    pub fn source_database(&self) -> &Path {
        &self.source
    }

    /// Retain this owner while validating or sanitizing. No path from a client
    /// may address a staged copy, and it is never a restore authorization.
    pub fn checked_path(&self) -> Result<PathBuf> {
        // A retained dirfd can continue to resolve a stage after normal
        // child-space retirement moved its namespace. Do not stream then.
        let path = self.stage.path();
        ensure!(
            path.canonicalize()? == path
                && self.source.canonicalize()? == self.source
                && self.stage.source_identity(&self.leaf)? == self.identity
                && self.stage.source_identity(&self.source_leaf)? == self.source_identity,
            "backup source or stage changed"
        );
        Ok(path.to_owned())
    }
}

fn check_companions(stage: &MigrationCopy, leaf: &str) -> Result<()> {
    for extension in ["-wal", "-shm", "-journal"] {
        match stage.source_identity(&format!("{leaf}{extension}")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn page_bytes(connection: &Connection) -> Result<u64> {
    let pages: u64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let size: u64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    pages.checked_mul(size).context("backup database size overflow")
}

#[cfg(test)]
#[path = "backup_export_source_tests.rs"]
mod tests;
