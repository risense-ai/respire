pub fn run_local_inner(args: Cli) -> Result<()> {
    if matches!(args.command, Some(Command::Capabilities)) {
        return run_capabilities();
    }
    let write_generation = rpc::sync_generation();
    let short_write = matches!(args.command.as_ref(), Some(Command::Update { .. }))
        || matches!(args.command.as_ref(), Some(Command::Remember {
            merge_ids: None, supersedes: None, see_also, .. }) if see_also.is_empty());
    if rpc::worker_active() {
        respire_app::service::ensure_runtime_profile()?;
    }
    set_json_mode(
        args.json || respire_app::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"),
    );
    if matches!(args.command, Some(Command::V)) {
        return crate::app_version::emit(json_mode());
    }
    if matches!(args.command, Some(Command::MemoryRevision)) {
        return run_memory_revision();
    }
    // The runtime worker already holds lock.db for the process lifetime.
    progress::phase("\u{6253}\u{5f00}\u{672c}\u{5730}\u{5e93}\u{ff1b}\u{7b49}\u{5f85}\u{6570}\u{636e}\u{5e93}\u{9501}", "Opening local library; waiting for database lock");
    let _library_lock = if rpc::worker_active() {
        None
    } else {
        Some(respire_app::lock::LibraryLock::acquire(
            &respire_app::service::data_dir(),
            std::time::Duration::from_secs(120),
        )?)
    };
    // A direct caller can wait here while a restore installs maintenance. The
    // pre-dispatch check cannot authorize a mutation after that ownership wait.
    respire_app::snapshot::ensure_no_incomplete_restore(&respire_app::service::data_dir())?;
    progress::phase("\u{6267}\u{884c}\u{672c}\u{5730}\u{64cd}\u{4f5c}", "Executing local operation");
    crate::host_hooks::check_direct_access()?;
    // -- Read-only gate (added 2026-09-20; same-day audit moved it) --
    // When this space's agent.json has readonly=true, every write command is refused.
    //
    // **Must sit before the "no embedder needed" early-return match below** - it used to sit after
    // `let command = args.command.unwrap_or(...)` (~line 4882), while resort/
    // split/repack/tree-cure/defrag/root-create writes **returned early** from the match around 4688
    // and never hit the gate - read-only was a no-op (reproduced: those commands still reached unlock).
    // Raised here so every write goes through.
    if let Some(cmd) = args.command.as_ref() {
        // -- Temporarily-off gate (added 2026-09-21; one of the personal-space three states) --
        // memory_off=true refuses both read and write, leaving only admin/identity/inject infrastructure -
        // the owner restores with agent-config --set memory_off=false (unlike team read-only, which is
        // server-enforced: off is a local switch, no need to block self-unlock).
        if matches!(
            cmd,
            Command::Sync | Command::SyncResolve { .. } | Command::SyncReset
        ) {
            respire_app::snapshot::ensure_sync_allowed(&respire_app::service::data_dir())?;
        }
        if respire_app::service::off_mode() {
            if !is_off_allowed(cmd) {
                anyhow::bail!(
                    "the memory store is temporarily off - this turn provides no memory service (recall and store both stop).\n\
                     \x20  restore: `rsrs agent-config --set workspace_mode=normal` then `rsrs inject --all` to redistribute the prompt."
                );
            }
        } else {
            let device_sync_write = respire_app::service::service_paused()
                && matches!(cmd, Command::Sync | Command::SyncConflicts { .. } | Command::SyncHistory { remote: true, .. });
            if is_write_command(cmd) || is_write_command_fine(cmd) || device_sync_write {
                respire_app::service::ensure_writable()?;
            }
            // H2 anti-self-unlock: in read-only, do not allow readonly back to false - **team read-only only**
            // (readonly_team=true, written by space join --readonly). Personal read-only
            // (GUI/agent-config) is the owner's own switch and must be liftable.
            // Even if a team member bypasses the local gate, the server still refuses writes by session token (the real line).
            if respire_app::service::readonly_mode()
                && respire_app::service::readonly_team()
                && is_readonly_switch_off(cmd)
            {
                anyhow::bail!(
                    "this space is read-only - you cannot lift read-only yourself.\n\
                     \x20  the space owner must lift it (the server enforces by session token; editing local agent.json does nothing);\n\
                     \x20  if you are the owner and need to write, re-issue a read-write session on the server then `rsrs space join`."
                );
            }
        }
    }
    // keygen/defrag/scope/config etc. do not need the embedder - handle early
    dispatch_light!(args);
    let command = args.command.unwrap_or(Command::Status);
    // Lazy-load the embedder: Show/Sync/Status/tree ops do not pay BGE cost.
    // The runtime worker puts the loaded model back so the next request does not pay it again.
    let mut embedder_hold = EmbedderHold(
        EMBEDDER_SLOT
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take(),
    );
    macro_rules! embedder {
        () => {{
            let model = build_local()?.retrieval_model()?;
            let generation = respire_app::core_sdk::generation_key(&model)?;
            if embedder_hold
                .0
                .as_ref()
                .is_some_and(|e| e.model_name() != model || e.generation_key() != generation)
            {
                embedder_hold.0 = None;
            }
            if embedder_hold.0.is_none() {
                embedder_hold.0 = Some(Box::new(BgeEmbedder::load_model(&model)?));
            }
            match embedder_hold.0.as_deref() {
                Some(e) => e,
                None => return Err(anyhow::anyhow!("embedder not loaded")),
            }
        }};
    }
    match command {
        Command::Remember {
            content,
            r#type,
            metadata,
            tags,
            title,
            project,
            computer,
            emotion,
            importance,
            parent,
            force,
            merge_ids,
            supersedes,
            see_also,
        } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_remember.rs")),
        Command::Import { file } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_import.rs")),
        Command::ShareImport {
            file,
            parent,
            go,
            title,
            force,
        } => {
            return run_share_import(&file, &parent, go, title.as_deref(), force);
        }
        Command::Recall {
            query,
            mode,
            limit,
            r#type,
            project,
            trace,
            titles,
            brief,
            render_budget_base64,
            render_min_score,
            no_related,
            intent,
        } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_recall.rs")),
        Command::Attach { id, parent } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_attach.rs")),
        Command::Chain { id, depth } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_chain.rs")),
        Command::Show { id } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_show.rs")),
        Command::List {
            limit,
            since,
            since_resort,
            metadata,
        } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_list.rs")),
        Command::Forget { id } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_forget.rs")),
        Command::Purge { id } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_purge.rs")),
        Command::Restore { id } => {
            if !build_local()?.all(true)?.iter().any(|entry| entry.id == id) {
                return Err(respire_app::input_error::InputError(format!("not found #{id}")).into());
            }
            let app = respire_app::service::App::open()?;
            let entry = app.restore(&id)?;
            emit_result(ResultEnvelope::new(
                "restore",
                OutputStatus::Ok,
                serde_json::json!({"id":entry.id,"restored":true}),
                vec![OutputItem::new(
                    "entry",
                    OutputStatus::Ok,
                    short_id(&entry.id),
                )],
            ))?;
        }
        Command::Promote { id } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_promote.rs")),
        Command::Demote { id, parent } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_demote.rs")),
        Command::Tree { from, depth, .. } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_tree.rs")),
        Command::Retitle { id, title } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_retitle.rs")),
        Command::RetitleMany { file } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_retitle_many.rs")),
        Command::SyncConflicts { id, all, refresh } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_sync_conflicts.rs")),
        Command::SyncResolve {
            epoch,
            rev,
            head_rev,
            action,
            content,
        } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_sync_resolve.rs")),
        Command::SyncHistory { id, remote } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_sync_history.rs")),
        Command::SyncRestore { op_id, rev, epoch } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_sync_restore.rs")),
        Command::SyncReset => {
            build_local()?.reset_sync_snapshot()?;
            let mut result = ResultEnvelope::new(
                "sync-reset",
                OutputStatus::Ok,
                serde_json::json!({"history_kept":false,"current_snapshots_kept":true,"pending_outbound_kept":true}),
                vec![
                    OutputItem::new("snapshot", OutputStatus::Ok, "reset"),
                    OutputItem::new("history", OutputStatus::Ok, "kept"),
                    OutputItem::new("pending outbound", OutputStatus::Ok, "kept"),
                ],
            );
            result.actions.push("sync".into());
            emit_result(result)?;
        }
        Command::Sync => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_sync.rs")),
        Command::Status => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_status.rs")),
        Command::Reembed => {
            let _operation = crate::model_progress::Operation::try_begin_background_index()?
                .ok_or_else(|| anyhow::anyhow!("another model operation is running"))?;
            let session = build_session()?;
            let store = build_local()?;
            let model = store.retrieval_model()?;
            let n = store.rebuild_index_with_progress(
                &session, embedder!(), &model,
                |done, total| crate::model_progress::update(
                    "index", &model, done as u64, Some(total as u64),
                ),
            )?;
            let dims = embedder!().dims();
            emit_result(ResultEnvelope::new(
                "reembed",
                OutputStatus::Ok,
                serde_json::json!({"reembedded":n,"dims":dims}),
                vec![
                    OutputItem::new("reembedded", OutputStatus::Ok, n.to_string()),
                    OutputItem::new("dimensions", OutputStatus::Ok, dims.to_string()),
                ],
            ))?;
        }
        Command::Candidates { content } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_candidates.rs")),
        Command::V => unreachable!("version is printed before run_local work"),
        Command::Passport => {
            return run_passport();
        }
        Command::History { id, limit } => include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/dispatch_history.rs")),
        Command::Update {
            id,
            metadata,
            title,
            content,
            tags,
            kind,
            importance,
        } => {
            if metadata.patch().is_some() || kind.as_deref().is_some_and(|kind| matches!(kind, "hypothesis" | "reflection" | "self")) {
                respire_app::metadata_gate::require_support()?;
            }
            if let Some(imp) = importance.as_deref() {
                // Two-tier: normal is retired; update may not set normal either
                if !matches!(imp, "important" | "trivial") {
                    anyhow::bail!(
                        "importance must be important/trivial (normal is retired), got \"{}\"",
                        imp
                    );
                }
            }
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(true)?;
            let full = respire_app::service::resolve_prefix(&all, &id)?;
            let stored = all
                .iter()
                .find(|m| m.id == full && !m.deleted)
                .ok_or_else(|| anyhow!("not found #{id}"))?;
            let mut entry = MemoryEngine::open(&session, stored)?;
            let original_entry = entry.clone();
            if let Some(t) = title {
                entry.title = t;
            }
            if let Some(c) = content {
                entry.content = c;
            }
            if let Some(tg) = tags {
                entry.tags = tg
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
            if let Some(k) = kind {
                entry.kind = Kind::try_from_str(&k.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?;
            }
            if let Some(imp) = importance {
                let imp = imp.trim().to_lowercase();
                if !matches!(imp.as_str(), "important" | "normal" | "trivial") {
                    anyhow::bail!(
                        "invalid importance: {imp} (choose important / trivial; normal is retired)"
                    );
                }
                entry.importance = imp;
            }
            metadata.apply(&mut entry);
            let metadata_receipt = metadata::prepare(&session, &store, &entry)?;
            let skill_missing = if entry.kind == Kind::Skill && entry.importance == "important" {
                let missing = skill::missing_sections(&entry.content);
                if !missing.is_empty() {
                    eprintln!(
                        "WARN skill template missing {} - add {} (see `rsrs audit`)",
                        missing.join("/"),
                        skill::SECTIONS.join("/")
                    );
                }
                missing
            } else {
                Vec::new()
            };
            entry.updated_at = store.edit_stamp(&stored.id)?;
            entry.modified_by = respire_app::service::device_tag();
            let user = entry.user.clone();
            let mut new_stored = MemoryEngine::seal(&session, embedder!(), &entry, &user)?;
            if respire_app::core_sdk::indexing_deferred() && !stored.local_artifact.is_empty() {
                let reusable: bool = respire_app::core_sdk::execute("index_reusable",
                    serde_json::json!({"before":original_entry,"after":entry}))?;
                if reusable { new_stored.local_artifact = stored.local_artifact.clone(); }
            }
            anyhow::ensure!(commit_prepared_write(short_write, write_generation, || store.put_checked(&new_stored, Some(&stored.ciphertext)))?, "memory was not saved");
            // Adoption: an update is adoption - if a query in the last 10 minutes hit this entry, record it in query_log
            let _ = store.mark_adopted(&stored.id);
            let hint = note_write_maintenance();
            let mut result = ResultEnvelope::new(
                "update",
                if skill_missing.is_empty() {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Warn
                },
                serde_json::json!({"action":"written", "id": stored.id, "maintenance_hint": hint,
                    "skill_template_missing": skill_missing,
                    "index_state": if new_stored.local_artifact.is_empty() { "pending" } else { "ready" }}),
                vec![OutputItem::new(
                    "entry",
                    OutputStatus::Ok,
                    format!(
                        "{} {}",
                        stored.id.get(..8).unwrap_or(&stored.id),
                        entry.title
                    ),
                )],
            );
            if !skill_missing.is_empty() {
                result.actions.push(format!(
                    "skill template missing {}; add {}",
                    skill_missing.join("/"),
                    skill::SECTIONS.join("/")
                ));
            }
            metadata::add_receipt(&mut result, metadata_receipt);
            result.details = serde_json::to_value(entry)?;
            emit_result(result)?;
            schedule_autosync(&session, &store);
        }
        // The commands above already returned at the top of this function (no embedder needed); this arm is unreachable - only to exhaust the match.
        Command::MemoryRevision
        | Command::Metadata { .. }
        | Command::Review { .. }
        | Command::Capabilities
        | Command::Keygen { .. }
        | Command::Session { .. }
        | Command::Grant { .. } => {
            unreachable!("already returned at the top of main")
        }
        Command::Defrag { .. }
        | Command::TreeCure { .. }
        | Command::TreeFloat { .. }
        | Command::Split { .. }
        | Command::Extract { .. }
        | Command::TreeDeepen { .. }
        | Command::Config { .. }
        | Command::Export { .. }
        | Command::Backup { .. }
        | Command::Snapshot { .. }
        | Command::Secret { .. }
        | Command::Fivekeys { .. }
        | Command::Register { .. }
        | Command::Login { .. }
        | Command::KeysExport { .. }
        | Command::SuperReset { .. }
        | Command::BookMaterial { .. }
        | Command::PortraitMaterial { .. }
        | Command::Doctor { .. }
        | Command::Audit { .. }
        | Command::Witness { .. }
        | Command::Model { .. }
        | Command::Share { .. }
        | Command::Diary { .. }
        | Command::DiarySearch { .. }
        | Command::DiaryFragment { .. }
        | Command::Taxonomy { .. }
        | Command::RootCreate { .. }
        | Command::Repack
        | Command::Logout { .. }
        | Command::Account { .. }
        | Command::Migrate { .. }
        | Command::Space { .. }
        | Command::Resort { .. }
        | Command::AgentConfig { .. }
        | Command::Prompt
        | Command::QueryLog { .. }
        | Command::Bench { .. }
        | Command::RunLog { .. }
        | Command::Classify { .. }
        | Command::ClassifyConfig { .. } => unreachable!(
            "commands that do not need the embedder already returned at the top of main"
        ),
    }
    Ok(())
}

