/// Diary = whole-store time chain (trivia is merged in; not filtered by importance):
pub fn run_diary(
    limit: usize,
    date: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    contains: Option<&str>,
) -> Result<()> {
    // Date aliases and check: today/yesterday resolve directly; else YYYY-MM-DD, bad format fails fast
    let today = chrono::Local::now().date_naive();
    let resolve_day = |s: &str| -> Result<chrono::NaiveDate> {
        match s {
            "today" => Ok(today),
            "yesterday" => Ok(today - chrono::Duration::days(1)),
            _ => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| anyhow!("date must be YYYY-MM-DD (or today/yesterday), got: {s}")),
        }
    };
    let date_day = date.map(resolve_day).transpose()?;
    let from_day = from.map(resolve_day).transpose()?;
    let to_day = to.map(resolve_day).transpose()?;
    if date_day.is_some() && (from_day.is_some() || to_day.is_some()) {
        anyhow::bail!("pick one of --date or --from/--to");
    }
    if let (Some(f), Some(t)) = (from_day, to_day) {
        if f > t {
            anyhow::bail!("--from is after --to: {f} > {t}");
        }
    }
    let date_s = date_day.map(|d| d.format("%Y-%m-%d").to_string());
    let from_s = from_day.map(|d| d.format("%Y-%m-%d").to_string());
    let to_s = to_day.map(|d| d.format("%Y-%m-%d").to_string());
    let needle = contains.map(|s| s.to_lowercase());

    let store = build_local()?;
    let mut all: Vec<respire_app::StoredMemory> = store
        .all(false)?
        .into_iter()
        .filter(|m| {
            let d = local_day_of(&m.local_created_at);
            if let Some(dd) = &date_s {
                return d == *dd;
            }
            if let Some(f) = &from_s {
                if d < *f {
                    return false;
                }
            }
            if let Some(t) = &to_s {
                if d > *t {
                    return false;
                }
            }
            true
        })
        .collect();
    if let Some(needle) = &needle {
        let keys = build_session()?;
        let mut matched = Vec::new();
        for mut memory in all {
            let entry = MemoryEngine::open(&keys, &memory)
                .with_context(|| format!("cannot decode diary entry {}", memory.id))?;
            let lowered = entry.content.to_lowercase();
            let position = lowered.find(needle);
            if position.is_some() || entry.title.to_lowercase().contains(needle) {
                // Map the lowercased byte match back to original Unicode scalars.
                let offset = position.map(|byte| {
                    let mut folded_bytes = 0;
                    entry.content.chars().take_while(|ch| {
                        let before = folded_bytes;
                        folded_bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                        before < byte
                    }).count()
                }).unwrap_or(0);
                let start = offset.saturating_sub(100);
                memory.local_content_head = entry.content.chars().skip(start).take(500).collect();
                matched.push(memory);
            }
        }
        all = matched;
    }
    let ranged = date_s.is_some() || from_s.is_some() || to_s.is_some();
    if ranged {
        all.sort_by(|a, b| a.local_created_at.cmp(&b.local_created_at));
    } else {
        all.sort_by(|a, b| b.local_created_at.cmp(&a.local_created_at));
        all.truncate(limit);
    }
    if json_mode() {
        let rows: Vec<serde_json::Value> = all
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id, "created_at": m.local_created_at,
                    "day": local_day_of(&m.local_created_at),
                    "kind": m.local_kind, "importance": m.local_importance,
                    "title": m.local_title,
                    "content": m.local_content_head,
                    // Device tag: how an AI tells which machine recorded this (old data is empty)
                    "device": m.local_device,
                    "modified_by": m.local_modified_by,
                })
            })
            .collect();
        let mut result = ResultEnvelope::new(
            "diary",
            OutputStatus::Ok,
            serde_json::json!({"count":rows.len()}),
            rows.iter()
                .map(|row| {
                    OutputItem::new(
                        row["id"].as_str().unwrap_or(""),
                        OutputStatus::Ok,
                        row["title"].as_str().unwrap_or(""),
                    )
                })
                .collect(),
        );
        result.details = serde_json::Value::Array(rows);
        emit_result(result)?;
        return Ok(());
    }
    if all.is_empty() {
        emit_result(ResultEnvelope::new(
            "diary",
            OutputStatus::Skip,
            serde_json::json!({"count":0}),
            vec![
                OutputItem::new("entries", OutputStatus::Skip, "0").action("widen date or keyword")
            ],
        ))?;
        return Ok(());
    }
    let strip = |s: &str| -> String {
        s.strip_prefix("[diary]")
            .map(|r| r.to_owned())
            .unwrap_or_else(|| s.to_owned())
    };
    let title_of = |m: &respire_app::StoredMemory| -> String {
        let head = strip(&m.local_content_head);
        let head = head.lines().next().unwrap_or("").to_owned();
        if m.local_title.trim().is_empty() {
            head
        } else {
            m.local_title.trim().to_owned()
        }
    };
    let mut items = Vec::new();
    let mut detail_rows = Vec::new();
    for m in &all {
        let dt = chrono::DateTime::parse_from_rfc3339(&m.local_created_at).ok();
        let d = dt
            .as_ref()
            .map(|x| {
                x.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|| local_day_of(&m.local_created_at));
        let t = dt
            .map(|x| x.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "--:--".to_owned());
        // Device label: diary rows also say which machine recorded them, so paths/commands are not copied across devices.
        let dev = dev_label(&m.local_device, &m.local_computer);
        let title = title_of(m);
        items.push(OutputItem::new(
            "entry",
            OutputStatus::Ok,
            format!("{} {} {} {}", d, t, title, dev),
        ));
        detail_rows.push(serde_json::json!({"id":m.id,"day":d,"time":t,"title":title,"kind":m.local_kind,"importance":m.local_importance,"device":dev,"content":strip(&m.local_content_head)}));
    }
    let mut result = ResultEnvelope::new(
        "diary",
        OutputStatus::Ok,
        serde_json::json!({"count":all.len(),"date":date_s,"from":from_s,"to":to_s,"contains":needle}),
        items,
    );
    result.details = serde_json::Value::Array(detail_rows);
    emit_result(result)?;
    Ok(())
}

