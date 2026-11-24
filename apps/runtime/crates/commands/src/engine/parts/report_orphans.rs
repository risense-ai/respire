pub fn report_orphans(store: &respire_app::transport::local::LocalStore, action: &str) {
    let all = match store.all(true) {
        Ok(a) => a,
        Err(_) => return,
    };
    let by_id: std::collections::HashMap<&str, &respire_app::StoredMemory> =
        all.iter().map(|m| (m.id.as_str(), m)).collect();
    let orphans: Vec<&respire_app::StoredMemory> = all
        .iter()
        .filter(|m| !m.deleted)
        .filter(|m| {
            !m.local_parent_id.is_empty()
                && by_id
                    .get(m.local_parent_id.as_str())
                    .map_or(true, |pp| pp.deleted)
        })
        .collect();
    if orphans.is_empty() {
        return;
    }
    eprintln!("   WARN [{action}] {} orphan(s) (parent deleted/merged; reparent with resort) - `rsrs audit --json` for the whole store:", orphans.len());
    for m in &orphans {
        eprintln!(
            "     {} {} <-was parent {}",
            &m.id[..8.min(m.id.len())],
            m.local_title,
            m.local_parent_id
        );
    }
}

/// Record a procedure run outcome. Local-only (same contract as query_log):
/// the row never enters the encrypted payload and never syncs.
pub fn run_run_log(id: &str, status: &str, note: &str) -> Result<()> {
    let store = build_local()?;
    let all = store.all(false)?;
    let full = respire_app::service::resolve_prefix(&all, id)?;
    let target = all
        .iter()
        .find(|m| m.id == full)
        .ok_or_else(|| anyhow!("no live entry {}", respire_app::service::short_id(&full)))?;
    if !skill::is_procedure(&target.local_kind) {
        // A refusal is a business state, not a crash: the runtime turns `bail`
        // into an opaque runtime_error, so answer with a Skip envelope instead.
        let short = respire_app::service::short_id(&full);
        let kind = if target.local_kind.trim().is_empty() {
            "untyped"
        } else {
            target.local_kind.as_str()
        };
        let mut result = ResultEnvelope::new(
            "run-log",
            OutputStatus::Skip,
            serde_json::json!({"id": full, "kind": kind, "reason": "not a procedure"}),
            vec![OutputItem::new(
                short.clone(),
                OutputStatus::Skip,
                "not a procedure",
            )],
        );
        result.actions.push(format!(
            "{short} is {kind}; convert it with `rsrs update {short} --kind skill` if it is a procedure"
        ));
        emit_result(result)?;
        return Ok(());
    }
    store.log_skill_run(&full, status, note)?;
    let stats = store
        .skill_run_stats(std::slice::from_ref(&full))?
        .remove(&full)
        .unwrap_or_default();
    emit_result(ResultEnvelope::new(
        "run-log",
        OutputStatus::Ok,
        serde_json::json!({
            "id": full,
            "title": target.local_title,
            "status": status,
            "ok": stats.ok,
            "fail": stats.fail,
            "confounded": stats.confounded,
            "no_effect": stats.no_effect,
            "last_status": stats.last_status,
        }),
        vec![OutputItem::new(
            respire_app::service::short_id(&full),
            OutputStatus::Ok,
            format!(
                "{status} \u{b7} ok {} / fail {} / confounded {} / no_effect {}",
                stats.ok, stats.fail, stats.confounded, stats.no_effect
            ),
        )],
    ))
}

