pub fn run_tree_deepen(
    root: Option<&str>,
    go: bool,
    titles_json: Option<&str>,
    auto: bool,
    min: f32,
) -> Result<()> {
    let app = respire_app::service::App::open()?;
    match (root, auto) {
        (_, true) => {
            let (roots, mut batch) = respire_app::service::DeepenAuto::open(&app.store)?;
            let mut total_built = 0usize;
            let mut total_moved = 0usize;
            let mut items = Vec::new();
            let mut details = Vec::new();
            for r in &roots {
                let plan = match batch.plan(&r.id, min) {
                    Ok(Some(p)) if !p.sub_roots.is_empty() => p,
                    Err(error) if error.downcast_ref::<respire_app::service::IndexPending>().is_some() => return Err(error),
                    _ => continue,
                };
                let titles: Vec<String> = plan.sub_roots.iter().map(|s| s.title.clone()).collect();
                let (b, m) = respire_app::service::deepen_apply(
                    &app.keys,
                    &app.embedder,
                    &app.store,
                    &r.id,
                    &plan,
                    &titles,
                )?;
                total_built += b;
                total_moved += m;
                let name = if r.title.is_empty() {
                    respire_app::service::short_id(&r.id)
                } else {
                    r.title.clone()
                };
                items.push(OutputItem::new(
                    name,
                    OutputStatus::Ok,
                    format!(
                        "built={b}; moved={m}; root={}",
                        respire_app::service::short_id(&r.id)
                    ),
                ));
                details.push(serde_json::json!({
                    "root": r.id,
                    "title": r.title,
                    "built": b,
                    "moved": m,
                    "sub_roots": titles,
                }));
            }
            let mut result = ResultEnvelope::new(
                "tree-deepen",
                OutputStatus::Ok,
                serde_json::json!({
                    "mode": "auto",
                    "roots": items.len(),
                    "built": total_built,
                    "moved": total_moved,
                }),
                items,
            );
            result.details = serde_json::Value::Array(details);
            emit_result(result)?;
        }
        (Some(root_id), false) => {
            let (rid, plan) = respire_app::service::deepen_plan(&app.store, root_id, min)?;
            if !go {
                let items = plan
                    .sub_roots
                    .iter()
                    .enumerate()
                    .map(|(index, sub)| {
                        OutputItem::new(
                            format!("sub_root_{}", index + 1),
                            OutputStatus::Pending,
                            format!("title={}; members={}", sub.title, sub.member_ids.len()),
                        )
                    })
                    .collect();
                let mut result = ResultEnvelope::new(
                    "tree-deepen",
                    OutputStatus::Pending,
                    serde_json::json!({
                        "mode": "plan",
                        "root": rid,
                        "sub_roots": plan.sub_roots.len(),
                        "min": min,
                    }),
                    items,
                );
                result.details = serde_json::to_value(&plan)?;
                result.actions.push(format!(
                    "tree-deepen --root {root_id} --go --titles '[\"title1\",\"title2\"]'"
                ));
                return emit_result(result);
            }
            let Some(tj) = titles_json else {
                anyhow::bail!("--go requires --titles '<sub-root title array JSON>'");
            };
            let titles: Vec<String> = serde_json::from_str(tj)
                .map_err(|e| anyhow!("title array failed to parse: {e}"))?;
            let (b, m) = respire_app::service::deepen_apply(
                &app.keys,
                &app.embedder,
                &app.store,
                &rid,
                &plan,
                &titles,
            )?;
            emit_result(ResultEnvelope::new(
                "tree-deepen",
                OutputStatus::Ok,
                serde_json::json!({
                    "mode": "apply",
                    "root": rid,
                    "built": b,
                    "moved": m,
                }),
                vec![OutputItem::new(
                    "tree-deepen",
                    OutputStatus::Ok,
                    format!(
                        "built={b}; moved={m}; root={}",
                        respire_app::service::short_id(&rid)
                    ),
                )],
            ))?;
        }
        (None, false) => {
            let all = app.store.all(false)?;
            let mut flat = 0usize;
            let mut items = Vec::new();
            let mut details = Vec::new();
            for r in all.iter().filter(|m| m.local_parent_id.is_empty()) {
                match respire_app::service::deepen_plan(&app.store, &r.id, min) {
                    Ok((_, plan)) => {
                        if plan.sub_roots.is_empty() {
                            continue;
                        }
                        flat += 1;
                        let title = if r.local_title.is_empty() {
                            "(untitled)"
                        } else {
                            &r.local_title
                        };
                        items.push(OutputItem::new(
                            respire_app::service::short_id(&r.id),
                            OutputStatus::Pending,
                            format!("title={title}; sub_roots={}", plan.sub_roots.len()),
                        ));
                        details.push(serde_json::json!({
                            "root": r.id,
                            "title": title,
                            "sub_roots": plan.sub_roots,
                        }));
                    }
                    Err(error) if error.downcast_ref::<respire_app::service::IndexPending>().is_some() => return Err(error),
                    Err(_) => {}
                }
            }
            let mut result = ResultEnvelope::new(
                "tree-deepen",
                if flat == 0 {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Pending
                },
                serde_json::json!({"mode":"scan","flat_roots":flat,"min":min}),
                items,
            );
            result.details = serde_json::Value::Array(details);
            if flat > 0 {
                result.actions.push("tree-deepen --root <id>".into());
                result.actions.push("tree-deepen --auto".into());
            }
            emit_result(result)?;
        }
    }
    Ok(())
}

