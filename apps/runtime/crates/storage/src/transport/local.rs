//! transport::local — local authoritative working store (SQLite)
//!
//! local-first: search / dedup / merge are all local. This store holds ciphertext + local plaintext index columns
//! (kind/tags/title/project/computer + plaintext embedding BLOB),
//! those plaintext columns serve local search only and are **never uploaded** (StoredMemory serde skip).
//!
//! Supported TEXT-ID schemas retain their rows during upgrades. Historical
//! INTEGER-ID demos use a different ciphertext format and require explicit export.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::MemoryTransport;
use crate::memory::model::StoredMemory;

mod catalog;
pub use catalog::ActiveTreeCatalog;
mod feedback;
pub use feedback::FeedbackCounts;
mod witness;
pub use witness::{WitnessReport,witness_checkpoint_file,register_witness_functions};
mod conflicts;
mod conflict_metadata;
pub use conflict_metadata::{ConflictMetadata, ConflictMetadataState};
mod grants;
mod retrieval;
mod resident;
pub use resident::MemorySourceChanged;
mod related;
mod revision;
pub use revision::read_memory_revision;
mod review;
pub use review::ReviewHistoryRow;
mod review_snapshot;
pub use review_snapshot::ReviewSnapshot;
mod scoped_metadata;
pub use scoped_metadata::{ScopedQueryLogRow, ScopedReviewRow};
mod sync;
pub use grants::AccessGrant;
pub use retrieval::IndexSourceChanged;
pub use sync::SyncVersionNotFound;

/// Query-log row (query-log command output). candidates=search candidates; adopted=used on a write (weak signal);
/// good/bad=model self-grade (main post-train signal: useful / misleading).
#[derive(serde::Serialize)]
pub struct QueryLogRow {
    pub request_id:Option<String>,
    pub id: i64,
    pub ts: String,
    pub query: String,
    pub project: String,
    pub scope: String,
    pub candidates: Vec<String>,
    pub adopted: Vec<String>,
    pub good: Vec<String>,
    pub bad: Vec<String>,
}

/// Query-log stats.
#[derive(serde::Serialize)]
pub struct QueryLogStats {
    pub total: i64,
    /// Queries with at least one adopted id
    pub with_adopted: i64,
    /// Queries with an empty candidate set (search negatives)
    pub empty_candidates: i64,
    /// Model self-grade "useful" total (post-train chosen)
    pub good_total: i64,
    /// Model self-grade "misleading" total (post-train rejected)
    pub bad_total: i64,
    /// Top adopted entries (id, count)
    pub top: Vec<(String, i64)>,
}

/// Run history for one procedure (skill/knowledge entry). Local-only.
#[derive(serde::Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillRunStats {
    pub ok: i64,
    pub fail: i64,
    /// verdicts that attribute nothing (other changes interfered / no observable
    /// effect): neither success nor failure, so ordering ignores them.
    pub confounded: i64,
    pub no_effect: i64,
    /// ok | fail | confounded | no_effect | "" (never run)
    pub last_status: String,
    pub last_ts: i64,
}

pub struct LocalStore {
    connection: Connection,
}

/// A local safety gate only; projections and history do not authenticate ciphertext.
pub fn has_memory_metadata(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_history)
         OR EXISTS(SELECT 1 FROM memories
           WHERE memory_contract IS NOT 0 OR summary IS NOT NULL
             OR source_refs IS NOT '[]' OR valid_until IS NOT NULL
             OR review_at IS NOT NULL OR artifact IS NOT NULL OR verification IS NOT NULL
             OR kind IN ('hypothesis','reflection','self'))",
        [],
        |row| row.get(0),
    )?)
}

fn map_audit(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "entry_id": row.get::<_, String>(0)?,
        "action": row.get::<_, String>(1)?,
        "title": row.get::<_, String>(2)?,
        "content_head": row.get::<_, String>(3)?,
        "actor": row.get::<_, String>(4)?,
        "ts": row.get::<_, String>(5)?,
    }))
}

