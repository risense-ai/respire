{
    match args.command.as_ref() {
        Some(Command::Metadata { command: metadata::Command::Status }) => return run_metadata_status(),
        Some(Command::Metadata { .. }) => anyhow::bail!("metadata enable/disable must run in the host CLI"),
        Some(Command::Review { args }) => return review::run(args),
        Some(Command::Grant { command }) => {
            let store = respire_app::service::open_store()?;
            let (action, result) = match command {
                GrantCommand::Create { root, label } => {
                    uuid::Uuid::parse_str(root).map_err(|error| respire_app::input_error::InputError(format!("grant root must be a full UUID: {error}")))?;
                    if label.trim().is_empty() || label.len() > 128 {
                        return Err(respire_app::input_error::InputError("grant label must be 1\u{2013}128 bytes of non-empty text".into()).into());
                    }
                    if !store.all(false)?.iter().any(|entry| entry.id == *root) {
                        return Err(respire_app::input_error::InputError("grant root does not exist or is deleted".into()).into());
                    }
                    let (grant, token) = store.create_grant(root, label)?;
                    (
                        "create",
                        serde_json::json!({"grant": grant, "token": token}),
                    )
                }
                GrantCommand::List => ("list", serde_json::json!({"grants": store.list_grants()?})),
                GrantCommand::Revoke { id } => (
                    "revoke",
                    serde_json::json!({"revoked": store.revoke_grant(id)?}),
                ),
            };
            let items = if action == "list" {
                result["grants"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|g| {
                        OutputItem::new(
                            g["id"].as_str().unwrap_or("grant"),
                            OutputStatus::Ok,
                            serde_json::to_string(g).unwrap_or_default(),
                        )
                    })
                    .collect()
            } else {
                vec![OutputItem::new(
                    "result",
                    OutputStatus::Ok,
                    serde_json::to_string(&result).unwrap_or_default(),
                )]
            };
            let mut envelope = ResultEnvelope::new(
                "grant",
                OutputStatus::Ok,
                serde_json::json!({"action":action}),
                items,
            );
            envelope.details = result;
            return emit_result(envelope);
        }
        Some(Command::Session { command }) => {
            let revoke = match command {
                SessionCommand::List => None,
                SessionCommand::Revoke { id } => Some(id.as_str()),
            };
            let action = if revoke.is_some() { "revoke" } else { "list" };
            let details = respire_app::auth::sessions(revoke)?;
            let rows = details.as_array().cloned().unwrap_or_default();
            let items = rows
                .iter()
                .map(|s| {
                    OutputItem::new(
                        s["id"]
                            .as_str()
                            .or_else(|| s["session_id"].as_str())
                            .unwrap_or("session"),
                        OutputStatus::Ok,
                        serde_json::to_string(s).unwrap_or_default(),
                    )
                })
                .collect();
            let mut envelope = ResultEnvelope::new(
                "session",
                OutputStatus::Ok,
                serde_json::json!({"action":action,"count":rows.len()}),
                items,
            );
            envelope.details = details;
            return emit_result(envelope);
        }
        Some(Command::Defrag { min, top }) => return run_defrag(*min, *top),
        Some(Command::TreeCure {
            top,
            id,
            parent,
            auto,
            min,
        }) => return run_tree_cure(*top, id.as_deref(), parent.as_deref(), *auto, *min),
        Some(Command::TreeFloat { go, min }) => return run_tree_float(*go, *min),
        Some(Command::Split { id, go, spec }) => return run_split(id, *go, spec.as_deref()),
        Some(command @ Command::Extract { .. }) => return run_extract_command(command),
        Some(Command::DiarySearch { query, limit, max_chars, mode }) => {
            let store = build_local()?;
            let keys = build_session()?;
            let result = respire_app::diary::search(&store, &keys, query, *limit, *max_chars, mode == "hybrid")?;
            if result.pending_fragments > 0 { respire_app::service::notify_index(); }
            return emit_diary_fragments("diary-search", result);
        }
        Some(Command::DiaryFragment { source_id, fragment_id, neighbors, max_chars }) => {
            let result = respire_app::diary::read_fragment(&build_local()?, &build_session()?, source_id, fragment_id, *neighbors, *max_chars)?;
            return emit_diary_fragments("diary-fragment", result);
        }
        Some(Command::Diary {
            limit,
            date,
            from,
            to,
            contains,
        }) => {
            return run_diary(
                *limit,
                date.as_deref(),
                from.as_deref(),
                to.as_deref(),
                contains.as_deref(),
            )
        }
        Some(Command::Repack) => return run_repack(),
        Some(Command::Tree {
            material: Some(root),
            ..
        }) => return run_tree_material(root),
        Some(Command::Tree {
            from,
            outline: true,
            ..
        }) => {
            let app = respire_app::service::App::open()?;
            let nodes = app.tree(from, usize::MAX)?;
            // --json contract: every command must emit JSON (this used to hard-take the text path,
            // the only CLI command that broke the contract - found in the 2026-09-20 audit).
            if json_mode() {
                let mut rows: Vec<serde_json::Value> = Vec::new();
                collect_outline_json(&nodes, 0, &mut rows);
                let mut result = ResultEnvelope::new(
                    "tree",
                    OutputStatus::Ok,
                    serde_json::json!({"count":rows.len(),"outline":true}),
                    rows.iter()
                        .map(|r| {
                            OutputItem::new(
                                r["short_id"].as_str().unwrap_or(""),
                                OutputStatus::Ok,
                                r["title"].as_str().unwrap_or(""),
                            )
                        })
                        .collect(),
                );
                result.details = serde_json::Value::Array(rows);
                emit_result(result)?;
                return Ok(());
            }
            let mut lines: Vec<String> = Vec::new();
            collect_outline(&nodes, 0, &mut lines);
            emit_result(ResultEnvelope::new(
                "tree",
                OutputStatus::Ok,
                serde_json::json!({"count":lines.len(),"outline":true}),
                lines
                    .iter()
                    .map(|line| OutputItem::new("node", OutputStatus::Ok, line))
                    .collect(),
            ))?;
            return Ok(());
        }
        Some(Command::Resort {
            go,
            spec,
            status,
            reset,
            threshold,
        }) => return run_resort(*go, spec.as_deref(), *status, *reset, *threshold),
        Some(Command::Prompt) => {
            let text = respire_app::inject::INSTRUCTIONS_MD;
            return emit_result(ResultEnvelope::new(
                "prompt",
                OutputStatus::Ok,
                serde_json::json!({"instructions":text}),
                vec![OutputItem::new("instructions", OutputStatus::Ok, text)],
            ));
        }
        Some(Command::AgentConfig { set }) => return run_agent_config(set.as_deref()),
        Some(Command::Tree { from, depth, .. }) if json_mode() => {
            let app = respire_app::service::App::open()?;
            let nodes = app.tree(from, *depth)?;
            let mut result = ResultEnvelope::new(
                "tree",
                OutputStatus::Ok,
                serde_json::json!({"count":nodes.len(),"depth":depth}),
                Vec::new(),
            );
            result.details = serde_json::to_value(&nodes)?;
            emit_result(result)?;
            return Ok(());
        }
        Some(Command::TreeDeepen {
            root,
            go,
            titles,
            auto,
            min,
        }) => return run_tree_deepen(root.as_deref(), *go, titles.as_deref(), *auto, *min),
        // Share: decrypt only, no retrieval (no BGE load), so it sits in the no-model section
        Some(Command::Share { root, raw, out }) => {
            return run_share(root.as_deref(), *raw, out.as_deref())
        }
        Some(Command::Doctor { remote }) => return run_runtime_doctor(*remote),
        Some(Command::Audit { json }) => return run_audit(*json),
        Some(Command::Witness { verify: _ }) => {
            let report=build_local()?.witness_verify()?;
            let mut result=ResultEnvelope::new("witness",if report.intact{OutputStatus::Ok}else{OutputStatus::Fail},serde_json::to_value(&report)?,Vec::new());
            if !report.intact {result.errors.push(format!("witness verification failed at sequence {}: {}",report.broken_at.map_or_else(||"unknown".to_owned(),|seq|seq.to_string()),report.reason.as_deref().unwrap_or("invalid chain")));}
            result.details=serde_json::to_value(report)?;
            return emit_result(result);
        },
        Some(Command::SuperReset { .. }) => {
            anyhow::bail!("super-reset requires private host key control");
        }
        Some(Command::QueryLog {
            limit,
            stats,
            json,
            cmd,
        }) => return run_query_log(*limit, *stats, *json, cmd.as_ref()),
        Some(Command::Bench { cmd }) => return run_bench(cmd),
        Some(Command::RunLog { id, status, note }) => {
            return run_run_log(id, status, note);
        }
        Some(Command::Classify {
            limit,
            all,
            root,
            max_chars,
            min_confidence,
            save,
            dry_run,
            ds,
            backend,
            samples,
            tree,
            batch,
            tree_depth,
            causal,
            min_kids,
            segments,
            out,
            auto,
            plan,
            rounds,
            api_base,
            model,
        }) => {
            return run_classify(
                *limit,
                *all,
                root.as_deref(),
                *max_chars,
                *min_confidence,
                save.as_deref(),
                api_base.as_deref(),
                model,
                *dry_run,
                ds.as_deref(),
                backend.as_deref(),
                *samples,
                *tree,
                *batch,
                *tree_depth,
                *causal,
                *min_kids,
                *segments,
                out.as_deref(),
                *auto,
                *plan,
                *rounds,
            )
        }
        Some(Command::Model { action }) => {
            return match action {
                ModelAction::Catalog | ModelAction::Configure { .. } | ModelAction::Switch { .. } | ModelAction::Progress | ModelAction::Resume => anyhow::bail!("model profile command must use the runtime interface"),
                ModelAction::Activate { model } => {
                    use crate::model_progress;
                    let _operation = model_progress::Operation::try_begin_background_index()?
                        .ok_or_else(|| anyhow::anyhow!("another model operation is running"))?;
                    let store = build_local()?;
                    anyhow::ensure!(store.retrieval_model()? == *model,"legacy model activate cannot change a profile selection; use model switch m3");
                    let keys = build_session()?;
                    let embedder = BgeEmbedder::load_model(model)?;
                    let count = store.rebuild_index_with_progress(
                        &keys,
                        &embedder,
                        model,
                        |done, total| {
                            model_progress::update("index", model, done as u64, Some(total as u64))
                        },
                    )?;
                    *EMBEDDER_SLOT
                        .lock()
                        .map_err(|_| anyhow!("embedder cache poisoned"))? = None;
                    emit_result(ResultEnvelope::new(
                        "model activate",
                        OutputStatus::Ok,
                        serde_json::json!({"model":model,"indexed":count,"dimensions":embedder.dims()}),
                        vec![OutputItem::new(
                            "active_model",
                            OutputStatus::Ok,
                            model.clone(),
                        )],
                    ))?;
                    Ok(())
                }
                ModelAction::Engine { engine, model, expected_revision } => {
                    use respire_app::memory::onnx;
                    let selected = respire_app::embedding::saved_selection()?;
                    respire_app::core_sdk::profiles::set_active(selected.clone());
                    anyhow::ensure!(selected.is_none() || (engine.is_none() && model.is_none()),"library uses a model profile; use model switch instead of legacy engine settings");
                    if let Some(engine) = engine {
                        let revision = onnx::save_engine_with_variant_revision(
                            onnx::Engine::parse(engine)?,
                            model.as_deref(),
                            expected_revision.as_deref(),
                        )?;
                        *EMBEDDER_SLOT
                            .lock()
                            .map_err(|_| anyhow!("embedder cache poisoned"))? = None;
                        let summary = serde_json::json!({"engine":engine,
                            "model":onnx::configured_variant(onnx::Engine::parse(engine)?)?.name,
                            "revision":revision,"variants":crate::model_assets::variant_inventory()?,
                            "active_selection":selected});
                        emit_result(ResultEnvelope::new("model",OutputStatus::Ok,summary,vec![]))?;
                        return Ok(());
                    } else if let Some(model) = model.as_deref() {
                        onnx::save_model_variant(model)?;
                        *EMBEDDER_SLOT.lock().map_err(|_| anyhow!("embedder cache poisoned"))? = None;
                    }
                    let engine = onnx::configured_engine()?;
                    emit_result(ResultEnvelope::new(
                        "model",
                        OutputStatus::Ok,
                        serde_json::json!({"engine":engine,
                            "model":onnx::configured_variant(engine)?.name,
                            "variants":crate::model_assets::variant_inventory()?, "active_selection":selected}),
                        vec![],
                    ))?;
                    Ok(())
                }
                ModelAction::Load { engine, model } => {
                    let selected = respire_app::embedding::saved_selection()?;
                    respire_app::core_sdk::profiles::set_active(selected.clone());
                    anyhow::ensure!(selected.is_none(),"library uses a model profile; use model switch instead of legacy model load");
                    let _operation = crate::model_progress::Operation::begin_load()?;
                    let engine = respire_app::memory::onnx::Engine::parse(engine)?;
                    crate::model_progress::update("verify", model, 0, None)?;
                    respire_app::memory::onnx::validate_selection(engine, model)?;
                    crate::model_progress::update("load", model, 0, None)?;
                    let result = respire_app::memory::onnx::load_selection(engine, model)?;
                    *EMBEDDER_SLOT.lock().map_err(|_| anyhow!("embedder cache poisoned"))? = None;
                    emit_result(ResultEnvelope::new("model load", OutputStatus::Ok, result, vec![]))?;
                    Ok(())
                }
                ModelAction::Probe { text, model } => {
                    let current = build_local()?.retrieval_model()?;
                    let selected = model.clone().unwrap_or_else(||current.clone());
                    anyhow::ensure!(selected == current,"probe must name the active library profile; switch before probing");
                    let result = BgeEmbedder::load_model(&selected)?.probe(text)?;
                    emit_result(ResultEnvelope::new(
                        "model",
                        OutputStatus::Ok,
                        result,
                        vec![],
                    ))?;
                    Ok(())
                }
            };
        }
        Some(Command::Export { file }) => return run_export(file),
        Some(Command::Backup { file }) => return run_backup(file),
        Some(Command::Snapshot { cmd }) => {
            let root = respire_app::service::data_dir();
            let details = match cmd {
                SnapshotCmd::Create { note } => {
                    serde_json::to_value(respire_app::snapshot::create(
                        &root,
                        &respire_app::service::session_user_of_dir(&root),
                        note,
                        &build_session()?,
                    )?)?
                }
                SnapshotCmd::List => serde_json::to_value(respire_app::snapshot::list_report(&root)?)?,
                SnapshotCmd::ResumeSync { reviewed_revision } => {
                    respire_app::service::ensure_writable()?;
                    respire_app::snapshot::resume_sync(&root, reviewed_revision)?;
                    serde_json::json!({"sync_quarantined":false,"reviewed_revision":reviewed_revision})
                }
                SnapshotCmd::Restore { .. } | SnapshotCmd::Recover { .. } => {
                    anyhow::bail!("snapshot restore/recovery must run in the host CLI")
                }
            };
            let mut result = ResultEnvelope::new(
                "snapshot",
                OutputStatus::Ok,
                serde_json::json!({"action":match cmd {SnapshotCmd::Create{..}=>"created",SnapshotCmd::List=>"list",SnapshotCmd::ResumeSync{..}=>"sync_resumed",SnapshotCmd::Restore{..}=>"restored",SnapshotCmd::Recover{..}=>"recovered"}}),
                Vec::new(),
            );
            if matches!(cmd,SnapshotCmd::Create{..}) && details["witness_intact"]==false {
                result.status=OutputStatus::Warn;
                result.errors.push("snapshot file created, but its witness chain is invalid; preserved for explicit recovery/repair".into());
            }
            if matches!(cmd,SnapshotCmd::List) && details["problems"].as_array().is_some_and(|items|!items.is_empty()) {
                result.status=OutputStatus::Warn;
                result.errors.push("some snapshots failed verification; inspect details.problems; healthy snapshots remain usable".into());
            }
            result.details = details;
            return emit_result(result);
        }
        Some(Command::Login { .. }) => anyhow::bail!("login must run in the host terminal"),
        Some(Command::BookMaterial { root }) => return run_book_material(root),
        Some(Command::PortraitMaterial { limit }) => return run_portrait_material(*limit),
        Some(Command::Taxonomy { list, ensure }) => return run_taxonomy(*list, ensure.as_deref()),
        Some(Command::RootCreate {
            title,
            content,
            yes,
        }) => return run_root_create(title, content.as_deref(), *yes),
        _ => {}
    }
}
