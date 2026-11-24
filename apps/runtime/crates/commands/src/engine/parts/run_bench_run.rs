/// run: execute an evalset - recall each case on the real retrieval path; emit hit-rate/MRR/noise; optional archive and baseline compare.
/// Eval is read-only: no query_log writes, no hit-count bump.
pub fn run_bench_run(
    file: &str,
    topk: usize,
    save: Option<&str>,
    baseline: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let text = respire_app::input_error::read_file(std::path::Path::new(file), "evalset")?;
    let cases = bench::parse_evalset(&text)?;
    anyhow::ensure!(!cases.is_empty(), "evalset is empty: {file}");

    // Read the baseline first: fail fast - eval is expensive (per-case recall); do not finish a run then fail on the baseline.
    let base_report: Option<bench::BenchReport> = match baseline {
        Some(bp) => {
            let btext = std::fs::read_to_string(bp)
                .map_err(|e| anyhow!("failed to read baseline: {bp} ({e})"))?;
            Some(
                serde_json::from_str(&btext)
                    .map_err(|e| anyhow!("baseline JSON failed to parse: {bp} ({e})"))?,
            )
        }
        None => None,
    };

    let session = build_session()?;
    let store = build_local()?;
    let (candidates, index_pending, resident) = respire_app::resident::recall_catalog(&session, &store)?;
    let _resident_scope = resident.enter();
    let active_ids: std::collections::HashSet<String> =
        candidates.iter().map(|m| m.id.clone()).collect();
    let embedder = BgeEmbedder::load_model(&store.retrieval_model()?)?;

    // Expected-id check: warn when not in the active store (deleted/merged) - those cases fail forever
    let mut missing: Vec<String> = Vec::new();
    for c in &cases {
        for e in &c.expect {
            if !active_ids.iter().any(|id| bench::id_match(id, e)) && !missing.contains(e) {
                missing.push(e.clone());
            }
        }
    }

    let mut results = Vec::with_capacity(cases.len());
    let mut latency = Vec::new();
    let mut selector_fallbacks = 0usize;
    for (i, c) in cases.iter().enumerate() {
        progress::phase(&format!("\u{8bc4}\u{4f30}\u{53ec}\u{56de} {}/{}", i + 1, cases.len()), &format!("Evaluating recall {}/{}", i + 1, cases.len()));
        let mut q = MemoryQuery::new(&c.query).limit(topk);
        if let Some(p) = &c.project {
            if !p.is_empty() {
                q = q.of_project(p);
            }
        }
        let started = std::time::Instant::now();
        let (ranked, _, fallback, _, _, _) = recall_select::recall(&session, &embedder, &candidates, &q, &store, None, None, None)?;
        latency.push(started.elapsed().as_micros());
        selector_fallbacks += usize::from(fallback.is_some());
        let ids: Vec<String> = ranked.iter().map(|r| r.entry.id.clone()).collect();
        let rank = bench::rank_of(&ids, &c.expect);
        let top1 = ranked.first();
        results.push(bench::CaseResult {
            query: c.query.clone(),
            expect: c.expect.clone(),
            project: c.project.clone(),
            rank,
            ids,
            top1_score: top1.map(|r| r.score),
            top1_title: top1.map(|r| r.entry.title.clone()).unwrap_or_default(),
        });
    }

    let metrics = bench::summarize(&results, topk);
    latency.sort_unstable();
    let mut params = bench::snapshot_params()?;
    params.insert("embedding_model".into(), embedder.model_name().into());
    params.insert("cli_version".into(), env!("CARGO_PKG_VERSION").into());
    params.insert(
        "recall_mode".into(),
        respire_app::service::read_agent_config()["recall_mode"]
            .as_str()
            .unwrap_or("fast")
            .into(),
    );
    params.insert("selector_fallbacks".into(), selector_fallbacks.to_string());
    params.insert("index_pending".into(), index_pending.to_string());
    params.insert("retrieval_policy".into(), "Core-owned".into());
    if !latency.is_empty() {
        params.insert("p50_us".into(), latency[latency.len() / 2].to_string());
        params.insert(
            "p95_us".into(),
            latency[(latency.len() * 95 / 100).min(latency.len() - 1)].to_string(),
        );
    }
    let report = bench::BenchReport {
        version: 1,
        ts: chrono::Utc::now().to_rfc3339(),
        evalset: file.to_owned(),
        topk,
        n_entries: candidates.len(),
        params,
        metrics: metrics.clone(),
        results,
    };

    // Baseline compare (optional; base_report was read up front)
    let cmp = base_report
        .as_ref()
        .map(|b| bench::compare(&b.results, &report.results));

    // Archive (optional)
    if let Some(sp) = save {
        let s = serde_json::to_string_pretty(&report)?;
        std::fs::write(sp, s).map_err(|e| anyhow!("failed to archive: {sp} ({e})"))?;
    }

    let m = &report.metrics;
    let mut status = if missing.is_empty() {
        OutputStatus::Ok
    } else {
        OutputStatus::Warn
    };
    let mut items = vec![
        OutputItem::new("evalset", OutputStatus::Ok, file),
        OutputItem::new(
            "cases",
            OutputStatus::Ok,
            format!("{} ({} positive, {} negative)", m.n, m.n_pos, m.n_neg),
        ),
        OutputItem::new("store", OutputStatus::Ok, report.n_entries.to_string()),
        OutputItem::new(
            "hit rate",
            OutputStatus::Ok,
            format!(
                "hit@1 {:.3}, hit@3 {:.3}, hit@{} {:.3}",
                m.hit1, m.hit3, topk, m.hitk
            ),
        ),
        OutputItem::new("MRR", OutputStatus::Ok, format!("{:.3}", m.mrr)),
        OutputItem::new(
            "noise",
            OutputStatus::Ok,
            format!(
                "empty {}, negative {}/{}",
                m.empty_results, m.neg_noise, m.n_neg
            ),
        ),
    ];
    if !missing.is_empty() {
        status = OutputStatus::Warn;
        items.push(
            OutputItem::new(
                "missing expected ids",
                OutputStatus::Warn,
                missing.join(", "),
            )
            .action("refresh evalset"),
        );
    }

    let missed: Vec<&bench::CaseResult> = report
        .results
        .iter()
        .filter(|r| !r.expect.is_empty() && r.rank.is_none())
        .collect();
    let mut detail = serde_json::json!({"report":report});
    if let (Some(b), Some(c)) = (&base_report, &cmp) {
        detail["baseline"] =
            serde_json::json!({ "file": baseline, "ts": b.ts, "metrics": b.metrics });
        detail["compare"] = serde_json::to_value(c)?;
    }
    if !missed.is_empty() {
        items.push(OutputItem::new(
            "misses",
            OutputStatus::Warn,
            missed.len().to_string(),
        ));
        if matches!(status, OutputStatus::Ok) {
            status = OutputStatus::Warn;
        }
    }
    if verbose {
        items.extend(report.results.iter().enumerate().map(|(i, r)| {
            let score = r
                .top1_score
                .map(|s| format!("{s:.3}"))
                .unwrap_or_else(|| "-".to_owned());
            OutputItem::new(
                format!("case {}", i + 1),
                if r.rank.is_some() {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Warn
                },
                format!("{} {} {}", bench_pos(r.rank), score, r.query),
            )
        }));
    }

    if let (Some(b), Some(c)) = (&base_report, &cmp) {
        items.push(OutputItem::new(
            "baseline",
            OutputStatus::Ok,
            format!(
                "{} hit@{} {:.3}->{:.3}, MRR {:.3}->{:.3}",
                baseline.unwrap_or(""),
                topk,
                b.metrics.hitk,
                m.hitk,
                b.metrics.mrr,
                m.mrr
            ),
        ));
        items.push(OutputItem::new(
            "comparison",
            if c.regressed.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            format!(
                "improved {}, regressed {}, same {}, only-new {}, only-baseline {}",
                c.improved.len(),
                c.regressed.len(),
                c.same,
                c.only_in_now,
                c.only_in_base
            ),
        ));
    }
    let mut result = ResultEnvelope::new(
        "bench",
        status,
        serde_json::json!({"evalset":file,"topk":topk,"cases":m.n,"hit1":m.hit1,"hit3":m.hit3,"hitk":m.hitk,"mrr":m.mrr,"missing":missing.len()}),
        items,
    );
    result.details = detail;
    if let Some(sp) = save {
        result.actions.push(format!("report saved to {sp}"));
    }
    emit_result(result)?;
    Ok(())
}