/// TUI/non-TUI dispatch for `extract`, kept out of `run_local_inner` to bound its stack frame.
pub fn run_extract_command(command: &Command) -> Result<()> {
    let Command::Extract {
        targets,
        tui,
        all,
        id,
        dry_run,
        since,
        limit,
        cwd,
        model,
        session,
        kind,
        importance,
        parent,
        include_subagents,
        force,
    } = command
    else {
        return Ok(());
    };
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());
    let filters_absent = since.is_none()
        && limit.is_none()
        && cwd.is_none()
        && model.is_none()
        && session.is_none()
        && kind.as_str() == "context"
        && importance.as_str() == "important"
        && parent.is_empty()
        && !*include_subagents
        && !*force;
    let bare = !*targets && id.is_none() && !*all && !*dry_run && filters_absent && !json_mode();
    if *tui {
        if !bare {
            anyhow::bail!(
                "--tui opens the interactive picker and cannot be combined with extraction options \
                 (--since/--limit/--cwd/--model/--session/--kind/--importance/--parent/--include-subagents/--force) \
                 or --json; drop --tui to apply them."
            );
        }
        anyhow::bail!("the extract TUI requires the standalone CLI or Client2");
    }
    if bare && interactive {
        anyhow::bail!("the extract TUI requires the standalone CLI or Client2");
    }
    run_extract(
        *targets,
        id.as_deref(),
        *dry_run,
        since.as_deref(),
        *limit,
        cwd.as_deref(),
        model.as_deref(),
        session.as_deref(),
        kind,
        importance,
        parent,
        *include_subagents,
        *force,
    )
}

