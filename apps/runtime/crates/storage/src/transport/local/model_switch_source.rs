//! Immutable source and short publication CAS for an isolated model build.
use super::*;
use anyhow::ensure;
use respire_core_sdk::profiles::{Selection, ACTIVE_SELECTION_KEY};
use rusqlite::types::ValueRef;
use std::{fs, path::Path, time::Duration};

const MAX_SOURCE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug)]
pub struct ModelSwitchSourceTooLarge;
impl std::fmt::Display for ModelSwitchSourceTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("model switch source row exceeds staging budget")
    }
}
impl std::error::Error for ModelSwitchSourceTooLarge {}

/// An in-process capability for the exact verified storage generation. The
/// owner/profile/instance fence remains with the Runtime coordinator.
pub struct ModelSwitchSourceStamp {
    database: String,
    pinned_database: DatabaseIdentity,
    revision: String,
    selection: Option<String>,
    legacy_model: Option<String>,
    task: uuid::Uuid,
    target: String,
}

/// Only stored live rows, not an open SQLite connection or a selected model.
/// Existing derived artifacts are intentionally not attached.
#[cfg(test)]
struct ModelSwitchSourceSnapshot {
    pub source: ModelSwitchSourceStamp,
    pub rows: Vec<StoredMemory>,
}

fn meta(connection: &Connection, key: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}

fn pending_task(connection: &Connection) -> Result<uuid::Uuid> {
    let raw = pending_task_id(connection)?;
    let id = uuid::Uuid::parse_str(&raw)?;
    ensure!(
        id.get_version_num() == 4 && id.to_string() == raw,
        "model build admission identity is invalid"
    );
    Ok(id)
}

pub(super) fn pending_task_id(connection: &Connection) -> Result<String> {
    let raw = meta(connection, "embedding_pending_task_v1")?
        .context("model build admission is unavailable")?;
    let task: serde_json::Value = serde_json::from_str(&raw)?;
    let raw = task["id"]
        .as_str()
        .context("model build admission identity is unavailable")?;
    Ok(raw.to_owned())
}

#[cfg(unix)]
type DatabaseIdentity = (u64, u64);
#[cfg(windows)]
type DatabaseIdentity = same_file::Handle;

#[cfg(unix)]
fn pin_database(path: &Path) -> Result<DatabaseIdentity> {
    use std::os::unix::fs::MetadataExt;
    // The resident SQLite connection keeps its inode alive. Never open and
    // close an unrelated FD to that inode: POSIX would drop SQLite's locks.
    let file = fs::symlink_metadata(path)?;
    ensure!(
        file.file_type().is_file(),
        "model switch database must be a regular file"
    );
    Ok((file.dev(), file.ino()))
}

#[cfg(windows)]
fn pin_database(path: &Path) -> Result<DatabaseIdentity> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const SHARE_READ: u32 = 0x1;
    const SHARE_WRITE: u32 = 0x2;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const REPARSE_POINT: u32 = 0x400;
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ | SHARE_WRITE)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & REPARSE_POINT == 0,
        "model switch database must be a regular file, not a reparse point"
    );
    Ok(same_file::Handle::from_file(file)?)
}

fn check_database_path(file: &DatabaseIdentity, path: &Path) -> Result<()> {
    ensure!(
        pin_database(path)? == *file,
        "model switch database identity changed"
    );
    Ok(())
}

pub(super) fn pending_target(connection: &Connection) -> Result<String> {
    let wire = meta(connection, "embedding_pending_selection_v1")?
        .context("model build target is unavailable")?;
    let target: Selection = serde_json::from_str(&wire)?;
    ensure!(
        !target.generation.is_empty() && serde_json::to_string(&target)? == wire,
        "model build target is not canonical"
    );
    Ok(wire)
}

/// Exactly one prepared locator for a live row in the captured owner library.
pub struct ModelSwitchArtifact<'a> {
    pub id: &'a str,
    pub ciphertext: &'a str,
    pub opaque: &'a [u8],
}

pub const MODEL_SWITCH_MAX_PROMOTION_ROWS: usize = 512;
pub const MODEL_SWITCH_MAX_PROMOTION_ROW_BYTES: usize = 256 * 1024;
pub const MODEL_SWITCH_MAX_PROMOTION_BYTES: usize = 2 * 1024 * 1024;

