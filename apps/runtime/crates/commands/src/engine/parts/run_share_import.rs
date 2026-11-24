/// Import a shared subtree: without `--go`, print attach-candidate bill (for the AI to judge); `--go` writes.
/// Refuse Core-reported conflicts before writing; `--force` is the explicit override.
pub fn run_share_import(
    file: &str,
    parent: &str,
    go: bool,
    title: Option<&str>,
    force: bool,
) -> Result<()> {
    let mut payload = respire_app::service::read_share_file(std::path::Path::new(file))?;
    if let Some(t) = title.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        payload.root_title = t.to_owned();
    }

    if !go {
        let embedder = BgeEmbedder::load_model(&build_local()?.retrieval_model()?)?;
        let bill = respire_app::service::share_candidates_with(&payload, &embedder)?;
        let cands = bill["candidates"].as_array().cloned().unwrap_or_default();
        let items = cands
            .iter()
            .take(8)
            .map(|c| {
                OutputItem::new(
                    c["short_id"].as_str().unwrap_or(""),
                    OutputStatus::Pending,
                    format!(
                        "score={} title={}",
                        c["score"].as_f64().unwrap_or(0.0),
                        c["title"].as_str().unwrap_or("")
                    ),
                )
            })
            .collect();
        let mut result = ResultEnvelope::new(
            "share-import",
            OutputStatus::Pending,
            serde_json::json!({"need_go":true,"root":payload.root_title,"entries":payload.items.len(),"chars":payload.total_chars(),"candidates":cands.len()}),
            items,
        );
        result.details = bill;
        result.actions.push(format!("share-import {file} --go"));
        emit_result(result)?;
        return Ok(());
    }

    let embedder = BgeEmbedder::load_model(&build_local()?.retrieval_model()?)?;
    let report = respire_app::service::import_share_payload(&payload, parent, force, &embedder)?;
    let status = if report.orphaned > 0 || report.skipped > 0 {
        OutputStatus::Warn
    } else {
        OutputStatus::Ok
    };
    let items = vec![
        OutputItem::new("imported", status, report.imported.to_string()),
        OutputItem::new(
            "skipped",
            if report.skipped > 0 {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            },
            report.skipped.to_string(),
        ),
        OutputItem::new(
            "reattached",
            OutputStatus::Ok,
            report.reattached.to_string(),
        ),
        OutputItem::new(
            "orphaned",
            if report.orphaned > 0 {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            },
            report.orphaned.to_string(),
        ),
    ];
    emit_result(ResultEnvelope::new(
        "share-import",
        status,
        serde_json::json!({"root_title":payload.root_title,"imported":report.imported,"skipped":report.skipped,"reattached":report.reattached,"orphaned":report.orphaned,"parent":parent}),
        items,
    ))
}

/// Whether this command writes the store (must refuse in read-only mode).
///

pub fn run_memory_revision() -> Result<()> {
    crate::host_hooks::check_direct_access()?;
    emit_result(ResultEnvelope::new(
        "memory-revision",
        OutputStatus::Ok,
        serde_json::to_value(respire_app::service::memory_revision()?)?,
        Vec::new(),
    ))
}

pub fn run_capabilities() -> Result<()> {
    emit_result(ResultEnvelope::new("capabilities", OutputStatus::Ok,
        serde_json::json!({
            "schema_version": 1,
            "cli_version": crate::app_version::embedded(),
            "capabilities": {
                "memory_list_metadata": true,
                "memory_metadata_flags": true,
                "memory_metadata_gate_default": false,
                "rerank": false,
                "model": {"install": false, "install_rerank": false, "profile_contract":1, "transport_contract":1, "methods":["model.catalog","model.configure","model.switch","model.resume","model.progress"], "selection_scope":"library"}
            }
        }), Vec::new()))
}

pub fn run_metadata_status() -> Result<()> {
    emit_result(metadata_status_result()?)
}

/// Offline read: no session unlock, store creation, or resident startup.
pub fn metadata_status_result() -> Result<ResultEnvelope> {
    let root = respire_app::service::data_dir();
    let path = respire_app::service::database_path(&root)?;
    let details = if path.try_exists()? {
        let store = respire_app::transport::local::LocalStore::open_read_only(&path)?;
        let backup = store.meta_get("memory_metadata_v1_backup")?
            .map(|value| serde_json::from_str::<serde_json::Value>(&value)).transpose()?;
        serde_json::json!({"enabled":store.memory_metadata_v1_enabled()?,
            "mode":respire_app::metadata_gate::mode(&store)?,
            "metadata_retained":store.has_memory_metadata()?,"backup":backup})
    } else {
        serde_json::json!({"enabled":false,"mode":"isolated","metadata_retained":false,"backup":null})
    };
    let capabilities: serde_json::Value = respire_app::core_sdk::execute("capabilities", serde_json::Value::Null)?;
    let mut result = ResultEnvelope::new("metadata", OutputStatus::Ok,
        serde_json::json!({"action":"status","enabled":details["enabled"],"mode":details["mode"],
            "sdk_supported":capabilities["memory_metadata_contract"].as_u64()==Some(1)}), Vec::new());
    result.details = details;
    Ok(result)
}

pub fn change_metadata_profile(change: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    if DIRECT_MODE.load(Ordering::Relaxed) {
        let _takeover = runtime_policy::takeover_lock()?;
        anyhow::ensure!(!crate::host_hooks::runtime_is_up(), "stop the local runtime before direct metadata configuration");
        change()
    } else {
        crate::host_hooks::change_profile(Box::new(change))
    }
}