/// Built-in catalog: --list prints it; --ensure <root,...> creates/fills roots; no args shows current state (built roots + lone roots).
pub fn run_taxonomy(list: bool, ensure: Option<&str>) -> Result<()> {
    if list {
        let items = respire_app::taxonomy::CATALOG
            .iter()
            .map(|c| {
                OutputItem::new(
                    if c.ai { "AI-domain" } else { "human-domain" },
                    OutputStatus::Ok,
                    format!("{} - {}", c.title, c.gist),
                )
            })
            .collect();
        return emit_result(ResultEnvelope::new(
            "taxonomy",
            OutputStatus::Ok,
            serde_json::json!({"count":respire_app::taxonomy::CATALOG.len()}),
            items,
        ));
    }
    let app = respire_app::service::App::open()?;
    match ensure {
        Some(names) => {
            let mut built: Vec<(String, String)> = Vec::new();
            for name in names.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let Some(idx) = respire_app::taxonomy::find_by_title(name) else {
                    anyhow::bail!("\"{name}\" is not a built-in catalog root - see rsrs taxonomy --list; a root outside the catalog uses root-create (needs the user to say yes)");
                };
                let rid = respire_app::taxonomy::ensure_category_root(
                    &app.keys,
                    &app.embedder,
                    &app.store,
                    idx,
                )?;
                built.push((name.to_owned(), rid));
            }
            auto_sync(&app.keys, &app.store);
            let mut result = ResultEnvelope::new(
                "taxonomy",
                OutputStatus::Ok,
                serde_json::json!({"ensured":built.len()}),
                built
                    .iter()
                    .map(|(t, id)| {
                        OutputItem::new(t, OutputStatus::Ok, respire_app::service::short_id(id))
                    })
                    .collect(),
            );
            result.details = serde_json::json!(built
                .iter()
                .map(|(t, id)| serde_json::json!({"title":t,"root_id":id}))
                .collect::<Vec<_>>());
            emit_result(result)?;
        }
        None => {
            let all = app.store.all(false)?;
            let mut built = 0usize;
            let mut missing: Vec<&str> = Vec::new();
            for c in respire_app::taxonomy::CATALOG.iter() {
                let rid = respire_app::taxonomy::root_id(c.title);
                if all.iter().any(|m| m.id == rid) {
                    built += 1;
                } else {
                    missing.push(c.title);
                }
            }
            let lone = respire_app::taxonomy::lone_roots(&app.store)?;
            let mut result = ResultEnvelope::new(
                "taxonomy",
                if missing.is_empty() {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Warn
                },
                serde_json::json!({"total":respire_app::taxonomy::CATALOG.len(),"built":built,"missing":missing,"lone_roots":lone.len()}),
                vec![
                    OutputItem::new("built", OutputStatus::Ok, built.to_string()),
                    OutputItem::new(
                        "lone roots",
                        if lone.is_empty() {
                            OutputStatus::Ok
                        } else {
                            OutputStatus::Warn
                        },
                        lone.len().to_string(),
                    ),
                ],
            );
            result.actions.push("taxonomy --ensure <roots>".into());
            emit_result(result)?;
        }
    }
    Ok(())
}

