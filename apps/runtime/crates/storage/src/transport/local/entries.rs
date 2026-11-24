use super::*;

/// purge helpers
impl LocalStore {
    /// Purge entry content: tombstone first, then clear ciphertext/body/vectors.
    pub fn purge_entry(&self, id: &str) -> Result<bool> {
        let id = id.trim_start_matches('#');
        let stamp = chrono::Utc::now().to_rfc3339();
        self.forget(id)?;
        self.connection
            .execute("DELETE FROM core_artifacts WHERE memory_id=?1", params![id])?;
        let n = self.connection.execute(
            "UPDATE memories SET ciphertext='', nonce='', embedding_enc='', embedding=NULL, title='', content_head='', tags='', project='', computer='', kind='', recall_count=0, dirty=1, updated_at=?2 WHERE id=?1",
            params![id, stamp],
        )?;
        Ok(n > 0)
    }

    /// Clear local content fields (called on an empty tombstone — remote already purged, local syncs the clear).
    pub fn purge_content(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM core_artifacts WHERE memory_id=?1", params![id])?;
        self.connection.execute(
            "UPDATE memories SET ciphertext='', nonce='', embedding_enc='', embedding=NULL, title='', content_head='', tags='', project='', computer='' WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// Clear tombstone content older than purge_days (auto-clean old tombstones).
    /// purge_days=0 → clear now; returns rows cleared.
    pub fn auto_purge_old(&self, days: i64) -> Result<usize> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        let ids: Vec<String> = {
            let mut stmt = self.connection.prepare(
                "SELECT id FROM memories WHERE deleted=1 AND ciphertext<>'' AND updated_at < ?1",
            )?;
            let rows = stmt.query_map(params![cutoff], |row| row.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in &ids {
            self.purge_content(id)?;
        }
        Ok(ids.len())
    }
}

impl MemoryTransport for LocalStore {
    fn put(&self, memory: &StoredMemory) -> Result<bool> {
        self.put_inner(memory, true)
    }

    fn all(&self, include_deleted: bool) -> Result<Vec<StoredMemory>> {
        let mut statement = self.catalog_statement(include_deleted)?;
        let rows = statement.query_map([], Self::row_to_memory)?;
        let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_retrieval_index(&mut out)?;
        Ok(out)
    }

    fn max_updated_at(&self) -> Result<Option<String>> {
        let value: Option<String> = self
            .connection
            .query_row("SELECT MAX(updated_at) FROM memories", [], |row| row.get(0))
            .ok();
        Ok(value.filter(|s| !s.is_empty()))
    }

    fn forget(&self, id: &str) -> Result<bool> {
        // list/show print 🆔 with a # prefix; paste as-is — this entry strips #
        let id = id.trim_start_matches('#');
        let stamp = self.edit_stamp(id)?;
        // Prefix match: exact id first, else unique prefix (ambiguous → leave and report)
        let n = self.connection.execute(
            "UPDATE memories SET deleted = 1, updated_at = ?2, dirty = 1 WHERE id = ?1 AND deleted = 0",
            params![id, stamp],
        )?;
        if n > 0 {
            return Ok(true);
        }
        let mut hits: Vec<String> = Vec::new();
        {
            let mut stmt = self
                .connection
                .prepare("SELECT id FROM memories WHERE deleted = 0 AND id LIKE ?1 || '%'")?;
            let rows = stmt.query_map(params![id], |row| row.get::<_, String>(0))?;
            for r in rows {
                hits.push(r?);
            }
        }
        if hits.len() == 1 {
            let stamp = self.edit_stamp(&hits[0])?;
            let n = self.connection.execute(
                "UPDATE memories SET deleted = 1, updated_at = ?2, dirty = 1 WHERE id = ?1",
                params![hits[0], stamp],
            )?;
            return Ok(n > 0);
        }
        if hits.len() > 1 {
            eprintln!(
                "warning: prefix {} matches {} entries; use a longer id",
                id,
                hits.len()
            );
            return Ok(false);
        }
        Ok(false)
    }

    fn count(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM memories WHERE deleted = 0",
            [],
            |row| row.get(0),
        )?)
    }
}
