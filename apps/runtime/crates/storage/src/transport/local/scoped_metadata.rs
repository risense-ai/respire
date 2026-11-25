use super::LocalStore;
use anyhow::Result;
use rusqlite::params;

impl LocalStore {
    pub fn active_diary_tracks(&self, id: &str, title: &str) -> Result<Vec<crate::memory::model::StoredMemory>> {
        let sql = format!(
            "{} WHERE deleted = 0 AND (id = ?1 OR title = ?2)",
            super::catalog::MEMORY_SELECT
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![id, title], Self::row_to_memory)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn active_diary_entries(
        &self,
        date: Option<&str>,
        day_of: impl Fn(&str) -> String,
    ) -> Result<Vec<crate::memory::model::StoredMemory>> {
        let sql = format!(
            "{} WHERE deleted = 0 AND importance = 'trivial' ORDER BY created_at DESC, id",
            super::catalog::MEMORY_SELECT
        );
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query([])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let created: &str = row.get_ref(13)?.as_str()?;
            if date.is_none_or(|date| day_of(created) == date) {
                entries.push(Self::row_to_memory(row)?);
            }
        }
        Ok(entries)
    }

    /// The caller must hold verified authority for this entire active library.
    pub fn active_memory_exact(
        &self,
        full_id: &str,
    ) -> Result<Option<crate::memory::model::StoredMemory>> {
        use rusqlite::OptionalExtension;
        let sql = format!(
            "{} WHERE deleted = 0 AND id = ?1",
            super::catalog::MEMORY_SELECT
        );
        Ok(self.connection.query_row(&sql, [full_id], Self::row_to_memory).optional()?)
    }

    /// Tombstone exactly one ID; REST identifiers never use the legacy prefix resolver.
    /// The caller must hold verified write authority for this active library.
    pub fn forget_memory_exact(&self, full_id: &str) -> Result<bool> {
        self.write_transaction(|| {
            let stamp = self.edit_stamp(full_id)?;
            Ok(self.connection.execute(
                "UPDATE memories SET deleted = 1, updated_at = ?2, dirty = 1 WHERE id = ?1 AND deleted = 0",
                params![full_id, stamp],
            )? > 0)
        })
    }

    /// Seek using the update index; its implicit rowid suffix breaks equal timestamps.
    /// The caller must hold verified authority for this entire active library.
    pub fn active_memory_page(
        &self,
        limit: usize,
        after: Option<(&str, i64)>,
    ) -> Result<Vec<(i64, crate::memory::model::StoredMemory)>> {
        anyhow::ensure!((1..=201).contains(&limit), "invalid memory page size");
        let select = super::catalog::MEMORY_SELECT.replacen(" FROM memories", ", rowid AS page_row_id FROM memories", 1);
        let filter = if after.is_some() {
            " AND (updated_at, rowid) < (?2, ?3)"
        } else {
            ""
        };
        let sql = format!(
            "{select} WHERE deleted = 0{filter} ORDER BY updated_at DESC, rowid DESC LIMIT ?1"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let limit = i64::try_from(limit)?;
        let mut rows = match after {
            Some((stamp, row_id)) => statement.query(params![limit, stamp, row_id])?,
            None => statement.query([limit])?,
        };
        let mut page = Vec::new();
        while let Some(row) = rows.next()? {
            page.push((row.get("page_row_id")?, Self::row_to_memory(row)?));
        }
        Ok(page)
    }
}

pub struct ScopedQueryLogRow {
    pub id: i64,
    pub recorded_at: String,
    pub candidate_count: i64,
    pub adopted_count: i64,
    pub good_count: i64,
    pub bad_count: i64,
}

pub struct ScopedReviewRow {
    pub sequence: i64,
    pub record_revision: String,
    pub action: String,
    pub happened_at: String,
}

impl LocalStore {
    /// Return only allowlisted metadata and count aggregates from one snapshot.
    pub fn scoped_query_log(
        &self,
        limit: usize,
    ) -> Result<(Vec<ScopedQueryLogRow>, super::QueryLogStats)> {
        anyhow::ensure!((1..=200).contains(&limit), "invalid query-log limit");
        let mut statement = self.connection.prepare(
            "WITH totals AS (
                 SELECT COUNT(*) AS total,
                        COUNT(CASE WHEN adopted != '[]' THEN 1 END) AS with_adopted,
                        COUNT(CASE WHEN candidates = '[]' THEN 1 END) AS empty_candidates,
                        COALESCE(SUM(CASE WHEN json_valid(good_ids) THEN json_array_length(good_ids) ELSE 0 END), 0) AS good_total,
                        COALESCE(SUM(CASE WHEN json_valid(bad_ids) THEN json_array_length(bad_ids) ELSE 0 END), 0) AS bad_total
                 FROM query_log
             ), page AS (
                 SELECT id, ts,
                        CASE WHEN json_valid(candidates) THEN json_array_length(candidates) ELSE 0 END AS candidates,
                        CASE WHEN json_valid(adopted) THEN json_array_length(adopted) ELSE 0 END AS adopted,
                        CASE WHEN json_valid(good_ids) THEN json_array_length(good_ids) ELSE 0 END AS good,
                        CASE WHEN json_valid(bad_ids) THEN json_array_length(bad_ids) ELSE 0 END AS bad
                 FROM query_log ORDER BY id DESC LIMIT ?1
             )
             SELECT page.id, page.ts, page.candidates, page.adopted, page.good, page.bad,
                    total, with_adopted, empty_candidates, good_total, bad_total
             FROM totals LEFT JOIN page ON TRUE ORDER BY page.id DESC",
        )?;
        let mut rows = statement.query([i64::try_from(limit)?])?;
        let mut page = Vec::new();
        let mut totals = None;
        while let Some(row) = rows.next()? {
            if totals.is_none() {
                totals = Some(super::QueryLogStats {
                    total: row.get(6)?,
                    with_adopted: row.get(7)?,
                    empty_candidates: row.get(8)?,
                    good_total: row.get(9)?,
                    bad_total: row.get(10)?,
                    top: Vec::new(),
                });
            }
            if let Some(id) = row.get(0)? {
                page.push(ScopedQueryLogRow {
                    id,
                    recorded_at: row.get(1)?,
                    candidate_count: row.get(2)?,
                    adopted_count: row.get(3)?,
                    good_count: row.get(4)?,
                    bad_count: row.get(5)?,
                });
            }
        }
        Ok((
            page,
            totals.ok_or_else(|| anyhow::anyhow!("missing query-log aggregate"))?,
        ))
    }

