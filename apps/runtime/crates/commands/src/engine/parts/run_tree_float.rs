/// Heat float: high-hit memories move up the cause chain (reattach to grandparent); the tree self-organizes by recall heat.
/// Dry-run prints a report (hot entry -> new cause); --go applies.
pub fn run_tree_float(go: bool, min: i64) -> Result<()> {
    let app = respire_app::service::App::open()?;
    let report = app.tree_float(go, min)?;
    if json_mode() {
        let mut result = ResultEnvelope::new(
            "tree-float",
            if report.items.is_empty() {
                OutputStatus::Ok
            } else if go {
                OutputStatus::Ok
            } else {
                OutputStatus::Pending
            },
            serde_json::json!({"items":report.items.len(),"applied":report.applied}),
            Vec::new(),
        );
        result.details = serde_json::to_value(&report)?;
        if !go && !report.items.is_empty() {
            result.actions.push("tree-float --go".into());
        }
        emit_result(result)?;
        return Ok(());
    }
    if report.items.is_empty() {
        emit_result(ResultEnvelope::new(
            "tree-float",
            OutputStatus::Ok,
            serde_json::json!({"items":0,"applied":report.applied,"min_hits":min}),
            vec![OutputItem::new("candidates", OutputStatus::Ok, "0")],
        ))?;
        return Ok(());
    }
    let mut items = Vec::new();
    for it in &report.items {
        items.push(
            OutputItem::new(
                "candidate",
                if go {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Pending
                },
                format!(
                    "{} good {} {} -> {} {}",
                    it.good_marks,
                    it.title,
                    respire_app::service::short_id(&it.id),
                    it.new_parent_title,
                    respire_app::service::short_id(&it.new_parent)
                ),
            )
            .action(if go { "applied" } else { "tree-float --go" }),
        );
    }
    let mut result = ResultEnvelope::new(
        "tree-float",
        if go {
            OutputStatus::Ok
        } else {
            OutputStatus::Pending
        },
        serde_json::json!({"items":report.items.len(),"applied":report.applied,"min_hits":min}),
        items,
    );
    if !go {
        result.actions.push("tree-float --go".into());
    }
    emit_result(result)?;
    Ok(())
}

/// Ask Core for a read-only report of related entries and tree structure.
///
/// **Trivia is not in the tree** (fixed 2026-09-20): drop trivial the same way as `tree`/`tree_cure_with_min` -
/// trivia belongs in the diary (inject §3.4); mixing it in inflates the tree bill (once reported "27 roots" vs a real 23).
/// Orphans after the drop (parent was trivia) count as promoted-to-root, same as `tree`.
pub fn run_defrag(min: f32, top: usize) -> Result<()> {
    use respire_app::memory::defrag;
    let store = build_local()?;
    let list = respire_app::service::tree_scope_list(&store.all(false)?);
    respire_app::service::require_candidates_ready(&store, &list)?;
    let report = defrag::analyze(&list, min)?;
    if json_mode() {
        let mut result = ResultEnvelope::new(
            "defrag",
            OutputStatus::Ok,
            serde_json::json!({"roots":report.roots,"clusters":report.clusters.len()}),
            Vec::new(),
        );
        result.details = serde_json::to_value(&report)?;
        emit_result(result)?;
        return Ok(());
    }
    let mut items = vec![
        OutputItem::new("active", OutputStatus::Ok, list.len().to_string()),
        OutputItem::new(
            "tree",
            OutputStatus::Ok,
            format!(
                "{} roots, max depth {}, {} lone leaves",
                report.roots, report.max_depth, report.orphans
            ),
        ),
    ];
    if report.clusters.is_empty() {
        emit_result(ResultEnvelope::new(
            "defrag",
            OutputStatus::Ok,
            serde_json::json!({"active":list.len(),"clusters":0,"threshold":min}),
            items,
        ))?;
        return Ok(());
    }
    if report.settled > 0 {
        items.push(OutputItem::new(
            "settled clusters",
            OutputStatus::Ok,
            report.settled.to_string(),
        ));
    }
    for (i, c) in report.clusters.iter().take(top).enumerate() {
        items.push(OutputItem::new(
            format!("cluster {}", i + 1),
            OutputStatus::Pending,
            format!("{} members", c.members.len()),
        ));
        for m in &c.members {
            items.push(OutputItem::new(
                "member",
                OutputStatus::Pending,
                format!(
                    "depth {} date {} id {} {}",
                    m.depth,
                    m.date,
                    &m.id[..m.id.len().min(8)],
                    m.title
                ),
            ));
        }
    }
    if report.clusters.len() > top {
        items.push(
            OutputItem::new(
                "omitted clusters",
                OutputStatus::Skip,
                (report.clusters.len() - top).to_string(),
            )
            .action("raise --top"),
        );
    }
    let mut result = ResultEnvelope::new(
        "defrag",
        OutputStatus::Pending,
        serde_json::json!({"active":list.len(),"clusters":report.clusters.len(),"threshold":min,"shown":top.min(report.clusters.len())}),
        items,
    );
    result
        .actions
        .push("review clusters and use remember --merge-ids or --parent".into());
    emit_result(result)?;
    Ok(())
}