/// Render the final classification status supplied by Core.

pub fn classify_status(value: &str) -> OutputStatus {
    match value {
        "fail" => OutputStatus::Fail,
        "warn" => OutputStatus::Warn,
        "skip" => OutputStatus::Skip,
        "pending" => OutputStatus::Pending,
        _ => OutputStatus::Ok,
    }
}

pub fn emit_classify_report(
    report: classify::ClassifyReport,
    mut details: serde_json::Value,
) -> Result<()> {
    let summary = serde_json::to_value(&report.summary)?;
    if let serde_json::Value::Object(ref mut map) = details {
        map.insert("report".to_owned(), serde_json::to_value(&report)?);
    }
    let mut rows = vec![OutputItem::new(
        "summary",
        classify_status(&report.status),
        format!(
            "total={} matched={} mismatch={} unrooted={} low={} failed={}",
            report.summary.total,
            report.summary.matched,
            report.summary.mismatch,
            report.summary.unrooted,
            report.summary.low_confidence,
            report.summary.failed
        ),
    )];
    rows.extend(report.items.iter().map(|item| {
        let value = format!(
            "current={} choice={} confidence={} {}",
            item.current,
            item.choice,
            item.confidence
                .map(|v| format!("{v:.2}"))
                .unwrap_or_default(),
            item.error.clone().unwrap_or_default()
        );
        OutputItem::new(
            format!("{}:{}", item.class, item.id),
            classify_status(&item.status),
            value,
        )
    }));
    let mut envelope = ResultEnvelope::new(
        report.command,
        classify_status(&report.status),
        summary,
        rows,
    );
    envelope.errors = report.errors;
    envelope.details = details;
    emit_result(envelope)
}

