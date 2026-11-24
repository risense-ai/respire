use super::*;

impl LocalStore {
    /// Read recoverable encrypted versions without moving either sync cursor.
    pub fn sync_history(&self, id: Option<&str>) -> Result<Vec<serde_json::Value>> {
        // Paused/read-only libraries intentionally keep their old schema until normal service resumes.
        let current_heads: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sync_remote_heads') WHERE name='wire')",
            [], |r| r.get(0),
        )?;
        let incoming = if current_heads {
            "SELECT epoch,rev,status,op_id,wire FROM sync_inbox WHERE (?1 IS NULL OR id=?1)
            UNION ALL SELECT (SELECT value FROM meta WHERE key='sync_v2_epoch'),h.rev,'applied',h.op_id,h.wire
            FROM sync_remote_heads h WHERE (?1 IS NULL OR h.id=?1) AND h.wire<>''
            AND NOT EXISTS(SELECT 1 FROM sync_inbox i WHERE i.epoch=(SELECT value FROM meta WHERE key='sync_v2_epoch') AND i.rev=h.rev)
            ORDER BY rev DESC LIMIT 200"
        } else {
            "SELECT epoch,rev,status,op_id,wire FROM sync_inbox WHERE (?1 IS NULL OR id=?1) ORDER BY rev DESC LIMIT 200"
        };
        let mut stmt = self.connection.prepare(incoming)?;
        let mut out=stmt.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?)))?
            .map(|row| {let (epoch,rev,status,op_id,wire)=row?;Ok(serde_json::json!({"epoch":epoch,"rev":rev,"status":status,"op_id":op_id,"blob":serde_json::from_str::<serde_json::Value>(&wire)?}))})
            .collect::<Result<Vec<_>>>()?;
        let mut stmt=self.connection.prepare("SELECT op_id,id,user,ciphertext,nonce,updated_at,deleted,state FROM sync_outbox WHERE (?1 IS NULL OR id=?1) ORDER BY seq DESC LIMIT 200")?;
        for row in stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, String>(7)?,
            ))
        })? {
            let (op_id, id, user, ciphertext, nonce, updated_at, deleted, status) = row?;
            out.push(serde_json::json!({"op_id":op_id,"status":status,"blob":{"id":id,"user":user,"ciphertext":ciphertext,"nonce":nonce,"updated_at":updated_at,"deleted":deleted,"embedding_enc":""}}));
        }
        Ok(out)
    }

    /// Stage an explicitly requested version for restoration without moving sync cursors.
    pub fn cache_history(&self, page: &Page) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        for c in &page.changes {
            self.connection.execute("INSERT INTO sync_inbox(epoch,rev,id,status,op_id,wire) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
                params![page.epoch,c.rev,c.blob.id,c.status,c.op_id,serde_json::to_string(&c.blob)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Restore retained ciphertext as a new edit without loading the embedding model.
    pub fn restore_sync_version(
        &self,
        keys: &SessionKeys,
        op_id: Option<&str>,
        rev: Option<i64>,
        epoch: Option<&str>,
    ) -> Result<String> {
        use rusqlite::OptionalExtension;
        let wire = if let Some(op) = op_id {
            let tuple=self.connection.query_row("SELECT id,user,ciphertext,nonce,updated_at,deleted FROM sync_outbox WHERE op_id=?1
                UNION ALL SELECT id,json_extract(wire,'$.user'),json_extract(wire,'$.ciphertext'),json_extract(wire,'$.nonce'),
                    json_extract(wire,'$.updated_at'),json_extract(wire,'$.deleted')
                FROM sync_remote_heads WHERE op_id=?1 AND wire<>''
                UNION ALL SELECT id,json_extract(wire,'$.user'),json_extract(wire,'$.ciphertext'),json_extract(wire,'$.nonce'),
                    json_extract(wire,'$.updated_at'),json_extract(wire,'$.deleted')
                FROM sync_inbox WHERE op_id=?1 LIMIT 1",[op],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,bool>(5)?))).optional()?;
            let Some((id, user, ciphertext, nonce, updated_at, deleted)) = tuple else {
                bail!(SyncVersionNotFound);
            };
            serde_json::to_string(&StoredMemory {
                id,
                user,
                ciphertext,
                nonce,
                updated_at,
                deleted,
                ..StoredMemory::new_pending(String::new(), String::new())
            })?
        } else if let Some(rev) = rev {
            let epoch = epoch.ok_or_else(|| {
                anyhow::anyhow!("restoring a remote version requires both --epoch and --rev")
            })?;
            self.connection
                .query_row(
                    "SELECT wire FROM sync_inbox WHERE epoch=?1 AND rev=?2
                    UNION ALL SELECT wire FROM sync_remote_heads WHERE rev=?2 AND wire<>''
                        AND ?1=(SELECT value FROM meta WHERE key='sync_v2_epoch') LIMIT 1",
                    params![epoch, rev],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!(SyncVersionNotFound))?
        } else {
            bail!("--op-id or --rev required");
        };
        let mut b: StoredMemory = serde_json::from_str(&wire)?;
        if b.ciphertext.is_empty() {
            bail!("this version has no restorable content");
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let stamp = self.edit_stamp(&b.id)?;
        crate::MemoryEngine::restore_version(keys, &mut b, &stamp)?;
        if !self.put_inner(&b, true)? {
            bail!("restored version was not written");
        }
        if let (Some(epoch), Some(rev)) = (epoch, rev) {
            self.track_restoration(epoch, rev, &b.id, "restore")?;
        }
        self.connection.execute(
            "DELETE FROM sync_inbox WHERE status='reset_saved'
                AND ((?1 IS NOT NULL AND op_id=?1) OR (epoch=?2 AND rev=?3))",
            params![op_id, epoch, rev],
        )?;
        tx.commit()?;
        Ok(b.id)
    }

    /// Legacy download and cursor update share a transaction; outgoing snapshots
    /// have already been captured by triggers before any remote version is applied.
    pub fn apply_legacy(
        &self,
        keys: &SessionKeys,
        blobs: Vec<StoredMemory>,
        cursor: u64,
    ) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for mut b in blobs {
            b.embedding_enc.clear();
            if !(b.deleted && b.ciphertext.is_empty()) {
                hydrate_local(keys, &mut b)?;
            }
            if self.put_synced(&b)? {
                changed += 1;
            }
        }
        self.meta_set("sync_cursor", &cursor.to_string())?;
        tx.commit()?;
        Ok(changed)
    }
}