impl LocalStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data dir: {}", parent.display()))?;
            respire_core_sdk::set_index_root(&std::fs::canonicalize(parent)?)?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open local memory db: {}", path.display()))?;
        connection.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 30000;
            ",
        )?;
        migrate(&connection)?;
        connection.execute_batch(include_str!("local/sync_schema.sql"))?;
        connection.execute_batch(include_str!("local/resident_schema.sql"))?;
        let store = Self { connection };
        store.initialize_sync_storage()?;
        store.maintain_sync_storage()?;
        Ok(store)
    }

    /// Connect to a library already initialized by its runtime owner, without recovery writes.
    pub fn open_existing(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            respire_core_sdk::set_index_root(&std::fs::canonicalize(parent)?)?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| {
            format!("failed to open initialized local memory db: {}", path.display())
        })?;
        witness::register_witness_functions(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 30000;")?;
        connection
            .prepare("SELECT seq, op_id, state FROM sync_outbox LIMIT 0")
            .context("runtime library schema is not initialized")?;
        Ok(Self { connection })
    }

    /// Benchmark an initialized library without migrations or database writes.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            respire_core_sdk::set_index_root(&std::fs::canonicalize(parent)?)?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ).with_context(|| format!("failed to open read-only memory db: {}", path.display()))?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        witness::register_witness_functions(&connection)?;
        Ok(Self { connection })
    }

    /// Write (dirty semantics same as put, mark dirty=1).
    pub fn put_inner(&self, memory: &StoredMemory, dirty: bool) -> Result<bool> {
        self.put_inner_policy(memory, dirty, false)
    }

    /// Session refreshes must not replace pending local work, even with a newer
    /// source/import stamp. Keep the guard and write under the same write lock.
    /// Sync and ordinary user writes retain their existing LWW policy.
    pub fn put_session_import(&self, memory: &StoredMemory) -> Result<bool> {
        let immediate = self.connection.is_autocommit().then(||
            rusqlite::Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)).transpose()?;
        let protected: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1 AND (dirty=1 OR deleted=1))",
            params![memory.id], |row| row.get(0),
        )?;
        let wrote = if protected { false } else { self.put_inner(memory, true)? };
        if let Some(immediate) = immediate { immediate.commit()?; }
        Ok(wrote)
    }

    fn put_inner_policy(&self, memory: &StoredMemory, dirty: bool, authoritative: bool) -> Result<bool> {
        let immediate = self.connection.is_autocommit().then(||
            rusqlite::Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)).transpose()?;
        self.connection
            .execute_batch("SAVEPOINT memory_with_chunks")?;
        let result = self.put_memory_and_chunks(memory, dirty, authoritative);
        if result.is_err() {
            self.connection
                .execute_batch("ROLLBACK TO memory_with_chunks")?;
        }
        self.connection
            .execute_batch("RELEASE memory_with_chunks")?;
        if result.is_ok() { if let Some(immediate) = immediate { immediate.commit()?; } }
        result
    }

    fn put_memory_and_chunks(&self, memory: &StoredMemory, dirty: bool, authoritative: bool) -> Result<bool> {
        // last-write-wins: same id and existing updated_at is newer → drop this write;
        // a tie (same updated_at) and local dirty=1 → also refuse — unpushed local edits (attach/delete) must not be overwritten by a sync loop
        // (hit 2026-09-08: after batch attach, sync reconcile pulled a same-stamp remote and wiped the unpushed attach).
        let existing: Option<(String, i64)> = self
            .connection
            .query_row(
                "SELECT updated_at, dirty FROM memories WHERE id = ?1",
                params![memory.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((old, was_dirty)) = existing {
            let order = super::compare_timestamps(&old, &memory.updated_at)?;
            if !authoritative && (order.is_gt() || (order.is_eq() && was_dirty == 1)) {
                return Ok(false); // local already has a newer / unpushed version
            }
        }
        // Preserve legacy derived bytes only for an unchanged encrypted source.
        // New writers never provide or persist raw Core features.
        let emb_blob = self.connection.query_row(
            "SELECT embedding FROM memories WHERE id=?1 AND ciphertext=?2",
            params![memory.id, memory.ciphertext], |row| row.get::<_, Option<Vec<u8>>>(0),
        ).optional()?.flatten();
        memory.local_meta.validate()?;
        self.connection.execute(
            "INSERT INTO memories
                (id, user, ciphertext, nonce, embedding_enc, updated_at, deleted,
                 kind, tags, title, project, computer, embedding, created_at, dirty, parent_id, content_head, recall_count, importance,
                 device, modified_by, memory_contract, summary, source_refs, valid_until, review_at, artifact, verification)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)
             ON CONFLICT(id) DO UPDATE SET
                user=excluded.user, ciphertext=excluded.ciphertext, nonce=excluded.nonce,
                embedding_enc=excluded.embedding_enc, updated_at=excluded.updated_at,
                deleted=excluded.deleted, kind=excluded.kind, tags=excluded.tags,
                title=excluded.title, project=excluded.project, computer=excluded.computer,
                embedding=excluded.embedding, created_at=excluded.created_at,
                dirty=excluded.dirty, parent_id=excluded.parent_id, content_head=excluded.content_head,
                importance=excluded.importance, device=excluded.device, modified_by=excluded.modified_by,
                memory_contract=excluded.memory_contract, summary=excluded.summary, source_refs=excluded.source_refs,
                valid_until=excluded.valid_until, review_at=excluded.review_at,
                artifact=excluded.artifact, verification=excluded.verification,
                recall_count=COALESCE((SELECT recall_count FROM memories WHERE id=excluded.id), excluded.recall_count)",
            params![
                memory.id,
                memory.user,
                memory.ciphertext,
                memory.nonce,
                memory.embedding_enc,
                memory.updated_at,
                memory.deleted as i64,
                memory.local_kind,
                memory.local_tags,
                memory.local_title,
                memory.local_project,
                memory.local_computer,
                emb_blob,
                memory.local_created_at,
                dirty as i64,
                memory.local_parent_id,
                memory.local_content_head,
                memory.local_recall_count,
                memory.local_importance,
                memory.local_device,
                memory.local_modified_by,
                memory.local_meta.memory_contract,
                memory.local_meta.summary,
                serde_json::to_string(&memory.local_meta.source_refs)?,
                memory.local_meta.valid_until,
                memory.local_meta.review_at,
                memory.local_meta.artifact.as_ref().map(serde_json::to_string).transpose()?,
                memory.local_meta.verification.as_ref().map(serde_json::to_string).transpose()?,
            ],
        )?;
        self.store_artifact(memory, &self.retrieval_model()?)?;
        Ok(true)
    }

    /// Import already-sealed new rows: the whole batch commits or rolls back, so a partial backup cannot remain.
    pub fn import_batch(&self, memories: &[StoredMemory]) -> Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        for memory in memories {
            if !self.put_inner(memory, true)? {
                anyhow::bail!("import conflict: {}", memory.id);
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Keep a compound local edit atomic, including any reads used to plan it.
    pub fn write_transaction<T>(&self, edit: impl FnOnce() -> Result<T>) -> Result<T> {
        let transaction = rusqlite::Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let result = edit()?;
        transaction.commit()?;
        Ok(result)
    }

    /// Pull-only write (sync pull step): write content but do not mark dirty — dirty is reserved for real local edits.
    pub fn put_synced(&self, memory: &StoredMemory) -> Result<bool> {
        self.put_inner(memory, false)
    }

    /// For LWW: local updated_at of one id (None if missing).
    pub fn updated_at_of(&self, id: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT updated_at FROM memories WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Read the version from the live store so a batch does not reuse an old snapshot and mint the same stamp twice.
    pub fn edit_stamp(&self, id: &str) -> Result<String> {
        let previous: Option<String> = self
            .connection
            .query_row(
                "SELECT updated_at FROM memories WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        super::timestamp_after(previous.as_deref())
    }

    /// Dirty rows (local new/edit/delete, pending cloud push). Includes tombstones.
    pub fn all_dirty(&self) -> Result<Vec<StoredMemory>> {
        let mut statement = self.connection.prepare(
            "SELECT id, user, ciphertext, nonce, embedding_enc, updated_at, deleted,
                 kind, tags, title, project, computer, embedding, created_at, parent_id, content_head, recall_count, importance, device, modified_by,
                 memory_contract, summary, source_refs, valid_until, review_at, COALESCE(artifact,'null'), COALESCE(verification,'null')
             FROM memories WHERE dirty = 1",
        )?;
        let rows = statement.query_map([], Self::row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Live rows pending a vector: embedding column empty (not yet embedded on this machine).
    /// Called after cross-device sync — remote only syncs ciphertext; this machine recomputes vectors with its own model (frozen 2026-09-14).
    pub fn missing_embedding(&self) -> Result<Vec<StoredMemory>> {
        let mut statement = self.connection.prepare(
            "SELECT id, user, ciphertext, nonce, embedding_enc, updated_at, deleted,
                 kind, tags, title, project, computer, embedding, created_at, parent_id, content_head, recall_count, importance, device, modified_by,
                 memory_contract, summary, source_refs, valid_until, review_at, COALESCE(artifact,'null'), COALESCE(verification,'null')
             FROM memories WHERE deleted = 0 AND NOT EXISTS (SELECT 1 FROM core_artifacts a WHERE a.memory_id=memories.id AND a.source=memories.ciphertext AND a.model=?1)",
        )?;
        let rows = statement.query_map([self.active_generation_key()?], Self::row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Clear dirty (called after a successful push).
    pub fn clear_dirty(&self, id: &str) -> Result<()> {
        self.connection
            .execute("UPDATE memories SET dirty = 0 WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Causal tree: direct children of a node (tombstones? live only).
    pub fn children(&self, parent_id: &str) -> Result<Vec<StoredMemory>> {
        if parent_id.is_empty() {
            return self.roots();
        }
        let mut statement = self.connection.prepare(
            "SELECT id, user, ciphertext, nonce, embedding_enc, updated_at, deleted,
                 kind, tags, title, project, computer, embedding, created_at, parent_id, content_head, recall_count, importance, device, modified_by,
                 memory_contract, summary, source_refs, valid_until, review_at, COALESCE(artifact,'null'), COALESCE(verification,'null')
             FROM memories WHERE parent_id = ?1 AND deleted = 0 ORDER BY recall_count DESC, updated_at",
        )?;
        let rows = statement.query_map(params![parent_id], Self::row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Causal tree: root set (entries with no cause, i.e. event sources).
    pub fn roots(&self) -> Result<Vec<StoredMemory>> {
        let mut statement = self.connection.prepare(
            "SELECT id, user, ciphertext, nonce, embedding_enc, updated_at, deleted,
                 kind, tags, title, project, computer, embedding, created_at, parent_id, content_head, recall_count, importance, device, modified_by,
                 memory_contract, summary, source_refs, valid_until, review_at, COALESCE(artifact,'null'), COALESCE(verification,'null')
             FROM memories WHERE parent_id = '' AND deleted = 0 ORDER BY recall_count DESC, updated_at",
        )?;
        let rows = statement.query_map([], Self::row_to_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Causal tree: reparent (new parent) — update parent_id, bump updated_at and mark dirty (syncs),
    /// used by promote / demote and --parent reattach.
    /// Cycle check is the caller's: the new parent must not be a descendant (this store does not recurse; the command layer does).
    pub fn set_parent(&self, id: &str, new_parent_id: &str) -> Result<bool> {
        let now = self.edit_stamp(id)?;
        let n = self.connection.execute(
            "UPDATE memories SET parent_id = ?2, updated_at = ?3, dirty = 1 WHERE id = ?1 AND deleted = 0",
            params![id, new_parent_id, now],
        )?;
        Ok(n > 0)
    }

    /// Causal tree: walk the cause chain (parent→grandparent→…), return root-to-parent order (excluding self).
    pub fn ancestor_chain(&self, id: &str) -> Result<Vec<String>> {
        let mut chain = Vec::new();
        let mut cur = id.to_owned();
        for _ in 0..64 {
            let pid: Option<String> = self
                .connection
                .query_row(
                    "SELECT parent_id FROM memories WHERE id = ?1",
                    params![cur],
                    |row| row.get(0),
                )
                .ok();
            match pid {
                Some(p) if !p.is_empty() => {
                    chain.push(p.clone());
                    cur = p;
                }
                _ => break,
            }
        }
        chain.reverse(); // root first
        Ok(chain)
    }

    /// Bump hit count (after recall returns, +1 on hit entries; local heat only, not dirty, not synced).
    pub fn bump_recall_count(&self, id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE memories SET recall_count = recall_count + 1 WHERE id = ?1",
            params![id],
        )? > 0)
    }
}

mod diagnostics;
mod entries;
mod schema;

use schema::migrate;
pub use schema::validate_migration_snapshot;

#[cfg(test)]
#[path = "local/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "local/lww_tie_tests.rs"]
mod lww_tie_tests;

#[cfg(test)]
#[path = "local/skill_run_tests.rs"]
mod skill_run_tests;