    pub fn scoped_review_history(&self, id: &str, limit: usize) -> Result<Vec<ScopedReviewRow>> {
        anyhow::ensure!(
            !id.is_empty() && (1..=200).contains(&limit),
            "invalid review filter"
        );
        let mut statement = self.connection.prepare(
            "SELECT seq,record_revision,action,happened_at
             FROM review_history WHERE entry_id=?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![id, i64::try_from(limit)?], |row| {
            Ok(ScopedReviewRow {
                sequence: row.get(0)?,
                record_revision: row.get(1)?,
                action: row.get(2)?,
                happened_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn bounded_page_and_whole_totals_exclude_private_columns() -> Result<()> {
        // The table deliberately has none of the private query columns.
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE query_log(id INTEGER PRIMARY KEY, ts TEXT, candidates TEXT,
                 adopted TEXT, good_ids TEXT, bad_ids TEXT);
             INSERT INTO query_log VALUES (1,'first','[]','[]','[]','[]');
             INSERT INTO query_log VALUES (2,'second','[\"candidate\"]',
                 '[\"adopted\"]','[\"good\"]','[]');",
        )?;
        let store = LocalStore { connection };
        let (page, totals) = store.scoped_query_log(1)?;
        assert_eq!(page.len(), 1);
        assert_eq!(
            (page[0].id, page[0].candidate_count, page[0].good_count),
            (2, 1, 1)
        );
        assert_eq!(
            (totals.total, totals.with_adopted, totals.empty_candidates),
            (2, 1, 1)
        );
        assert_eq!((totals.good_total, totals.bad_total), (1, 0));
        assert!(store.scoped_query_log(0).is_err());
        assert!(store.scoped_query_log(201).is_err());
        store.connection.execute("DELETE FROM query_log", [])?;
        let (page, totals) = store.scoped_query_log(200)?;
        assert!(page.is_empty());
        assert_eq!(totals.total, 0);
        Ok(())
    }

    #[test]
    fn active_memory_reads_use_full_ids_tombstones_and_stable_ties() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = LocalStore::open(&temp.path().join("library.db"))?;
        for (id, stamp) in [("abc-older", "1"), ("abc-first", "2"), ("abc-last", "2")] {
            let mut row = crate::memory::model::StoredMemory::new_pending(id.into(), "owner".into());
            row.updated_at = stamp.into();
            store.put_inner(&row, false)?;
        }
        store.connection.execute("UPDATE memories SET deleted=1 WHERE id='abc-older'", [])?;

        assert!(store.active_memory_exact("abc")?.is_none());
        assert!(store.active_memory_exact("abc-older")?.is_none());
        assert!(store.active_memory_exact("abc-first")?.is_some());
        let first = store.active_memory_page(1, None)?;
        let (row_id, first) = first.first().ok_or_else(|| anyhow::anyhow!("empty first page"))?;
        assert_eq!(first.id, "abc-last");
        let next = store.active_memory_page(201, Some((&first.updated_at, *row_id)))?;
        assert_eq!(next.iter().map(|(_, row)| row.id.as_str()).collect::<Vec<_>>(), ["abc-first"]);
        assert!(store.active_memory_page(202, None).is_err());
        assert!(!store.forget_memory_exact("abc")?);
        assert!(!store.forget_memory_exact("abc-%")?);
        assert!(!store.forget_memory_exact("missing")?);
        assert!(store.active_memory_exact("abc-first")?.is_some());
        store.connection.execute(
            "UPDATE memories SET updated_at = '2026-01-01T00:00:00Z' WHERE id = 'abc-first'", [],
        )?;
        assert!(store.forget_memory_exact("abc-first")?);
        assert!(!store.forget_memory_exact("abc-first")?);
        assert!(store.active_memory_exact("abc-first")?.is_none());
        assert!(store.active_memory_exact("abc-last")?.is_some());
        let (deleted, dirty, stamp): (bool, bool, String) = store.connection.query_row(
            "SELECT deleted, dirty, updated_at FROM memories WHERE id = 'abc-first'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert!(deleted && dirty);
        assert!(crate::transport::timestamp_after(Some(&stamp)).is_ok());
        assert!(stamp.as_str() > "2026-01-01T00:00:00Z");
        assert!(store.sync_counts()?.0 > 0);
        Ok(())
    }

    #[test]
    fn review_filter_is_exact_and_page_is_newest_first() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE review_history(seq INTEGER PRIMARY KEY, entry_id TEXT,
                record_revision TEXT, action TEXT, happened_at TEXT);
             INSERT INTO review_history VALUES (1,'entry','a','approve','first'),
                (2,'other','b','approve','second'), (3,'entry','c','approve','third');",
        )?;
        let store = LocalStore { connection };
        let page = store.scoped_review_history("entry", 1)?;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].sequence, 3);
        assert!(store.scoped_review_history("ent", 1)?.is_empty());
        assert!(store.scoped_review_history("", 1).is_err());
        Ok(())
    }
}