fn run_runtime_doctor(check_remote: bool) -> Result<()> {
    let mut rows = Vec::new();
    let roots = respire_app::service::resolved_roots()?;
    progress::phase("Checking local data and files", "Checking local data and files");
    let store = build_local();
    match &store {
        Ok(store) => match store.all(true) {
            Ok(entries) => rows.push(OutputItem::new("store", OutputStatus::Ok, format!("{} entries", entries.len()))),
            Err(error) => rows.push(OutputItem::new("store", OutputStatus::Fail, format!("read failed: {error:#}"))),
        },
        Err(error) => rows.push(OutputItem::new("store", OutputStatus::Fail, format!("open failed: {error:#}"))),
    }
    rows.push(OutputItem::new("data dir", OutputStatus::Ok, roots.account_data_root.display().to_string()));
    let model_task = crate::model_progress::status()?;
    let index_state = rpc::index_status();
    progress::phase("Checking model and index", "Checking model and index");
    if model_task["active"] == true {
        rows.push(OutputItem::new("embedder", OutputStatus::Warn, "Model operation in progress; inference check deferred"));
    } else {
        match crate::model_assets::assets_status() {
            Ok(status) if status.complete => rows.push(OutputItem::new("embedder", OutputStatus::Ok, format!("fresh inference verified: {}", serde_json::to_string(&status.backend_load)?))),
            Ok(status) => rows.push(OutputItem::new("embedder", OutputStatus::Fail, format!("Assets or backend unavailable: {}; install or repair assets through the host CLI", serde_json::to_string(&status.backend_load)?))),
            Err(error) => rows.push(OutputItem::new("embedder", OutputStatus::Fail, format!("inference unavailable: {error:#}; install or repair assets through the host CLI"))),
        }
    }
    if let Ok(store) = &store {
        match store.retrieval_model().and_then(|model| store.index_pending(&model)) {
            Ok(false) => rows.push(OutputItem::new("model index", OutputStatus::Ok, "Index ready")),
            Ok(true) => rows.push(OutputItem::new("model index", OutputStatus::Warn, format!("Index pending: {index_state}"))),
            Err(error) => rows.push(OutputItem::new("model index", OutputStatus::Fail, format!("Index check failed: {error:#}"))),
        }
    }
    match remote_config_from_session_or_env() {
        Ok(Some((address, _))) if check_remote => {
            progress::phase("Checking server connection; waiting for response", "Checking server connection; waiting for response");
            match ureq::get(&format!("{}/health", address.trim().trim_end_matches('/')))
                .timeout(std::time::Duration::from_secs(5)).call()
            {
                Ok(response) => rows.push(OutputItem::new("remote", OutputStatus::Ok, format!("Configured remote /health -> {}", response.status()))),
                Err(ureq::Error::Status(code, _)) => rows.push(OutputItem::new("remote", OutputStatus::Fail, format!("Remote health returned HTTP {code}"))),
                Err(ureq::Error::Transport(error)) => rows.push(OutputItem::new("remote", OutputStatus::Fail, format!("Remote health transport failed: {:?}", error.kind()))),
            }
        }
        Ok(Some(_)) => rows.push(OutputItem::new("remote", OutputStatus::Ok, "Configured (--remote probes reachability)")),
        Ok(None) => rows.push(OutputItem::new("remote", OutputStatus::Skip, "Not configured (local-only)")),
        Err(_) => rows.push(OutputItem::new("remote", OutputStatus::Fail, "Remote configuration is incomplete or invalid; repair it through the host CLI")),
    }
    let mode = respire_app::service::workspace_mode();
    rows.push(OutputItem::new("memory status", if mode == "normal" || mode == "readonly" {
        OutputStatus::Ok
    } else {
        OutputStatus::Fail
    }, mode));
    let (count, threshold) = respire_app::service::counter_peek(&respire_app::service::maintenance_path());
    rows.push(OutputItem::new("tidy counter", if count < threshold { OutputStatus::Ok } else { OutputStatus::Warn }, format!("{count}/{threshold}")));
    rows.push(OutputItem::new("lock", OutputStatus::Skip, "Ownership is verified by the Runtime lifecycle; this diagnostic does not reacquire the process lock"));
    rows.push(OutputItem::new("Runtime version", OutputStatus::Ok, crate::app_version::embedded()));
    let count = |status: OutputStatus| rows.iter().filter(|row| row.status.as_str() == status.as_str()).count();
    let mut result = ResultEnvelope::new("doctor", output::status_from_items(&rows), serde_json::json!({
        "runtime_version": crate::app_version::embedded(), "scope": "runtime",
        "pass": count(OutputStatus::Ok), "warn": count(OutputStatus::Warn),
        "fail": count(OutputStatus::Fail), "skip": count(OutputStatus::Skip), "total": rows.len(),
        "host_checks": "Run host CLI diagnostics for credentials, injection, installation and CLI updates"
    }), rows);
    result.details = serde_json::json!({"model_operation": model_task, "retrieval_index": index_state});
    emit_result(result)
}

