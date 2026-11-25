//! Durable outgoing versions and atomic incoming pages.
use super::LocalStore;
use crate::transport::protocol::*;
use crate::{hydrate_local, SessionKeys, StoredMemory};
use anyhow::{bail, Result};
use rusqlite::{params, Transaction, TransactionBehavior};

#[derive(Debug)]
pub struct SyncVersionNotFound;

impl std::fmt::Display for SyncVersionNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("version is no longer retained locally")
    }
}

impl std::error::Error for SyncVersionNotFound {}

#[cfg(test)]
#[path = "sync_diary_tests.rs"]
mod diary_tests;

impl LocalStore {
    /// A transport conflict receipt is not permission to discard a confirmed
    /// diary working copy. Keep distinct live diary branches visible until an
    /// explicit resolution; the canonical head and conflict bytes remain intact.
    pub(super) fn protects_diary_working_copy(&self, incoming: &StoredMemory) -> Result<bool> {
        if incoming.deleted || incoming.local_importance != "trivial" {
            return Ok(false);
        }
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM memories m WHERE m.id=?1 AND m.deleted=0 AND m.importance='trivial'
            AND (m.ciphertext<>?2 OR m.nonce<>?3 OR m.updated_at<>?4)
            AND (EXISTS(SELECT 1 FROM sync_outbox o WHERE o.id=m.id AND o.state='conflict_saved'
                AND o.ciphertext=m.ciphertext AND o.nonce=m.nonce AND o.updated_at=m.updated_at AND o.deleted=m.deleted
                AND NOT EXISTS(SELECT 1 FROM sync_inbox i JOIN sync_resolutions r ON r.epoch=i.epoch AND r.conflict_rev=i.rev
                    WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch') AND i.op_id=o.op_id))
            OR EXISTS(SELECT 1 FROM sync_inbox i WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch')
                AND i.id=m.id AND i.status IN ('conflict_saved','legacy_rejected') AND json_valid(i.wire)
                AND json_extract(i.wire,'$.ciphertext')=m.ciphertext AND json_extract(i.wire,'$.nonce')=m.nonce
                AND json_extract(i.wire,'$.updated_at')=m.updated_at AND json_extract(i.wire,'$.deleted')=m.deleted
                AND NOT EXISTS(SELECT 1 FROM sync_resolutions r WHERE r.epoch=i.epoch AND r.conflict_rev=i.rev))))",
            params![incoming.id, incoming.ciphertext, incoming.nonce, incoming.updated_at], |r| r.get(0),
        )?)
    }

    /// Read a bounded immutable batch; the same op_id always carries the same bytes.
    pub fn outgoing(&self) -> Result<Vec<Operation>> {
        self.outgoing_through(i64::MAX)
    }

    /// Move the current remote state out of the transport history before reclaiming it.
    pub(super) fn initialize_sync_storage(&self) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        Self::initialize_sync_schema(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn initialize_sync_schema(connection: &rusqlite::Connection) -> Result<()> {
        let columns = connection
            .prepare("PRAGMA table_info(sync_remote_heads)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, declaration) in [
            ("wire", "TEXT NOT NULL DEFAULT ''"),
            ("op_id", "TEXT"),
            ("decode_error", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !columns.iter().any(|column| column == name) {
                connection.execute_batch(&format!(
                    "ALTER TABLE sync_remote_heads ADD COLUMN {name} {declaration}"
                ))?;
            }
        }
        connection.execute_batch(include_str!("sync_revision_schema.sql"))?;
        Self::backfill_sync_heads(connection)
    }

    pub(super) fn backfill_sync_heads(connection: &rusqlite::Connection) -> Result<()> {
        connection.execute(
            "UPDATE sync_remote_heads AS h SET (wire,op_id,decode_error)=(
                SELECT i.wire,i.op_id,i.decode_error FROM sync_inbox i
                WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch')
                AND i.rev=h.rev AND i.id=h.id
            ) WHERE h.wire='' AND EXISTS(
                SELECT 1 FROM sync_inbox i
                WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch')
                AND i.rev=h.rev AND i.id=h.id
            )",
            [],
        )?;
        Ok(())
    }

    /// Reclaim completed payloads, keeping immutable unconfirmed operations and their dependencies.
    fn prune_sync_history(&self) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let outgoing = self.connection.execute(
            "DELETE FROM sync_outbox WHERE seq IN (
                SELECT o.seq FROM sync_outbox o WHERE
                (o.state='applied' OR o.state LIKE 'rebased:%'
                    OR (o.state='conflict_saved' AND EXISTS(
                        SELECT 1 FROM sync_inbox i WHERE i.op_id=o.op_id))
                    OR (o.state='legacy' AND EXISTS(
                        SELECT 1 FROM memories m WHERE m.id=o.id AND m.ciphertext=o.ciphertext
                        AND m.nonce=o.nonce AND m.updated_at=o.updated_at AND m.deleted=o.deleted)))
                AND NOT EXISTS(SELECT 1 FROM sync_outbox c
                    WHERE c.state='pending' AND c.parent_op_id=o.op_id)
                AND NOT EXISTS(SELECT 1 FROM sync_resolution_outbox r WHERE r.restore_op_id=o.op_id)
                LIMIT 1000
            )",
            [],
        )?;
        let incoming = self.connection.execute(
            "DELETE FROM sync_inbox WHERE rowid IN (
                SELECT i.rowid FROM sync_inbox i WHERE
                ((i.status='applied' AND i.decode_error='' AND NOT EXISTS(
                    SELECT 1 FROM sync_remote_heads h WHERE h.id=i.id AND h.rev=i.rev AND h.wire=''))
                    OR (i.status='reset_saved' AND EXISTS(
                        SELECT 1 FROM sync_remote_heads h WHERE h.id=i.id AND h.wire=i.wire))
                    OR EXISTS(SELECT 1 FROM sync_resolutions r
                        WHERE r.epoch=i.epoch AND r.conflict_rev=i.rev))
                AND NOT EXISTS(SELECT 1 FROM sync_resolution_outbox r
                    WHERE r.epoch=i.epoch AND r.conflict_rev=i.rev)
                AND NOT EXISTS(SELECT 1 FROM sync_outbox o WHERE o.op_id=i.op_id)
                LIMIT 1000
            )",
            [],
        )?;
        let analysis = self.connection.execute(
            "DELETE FROM sync_conflict_analysis WHERE rowid IN (
                SELECT a.rowid FROM sync_conflict_analysis a WHERE NOT EXISTS(
                    SELECT 1 FROM sync_inbox i WHERE i.epoch=a.epoch AND i.rev=a.rev)
                LIMIT 1000
            )",
            [],
        )?;
        tx.commit()?;
        Ok(outgoing + incoming + analysis)
    }

    /// Run outside a transaction under the runtime's local write gate.
    pub fn maintain_sync_storage(&self) -> Result<()> {
        while self.prune_sync_history()? != 0 {}
        let pages: i64 = self.connection.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: i64 = self.connection.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let page_size: i64 = self.connection.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let vacuum = free * page_size >= 16 * 1024 * 1024 && free * 4 >= pages;
        let checkpoint_pending = self.meta_get("sync_storage_checkpoint_pending")?.as_deref() == Some("1");
        if !vacuum && !checkpoint_pending {
            return Ok(());
        }
        // A reader can arrive between VACUUM and its checkpoint; retry truncation even with no free pages.
        self.meta_set("sync_storage_checkpoint_pending", "1")?;
        self.connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let compact = (|| -> rusqlite::Result<bool> {
            let busy: i64 = self.connection.query_row(
                "PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            if busy != 0 {
                return Ok(false);
            }
            if vacuum {
                self.connection.execute_batch("VACUUM")?;
            }
            let busy: i64 = self.connection.query_row(
                "PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            Ok(busy == 0)
        })();
        self.connection.busy_timeout(std::time::Duration::from_secs(30))?;
        match compact {
            Ok(true) => self.meta_set("sync_storage_checkpoint_pending", "0"),
            Ok(false) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(error.code, rusqlite::ErrorCode::DiskFull | rusqlite::ErrorCode::SystemIoFailure) => {
                    eprintln!("automatic SQLite compaction deferred: {error}");
                    Ok(())
                }
            Err(error) => Err(anyhow::anyhow!(error).context(
                "automatic SQLite compaction failed; current memories and pending operations are retained"
            )),
        }
    }

    pub fn outgoing_boundary(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT coalesce(max(seq),0) FROM sync_outbox WHERE state='pending'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn outgoing_through(&self, boundary: i64) -> Result<Vec<Operation>> {
        let mut stmt=self.connection.prepare("SELECT op_id,id,base_rev,parent_op_id,user,ciphertext,nonce,embedding_enc,updated_at,deleted FROM sync_outbox WHERE state='pending' AND seq<=?2 ORDER BY seq LIMIT ?1")?;
        let rows = stmt.query_map([PUSH_ITEMS as i64, boundary], |r| {
            Ok(Operation {
                op_id: r.get(0)?,
                base_rev: r.get(2)?,
                parent_op_id: r.get(3)?,
                blob: StoredMemory {
                    id: r.get(1)?,
                    user: r.get(4)?,
                    ciphertext: r.get(5)?,
                    nonce: r.get(6)?,
                    embedding_enc: r.get(7)?,
                    updated_at: r.get(8)?,
                    deleted: r.get::<_, i64>(9)? != 0,
                    ..StoredMemory::new_pending(String::new(), String::new())
                },
            })
        })?;
        let mut batch = Vec::new();
        let mut bytes = 256;
        for row in rows {
            let op = row?;
            let size = serde_json::to_vec(&op)?.len() + 1;
            if !batch.is_empty() && bytes + size > BATCH_BYTES {
                break;
            }
            if bytes + size > BATCH_BYTES {
                bail!(
                    "single sync item exceeds server batch limit: {} (kept locally)",
                    op.blob.id
                );
            }
            bytes += size;
            batch.push(op);
        }
        Ok(batch)
    }

    /// Scope prevents accidentally acknowledging an outbox against a different vault.
    pub fn begin_sync_epoch(&self, epoch: &str) -> Result<()> {
        self.begin_sync_epoch_requeued(epoch).map(|_| ())
    }

    /// Report an identity migration so a finite caller cannot silently expand its scope.
    pub fn begin_sync_epoch_requeued(&self, epoch: &str) -> Result<bool> {
        let mut requeued = false;
        if let Some(old) = self.meta_get("sync_v2_epoch")? {
            if old != epoch {
                bail!(
                    "server sync epoch changed; local edits kept; rebuild the snapshot explicitly"
                );
            }
        } else {
            let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let legacy_parent:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sync_outbox p JOIN sync_outbox c ON c.parent_op_id=p.op_id WHERE p.state='legacy' AND c.state='pending')",[],|r|r.get(0))?;
            if legacy_parent {
                self.requeue_pending()?;
                requeued = true;
            }
            self.meta_set("sync_v2_epoch", epoch)?;
            tx.commit()?;
        }
        Ok(requeued)
    }

    /// Receipts clear only the sent version, never a later local edit.
    pub fn acknowledge(
        &self,
        sent: &[Operation],
        receipts: &[Receipt],
        legacy: bool,
    ) -> Result<usize> {
        if sent.len() != receipts.len() {
            bail!("sync ack count mismatch");
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut applied = 0;
        for (op, r) in sent.iter().zip(receipts) {
            if r.op_id != op.op_id
                || !matches!(
                    r.status.as_str(),
                    "applied" | "conflict_saved" | "legacy_rejected"
                )
            {
                bail!("sync ack does not match operations");
            }
            self.connection.execute(
                "UPDATE sync_outbox SET state=?2 WHERE op_id=?1 AND state='pending'",
                params![op.op_id, if legacy { "legacy" } else { r.status.as_str() }],
            )?;
            self.connection.execute("UPDATE memories SET dirty=0 WHERE id=?1 AND ciphertext=?2 AND nonce=?3 AND updated_at=?4 AND deleted=?5",
                params![op.blob.id,op.blob.ciphertext,op.blob.nonce,op.blob.updated_at,i64::from(op.blob.deleted)])?;
            if r.status == "applied" {
                applied += 1;
            }
        }
        tx.commit()?;
        Ok(applied)
    }

    /// Raw ciphertext and cursor commit together, even if local keys cannot decode it.
    pub fn apply_page(&self, keys: &SessionKeys, page: &Page, snapshot: bool) -> Result<usize> {
        let key = if snapshot {
            "sync_v2_snapshot_cursor"
        } else {
            "sync_v2_cursor"
        };
        let previous = self
            .meta_get(key)?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if page.cursor < previous
            || page.cursor > page.until
            || (page.has_more && page.cursor == previous)
        {
            bail!("server returned an invalid page cursor");
        }
        let mut last = previous;
        let tx = self.connection.unchecked_transaction()?;
        for change in &page.changes {
            if change.rev <= last || change.rev > page.cursor {
                bail!("server page version order is wrong");
            }
            last = change.rev;
            let wire = serde_json::to_string(&change.blob)?;
            let mut b = change.blob.clone();
            // Vectors are device-local derived state, not a legacy encrypted cache.
            b.embedding_enc.clear();
            let error = if b.deleted && b.ciphertext.is_empty() {
                String::new()
            } else {
                hydrate_local(keys, &mut b)
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_default()
            };
            self.connection.execute("INSERT INTO sync_inbox(epoch,rev,id,status,op_id,wire,decode_error) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(epoch,rev) DO UPDATE SET decode_error=excluded.decode_error",
                params![page.epoch,change.rev,b.id,change.status,change.op_id,wire,error])?;
            if change.status == "applied" {
                self.connection.execute("INSERT INTO sync_remote_heads(id,rev,wire,op_id,decode_error) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET rev=excluded.rev,wire=excluded.wire,op_id=excluded.op_id,decode_error=excluded.decode_error WHERE excluded.rev>=sync_remote_heads.rev",params![b.id,change.rev,wire,change.op_id,error])?;
                let pending: bool = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE id=?1 AND state='pending')",
                    [&b.id],
                    |r| r.get(0),
                )?;
                if !pending && error.is_empty() && !self.protects_diary_working_copy(&b)? {
                    self.put_inner_policy(&b, false, true)?;
                    self.connection.execute("INSERT INTO sync_base(id,rev) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET rev=excluded.rev",params![b.id,change.rev])?;
                }
            }
        }
        self.meta_set(key, &page.cursor.to_string())?;
        if snapshot {
            self.meta_set("sync_v2_snapshot_until", &page.until.to_string())?;
            if !page.has_more {
                self.meta_set("sync_v2_cursor", &page.cursor.to_string())?;
                self.meta_set("sync_v2_snapshot_done", "1")?;
            }
        }
        tx.commit()?;
        Ok(page.changes.len())
    }

    /// Restore the latest received canonical state after acknowledgments without
    /// re-downloading the full account or destroying pending outgoing versions.
    pub fn materialize_received(&self, keys: &SessionKeys) -> Result<()> {
        let mut after = String::new();
        while let Some(next) = self.materialize_received_batch(keys, &after)? {
            after = next;
        }
        Ok(())
    }

    pub fn materialize_received_batch(
        &self,
        keys: &SessionKeys,
        after: &str,
    ) -> Result<Option<String>> {
        // Materialization reads before writing; reserve the writer before establishing its snapshot.
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut stmt = self.connection.prepare(
            "SELECT h.id,h.wire,h.rev FROM sync_remote_heads h
            LEFT JOIN sync_base e ON e.id=h.id
            WHERE h.id>?1 AND h.wire<>'' AND h.decode_error='' AND (e.rev IS NULL OR e.rev<>h.rev)
            AND NOT EXISTS (SELECT 1 FROM sync_outbox o INDEXED BY sync_outbox_unconfirmed
                WHERE o.id=h.id AND o.state='pending') ORDER BY h.id LIMIT ?2",
        )?;
        let wires = stmt
            .query_map(params![after, PUSH_ITEMS as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = wires.last().map(|row| row.0.clone());
        for (id, wire, rev) in wires {
            let pending: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE id=?1 AND state='pending')",
                [&id],
                |r| r.get(0),
            )?;
            if pending {
                continue;
            }
            let mut b: StoredMemory = serde_json::from_str(&wire)?;
            let unchanged:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1 AND ciphertext=?2 AND updated_at=?3 AND deleted=?4)",
                params![b.id,b.ciphertext,b.updated_at,i64::from(b.deleted)],|r|r.get(0))?;
            if !unchanged {
                b.embedding_enc.clear();
                if !(b.deleted && b.ciphertext.is_empty()) {
                    hydrate_local(keys, &mut b)?;
                }
                if self.protects_diary_working_copy(&b)? {
                    continue;
                }
                self.put_inner_policy(&b, false, true)?;
            }
            self.connection.execute("INSERT INTO sync_base(id,rev) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET rev=excluded.rev",params![b.id,rev])?;
        }
        tx.commit()?;
        Ok(next)
    }

    /// Counts have distinct meanings: transport completion is not content equality.
    pub fn sync_counts(&self) -> Result<(i64, i64, i64)> {
        Ok((
            self.connection.query_row(
                "SELECT count(*) FROM sync_outbox WHERE state='pending'",
                [],
                |r| r.get(0),
            )?,
            self.connection.query_row(
                "SELECT count(*) FROM sync_inbox i WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch')
                AND i.status IN ('conflict_saved','legacy_rejected') AND NOT EXISTS(SELECT 1 FROM sync_resolutions r WHERE r.epoch=i.epoch AND r.conflict_rev=i.rev)",
                [],
                |r| r.get(0),
            )?,
            self.connection.query_row(
                "SELECT count(*) FROM sync_inbox WHERE decode_error<>''",
                [],
                |r| r.get(0),
            )?,
        ))
    }

    /// Explicit recovery after restoring a server backup. Pending edits get fresh
    /// identities with unknown bases; durable replacement copies permit later reclamation.
    pub fn reset_sync_snapshot(&self) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        self.requeue_pending()?;
        // A reset invalidates unconfirmed recovery decisions even when their
        // content was already acknowledged. Keep operation/history and intent
        // identities, but never resend or reuse their old resolution binding.
        self.connection.execute(
            "DELETE FROM sync_resolution_outbox AS r WHERE EXISTS(
                SELECT 1 FROM sync_recovery_intents i WHERE i.epoch=r.epoch
                AND i.conflict_rev=r.conflict_rev AND i.op_id=r.restore_op_id)",
            [],
        )?;
        // A server backup can replace current content; keep one recovery snapshot, not its edit history.
        self.connection.execute(
            "INSERT INTO sync_inbox(epoch,rev,id,status,op_id,wire,decode_error)
            SELECT (SELECT value FROM meta WHERE key='sync_v2_epoch'),rev,id,'reset_saved',op_id,wire,decode_error
            FROM sync_remote_heads WHERE wire<>''
                AND (SELECT value FROM meta WHERE key='sync_v2_epoch') IS NOT NULL
            ON CONFLICT(epoch,rev) DO UPDATE SET status='reset_saved',
                wire=excluded.wire,op_id=excluded.op_id,decode_error=excluded.decode_error",
            [],
        )?;
        self.connection.execute("DELETE FROM sync_base", [])?;
        self.connection
            .execute("DELETE FROM sync_remote_heads", [])?;
        self.connection
            .execute("DELETE FROM meta WHERE key LIKE 'sync_v2_%'", [])?;
        tx.commit()?;
        Ok(())
    }

    fn requeue_pending(&self) -> Result<()> {
        let archived = format!("rebased:{}", uuid::Uuid::new_v4());
        self.connection.execute(
            "UPDATE sync_outbox SET state=?1 WHERE state='pending'",
            [&archived],
        )?;
        self.connection.execute("INSERT INTO sync_outbox(op_id,id,user,ciphertext,nonce,embedding_enc,updated_at,deleted)
            SELECT lower(hex(randomblob(16))),id,user,ciphertext,nonce,embedding_enc,updated_at,deleted
            FROM sync_outbox WHERE state=?1 ORDER BY seq",[&archived])?;
        self.connection.execute("UPDATE sync_outbox AS c SET parent_op_id=(SELECT p.op_id FROM sync_outbox p WHERE p.state='pending' AND p.id=c.id AND p.seq<c.seq ORDER BY p.seq DESC LIMIT 1) WHERE c.state='pending'",[])?;
        Ok(())
    }
}

mod history;

#[cfg(test)]
#[path = "sync/tests.rs"]
mod tests;
