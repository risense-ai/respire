use super::LocalStore;
use anyhow::Result;
use rusqlite::params;

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