/// One bounded network pass on the runtime synchronization worker.
pub fn background_sync_once() -> Result<bool> {
    if respire_app::service::service_paused() { return Ok(false); }
    let configured = sync_phase(|| Ok(respire_app::service::autosync_active() && remote_configured()))?;
    if !configured {
        return Ok(false);
    }
    let (session, local, remote) =
        sync_phase(|| Ok((build_session()?, build_local()?, build_remote()?)))?;
    let stats = sync_with_retry(&session, &local, &remote)?;
    crate::rpc_serve::deliver_post_sync(stats.pulled, stats.pushed)?;
    eprintln!(
        "command=sync status=ok mode=auto pulled={} pushed={}",
        stats.pulled, stats.pushed
    );
    Ok(true)
}

/// Implicit synchronization only notifies the resident runtime after local commit.
/// Direct writes leave durable work for an explicit sync or runtime recovery.
pub fn schedule_autosync(_session: &SessionKeys, _local: &LocalStore) {
    if rpc::worker_active() && respire_app::service::autosync_active() && remote_configured() {
        crate::rpc_serve::kick_autosync();
    }
}
pub fn auto_sync(session: &SessionKeys, local: &LocalStore) {
    schedule_autosync(session, local);
}


pub fn run_passport() -> Result<()> {
    if rpc::worker_active() {
        let store = respire_app::service::open_store()?;
        let si = respire_app::service::session_info();
        let user = match si.user.trim() {
            "" => "local".to_owned(),
            user => user.to_owned(),
        };
        let alive = store.count()?;
        let devices = store.devices_count()?;
        let items = vec![
            OutputItem::new("user", OutputStatus::Ok, user.clone()),
            OutputItem::new("memories", OutputStatus::Ok, alive.to_string()),
            OutputItem::new("agents", OutputStatus::Skip, "host_owned"),
            OutputItem::new("devices", OutputStatus::Ok, devices.to_string()),
        ];
        let mut result = ResultEnvelope::new(
            "passport",
            OutputStatus::Ok,
            serde_json::json!({
                "user": user,
                "memories": alive,
                "agents": null,
                "devices": devices,
            }),
            items,
        );
        result.details = serde_json::json!({
            "local_first": true,
            "encrypted_sync": true,
            "user_owned": true,
        });
        return emit_result(result);
    }
    let si = respire_app::service::session_info();
    let st = respire_app::service::status_light().ok();
    let alive = st.as_ref().map(|s| s.local_alive).unwrap_or(0);
    let devices = respire_app::service::open_store()
        .and_then(|s| s.devices_count())
        .unwrap_or(0);
    let user = {
        let u = si.user.trim();
        if u.is_empty() {
            "local".to_owned()
        } else {
            u.to_owned()
        }
    };
    let items = vec![
        OutputItem::new("user", OutputStatus::Ok, user.clone()),
        OutputItem::new("memories", OutputStatus::Ok, alive.to_string()),
        OutputItem::new("agents", OutputStatus::Skip, "host_owned"),
        OutputItem::new("devices", OutputStatus::Ok, devices.to_string()),
    ];
    let mut result = ResultEnvelope::new(
        "passport",
        OutputStatus::Ok,
        serde_json::json!({
            "user": user,
            "memories": alive,
            "agents": null,
            "devices": devices,
        }),
        items,
    );
    result.details = serde_json::json!({
        "local_first": true,
        "encrypted_sync": true,
        "user_owned": true,
    });
    emit_result(result)
}