pub fn run_metadata_host(command: &metadata::Command) -> Result<()> {
    runtime_policy::require_host("metadata deployment configuration")?;
    anyhow::ensure!(!rpc::worker_active(), "metadata enable/disable must run in the host CLI");
    respire_app::service::ensure_writable()?;
    anyhow::ensure!(!respire_app::service::off_mode(), "restore normal service before changing the metadata gate");
    let root = respire_app::service::data_dir();
    // The lifecycle hook requires a Send closure, so shared state crosses it
    // by owned handles instead of borrows.
    let details = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
    match command {
        metadata::Command::Enable { mode, deployment_review } => {
            let (deployment, review) = metadata::deployment(mode, deployment_review.as_deref())?;
            respire_app::metadata_gate::require_support()?;
            let keys = build_session()?;
            let account = respire_app::service::session_user_of_dir(&root);
            let root = root.clone();
            let details = details.clone();
            change_metadata_profile(move || {
                let ownership = respire_app::lock::LibraryLock::acquire(&root, std::time::Duration::from_secs(15))?;
                let receipt = respire_app::metadata_gate::enable(&root, &ownership, &account, &keys, deployment)?;
                *details.lock().map_err(|_| anyhow::anyhow!("metadata details lock poisoned"))? =
                    serde_json::json!({"receipt":receipt,"deployment_review":review,
                    "deployment_assurance":"operator_attestation_only","old_client_access_enforced":false});
                Ok(())
            })?;
        }
        metadata::Command::Disable => {
            let root = root.clone();
            let details = details.clone();
            change_metadata_profile(move || {
                let ownership = respire_app::lock::LibraryLock::acquire(&root, std::time::Duration::from_secs(15))?;
                *details.lock().map_err(|_| anyhow::anyhow!("metadata details lock poisoned"))? =
                    respire_app::metadata_gate::disable(&root, &ownership)?;
                Ok(())
            })?;
        }
        metadata::Command::Status => return run_metadata_status(),
    }
    let mut result = ResultEnvelope::new("metadata", OutputStatus::Ok,
        serde_json::json!({"action":if matches!(command,metadata::Command::Enable{..}){"enabled"}else{"disabled"},
            "enabled":matches!(command,metadata::Command::Enable{..})}), Vec::new());
    result.details = details.lock().map_err(|_| anyhow::anyhow!("metadata details lock poisoned"))?.clone();
    emit_result(result)
}

pub fn run_local(args: Cli) -> Result<()> {
    respire_app::service::validate_client_config()?;
    set_json_mode(args.json || respire_app::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"));
    if rpc::worker_active() && !crate::rpc_serve::store_ready() && matches!(args.command, Some(Command::Status)) {
        let mut result = ResultEnvelope::new("status", OutputStatus::Warn, serde_json::json!({
            "workspace":respire_app::service::workspace_mode(), "unlocked":false,
            "credential_state":"required", "store_ready":false,
            "session":respire_app::service::session_info(), "data_dir":respire_app::service::data_dir(),
            "autosync":false, "sync_scheduler":{"phase":"paused"},
            "retrieval_index":rpc::index_status(), "local_total":null, "local_alive":null
        }), Vec::new());
        result.actions.push("Supply private key material from the CLI or Client2".into());
        return emit_result(result);
    }
    if respire_app::service::off_mode() && matches!(args.command, Some(Command::Status)) {
        let session = respire_app::service::session_info();
        return emit_result(ResultEnvelope::new("status", OutputStatus::Ok, serde_json::json!({
            "workspace":"off", "unlocked":session.has_local_keys, "session":session,
            "data_dir":respire_app::service::data_dir(), "autosync":false,
            "sync_scheduler":{"phase":"paused"}, "retrieval_index":rpc::index_status(),
            "local_total":null, "local_alive":null
        }), Vec::new()));
    }
    if respire_app::service::off_mode() && args.command.as_ref().is_some_and(|command| !is_off_allowed(command)) {
        if !json_mode() && !rpc::worker_active() { return Ok(()); }
        let command = match args.command.as_ref() { Some(Command::Recall { .. }) => "recall", Some(Command::Remember { .. }) => "remember", _ => "memory" };
        let mut result = ResultEnvelope::new(command, OutputStatus::Ok, serde_json::json!({"mode":"off", "skipped":true, "count":0}), Vec::new());
        result.details = serde_json::json!([]);
        return emit_result(result);
    }
    let command = match args.command.as_ref() {
        Some(Command::Review { .. }) => "review",
        Some(Command::Recall { .. }) => "recall",
        Some(Command::Remember { .. }) => "remember",
        Some(Command::Candidates { .. }) => "candidates",
        Some(Command::Bench { .. }) => "bench",
        Some(Command::TreeDeepen { .. }) => "tree-deepen",
        Some(Command::TreeCure { .. }) => "tree-cure",
        Some(Command::TreeFloat { .. }) => "tree-float",
        Some(Command::Defrag { .. }) => "defrag",
        Some(Command::ShareImport { .. }) => "share-import",
        _ => "cli",
    };
    match run_local_inner(args) {
        Err(error) if error.downcast_ref::<respire_app::service::IndexPending>().is_some() => {
            // run_local_inner has released its direct LibraryLock before this
            // authenticated request starts the resident index worker.
            let indexing = rpc::request_index()?;
            let mut result = ResultEnvelope::new(
                command, OutputStatus::Pending,
                serde_json::json!({"state":"index_pending", "retrieval_index":indexing}),
                vec![OutputItem::new("index", OutputStatus::Pending, error.to_string())],
            );
            result.actions.push("status".into());
            emit_result(result)
        }
        result => result,
    }
}

pub fn commit_prepared_write<T>(simple: bool, generation: usize, action: impl FnOnce() -> Result<T>) -> Result<T> {
    if simple && rpc::worker_active() { rpc::foreground_phase(generation, action) }
    else { action() }
}