pub(super) const INSERT_INLINE_ARTIFACT: &str =
    "INSERT OR REPLACE INTO core_artifacts(memory_id,model,source,artifact)
     SELECT id,?2,ciphertext,?4 FROM memories WHERE id=?1 AND ciphertext=?3 AND deleted=0";

pub(super) fn validate_inline_artifact(opaque: &[u8]) -> Result<()> {
    ensure!(
        !opaque.is_empty() && !opaque.starts_with(b"rsi1:"),
        "expected inline opaque candidate artifact"
    );
    Ok(())
}

impl ModelSwitchSourceStamp {
    pub(super) fn check(&self, connection: &Connection, target: &Selection) -> Result<()> {
        ensure!(
            connection.path() == Some(self.database.as_str()),
            "model build source is not the selected library"
        );
        check_database_path(&self.pinned_database, Path::new(&self.database))?;
        ensure!(
            meta(connection, ACTIVE_SELECTION_KEY)? == self.selection
                && meta(connection, "retrieval_model")? == self.legacy_model,
            "active model selection changed before model publication; resume the switch"
        );
        ensure!(
            pending_task(connection)? == self.task,
            "model build admission changed before publication; resume the switch"
        );
        ensure!(
            pending_target(connection)? == self.target
                && serde_json::to_string(target)? == self.target,
            "prepared target changed before model publication; resume the switch"
        );
        Ok(())
    }
}

/// A read-only WAL snapshot opened from the retained serving store. Send the
/// Reader to the owning preparation job, not the resident SQLite connection.
/// Stage the ciphertext promptly: keeping a WAL snapshot open during native
/// inference would unnecessarily retain newer WAL frames.
pub struct ModelSwitchRead {
    reader: LocalStore,
    source: ModelSwitchSourceStamp,
}

impl ModelSwitchRead {
    /// Copy one encrypted row at a time to the job-owned staging area.
    /// Source rows cannot be reread or silently refreshed within this job.
    pub fn stage(
        self,
        mut check: impl FnMut() -> Result<()>,
        mut write: impl FnMut(StoredMemory, usize) -> Result<()>,
    ) -> Result<ModelSwitchSourceStamp> {
        check()?;
        let Self { reader, source } = self;
        {
            let mut statement = reader.catalog_statement(false)?;
            let mut cursor = statement.query([])?;
            while let Some(row) = cursor.next()? {
                check()?;
                let mut bytes = std::mem::size_of::<StoredMemory>();
                for index in 0..row.as_ref().column_count() {
                    if let ValueRef::Text(data) | ValueRef::Blob(data) = row.get_ref(index)? {
                        bytes = bytes.saturating_add(data.len());
                    }
                }
                ensure!(bytes <= MAX_SOURCE_BYTES, ModelSwitchSourceTooLarge);
                write(LocalStore::row_to_memory(row)?, bytes)?;
            }
        }
        check()?;
        reader.connection.execute_batch("COMMIT")?;
        check_database_path(&source.pinned_database, Path::new(&source.database))?;
        Ok(source)
    }
}