/// App::open success -> StatusInfo (unlocked); not unlocked -> None.
pub fn open_app_status() -> Option<respire_app::service::StatusInfo> {
    let app = respire_app::service::App::open().ok()?;
    app.status().ok()
}

/// Subtree material: export this node's subtree as Markdown (for an external AI/App to install).
/// 2026-09-21: moved here from `scope --material` (local-subtree feature dropped; material export stays for install).
pub fn run_tree_material(prefix: &str) -> Result<()> {
    let session = build_session()?;
    let store = build_local()?;
    let all = store.all(false)?;
    let root_id = respire_app::service::resolve_prefix(&all, prefix)?;
    let members = respire_app::service::subtree_members(&all, &root_id);
    let mut entries: Vec<respire_app::MemoryEntry> = all
        .iter()
        .filter(|m| members.contains(&m.id))
        .filter_map(|s| MemoryEngine::open(&session, s).ok())
        .collect();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut md = String::new();
    md.push_str("# rsrs subtree material\n\n");
    md.push_str(&format!("Node: {root_id}\nCount: {}\n\n", entries.len()));
    for e in &entries {
        let title = if e.title.is_empty() {
            short_id(&e.id)
        } else {
            e.title.clone()
        };
        md.push_str(&format!("## {title}\n\n{}\n\n", e.content));
    }
    let mut result = ResultEnvelope::new(
        "tree-material",
        OutputStatus::Ok,
        serde_json::json!({"node": root_id, "count": entries.len()}),
        vec![OutputItem::new(
            "material",
            OutputStatus::Ok,
            format!("{} entries", entries.len()),
        )],
    );
    result.details = serde_json::json!({"material": md});
    emit_result(result)
}

/// Multiple accounts on one machine: list profiles / use switches / remove deletes.

pub fn run_export(file: &str) -> Result<()> {
    let n = respire_app::service::export_json(std::path::Path::new(file))?;
    emit_result(ResultEnvelope::new(
        "export",
        OutputStatus::Ok,
        serde_json::json!({"exported":n,"path":file}),
        vec![
            OutputItem::new("entries", OutputStatus::Ok, n.to_string()),
            OutputItem::new("path", OutputStatus::Ok, file),
        ],
    ))
}