/// Whole-store health audit: orphans / illegal importance / duplicate titles / truncated titles / deep chains - a structured report for an AI to patch after.
/// Query log: list recent queries and adoption/self-grade, or stats (--stats), or report a self-grade (mark).
/// Post-training terms: candidates ≠ hits; chosen = model mark --good, rejected = mark --bad;
/// adopted (used when writing) is a weak signal only.
pub fn run_query_log(
    limit: usize,
    stats: bool,
    json_flag: bool,
    cmd: Option<&QueryLogCmd>,
) -> Result<()> {
    if json_flag {
        set_json_mode(true);
    }
    let store = build_local()?;
    // mark: model self-grade - the main post-training signal
    if let Some(QueryLogCmd::Mark { ids, good, bad, query_log_id, source }) = cmd {
        if *good == *bad {
            anyhow::bail!("pick exactly one of --good or --bad");
        }
        let id_list: Vec<String> = ids
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        anyhow::ensure!(!id_list.is_empty(), "ids is empty");
        let (unmatched,revision)=if let Some(request_id)=query_log_id {
            (Vec::new(),Some(store.mark_query_feedback(request_id,&id_list,*good,source)?))
        }else{
            anyhow::ensure!(source=="model","legacy grading without --query-log-id supports only --source model; use an explicit query request for human/task_outcome attribution");
            anyhow::ensure!(!respire_app::feedback::enabled()?,"enabled feedback requires --query-log-id from the recall summary and complete memory IDs");
            (store.mark_verdict(&id_list,*good)?,None)
        };
        let status = if unmatched.is_empty() {
            OutputStatus::Ok
        } else {
            OutputStatus::Warn
        };
        let mut result = ResultEnvelope::new(
            "query-log",
            status,
            serde_json::json!({"marked":id_list.len()-unmatched.len(),"good":*good,"query_log_id":query_log_id,"source":source,"revision":revision}),
            vec![
                OutputItem::new(
                    "marked",
                    status,
                    (id_list.len() - unmatched.len()).to_string(),
                ),
                OutputItem::new(
                    "verdict",
                    OutputStatus::Ok,
                    if *good { "good" } else { "bad" },
                ),
            ],
        );
        result.details = serde_json::json!({"unmatched":unmatched});
        if !unmatched.is_empty() {
            result.actions.push("query-log --stats".into());
        }
        emit_result(result)?;
        return Ok(());
    }
    if stats {
        let s = store.query_log_stats()?;
        let adopt_rate = if s.total > 0 {
            s.with_adopted as f64 / s.total as f64 * 100.0
        } else {
            0.0
        };
        let empty_rate = if s.total > 0 {
            s.empty_candidates as f64 / s.total as f64 * 100.0
        } else {
            0.0
        };
        let mut result = ResultEnvelope::new(
            "query-log",
            OutputStatus::Ok,
            serde_json::json!({"stats":true,"queries":s.total,"adopted":s.with_adopted,"empty_candidates":s.empty_candidates,"good":s.good_total,"bad":s.bad_total,"adopt_rate":adopt_rate,"empty_rate":empty_rate}),
            vec![
                OutputItem::new("queries", OutputStatus::Ok, s.total.to_string()),
                OutputItem::new(
                    "adopted",
                    OutputStatus::Ok,
                    format!("{} ({adopt_rate:.1}%)", s.with_adopted),
                ),
                OutputItem::new(
                    "empty candidates",
                    OutputStatus::Ok,
                    format!("{} ({empty_rate:.1}%)", s.empty_candidates),
                ),
                OutputItem::new(
                    "self grade",
                    OutputStatus::Ok,
                    format!("good {} / bad {}", s.good_total, s.bad_total),
                ),
            ],
        );
        result.details = serde_json::to_value(&s)?;
        result.items.extend(s.top.iter().map(|(id, c)| {
            OutputItem::new(
                "top adopted",
                OutputStatus::Ok,
                format!("{} count {}", respire_app::service::short_id(id), c),
            )
        }));
        emit_result(result)?;
        return Ok(());
    }
    let rows = store.query_log_rows(limit)?;
    if rows.is_empty() {
        let mut result = ResultEnvelope::new(
            "query-log",
            OutputStatus::Skip,
            serde_json::json!({"count":0}),
            vec![OutputItem::new("rows", OutputStatus::Skip, "0")],
        );
        result.actions.push("query-log --stats".into());
        emit_result(result)?;
        return Ok(());
    }
    let mut items = Vec::new();
    let mut detail_rows = Vec::new();
    for r in &rows {
        let fmt = |v: &Vec<String>| {
            if v.is_empty() {
                "-".to_owned()
            } else {
                v.iter()
                    .map(|a| respire_app::service::short_id(a))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        };
        let project = if r.project.is_empty() {
            String::new()
        } else {
            format!(" [{}]", r.project)
        };
        items.push(OutputItem::new(
            "query",
            OutputStatus::Ok,
            format!(
                "{} {}{} cand={} adopted={} good={} bad={}",
                r.ts,
                r.query,
                project,
                fmt(&r.candidates),
                fmt(&r.adopted),
                fmt(&r.good),
                fmt(&r.bad)
            ),
        ));
        detail_rows.push(serde_json::to_value(r)?);
    }
    let mut result = ResultEnvelope::new(
        "query-log",
        OutputStatus::Ok,
        serde_json::json!({"count":rows.len()}),
        items,
    );
    result.details = serde_json::Value::Array(detail_rows);
    emit_result(result)?;
    Ok(())
}

/// Human-readable rank: Some(n) -> #n+1; None -> miss.
pub fn bench_pos(p: Option<usize>) -> String {
    match p {
        Some(n) => format!("#{}", n + 1),
        None => "miss".to_owned(),
    }
}

/// bench entry: run an evalset / mine an evalset from query-log.
pub fn run_bench(cmd: &BenchCmd) -> Result<()> {
    match cmd {
        BenchCmd::Run {
            file,
            topk,
            save,
            baseline,
            verbose,
        } => run_bench_run(file, *topk, save.as_deref(), baseline.as_deref(), *verbose),
        BenchCmd::Mine { out, limit, strict } => run_bench_mine(out, *limit, *strict),
    }
}

/// mine: draft an evalset from query-log - expect = good (strict uses only this) ∪ adopted;
/// keep only ids still active; merge same query. Writes JSONL.
pub fn run_bench_mine(out: &str, limit: usize, strict: bool) -> Result<()> {
    let store = build_local()?;
    let rows = store.query_log_rows(limit * 5)?;
    let active: std::collections::HashSet<String> = store.all(false)?
        .iter()
        .map(|m| m.id.clone())
        .collect();
    let cases = bench::mine_from_rows(&rows, strict, limit, &active);
    anyhow::ensure!(
        !cases.is_empty(),
        "nothing to mine: query-log needs a self-grade good (rsrs query-log mark --good) or an adopted record, and the entry must still be in the store"
    );
    let mut buf = String::new();
    for c in &cases {
        buf.push_str(&serde_json::to_string(c)?);
        buf.push('\n');
    }
    std::fs::write(out, buf).map_err(|e| anyhow!("write failed: {out} ({e})"))?;
    emit_result(ResultEnvelope::new(
        "bench-mine",
        OutputStatus::Ok,
        serde_json::json!({"cases":cases.len(),"path":out,"strict":strict}),
        vec![
            OutputItem::new("eval cases", OutputStatus::Ok, cases.len().to_string()),
            OutputItem::new("path", OutputStatus::Ok, out),
            OutputItem::new(
                "source",
                OutputStatus::Ok,
                if strict { "good" } else { "good and adopted" },
            ),
        ],
    ))?;
    Ok(())
}