/// Create a root outside the catalog: AI proposes -> user allows -> --yes writes. Without --yes, only print the ask; do not write.
pub fn run_root_create(title: &str, content: Option<&str>, yes: bool) -> Result<()> {
    if respire_app::taxonomy::find_by_title(title).is_some() {
        anyhow::bail!("\"{title}\" is already a built-in catalog root - rsrs taxonomy --ensure \"{title}\" creates it");
    }
    if !yes {
        let msg = format!(
            "creating a top-level root needs the user to say yes: ask \"create top-level '{title}'? gist: {}\" - rerun with --yes after they allow it; do not create without that",
            content.unwrap_or(title)
        );
        let mut result = ResultEnvelope::new(
            "root-create",
            OutputStatus::Pending,
            serde_json::json!({"title":title,"content":content.unwrap_or(title)}),
            vec![
                OutputItem::new("confirmation", OutputStatus::Pending, msg.clone())
                    .action("rerun with --yes after user approval"),
            ],
        );
        result.actions.push("root-create --yes".into());
        emit_result(result)?;
        return Ok(());
    }
    let app = respire_app::service::App::open()?;
    let clash = app
        .store
        .all(false)?
        .into_iter()
        .any(|m| m.local_title == title);
    if clash {
        anyhow::bail!("an entry titled \"{title}\" already exists - rename, or hang under it (rsrs show for the id)");
    }
    let rid = respire_app::taxonomy::root_id(title);
    let now = now_stamp();
    let entry = respire_app::MemoryEntry {
        metadata: Default::default(),
        unknown_fields: Default::default(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
        id: rid.clone(),
        kind: Kind::Knowledge,
        tags: vec!["catalog root".to_owned(), "custom".to_owned()],
        title: title.to_owned(),
        content: content.unwrap_or(title).to_owned(),
        user: current_user(),
        computer: respire_app::service::device_tag(),
        device: respire_app::service::device_tag(),
        modified_by: respire_app::service::device_tag(),
        project: String::new(),
        created_at: now.clone(),
        updated_at: now,
        emotion: -1.0,
        parent_id: String::new(),
        importance: "important".to_owned(),
    };
    let stored = MemoryEngine::seal(&app.keys, &app.embedder, &entry, &entry.user)?;
    app.store.put(&stored)?;
    auto_sync(&app.keys, &app.store);
    let mut result = ResultEnvelope::new(
        "root-create",
        OutputStatus::Ok,
        serde_json::json!({ "root_id": rid, "title": title }),
        vec![OutputItem::new(
            "root",
            OutputStatus::Ok,
            respire_app::service::short_id(&rid),
        )],
    );
    result.actions.push(format!(
        "remember <body> --parent {}",
        respire_app::service::short_id(&rid)
    ));
    emit_result(result)?;
    Ok(())
}

/// Tree hygiene: report (root sizes + attach suggestions) or apply (--id --parent).
pub fn run_tree_cure(
    top: usize,
    id: Option<&str>,
    parent: Option<&str>,
    auto: bool,
    min: f32,
) -> Result<()> {
    if auto {
        let app = respire_app::service::App::open()?;
        let candidates = app.store.all(false)?;
        respire_app::service::require_candidates_ready(&app.store, &candidates)?;
        let report: respire_app::core_sdk::reports::TreeCureReport = respire_app::core_sdk::execute(
            "tree_cure",
            serde_json::json!({
                "snapshots": respire_app::core_sdk::metadata_snapshots(&candidates),
                "top": top, "min": min, "auto": true,
            }),
        )?;
        app.store.write_transaction(|| {
            let current = app.store.all(false)?;
            for suggestion in &report.suggests {
                let row = current.iter().find(|row| row.id == suggestion.orphan.id)
                    .ok_or_else(|| anyhow!("tree-cure memory disappeared: {}", suggestion.orphan.id))?;
                respire_app::metadata_gate::ensure_write(&app.store, &MemoryEngine::open(&app.keys, row)?)?;
            }
            for suggestion in &report.suggests {
                respire_app::service::reparent(
                    &app.keys,
                    &app.store,
                    &suggestion.orphan.id,
                    &suggestion.target.id,
                )?;
            }
            Ok(())
        })?;
        if !report.suggests.is_empty() {
            auto_sync(&app.keys, &app.store);
        }
        let items = report
            .suggests
            .iter()
            .map(|suggestion| {
                OutputItem::new(
                    "attach",
                    OutputStatus::Ok,
                    format!(
                        "{} -> {}",
                        suggestion.orphan.title, suggestion.target_tree.title
                    ),
                )
            })
            .collect();
        let mut result = ResultEnvelope::new(
            "tree-cure",
            OutputStatus::Ok,
            serde_json::json!({"attached": report.suggests.len(), "failed": 0, "threshold": min}),
            items,
        );
        result.details = serde_json::to_value(&report)?;
        emit_result(result)?;
        return Ok(());
    }
    let app = respire_app::service::App::open()?;
    if let (Some(id), Some(parent)) = (id, parent) {
        let (child, parent_full) = app.attach(id, parent)?;
        emit_result(ResultEnvelope::new(
            "tree-cure",
            OutputStatus::Ok,
            serde_json::json!({"child":child,"parent":parent_full}),
            vec![
                OutputItem::new(
                    "child",
                    OutputStatus::Ok,
                    respire_app::service::short_id(&child),
                ),
                OutputItem::new(
                    "parent",
                    OutputStatus::Ok,
                    respire_app::service::short_id(&parent_full),
                ),
            ],
        ))?;
        return Ok(());
    }
    let report = app.tree_cure_with_min(top, min)?;
    if json_mode() {
        let mut result = ResultEnvelope::new(
            "tree-cure",
            OutputStatus::Ok,
            serde_json::json!({"roots":report.roots,"suggestions":report.suggests.len()}),
            Vec::new(),
        );
        result.details = serde_json::to_value(&report)?;
        emit_result(result)?;
        return Ok(());
    }
    let mut items = vec![OutputItem::new(
        "roots",
        OutputStatus::Ok,
        format!(
            "{} total, {} lone leaf",
            report.roots.len(),
            report.lone_roots
        ),
    )];
    for r in report.roots.iter().take(top) {
        items.push(OutputItem::new(
            "root",
            OutputStatus::Ok,
            format!(
                "{} children {} {}",
                r.descendants,
                r.title,
                respire_app::service::short_id(&r.id)
            ),
        ));
    }
    if report.suggests.is_empty() {
        emit_result(ResultEnvelope::new(
            "tree-cure",
            OutputStatus::Ok,
            serde_json::json!({"roots":report.roots.len(),"suggestions":0,"threshold":min}),
            items,
        ))?;
        return Ok(());
    }
    for s in &report.suggests {
        items.push(
            OutputItem::new(
                "suggestion",
                OutputStatus::Pending,
                format!(
                    "{} {} -> {} {} tree {}",
                    s.orphan.title,
                    respire_app::service::short_id(&s.orphan.id),
                    s.target.title,
                    respire_app::service::short_id(&s.target.id),
                    s.target_tree.title
                ),
            )
            .action("tree-cure --id <id> --parent <parent>"),
        );
    }
    let mut result = ResultEnvelope::new(
        "tree-cure",
        OutputStatus::Pending,
        serde_json::json!({"roots":report.roots.len(),"suggestions":report.suggests.len(),"threshold":min}),
        items,
    );
    result
        .actions
        .push("tree-cure --id <lone-id> --parent <target-id>".into());
    emit_result(result)?;
    Ok(())
}