pub fn run_backup(file: &str) -> Result<()> {
    let p = respire_app::service::backup_db(std::path::Path::new(file))?;
    emit_result(ResultEnvelope::new(
        "backup",
        OutputStatus::Ok,
        serde_json::json!({"backed_up":p.to_string_lossy()}),
        vec![OutputItem::new(
            "path",
            OutputStatus::Ok,
            p.display().to_string(),
        )],
    ))
}

/// Export decrypt keys: super password + Secret Key + vault material (how a new machine logs in).
/// Under v3, Account Secret is no longer a decrypt key - export the v3 three-piece; old v1 material is listed as a side note.

pub fn run_book_material(root: &str) -> Result<()> {
    let app = respire_app::service::App::open()?;
    let m = app.book_material(root)?;
    let mut result = ResultEnvelope::new(
        "book-material",
        OutputStatus::Ok,
        serde_json::json!({"title":m.root.title,"chapters":m.chapters.len(),"entries":m.total_entries,"chars":m.total_chars}),
        vec![
            OutputItem::new("volume", OutputStatus::Ok, m.root.title.clone()),
            OutputItem::new("chapters", OutputStatus::Ok, m.chapters.len().to_string()),
            OutputItem::new("entries", OutputStatus::Ok, m.total_entries.to_string()),
            OutputItem::new("chars", OutputStatus::Ok, m.total_chars.to_string()),
        ],
    );
    result.details = serde_json::to_value(&m)?;
    emit_result(result)
}

pub fn run_portrait_material(limit: usize) -> Result<()> {
    let app = respire_app::service::App::open()?;
    let m = app.portrait_material(limit)?;
    let count = m
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|v| v.len())
        .unwrap_or(0);
    let mut result = ResultEnvelope::new(
        "portrait-material",
        OutputStatus::Ok,
        serde_json::json!({"limit":limit,"count":count}),
        vec![
            OutputItem::new("entries", OutputStatus::Ok, count.to_string()),
            OutputItem::new("limit", OutputStatus::Ok, limit.to_string()),
        ],
    );
    result.details = m;
    emit_result(result)
}

/// Share a subtree: emit a prompt another AI can paste.
///
/// Missing `--root` uses this machine's scope root; if that is also unset, error and list candidate roots -
/// sharing a subtree is an explicit action; do not silently pick one (2026-09-21).
pub fn run_share(root: Option<&str>, raw: bool, out: Option<&str>) -> Result<()> {
    let root_id = match root.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(r) => r.to_owned(),
        None => {
            return Err(anyhow!(
                "no subtree specified: `rsrs share --root <id>` (run `rsrs tree --outline` first to see subtree ids)"
            ));
        }
    };
    let (payload, encoded) = respire_app::service::export_subtree_payload(&root_id)?;
    let prompt = respire_app::share::build_prompt(&payload, &encoded);

    if let Some(path) = out.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        std::fs::write(path, &prompt).map_err(|e| anyhow!("write failed {path}: {e}"))?;
        let mut result = ResultEnvelope::new(
            "share",
            OutputStatus::Ok,
            serde_json::json!({"path":path,"root":payload.root_title,"count":payload.items.len(),"chars":payload.total_chars(),"prompt_chars":prompt.chars().count()}),
            vec![OutputItem::new("file", OutputStatus::Ok, path)],
        );
        result.actions.push("protect plaintext payload".into());
        emit_result(result)?;
        return Ok(());
    }

    let status = if raw {
        OutputStatus::Warn
    } else {
        OutputStatus::Ok
    };
    let mut result = ResultEnvelope::new(
        "share",
        status,
        serde_json::json!({"root":payload.root_title,"count":payload.items.len(),"chars":payload.total_chars(),"prompt_chars":prompt.chars().count()}),
        vec![OutputItem::new("prompt", status, prompt)],
    );
    result.details = serde_json::json!({"payload":encoded});
    result.actions.push("protect plaintext payload".into());
    emit_result(result)
}

