use super::*;

impl LocalStore {
    /// Query log: each recall writes one candidate row (including empty — empty is a search negative, DPO material).
    /// Note: this records the candidate set, not hits — a hit is a model mark --good, see mark_verdict.
    /// Local-only table, not dirty, not synced.
    pub fn log_query(
        &self,
        query: &str,
        project: &str,
        scope: &str,
        candidates: &[String],
        candidate_scores: &[f32],
    ) -> Result<()> {
        let now = chrono::Utc::now();
        self.connection.execute(
            "INSERT INTO query_log (ts_unix, ts, query, project, scope, candidates, candidate_scores, adopted)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'[]')",
            params![
                now.timestamp(),
                now.to_rfc3339(),
                query,
                project,
                scope,
                serde_json::to_string(candidates)?,
                serde_json::to_string(candidate_scores)?
            ],
        )?;
        Ok(())
    }

    /// Recall hit-heat writeback: hit entries recall_count + 1 (source of the heat-axis hit_score).
    pub fn bump_recall(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            self.connection.execute(
                "UPDATE memories SET recall_count = recall_count + 1 WHERE id = ?1",
                params![id],
            )?;
        }
        Ok(())
    }

    /// For the passport: distinct computer count in this library (live rows).
    pub fn devices_count(&self) -> Result<usize> {
        Ok(self.connection.query_row(
            "SELECT COUNT(DISTINCT computer) FROM memories WHERE deleted = 0",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// Entry-change audit query: action stream for this id (or whole library --limit) (trigger-recorded, see entry_audit).
    pub fn entry_audit(
        &self,
        entry_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let rows = if let Some(eid) = entry_id {
            let mut stmt = self.connection.prepare(
                "SELECT entry_id, action, title_head, content_head, actor, ts
                 FROM entry_audit WHERE entry_id = ?1 ORDER BY seq DESC LIMIT ?2",
            )?;
            let v: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![eid, limit as i64], map_audit)?
                .flatten()
                .collect();
            v
        } else {
            let mut stmt = self.connection.prepare(
                "SELECT entry_id, action, title_head, content_head, actor, ts
                 FROM entry_audit ORDER BY seq DESC LIMIT ?1",
            )?;
            let v: Vec<serde_json::Value> = stmt
                .query_map(rusqlite::params![limit as i64], map_audit)?
                .flatten()
                .collect();
            v
        };
        Ok(rows)
    }

    /// Model self-grade report (main post-train signal): useful recall hits --good, misleading --bad.
    /// Look back 10 minutes (at most 20 rows) for candidate logs containing this id (8-char prefix ok), write good_ids/bad_ids.
    /// Returns ids that matched no candidate (caller warns).
    pub fn mark_verdict(&self, ids: &[String], good: bool) -> Result<Vec<String>> {
        let cutoff = chrono::Utc::now().timestamp() - 600;
        let rows:Vec<(i64,String)>= {
            let mut statement=self.connection.prepare("SELECT id,candidates FROM query_log WHERE ts_unix>=?1 ORDER BY id DESC LIMIT 20")?;
            let rows=statement.query_map([cutoff],|row|Ok((row.get(0)?,row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut unmatched=ids.to_vec();
        for (row_id,encoded) in rows {
            if unmatched.is_empty(){break;}
            let candidates:Vec<String>=serde_json::from_str(&encoded)?;
            let mut resolved=Vec::new();
            unmatched.retain(|prefix|{
                if let Some(id)=candidates.iter().find(|id|*id==prefix||id.starts_with(prefix.as_str())) {
                    resolved.push(id.clone());false
                }else{true}
            });
            if resolved.is_empty(){continue;}
            // Legacy selection stays compatible while disabled, but every
            // accepted mark uses the same attributed event/mirror authority.
            // Assign old rows an identity; concurrent assignments retain the
            // first committed UUID rather than fabricating a second request.
            self.connection.execute("UPDATE query_log SET request_id=COALESCE(request_id,?2) WHERE id=?1",params![row_id,uuid::Uuid::new_v4().to_string()])?;
            let request_id:String=self.connection.query_row("SELECT request_id FROM query_log WHERE id=?1",[row_id],|row|row.get(0))?;
            self.mark_query_feedback(&request_id,&resolved,good,"model")?;
        }
        Ok(unmatched)
    }

    /// Adoption attribution: called after a successful update/attach/demote/merge/attach —
    /// if a query in the last 10 minutes (at most 20 rows back) hit this id, record the id in that query's adopted.
    /// Returns how many query rows were marked.
    pub fn mark_adopted(&self, id: &str) -> Result<usize> {
        let cutoff = chrono::Utc::now().timestamp() - 600;
        let rows: Vec<(i64, String)> = {
            let mut stmt = self
                .connection
                .prepare("SELECT id, candidates FROM query_log WHERE ts_unix >= ?1 ORDER BY id DESC LIMIT 20")?;
            let it = stmt.query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?;
            it.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut marked = 0;
        for (row_id, cands_json) in rows {
            let ids: Vec<String> = serde_json::from_str(&cands_json).unwrap_or_default();
            if !ids.iter().any(|x| x == id) {
                continue;
            }
            let adopted: Vec<String> = {
                let cur: String = self.connection.query_row(
                    "SELECT adopted FROM query_log WHERE id = ?1",
                    params![row_id],
                    |r| r.get(0),
                )?;
                serde_json::from_str(&cur).unwrap_or_default()
            };
            if adopted.iter().any(|x| x == id) {
                continue;
            }
            let mut next = adopted;
            next.push(id.to_owned());
            self.connection.execute(
                "UPDATE query_log SET adopted = ?2 WHERE id = ?1",
                params![row_id, serde_json::to_string(&next)?],
            )?;
            marked += 1;
        }
        Ok(marked)
    }

    /// List recent query logs (newest first).
    pub fn query_log_rows(&self, limit: usize) -> Result<Vec<QueryLogRow>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, ts, query, project, scope, candidates, adopted, good_ids, bad_ids, request_id FROM query_log ORDER BY id DESC LIMIT ?1",
        )?;
        let it = stmt.query_map(params![limit as i64], |r| {
            Ok(QueryLogRow {
                request_id:r.get(9)?,
                id: r.get(0)?,
                ts: r.get(1)?,
                query: r.get(2)?,
                project: r.get(3)?,
                scope: r.get(4)?,
                candidates: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                adopted: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                good: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                bad: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
            })
        })?;
        Ok(it.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Query-log stats: total queries, queries with adopted, empty-candidate count, self-grade good/bad totals, top adopted entries.
    pub fn query_log_stats(&self) -> Result<QueryLogStats> {
        let total: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM query_log", [], |r| r.get(0))?;
        let with_adopted: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM query_log WHERE adopted != '[]'",
            [],
            |r| r.get(0),
        )?;
        let empty_candidates: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM query_log WHERE candidates = '[]'",
            [],
            |r| r.get(0),
        )?;
        // Self-grade counts: expand JSON arrays (old SQLite without json1 falls back to 0)
        let count_json = |col: &str| -> i64 {
            self.connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM query_log, json_each(query_log.{col})"),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0)
        };
        let good_total = count_json("good_ids");
        let bad_total = count_json("bad_ids");
        // Top adopted entries: expand JSON arrays and count
        let mut top: Vec<(String, i64)> = Vec::new();
        let ok = self
            .connection
            .prepare("SELECT je.value, COUNT(*) c FROM query_log, json_each(query_log.good_ids) je GROUP BY je.value ORDER BY c DESC LIMIT 5")
            .and_then(|mut s| {
                let it = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                top = it.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(())
            });
        if ok.is_err() {
            top.clear(); // old SQLite without json1 falls back to empty
        }
        Ok(QueryLogStats {
            total,
            with_adopted,
            empty_candidates,
            good_total,
            bad_total,
            top,
        })
    }

    /// Record one procedure run verdict. Local-only: not encrypted-payload
    /// state, not dirty, not synced — the same contract as `query_log`.
    pub fn log_skill_run(&self, skill_id: &str, status: &str, note: &str) -> Result<()> {
        anyhow::ensure!(
            matches!(status, "ok" | "fail" | "confounded" | "no_effect"),
            "run status must be ok, fail, confounded or no_effect"
        );
        self.connection.execute(
            "INSERT INTO skill_runs (skill_id, status, note, ts_unix) VALUES (?1,?2,?3,?4)",
            params![
                skill_id,
                status,
                note,
                chrono::Utc::now().timestamp()
            ],
        )?;
        Ok(())
    }

    /// Verdict counts and the latest outcome per procedure id (missing ids omitted).
    pub fn skill_run_stats(&self, ids: &[String]) -> Result<HashMap<String, SkillRunStats>> {
        let mut out = HashMap::new();
        let mut stmt = self.connection.prepare(
            "SELECT
                COALESCE(SUM(status = 'ok'), 0),
                COALESCE(SUM(status = 'fail'), 0),
                COALESCE(SUM(status = 'confounded'), 0),
                COALESCE(SUM(status = 'no_effect'), 0)
             FROM skill_runs WHERE skill_id = ?1",
        )?;
        let mut last = self.connection.prepare(
            "SELECT status, ts_unix FROM skill_runs WHERE skill_id = ?1 ORDER BY id DESC LIMIT 1",
        )?;
        for id in ids {
            let (ok, fail, confounded, no_effect): (i64, i64, i64, i64) = stmt
                .query_row(params![id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
            if ok == 0 && fail == 0 && confounded == 0 && no_effect == 0 {
                continue;
            }
            let (last_status, last_ts): (String, i64) = last
                .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .unwrap_or_default();
            out.insert(
                id.clone(),
                SkillRunStats {
                    ok,
                    fail,
                    confounded,
                    no_effect,
                    last_status,
                    last_ts,
                },
            );
        }
        Ok(out)
    }

    /// Procedures whose latest N attributable runs all failed. An ok breaks the
    /// streak; confounded / no_effect runs are not verdicts, so they neither
    /// count toward it nor reset it.
    pub fn failing_skills(&self, streak: usize) -> Result<Vec<(String, i64)>> {
        anyhow::ensure!(streak > 0, "streak must be positive");
        let mut stmt = self.connection.prepare(
            "SELECT skill_id, status FROM skill_runs ORDER BY id DESC",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut seen: HashMap<String, (usize, bool)> = HashMap::new();
        for (id, status) in rows {
            let slot = seen.entry(id).or_insert((0, true));
            if slot.0 >= streak {
                continue;
            }
            if status == "fail" {
                slot.0 += 1;
            } else if status == "ok" {
                slot.1 = false;
                slot.0 = streak; // stop counting this procedure
            }
            // confounded / no_effect: no verdict, keep counting older runs
        }
        let mut out: Vec<(String, i64)> = seen
            .into_iter()
            .filter(|(_, (n, failing))| *failing && *n >= streak)
            .map(|(id, (n, _))| (id, n as i64))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    pub fn verify_sqlite_integrity(&self)->Result<()> {
        let value:String=self.connection.query_row("PRAGMA quick_check",[],|row|row.get(0))?;
        anyhow::ensure!(value=="ok","runtime library integrity check failed");Ok(())
    }
    pub fn snapshot_quarantine_id(&self)->Result<Option<String>> {
        let value:Option<String>=self.connection.query_row("SELECT value FROM meta WHERE key='snapshot_sync_quarantine_v1'",[],|row|row.get(0)).optional()?;
        value.map(|value|{let parsed:serde_json::Value=serde_json::from_str(&value)?;
            parsed["snapshot"].as_str().map(str::to_owned).ok_or_else(||anyhow::anyhow!("invalid snapshot quarantine identity"))}).transpose()
    }

    /// Unlike legacy best-effort meta lookups, this safety gate propagates every
    /// SQLite error; unavailable quarantine metadata must never enable sync.
    pub fn ensure_snapshot_sync_allowed(&self) -> Result<()> {
        let quarantined: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key='snapshot_sync_quarantine_v1')",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(!quarantined,"snapshot restore quarantined sync; compare differences before explicit snapshot resume-sync");
        Ok(())
    }

    /// Missing is disabled; malformed values and database failures are errors.
    /// Enabling requires the external writer-upgrade and backup gates.
    pub fn memory_metadata_v1_enabled(&self) -> Result<bool> {
        let value: Option<String> = self.connection.query_row(
            "SELECT value FROM meta WHERE key = 'memory_metadata_v1_enabled'",
            [],
            |row| row.get(0),
        ).optional()?;
        match value.as_deref() {
            None | Some("0") => Ok(false),
            Some("1") => Ok(true),
            Some(_) => anyhow::bail!("invalid memory_metadata_v1_enabled value: expected 0 or 1"),
        }
    }

    /// Detect retained new-format local state without loading encrypted records.
    pub fn has_memory_metadata(&self) -> Result<bool> {
        has_memory_metadata(&self.connection)
    }

    /// meta read/write (sync cursor etc.).
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok())
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO meta (key, value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Persist a Core-owned index locator only if the encrypted source is current.
    /// Index work must never create a content version or an outgoing sync operation.
    pub fn set_artifact(
        &self,
        id: &str,
        artifact: &[u8],
        source_ciphertext: &str,
    ) -> Result<bool> {
        let n = self.connection.execute(
            "INSERT OR REPLACE INTO core_artifacts(memory_id,model,source,artifact)
             SELECT id,?2,ciphertext,?4 FROM memories WHERE id=?1 AND ciphertext=?3 AND deleted=0",
            params![id, self.active_generation_key()?, source_ciphertext, artifact],
        )?;
        Ok(n > 0)
    }

    pub fn chunked_ids(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .connection
            .prepare("SELECT DISTINCT memory_id FROM memory_chunks")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
    }
}