impl LocalStore {
    /// The initial detached dispatcher path only replaces the main index;
    /// caller retains the established generation-safe diary path otherwise.
    pub fn has_live_switch_diary(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM memories WHERE deleted=0 AND importance='trivial')",
            [],
            |row| row.get(0),
        )?)
    }

    /// Establish a committed WAL snapshot under the owner scope without migrations
    /// or Core index-root side effects. The serving connection must remain live.
    pub fn begin_model_switch_read(&self) -> Result<ModelSwitchRead> {
        let database = self.library_identity()?.to_owned();
        ensure!(
            !database.is_empty(),
            "model build requires a persistent library"
        );
        let pinned_database = pin_database(Path::new(&database))?;
        let reader = Self {
            connection: Connection::open_with_flags(
                &database,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?,
        };
        reader.connection.busy_timeout(Duration::from_secs(1))?;
        reader.connection.execute_batch("BEGIN DEFERRED")?;
        let source = ModelSwitchSourceStamp {
            database,
            pinned_database,
            revision: reader.memory_revision()?,
            selection: meta(&reader.connection, ACTIVE_SELECTION_KEY)?,
            legacy_model: meta(&reader.connection, "retrieval_model")?,
            task: pending_task(&reader.connection)?,
            target: pending_target(&reader.connection)?,
        };
        check_database_path(&source.pinned_database, Path::new(&source.database))?;
        Ok(ModelSwitchRead { reader, source })
    }

    #[cfg(test)]
    fn capture_model_switch_source(
        &self,
        check: impl FnMut() -> Result<()>,
    ) -> Result<ModelSwitchSourceSnapshot> {
        self.capture_source_with_budget(check, MAX_SOURCE_BYTES)
    }

    #[cfg(test)]
    fn capture_source_with_budget(
        &self,
        check: impl FnMut() -> Result<()>,
        mut budget: usize,
    ) -> Result<ModelSwitchSourceSnapshot> {
        let mut rows = Vec::new();
        let source = self.begin_model_switch_read()?.stage(check, |row, bytes| {
            budget = budget
                .checked_sub(bytes)
                .context("model switch source exceeds snapshot budget")?;
            rows.push(row);
            Ok(())
        })?;
        Ok(ModelSwitchSourceSnapshot { source, rows })
    }

    pub(super) fn model_switch_foreground_generation(&self) -> Result<String> {
        if let Some(selection) = self
            .active_selection()?
            .filter(|selection| !selection.generation.is_empty())
        {
            return Ok(selection.generation);
        }
        self.active_generation_key()
    }

    pub fn validate_model_switch_stage(
        &self,
        target: &Selection,
        source: &ModelSwitchSourceStamp,
    ) -> Result<()> {
        source.check(&self.connection, target)?;
        ensure!(
            self.memory_revision()? == source.revision,
            "library sources changed before model stage"
        );
        ensure!(
            self.model_switch_foreground_generation()? != target.generation,
            "model stage cannot modify the active generation"
        );
        Ok(())
    }

    /// Publish already validated candidate artifacts. The Runtime caller must
    /// hold the live owner/instance, profile-lock and final admission fence.
    /// Keep the original resident SQLite connection open through publication.
    /// Verify source revisions and prior selection in the same transaction
    /// as the existing complete-index check and active-selection publication.
    pub fn publish_model_switch_snapshot(
        &self,
        target: &Selection,
        source: &ModelSwitchSourceStamp,
        prepared: &[ModelSwitchArtifact<'_>],
    ) -> Result<()> {
        self.publish_selection(target, &source.revision, Some((source, prepared)))
    }
}

/// Inline opaque Core artifacts are already accepted by the existing rsi1
/// reader's inline branch. No new locator or persistent schema is needed.
pub(super) fn promote_inline(
    tx: &rusqlite::Transaction<'_>,
    generation: &str,
    rows: &[ModelSwitchArtifact<'_>],
) -> Result<()> {
    ensure!(
        rows.len() <= MODEL_SWITCH_MAX_PROMOTION_ROWS,
        "model promotion exceeds row limit"
    );
    let live_rows: i64 = tx.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM memories WHERE deleted=0 LIMIT ?1)",
        [MODEL_SWITCH_MAX_PROMOTION_ROWS as i64 + 1],
        |row| row.get(0),
    )?;
    ensure!(
        usize::try_from(live_rows)? == rows.len(),
        "prepared model switch row count does not cover the live source"
    );
    let mut distinct = std::collections::HashSet::with_capacity(rows.len());
    let mut remaining = MODEL_SWITCH_MAX_PROMOTION_BYTES;
    let mut insert = tx.prepare(INSERT_INLINE_ARTIFACT)?;
    for row in rows {
        ensure!(
            distinct.insert(row.id),
            "duplicate prepared model switch source"
        );
        let bytes = row
            .id
            .len()
            .saturating_add(row.ciphertext.len())
            .saturating_add(row.opaque.len());
        ensure!(
            bytes <= MODEL_SWITCH_MAX_PROMOTION_ROW_BYTES,
            "model promotion exceeds per-row byte limit"
        );
        remaining = remaining
            .checked_sub(bytes)
            .context("model promotion exceeds byte limit")?;
        validate_inline_artifact(row.opaque)?;
        ensure!(
            insert.execute(rusqlite::params![
                row.id,
                generation,
                row.ciphertext,
                row.opaque
            ])? == 1,
            "model promotion source is absent or changed"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