/// Extract local coding-agent sessions into rsrs memories (see `extract --help`).
pub fn run_extract(
    targets_only: bool,
    id: Option<&str>,
    dry_run: bool,
    since: Option<&str>,
    limit: Option<usize>,
    cwd: Option<&str>,
    model_filter: Option<&str>,
    session_filter: Option<&str>,
    kind: &str,
    importance: &str,
    parent: &str,
    include_subagents: bool,
    force: bool,
) -> Result<()> {
    if !matches!(importance, "important" | "trivial") {
        anyhow::bail!("--importance must be important or trivial");
    }
    if !matches!(
        kind,
        "context" | "decision" | "preference" | "task" | "emotion" | "time" | "skill" | "knowledge"
    ) {
        anyhow::bail!("--kind must be context|decision|preference|task|emotion|time|skill|knowledge");
    }
    if targets_only {
        let sources = respire_app::session_import::source_list();
        let installed = sources.iter().filter(|source| source.installed).count();
        let items = sources
            .iter()
            .map(|source| {
                OutputItem::new(
                    source.id.clone(),
                    if source.installed { OutputStatus::Ok } else { OutputStatus::Skip },
                    if source.installed {
                        format!("{} ({} root(s))", source.name, source.roots.len())
                    } else {
                        format!("{} not found", source.name)
                    },
                )
            })
            .collect();
        let details = serde_json::Value::Array(
            sources
                .iter()
                .map(|source| {
                    serde_json::json!({
                        "id": source.id,
                        "name": source.name,
                        "installed": source.installed,
                        "roots": source.roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        );
        let mut result = ResultEnvelope::new(
            "extract",
            OutputStatus::Ok,
            serde_json::json!({"count": sources.len(), "installed": installed}),
            items,
        );
        result.details = details;
        result.actions.push("extract --all --dry-run".into());
        return emit_result(result);
    }

    let scan_options = respire_app::session_import::ScanOptions {
        roots: Vec::new(),
        cwd: cwd.map(str::to_owned),
        model: model_filter.map(str::to_owned),
        since: match since {
            None => None,
            Some(value) => Some(
                respire_app::session_import::scan_util::date_bound(value, false).ok_or_else(|| {
                    respire_app::input_error::InputError(format!(
                        "invalid --since '{value}': use today|yesterday|Nd|YYYY-MM-DD"
                    ))
                })?,
            ),
        },
        until: None,
        limit: limit.unwrap_or(200),
        include_subagents,
    };

    let scanners: Vec<Box<dyn respire_app::session_import::SessionScanner>> = match id {
        Some(id) => vec![respire_app::session_import::find_scanner(id).ok_or_else(|| {
            anyhow::anyhow!("unknown platform '{id}' (run `rsrs extract --targets`)")
        })?],
        None => respire_app::session_import::scanners()
            .into_iter()
            .filter(|scanner| !scanner.roots().is_empty())
            .collect(),
    };
    if scanners.is_empty() {
        return emit_result(ResultEnvelope::new(
            "extract",
            OutputStatus::Skip,
            serde_json::json!({"reason": "no installed session stores detected"}),
            Vec::new(),
        ));
    }

    let mut sessions = Vec::new();
    for scanner in &scanners {
        let label = format!("Scanning {} sessions", scanner.name());
        progress::phase(&label, &label);
        let mut found = scanner.scan(&scan_options);
        found.retain(|session| scan_options.matches(session));
        if let Some(filter) = session_filter {
            found.retain(|session| session.id.contains(filter));
        }
        sessions.extend(found);
    }
    let sessions = respire_app::session_import::dedupe(sessions);
    let discovered = sessions.len();
    let mut cursor = respire_app::session_import::Cursor::load();
    let fresh: Vec<respire_app::session_import::Session> = sessions
        .into_iter()
        .filter(|session| force || !cursor.is_imported(session))
        .collect();
    let not_imported = fresh.len();
    let pending = respire_app::session_import::cap_per_platform(fresh, scan_options.limit);

    let convert_options = respire_app::session_import::ConvertOptions {
        kind: respire_app::Kind::try_from_str(&kind.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?,
        importance: importance.to_owned(),
        parent: String::new(),
        user: respire_app::service::current_user(),
        device: respire_app::service::device_tag(),
        project: String::new(),
    };
    let mut entries: Vec<respire_app::MemoryEntry> = pending
        .iter()
        .map(|session| respire_app::session_import::session_entry(session, &convert_options))
        .collect();

    let platform_counts: Vec<(String, usize)> = scanners
        .iter()
        .map(|scanner| {
            let id = scanner.id();
            (
                id.to_owned(),
                pending.iter().filter(|session| session.platform == id).count(),
            )
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    let items: Vec<OutputItem> = platform_counts
        .iter()
        .map(|(id, count)| OutputItem::new(id.clone(), OutputStatus::Ok, format!("{count} session(s)")))
        .collect();

    if dry_run {
        let mut result = ResultEnvelope::new(
            "extract",
            OutputStatus::Ok,
            serde_json::json!({
                "dry_run": true,
                "scanned": discovered,
                "pending": entries.len(),
                "skipped_imported": discovered - not_imported,
                "deferred_by_limit": not_imported - pending.len(),
                "by_platform": platform_counts.iter().map(|(id, count)| serde_json::json!({"id": id, "count": count})).collect::<Vec<_>>(),
            }),
            items,
        );
        result.details = serde_json::json!({
            "sessions": pending.iter().map(|session| session.to_json()).collect::<Vec<_>>(),
        });
        result.actions.push("extract --all".into());
        return emit_result(result);
    }

    if entries.is_empty() {
        let mut result = ResultEnvelope::new(
            "extract",
            OutputStatus::Skip,
            serde_json::json!({"imported": 0, "scanned": discovered, "skipped_imported": discovered - not_imported, "deferred_by_limit": not_imported}),
            Vec::new(),
        );
        if discovered > 0 && not_imported == 0 {
            result.actions.push("extract --all --force".into());
        }
        return emit_result(result);
    }

    progress::phase("Embedding and storing memories", "Embedding and storing memories");
    let outcome = respire_app::session_import::import_entries(
        &mut entries,
        parent,
        |done, total| progress::phase_count("\u{5199}\u{5165}\u{8bb0}\u{5fc6}", "Storing memories", done, total),
        |start, completed| {
            for (offset, ok) in completed.iter().enumerate() {
                if *ok {
                    cursor.mark(&pending[start + offset]);
                }
            }
            cursor.save()?;
            Ok(())
        },
    )?;
    let imported = outcome.imported;

    let mut result = ResultEnvelope::new(
        "extract",
        OutputStatus::Ok,
        serde_json::json!({
            "imported": imported,
            "scanned": discovered,
            "deferred_by_limit": not_imported - pending.len(),
            "by_platform": platform_counts.iter().map(|(id, count)| serde_json::json!({"id": id, "count": count})).collect::<Vec<_>>(),
        }),
        items,
    );
    result
        .actions
        .push("tidy imported roots: rsrs list --since-resort --json".into());
    emit_result(result)
}

/// Inject-source distribution: no args injects every detected host; --targets lists status; --id injects/uninstalls.

pub fn run_split(id: &str, go: bool, spec_json: Option<&str>) -> Result<()> {
    let app = respire_app::service::App::open()?;
    let detail = app.show(id)?;
    if !go {
        let m = respire_app::service::split_material(&detail.entry, &detail.ancestors);
        let mut result = ResultEnvelope::new(
            "split",
            OutputStatus::Skip,
            serde_json::json!({"id": id, "content_len": m.content_len}),
            vec![
                OutputItem::new("plan", OutputStatus::Skip, "inspection only")
                    .action(format!("rsrs split {id} --go --spec '<split-plan-json>'")),
                OutputItem::new("id", OutputStatus::Ok, m.id.clone()),
                OutputItem::new("title", OutputStatus::Ok, m.title.clone()),
                OutputItem::new("kind", OutputStatus::Ok, m.kind.clone()),
                OutputItem::new("content_len", OutputStatus::Ok, m.content_len.to_string()),
                OutputItem::new("parent", OutputStatus::Ok, m.parent_id.clone()),
                OutputItem::new("CONTENT", OutputStatus::Skip, m.content.clone()),
            ],
        );
        result.details = serde_json::to_value(&m)?;
        result
            .actions
            .push("read details and provide --spec JSON".to_owned());
        return emit_result(result);
    }
    let Some(spec_json) = spec_json else {
        return Err(respire_app::input_error::InputError(
            "--go requires --spec '<split-plan JSON>' (run with no flags first to get material)".into()
        ).into());
    };
    let spec: respire_app::service::SplitSpec =
        serde_json::from_str(spec_json).map_err(|e| respire_app::input_error::InputError(format!("plan JSON failed to parse: {e}")))?;
    let n =
        respire_app::service::split_exec(&app.keys, &app.embedder, &app.store, &detail.entry, &spec)?;
    let mut result = ResultEnvelope::new(
        "split",
        OutputStatus::Ok,
        serde_json::json!({"created": n, "root_replaced": spec.summary.is_some()}),
        vec![OutputItem::new("created", OutputStatus::Ok, n.to_string())],
    );
    result.details = serde_json::json!({"root_replaced": spec.summary.is_some()});
    emit_result(result)
}

/// Post-op orphan check: report, do not fix - CLI detects, the model decides; the AI patches with resort after reading.
/// purge resolver: **do not filter deleted entries** - purge's point is to clear tombstone ciphertext.
/// Exact id -> 8-char prefix (including deleted); multiple hits report "prefix not unique" so we do not delete the wrong one.
pub fn resolve_prefix_incl_deleted(
    store: &respire_app::transport::local::LocalStore,
    prefix: &str,
) -> Result<String> {
    let prefix = prefix.trim_start_matches('#');
    let all = store.all(true)?;
    if let Some(m) = all.iter().find(|s| s.id == prefix) {
        return Ok(m.id.clone());
    }
    let chars: Vec<char> = prefix.chars().collect();
    let p8: String = chars.iter().take(8).collect();
    let hits: Vec<String> = all
        .iter()
        .filter(|s| s.id.chars().take(8).collect::<String>() == p8)
        .map(|s| s.id.clone())
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => anyhow::bail!("no such entry (including deleted): {prefix}"),
        n => anyhow::bail!("prefix not unique ({n} entries): {prefix}"),
    }
}

