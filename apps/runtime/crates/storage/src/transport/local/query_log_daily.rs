use super::LocalStore;
use anyhow::{Context, Result};
use chrono::{Days, Local, NaiveDate};
use rusqlite::{params, Connection};

const DAYS: u64 = 30;
const DAY_SECONDS: i64 = 86_400;

// ts_unix is indexed UTC seconds. Coarse UTC bounds keep the scan in the
// requested period; local calendar conversion selects exact days including
// DST transitions. Keep the literal adopted/empty predicates of legacy stats.
const DAILY_SQL: &str = "
    WITH bounded AS (
        SELECT date(ts_unix, 'unixepoch', 'localtime') AS day, candidates, adopted, good_ids
        FROM query_log WHERE ts_unix >= ?1 AND ts_unix < ?2
    )
    SELECT day, COUNT(*),
           SUM(CASE WHEN candidates != '[]' THEN 1 ELSE 0 END),
           SUM(CASE WHEN adopted != '[]' THEN 1 ELSE 0 END),
           CASE WHEN SUM(CASE WHEN json_valid(good_ids) THEN 0 ELSE 1 END) > 0 THEN 0
                ELSE SUM((SELECT COUNT(*) FROM json_each(
                    CASE WHEN json_valid(good_ids) THEN good_ids ELSE '[]' END)))
           END
    FROM bounded WHERE day >= ?3 AND day <= ?4 GROUP BY day ORDER BY day";

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct QueryLogDay {
    day: String,
    recalls: i64,
    hits: i64,
    adopted: i64,
    good: i64,
}

impl LocalStore {
    pub fn query_log_daily_stats(&self) -> Result<Vec<QueryLogDay>> {
        daily(&self.connection, Local::now().date_naive())
    }
}

fn daily(db: &Connection, today: NaiveDate) -> Result<Vec<QueryLogDay>> {
    let first = today
        .checked_sub_days(Days::new(DAYS - 1))
        .context("query log date range")?;
    let midnight = |day: NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc()
            .timestamp()
    };
    // +/- 48 hours covers every UTC offset; exact local day filtering below
    // also rules out future calendar dates. No log row limit truncates counts.
    let lower = midnight(first) - 2 * DAY_SECONDS;
    let upper = midnight(today) + 3 * DAY_SECONDS;
    let mut stmt = db.prepare(DAILY_SQL)?;
    let found = stmt.query_map(
        params![lower, upper, first.to_string(), today.to_string()],
        |row| {
            let day: String = row.get(0)?;
            Ok(QueryLogDay {
                day,
                recalls: row.get(1)?,
                hits: row.get(2)?,
                adopted: row.get(3)?,
                good: row.get(4)?,
            })
        },
    )?;
    Ok(found.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn aggregates_complete_indexed_local_days_and_retains_legacy_predicates() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE query_log(ts_unix INTEGER NOT NULL, candidates TEXT NOT NULL,
            adopted TEXT NOT NULL, good_ids TEXT NOT NULL);
            CREATE INDEX idx_query_log_ts ON query_log(ts_unix);",
        )?;
        let today = NaiveDate::from_ymd_opt(2026, 3, 9).unwrap();
        assert!(daily(&db, today)?.is_empty());
        let previous = today.pred_opt().unwrap();
        let stamp = |date: NaiveDate, h, m| {
            Local
                .from_local_datetime(&date.and_hms_opt(h, m, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp()
        };
        let insert = |when: i64, candidates: &str, adopted: &str, good: &str| -> Result<()> {
            db.execute(
                "INSERT INTO query_log VALUES(?1,?2,?3,?4)",
                params![when, candidates, adopted, good],
            )?;
            Ok(())
        };
        insert(stamp(previous, 23, 55), "[]", "[]", "[]")?;
        insert(stamp(today, 0, 5), "[1]", "[1]", "[1,2]")?;
        insert(stamp(today, 12, 0), "[ ]", "[ ]", "[]")?;
        for _ in 0..201 {
            insert(stamp(today, 18, 0), "[]", "[]", "[]")?;
        }
        insert(stamp(today.succ_opt().unwrap(), 12, 0), "[1]", "[1]", "[1]")?;
        insert(
            stamp(today.checked_sub_days(Days::new(DAYS)).unwrap(), 12, 0),
            "[1]",
            "[1]",
            "[1]",
        )?;
        let result = daily(&db, today)?;
        assert_eq!(result.len(), 2);

        assert_eq!(
            result[0],
            QueryLogDay {
                day: previous.to_string(),
                recalls: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            result.last(),
            Some(&QueryLogDay {
                day: today.to_string(),
                recalls: 203,
                hits: 2,
                adopted: 2,
                good: 2,
            })
        );
        let mut explain = db.prepare(&format!("EXPLAIN QUERY PLAN {DAILY_SQL}"))?;
        let plan = explain
            .query_map(params![0, 200, "a", "z"], |r| r.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            plan.iter()
                .any(|line| line.contains("SEARCH") && line.contains("idx_query_log_ts")),
            "{plan:?}"
        );
        insert(stamp(today, 20, 0), "[]", "[]", "invalid")?;
        assert_eq!(daily(&db, today)?.last().unwrap().good, 0);
        Ok(())
    }
}