pub fn emit_classify_details(
    command: &str,
    status: OutputStatus,
    summary: serde_json::Value,
    details: serde_json::Value,
    rows: Vec<OutputItem>,
) -> Result<()> {
    let mut result = ResultEnvelope::new(command, status, summary, rows);
    result.details = details;
    emit_result(result)
}

pub fn run_classify(
    limit: usize,
    all: bool,
    root: Option<&str>,
    max_chars: usize,
    min_confidence: f32,
    save: Option<&str>,
    api_base: Option<&str>,
    model: &str,
    dry_run: bool,
    ds: Option<&str>,
    backend: Option<&str>,
    samples: usize,
    tree: bool,
    batch: usize,
    tree_depth: usize,
    causal: bool,
    min_kids: usize,
    segments: usize,
    out: Option<&str>,
    auto: bool,
    plan: bool,
    rounds: usize,
) -> Result<()> {
    let generation = rpc::sync_generation();
    let (store, memories, selected_root, backend, session) =
        rpc::foreground_phase(generation, || {
            let store = build_local()?;
            let memories = store.all(false)?;
            let selected_root = root
                .map(|prefix| respire_app::service::resolve_prefix(&memories, prefix))
                .transpose()?;
            let backend = if plan {
                None
            } else {
                Some(build_classify_backend(
                    api_base, model, dry_run, ds, backend,
                )?)
            };
            let session = if auto && !dry_run && !plan {
                Some(build_session()?)
            } else {
                None
            };
            Ok((store, memories, selected_root, backend, session))
        })?;
    let source_revision = classify_source_revision(&memories);
    let mode = if plan {
        "preview"
    } else if auto {
        "auto"
    } else if causal {
        "causal"
    } else if tree {
        "tree"
    } else {
        "standard"
    };
    if progress_enabled() && !dry_run && !plan {
        eprintln!("Running classification...");
    }
    let mut result = classify::execute(
        &memories,
        backend.as_ref(),
        serde_json::json!({
            "mode": mode, "limit": limit, "all": all, "root": selected_root,
            "max_chars": max_chars, "min_confidence": min_confidence, "dry_run": dry_run,
            "samples": samples, "batch": batch, "tree_depth": tree_depth,
            "min_kids": min_kids, "segments": segments, "rounds": rounds,
        }),
    )?;

    rpc::foreground_phase(generation, || Ok(()))?;
    let mut applied = 0usize;
    if let Some(session) = session.as_ref() {
        applied = rpc::foreground_phase(generation, || {
            let current = store.all(false)?;
            anyhow::ensure!(
                classify_source_revision(&current) == source_revision,
                "memory data changed during classification; retry against the current library"
            );
            let ids: std::collections::HashSet<&str> =
                current.iter().map(|memory| memory.id.as_str()).collect();
            let mut changed = std::collections::HashSet::new();
            for action in &result.actions {
                anyhow::ensure!(
                    ids.contains(action.id.as_str()),
                    "classification refers to a missing memory: {}",
                    action.id
                );
                anyhow::ensure!(
                    action.parent_id.is_empty() || ids.contains(action.parent_id.as_str()),
                    "classification refers to a missing parent: {}",
                    action.parent_id
                );
                anyhow::ensure!(
                    changed.insert(action.id.as_str()),
                    "classification returned duplicate actions for {}",
                    action.id
                );
                anyhow::ensure!(
                    action.id != action.parent_id,
                    "classification cannot attach a memory to itself"
                );
            }
            store.write_transaction(|| {
                let current = store.all(false)?;
                for action in &result.actions {
                    let row = current.iter().find(|row| row.id == action.id)
                        .ok_or_else(|| anyhow!("classification memory disappeared: {}", action.id))?;
                    respire_app::metadata_gate::ensure_write(&store, &MemoryEngine::open(session, row)?)?;
                }
                for action in &result.actions {
                    respire_app::service::reparent(session, &store, &action.id, &action.parent_id)?;
                }
                Ok(())
            })?;
            if !result.actions.is_empty() {
                respire_app::service::counter_reset(
                    &respire_app::service::maintenance_path(),
                    &now_stamp(),
                )?;
            }
            Ok(result.actions.len())
        })?;
        if applied > 0 {
            auto_sync(session, &store);
        }
    }

    let operations: Vec<serde_json::Value> = result
        .actions
        .iter()
        .map(|action| serde_json::json!({"id": action.id, "parent": action.parent_id}))
        .collect();
    if let Some(path) = out {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&serde_json::json!({"ops": operations}))?,
        )
        .map_err(|error| anyhow!("failed to write action preview: {path} ({error})"))?;
    }
    let details = serde_json::json!({
        "mode": mode, "dry_run": dry_run, "items": result.items,
        "ops": operations, "warnings": result.warnings, "applied": applied,
        "backend": backend.as_ref().map(|value| value.name),
        "api_base": backend.as_ref().map(|value| value.base_shown.as_str()),
        "model": backend.as_ref().map(|value| value.model.as_str()),
    });
    if let Some(path) = save {
        std::fs::write(path, serde_json::to_string_pretty(&details)?)
            .map_err(|error| anyhow!("failed to save suggestions: {path} ({error})"))?;
    }
    if let Some(report) = result.report.take() {
        return emit_classify_report(report, details);
    }
    let status = if dry_run {
        OutputStatus::Skip
    } else if result.warnings.is_empty() {
        OutputStatus::Ok
    } else {
        OutputStatus::Warn
    };
    emit_classify_details(
        if plan { "classify-plan" } else { "classify" },
        status,
        result.summary,
        details,
        vec![
            OutputItem::new("mode", status, mode),
            OutputItem::new("suggested_edits", status, result.actions.len().to_string()),
            OutputItem::new("applied", status, applied.to_string()),
        ],
    )
}

