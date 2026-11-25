//! Pin the exact serving database, then copy its committed WAL generation on
//! a worker. A raw copy is private and may contain grants; it is not an export.
use super::{Connection, LocalStore, OpenFlags};
use anyhow::{ensure, Context, Result};
use respire_spawn::owner_credential::{MigrationCopy, MigrationSource};
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
    source: SourceIdentity,
    source_pin: MigrationSource,
}

/// Owns the native identity-checked private stage until the caller drops it.
/// Never stream this raw copy; detached grant stripping and Vault seal follow.
pub struct BackupExportCopy {
    stage: MigrationCopy,
    leaf: String,
    identity: [u64; 2],
    source: SourceIdentity,
}

struct SourceIdentity {
    path: PathBuf,
    leaf: String,
    directory: [u64; 2],
    file: [u64; 2],
}

impl SourceIdentity {
    fn check_live(&self, check_wal: bool) -> Result<()> {
        let parent = self.path.parent().context("backup source lost its directory")?;
        let current = MigrationSource::open(parent, false)?;
        ensure!(
            current.identity()? == self.directory && current.file_identity(&self.leaf)? == self.file,
            "backup source generation is no longer named"
        );
        if check_wal {
            check_companions(&current, &self.leaf)?;
        }
        Ok(())
    }
}

impl LocalStore {
    /// Short admission only. Select the store from the verified owner scope
    /// before calling; `stage_parent` is the verified owner account root,
    /// not the selected child, and never comes from an HTTP path.
    pub fn prepare_backup_export(&self, stage_parent: &Path) -> Result<BackupExportSource> {
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
        ensure!(stage_parent.canonicalize()? == stage_parent, "backup stage root is not canonical");
        let stage = MigrationCopy::create(stage_parent, &stage_leaf)?;
        let stage_identity = stage.source_identity(&stage_leaf)?;
        let source_pin = MigrationSource::open(&parent, true)?;
        let source = SourceIdentity {
            path: parent.join(&source_leaf),
            directory: source_pin.identity()?,
            file: source_pin.file_identity(&source_leaf)?,
            leaf: source_leaf,
        };
        check_companions(&source_pin, &source.leaf)?;
        let reader = Connection::open_with_flags(
            &source.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        reader.busy_timeout(Duration::ZERO)?;
        let pinned = BackupExportSource {
            reader, stage, stage_leaf, stage_identity, source, source_pin,
        };
        pinned.check_paths()?;
        Ok(pinned)
    }
}

impl BackupExportSource {
    fn check_paths(&self) -> Result<()> {
        ensure!(
            self.source_pin.file_identity(&self.source.leaf)? == self.source.file
                && self.stage.source_identity(&self.stage_leaf)? == self.stage_identity,
            "backup source or stage changed"
        );
        self.source.check_live(true)
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
        // Do not retain a selected child file or directory in the response.
        drop(self.reader);
        drop(self.source_pin);
        Ok(BackupExportCopy {
            source: self.source,
            stage: self.stage, leaf: self.stage_leaf, identity: self.stage_identity,
        })
    }
}

impl BackupExportCopy {
    pub fn source_database(&self) -> &Path {
        &self.source.path
    }

    /// Retain this owner while validating or sanitizing. No path from a client
    /// may address a staged copy, and it is never a restore authorization.
    pub fn checked_path(&self) -> Result<PathBuf> {
        let path = self.stage.path();
        ensure!(
            path.canonicalize()? == path && self.stage.source_identity(&self.leaf)? == self.identity,
            "backup stage changed"
        );
        self.source.check_live(false)?;
        Ok(path.to_owned())
    }
}

fn check_companions(source: &MigrationSource, leaf: &str) -> Result<()> {
    for extension in ["-wal", "-shm", "-journal"] {
        match source.file_identity(&format!("{leaf}{extension}")) {
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
