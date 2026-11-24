fn model_engines() -> &'static [&'static str] {
    if cfg!(any(windows, target_os = "macos")) {
        &["cpu", "gpu", "npu"]
    } else {
        &["cpu"]
    }
}

fn selected_model_mirror() -> Result<String> {
    Ok(respire_client_host::settings::mirror_from_env()?.unwrap_or_else(|| "auto".to_owned()))
}

fn cancel_model(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Ok(());
    }
    let started = Instant::now();
    let mut cancellation_accepted = false;
    let mut cancel_sent = false;
    loop {
        let progress = crate::rpc::model_control(id, !cancel_sent)
            .map_err(|error| error.to_string())?;
        cancel_sent = true;
        if progress["stale"] == true {
            // The original task finished after accepting cancellation. A newly
            // scheduled task must not be cancelled or reported as its failure.
            if cancellation_accepted {
                return Ok(());
            }
            return Err(t("\u{6a21}\u{578b}\u{4efb}\u{52a1}\u{5df2}\u{53d8}\u{5316}\u{ff1b}\u{5237}\u{65b0}\u{8fdb}\u{5ea6}\u{540e}\u{91cd}\u{8bd5}", "The model task changed; refresh progress and retry"));
        }
        cancellation_accepted |= progress["cancelled"] == true;
        if progress["active"] != true {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(45) {
            return Err(t("\u{53d6}\u{6d88}\u{5c1a}\u{672a}\u{5b8c}\u{6210}\u{ff1b}\u{7b49}\u{5f85}\u{5f53}\u{524d}\u{7f51}\u{7edc}\u{8bfb}\u{53d6}\u{7ed3}\u{675f}\u{540e}\u{91cd}\u{8bd5}", "Cancellation is still pending; wait for the current network read before retrying"));
        }
        std::thread::sleep(FAST);
    }
}

fn model_key(app: &mut App, code: KeyCode) {
    if let Err(error) = try_model_key(app, code) {
        app.set_notice(format!("{error:#}"));
    }
}

fn try_model_key(app: &mut App, code: KeyCode) -> Result<()> {
    if code != KeyCode::Enter {
        return Ok(());
    }
    match app.cursor {
        0 => ask(
            app,
            t(
                "\u{5b89}\u{88c5}\u{6216}\u{6821}\u{9a8c} BGE-M3 \u{91cf}\u{5316}\u{6a21}\u{578b}\u{ff08}543 MiB\u{ff09}\u{ff1f}\u{6821}\u{9a8c}\u{901a}\u{8fc7}\u{5219}\u{8df3}\u{8fc7}\u{3002}",
                "Install or verify BGE-M3 quantized (543 MiB model)? Verified files are skipped.",
            ),
            ConfirmKind::InstallM3,
        ),
        1 => ask(
            app,
            t(
                "\u{5378}\u{8f7d} BGE-M3 \u{5e76}\u{5220}\u{9664}\u{6a21}\u{578b}\u{6587}\u{4ef6}\u{ff1f}\u{5378}\u{8f7d}\u{540e} recall \u{9700}\u{8981}\u{91cd}\u{65b0}\u{5b89}\u{88c5}\u{3002}",
                "Uninstall BGE-M3 and delete its files? Recall will need a reinstall.",
            ),
            ConfirmKind::UninstallM3,
        ),
        2 => {
            let current = selected_model_mirror()?;
            app.overlay = Overlay::ModelMirror(text_input::Input::new(current));
        }
        cursor => {
            let engines = model_engines();
            if let Some(engine) = cursor.checked_sub(3).and_then(|index| engines.get(index)) {
                run_confirm(app, ConfirmKind::Engine((*engine).to_owned()));
            } else if cfg!(windows) && cursor == 3 + engines.len() {
                ask(
                    app,
                    t(
                        "\u{901a}\u{8fc7} Windows ML \u{4e0b}\u{8f7d}\u{517c}\u{5bb9}\u{7684} NPU \u{63a8}\u{7406}\u{5f15}\u{64ce}\u{ff1f}",
                        "Download compatible NPU providers through Windows ML?",
                    ),
                    ConfirmKind::InstallEngines,
                );
            } else if cursor == 3 + engines.len() + usize::from(cfg!(windows)) {
                run_confirm(app, ConfirmKind::ProbeEngine);
            } else {
                let index = 4 + engines.len() + usize::from(cfg!(windows));
                match cursor.checked_sub(index) {
                    Some(0) => run_confirm(app, ConfirmKind::RecallMode("fast".to_owned())),
                    Some(1) if !recall_api::ready()? => {
                        app.overlay = Overlay::RecallApi(recall_api::Form::new()?);
                        app.set_notice(t("\u{8bf7}\u{5148}\u{914d}\u{7f6e}\u{9ad8}\u{8d28}\u{91cf}\u{53ec}\u{56de} API", "Configure the recall API first"));
                    }
                    Some(1) => ask(
                        app,
                        t(
                            "\u{542f}\u{7528}\u{9ad8}\u{8d28}\u{91cf}\u{53ec}\u{56de}\u{ff1f}\u{67e5}\u{8be2}\u{548c}\u{5019}\u{9009}\u{6807}\u{9898}\u{5c06}\u{53d1}\u{9001}\u{5230}\u{5df2}\u{914d}\u{7f6e}\u{7684}\u{6a21}\u{578b}\u{670d}\u{52a1}\u{ff1b}\u{5931}\u{8d25}\u{65f6}\u{9000}\u{56de}\u{672c}\u{5730}\u{7ed3}\u{679c}\u{3002}",
                            "Enable high-quality recall? Queries and candidate titles go to your configured model service; failures use local results.",
                        ),
                        ConfirmKind::RecallMode("quality".to_owned()),
                    ),
                    Some(2) => app.overlay = Overlay::RecallApi(recall_api::Form::new()?),
                    Some(3) => app.overlay = Overlay::RecallTest(recall_test::Form::new("fast")),
                    Some(4) => app.overlay = Overlay::RecallTest(recall_test::Form::new("quality")),
                    Some(5) => {
                        if let Some(id) = app.live.model_progress["id"].as_str().filter(|_| app.live.model_progress["active"] == true) {
                            let id = id.to_owned();
                            ask(app, t("\u{53d6}\u{6d88}\u{5f53}\u{524d}\u{6a21}\u{578b}\u{4efb}\u{52a1}\u{ff1f}\u{5df2}\u{5b8c}\u{6210}\u{7684}\u{6a21}\u{578b}\u{6587}\u{4ef6}\u{548c}\u{539f}\u{7d22}\u{5f15}\u{4f1a}\u{4fdd}\u{7559}\u{3002}", "Cancel the current model task? Completed files and the original index are kept."), ConfirmKind::CancelModel(id));
                        } else { app.set_notice(t("\u{6ca1}\u{6709}\u{6b63}\u{5728}\u{8fd0}\u{884c}\u{7684}\u{6a21}\u{578b}\u{4efb}\u{52a1}", "No model task is running")); }
                    }
                    Some(6) => {
                        let id = app.live.model_progress["id"].as_str().unwrap_or("").to_owned();
                        ask(app, t("\u{53d6}\u{6d88}\u{5f53}\u{524d}\u{4efb}\u{52a1}\u{5e76}\u{4ece}\u{6240}\u{9009}\u{4e0b}\u{8f7d}\u{6e90}\u{91cd}\u{65b0}\u{4e0b}\u{8f7d}\u{672a}\u{5b8c}\u{6210}\u{6587}\u{4ef6}\u{ff1f}", "Cancel the current task and redownload unfinished files from the selected source?"), ConfirmKind::RestartM3(id, selected_model_mirror()?));
                    }
                    Some(7) => app.overlay = Overlay::RecallTest(recall_test::Form::new("benchmark")),
                    Some(8) => match model_weights::Form::new(&app.live.model_settings) {
                        Ok(form) => app.overlay = Overlay::ModelWeights(form),
                        Err(error) => app.set_notice(format!("{error:#}")),
                    },
                    Some(9) => match recall_fusion::Form::new() {
                        Ok(form) => app.overlay = Overlay::RecallFusion(form),
                        Err(error) => app.set_notice(error),
                    },
                    Some(10) => app.page = Page::Home,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn model_weights_key(app: &mut App, code: KeyCode) {
    let Overlay::ModelWeights(form) = &mut app.overlay else {
        return;
    };
    let action = form.key(code);
    let (engine, variant) = form.selection();
    let variant = variant.to_owned();
    let size = form.selected_size();
    match action {
        Some(model_weights::Action::Back) => app.overlay = Overlay::None,
        Some(model_weights::Action::Save) => {
            if !model_engines().contains(&engine) {
                app.set_notice(t("\u{6b64}\u{7cfb}\u{7edf}\u{4e0d}\u{652f}\u{6301}\u{8be5}\u{5f15}\u{64ce}\u{ff1b}\u{8bf7}\u{9009}\u{62e9} CPU", "This OS does not support that engine; choose CPU"));
            } else if !model_weights::Form::present(&app.live.model_settings, &variant) {
                app.set_notice(t("\u{8bf7}\u{5148}\u{4e0b}\u{8f7d}\u{6240}\u{9009}\u{6743}\u{91cd}\u{ff1b}\u{4e0d}\u{4f1a}\u{81ea}\u{52a8}\u{4e0b}\u{8f7d}\u{6216}\u{4fdd}\u{5b58}", "Download the selected weights first; nothing was downloaded or saved"));
            } else {
                ask(app, t(&format!("\u{6821}\u{9a8c}\u{5e76}\u{52a0}\u{8f7d} {engine} / {variant}\u{ff1f}\u{8bd5}\u{63a8}\u{7406}\u{6210}\u{529f}\u{540e}\u{4fdd}\u{5b58}\u{ff1b}\u{4e0d}\u{4f1a}\u{4e0b}\u{8f7d}\u{3002}"),
                    &format!("Validate and load {engine} / {variant}? Save only after trial inference succeeds; no download.")),
                    ConfirmKind::Weights(engine.to_owned(), variant));
            }
        }
        Some(model_weights::Action::Install) => {
            let mib = size as f64 / 1048576.0;
            ask(app, t(&format!("\u{4e0b}\u{8f7d} {variant}\u{ff08}{mib:.0} MiB\u{ff09}\u{ff1f}\u{57fa}\u{7840}\u{6a21}\u{578b}\u{53ca} CPU \u{5bb9}\u{5668}\u{53ef}\u{80fd}\u{53e6}\u{9700}\u{5404} 543 MiB\u{ff0c}tokenizer \u{7ea6} 17 MiB\u{ff1b}\u{5df2}\u{9a8c}\u{8bc1}\u{6587}\u{4ef6}\u{8df3}\u{8fc7}\u{3002}\u{53ea}\u{4e0b}\u{8f7d}\u{ff0c}\u{4e0d}\u{4fdd}\u{5b58}\u{9009}\u{62e9}\u{3002}"),
                    &format!("Download {variant} ({mib:.0} MiB)? Baseline and CPU container may each need another 543 MiB plus a 17 MiB tokenizer; verified files are skipped. Downloads only; selection is not saved.")),
                    ConfirmKind::InstallVariant(engine.to_owned(), variant));
        }
        None => {}
    }
}

fn save_model_settings_with(
    engine: &str,
    variant: Option<&str>,
    mut action: impl FnMut(&[&str]) -> Result<Value, String>,
) -> Result<Value, String> {
    let current;
    let variant = if let Some(variant) = variant {
        variant
    } else {
        current = checked_result(action(&["model", "engine"])?)?;
        current["summary"]["model"].as_str().ok_or_else(|| {
            t(
                "\u{65e0}\u{6cd5}\u{8bfb}\u{53d6}\u{5f53}\u{524d}\u{6743}\u{91cd}",
                "Cannot read current weights",
            )
        })?
    };
    let args = ["model", "load", engine, "--model", variant];
    let loaded = checked_result(action(&args)?)?;
    if loaded["summary"]["loaded"] != true
        || loaded["summary"]["validated"] != true
        || loaded["summary"]["saved"] != true
    {
        return Err(t("\u{52a0}\u{8f7d}\u{56de}\u{6267}\u{4e0d}\u{5b8c}\u{6574}\u{ff0c}\u{672a}\u{786e}\u{8ba4}\u{52a0}\u{8f7d}\u{6210}\u{529f}", "Incomplete load receipt; loading was not confirmed"));
    }
    let read_back = checked_result(action(&["model", "engine"])?)?;
    let summary = &read_back["summary"];
    if summary["engine"] != engine || summary["model"] != variant {
        return Err(t("runtime \u{56de}\u{8bfb}\u{4e0e}\u{9009}\u{62e9}\u{4e0d}\u{4e00}\u{81f4}\u{ff1b}\u{8bf7}\u{68c0}\u{67e5}\u{73af}\u{5883}\u{53d8}\u{91cf}\u{8986}\u{76d6}\u{5e76}\u{5237}\u{65b0}", "Runtime read-back differs from selection; check environment overrides and refresh"));
    }
    Ok(summary.clone())
}

fn save_model_settings(
    shared: &Arc<Mutex<Live>>,
    engine: &str,
    variant: Option<&str>,
) -> Result<String, String> {
    let settings = save_model_settings_with(engine, variant, retrieval_action)?;
    if let Ok(mut live) = shared.lock() {
        live.engine = settings["engine"].as_str().unwrap_or("").to_owned();
        live.model_settings = settings;
    }
    Ok(t("\u{6743}\u{91cd}\u{5df2}\u{6821}\u{9a8c}\u{3001}\u{52a0}\u{8f7d}\u{3001}\u{8bd5}\u{63a8}\u{7406}\u{5e76}\u{4fdd}\u{5b58}\u{ff1b}\u{5df2}\u{56de}\u{8bfb}\u{8bbe}\u{7f6e}", "Weights validated, loaded, trial inference passed, saved and read back."))
}

fn probe_notice(value: Value) -> Result<String, String> {
    let result = &value["summary"];
    if result["ready"] != true || !result["cosine"].as_f64().is_some_and(|v| v >= 0.99) {
        return Err(t("\u{63a8}\u{7406}\u{9a8c}\u{8bc1}\u{56de}\u{6267}\u{4e0d}\u{5b8c}\u{6574}\u{ff0c}\u{672a}\u{62a5}\u{544a}\u{9a8c}\u{8bc1}\u{6210}\u{529f}", "Incomplete inference receipt; validation was not accepted"));
    }
    Ok(format!("{}: {}  cosine {}  {} ms", t("\u{6570}\u{503c}\u{9a8c}\u{8bc1}\u{901a}\u{8fc7}\u{ff08}\u{4e0d}\u{4ee3}\u{8868}\u{5b9e}\u{6d4b}\u{7b97}\u{5b50}\u{786c}\u{4ef6}\u{5206}\u{914d}\u{ff09}", "Numerical check passed (node placement not measured)"),
        result["selected"].as_str().unwrap_or("unknown"), result["cosine"], result["elapsed_ms"]))
}

fn workspace_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('0') => app.page = Page::Home,
        KeyCode::Up => app.cursor = 0,
        KeyCode::Down => app.cursor = 1,
        KeyCode::Left if app.cursor == 0 => {
            app.workspace_draft = app.workspace_draft.saturating_sub(1)
        }
        KeyCode::Right if app.cursor == 0 => app.workspace_draft = (app.workspace_draft + 1).min(2),
        KeyCode::Enter if app.cursor == 1 => app.page = Page::Home,
        KeyCode::Enter => {
            let mode = ["normal", "readonly", "off"][app.workspace_draft];
            ask(
                app,
                t(
                    &format!("\u{628a}\u{6574}\u{53f0}\u{8bbe}\u{5907}\u{7684}\u{5de5}\u{4f5c}\u{6a21}\u{5f0f}\u{6539}\u{6210} {}\u{ff1f}", workspace_label(mode)),
                    &format!("Change service mode for this device to {}?", workspace_label(mode)),
                ),
                ConfirmKind::Workspace(mode.to_owned()),
            );
        }
        _ => {}
    }
    false
}

fn version_key(app: &mut App, code: KeyCode) -> bool {
    let tools = tools_on_path();
    match code {
        KeyCode::Char('0') => app.page = Page::Home,
        KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down => app.cursor = (app.cursor + 1).min(2),
        KeyCode::Left if app.cursor == 1 && !tools.is_empty() => {
            app.manager_pick = app.manager_pick.saturating_sub(1);
        }
        KeyCode::Right if app.cursor == 1 && !tools.is_empty() => {
            app.manager_pick = (app.manager_pick + 1).min(tools.len() - 1);
        }
        KeyCode::Enter if app.cursor == 2 => app.page = Page::Home,
        KeyCode::Enter if app.cursor == 0 => check_versions(app),
        KeyCode::Enter => {
            let Some(manager) = tools.get(app.manager_pick).copied() else {
                app.set_notice(t(
                    "\u{6ca1}\u{6709}\u{627e}\u{5230} npm\u{3001}pnpm\u{3001}yarn \u{6216} bun",
                    "npm, pnpm, yarn, and bun were not found",
                ));
                return false;
            };
            if matches!(manager, Manager::Direct) {
                app.set_notice(t(
                    "\u{5f53}\u{524d}\u{7a97}\u{53e3}\u{662f}\u{76f4}\u{63a5}\u{8fd0}\u{884c}\u{7684}\u{7a0b}\u{5e8f}\u{ff0c}\u{5305}\u{7ba1}\u{7406}\u{5668}\u{66f4}\u{65b0}\u{4e0d}\u{4f1a}\u{6362}\u{6389}\u{8fd9}\u{4e2a}\u{7a97}\u{53e3}",
                    "This window is a direct binary. A package-manager update will not replace it",
                ));
                return false;
            }
            let spec = if app.live.update_spec.is_empty() {
                channel_spec()
            } else {
                app.live.update_spec.clone()
            };
            ask(
                app,
                t(
                    &format!(
                        "\u{7528} {} \u{66f4}\u{65b0}\u{5230} {spec}\u{ff1f}",
                        manager_name(manager)
                    ),
                    &format!("Update to {spec} with {}?", manager_name(manager)),
                ),
                ConfirmKind::Update(manager_name(manager).to_owned(), spec),
            );
        }
        _ => {}
    }
    false
}

fn lang_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('1') => app.cursor = 0,
        KeyCode::Char('2') => app.cursor = 1,
        KeyCode::Enter => {
            let lang = if app.cursor == 0 { Lang::Zh } else { Lang::En };
            match i18n::set_lang(lang) {
                Ok(()) => app.set_notice(t(
                    "\u{8bed}\u{8a00}\u{5df2}\u{5207}\u{6362}",
                    "Language saved",
                )),
                Err(error) => app.set_notice(error.to_string()),
            }
        }
        _ => {}
    }
}

fn ask(app: &mut App, text: String, kind: ConfirmKind) {
    app.overlay = Overlay::Confirm {
        text,
        cursor: 1,
        kind,
    };
}

fn confirm_key(app: &mut App, code: KeyCode) -> bool {
    let Overlay::Confirm { cursor, .. } = &mut app.overlay else {
        return false;
    };
    match code {
        KeyCode::Up | KeyCode::Left | KeyCode::Char('1') => *cursor = 0,
        KeyCode::Down | KeyCode::Right | KeyCode::Char('2') => *cursor = 1,
        KeyCode::Enter if *cursor == 1 => app.overlay = Overlay::None,
        KeyCode::Enter => {
            let kind = match std::mem::replace(&mut app.overlay, Overlay::None) {
                Overlay::Confirm { kind, .. } => kind,
                other => {
                    app.overlay = other;
                    return false;
                }
            };
            run_confirm(app, kind);
        }
        KeyCode::Char('0') => app.overlay = Overlay::None,
        _ => {}
    }
    false
}

fn run_confirm(app: &mut App, kind: ConfirmKind) {
    let host_model = matches!(
        &kind,
        ConfirmKind::InstallM3
            | ConfirmKind::InstallVariant(_, _)
            | ConfirmKind::UninstallM3
            | ConfirmKind::InstallEngines
            | ConfirmKind::RestartM3(_, _)
    );
    if host_model {
        app.overlay = Overlay::HostModel;
    }
    let label = match &kind {
        ConfirmKind::Switch(name) => t(
            &format!("\u{6b63}\u{5728}\u{5207}\u{6362}\u{5230} {name}"),
            &format!("Switching to {name}"),
        ),
        ConfirmKind::Migrate(_, _) => t("\u{6b63}\u{5728}\u{8fc1}\u{79fb}\u{6240}\u{9009}\u{65e7}\u{8d26}\u{6237}", "Migrating selected legacy account"),
        ConfirmKind::Sync => t("\u{6b63}\u{5728}\u{540c}\u{6b65}", "Syncing"),
        ConfirmKind::InstallM3 => t("\u{6b63}\u{5728}\u{5b89}\u{88c5}\u{6216}\u{6821}\u{9a8c} BGE-M3", "Installing or verifying BGE-M3"),
        ConfirmKind::UninstallM3 => t("\u{6b63}\u{5728}\u{5378}\u{8f7d} BGE-M3", "Uninstalling BGE-M3"),
        ConfirmKind::CancelModel(_) => t("\u{6b63}\u{5728}\u{53d6}\u{6d88}\u{6a21}\u{578b}\u{4efb}\u{52a1}\u{ff0c}\u{7b49}\u{5f85}\u{5f53}\u{524d}\u{7f51}\u{7edc}\u{8bfb}\u{53d6}\u{7ed3}\u{675f}", "Cancelling model task; waiting for the current network read"),
        ConfirmKind::RestartM3(_, _) => t("\u{6b63}\u{5728}\u{53d6}\u{6d88}\u{65e7}\u{4efb}\u{52a1}\u{5e76}\u{91cd}\u{65b0}\u{5b89}\u{6392}\u{4e0b}\u{8f7d}", "Cancelling the old task and scheduling a new download"),
        ConfirmKind::Weights(_, _) => t("\u{6b63}\u{5728}\u{6821}\u{9a8c}\u{5e76}\u{52a0}\u{8f7d}\u{ff1b}\u{8bd5}\u{63a8}\u{7406}\u{6210}\u{529f}\u{540e}\u{4fdd}\u{5b58}", "Validating and loading; saving after trial inference"),
        ConfirmKind::InstallVariant(_, _) => t("\u{6b63}\u{5728}\u{4e0b}\u{8f7d}\u{6240}\u{9009}\u{6743}\u{91cd}\u{ff1b}\u{4e0d}\u{5207}\u{6362}\u{6a21}\u{578b}", "Downloading selected weights; current model unchanged"),
        ConfirmKind::Engine(_) => t("\u{6b63}\u{5728}\u{6821}\u{9a8c}\u{5e76}\u{52a0}\u{8f7d}\u{63a8}\u{7406}\u{5f15}\u{64ce}", "Validating and loading inference engine"),
        ConfirmKind::InstallEngines => t("\u{6b63}\u{5728}\u{4e0b}\u{8f7d} NPU \u{63a8}\u{7406}\u{5f15}\u{64ce}", "Downloading NPU providers"),
        ConfirmKind::ProbeEngine => t("\u{6b63}\u{5728}\u{9a8c}\u{8bc1}\u{63a8}\u{7406}\u{5f15}\u{64ce}", "Checking inference engine"),
        ConfirmKind::RecallMode(_) => t("\u{6b63}\u{5728}\u{4fdd}\u{5b58}\u{53ec}\u{56de}\u{6a21}\u{5f0f}", "Saving recall mode"),
        ConfirmKind::Workspace(_) => t("\u{6b63}\u{5728}\u{5207}\u{6362}\u{5de5}\u{4f5c}\u{533a}", "Changing workspace"),
        ConfirmKind::Autosync(_) => t("\u{6b63}\u{5728}\u{4fdd}\u{5b58}\u{81ea}\u{52a8}\u{540c}\u{6b65}", "Saving auto-sync"),
        ConfirmKind::Update(tool, spec) => t(
            &format!("\u{6b63}\u{5728}\u{7528} {tool} \u{66f4}\u{65b0} {spec}"),
            &format!("Updating {spec} with {tool}"),
        ),
    };
    let changing_model = matches!(&kind, ConfirmKind::Engine(_) | ConfirmKind::Weights(_, _));
    if changing_model {
        app.overlay = Overlay::SavingWeights;
        MODEL_REVISION.fetch_add(1, Ordering::AcqRel);
    }
    app.set_notice(label);
    let shared = Arc::clone(&app.shared);
    let slot = Arc::clone(if changing_model || host_model {
        &app.model_outcome
    } else {
        &app.outcome
    });
    std::thread::spawn(move || {
        let result = match kind {
            ConfirmKind::Switch(name) => {
                switch_account(&name)
            }
            ConfirmKind::Migrate(id, account) => host_command(&["migrate", "--source", &id, "--account", &account])
                .map(|_| t("\u{8fc1}\u{79fb}\u{5b8c}\u{6210}\u{ff1b}\u{53ef}\u{5728}\u{8d26}\u{6237}\u{9875}\u{9762}\u{5207}\u{6362}", "Migration complete; select it on the Accounts page")),
            ConfirmKind::Sync => rpc(&["sync"]).map(|value| sync_notice(&value)),
            ConfirmKind::InstallM3 => selected_model_mirror().map_err(|error| format!("{error:#}"))
                .and_then(|mirror| host_command(&["model", "install-m3", "--mirror", &mirror]))
                .map(|_| t("BGE-M3 \u{5904}\u{7406}\u{7ed3}\u{675f}", "BGE-M3 step finished")),
            ConfirmKind::CancelModel(id) => cancel_model(&id)
                .map(|_| t("\u{5df2}\u{53d6}\u{6d88}\u{6a21}\u{578b}\u{4efb}\u{52a1}", "Model task cancelled")),
            ConfirmKind::RestartM3(id, mirror) => cancel_model(&id)
                .and_then(|_| host_command(&["model", "install-m3", "--mirror", &mirror]))
                .map(|_| t("BGE-M3 \u{5b89}\u{88c5}\u{5b8c}\u{6210}", "BGE-M3 installation complete")),
            ConfirmKind::UninstallM3 => {
                host_command(&["model", "uninstall-m3"]).map(|_| t("BGE-M3 \u{5df2}\u{5378}\u{8f7d}", "BGE-M3 uninstalled"))
            }
            ConfirmKind::Engine(engine) => save_model_settings(&shared, &engine, None),
            ConfirmKind::Weights(engine, variant) => save_model_settings(&shared, &engine, Some(&variant)),
            ConfirmKind::InstallVariant(engine, variant) => selected_model_mirror().map_err(|error| format!("{error:#}"))
                .and_then(|mirror| host_command(&["model", "install-m3", "--model", &variant, "--for-engine", &engine,
                "--mirror", &mirror])).map(|_| t(
                    "\u{9884}\u{4e0b}\u{8f7d}\u{5b8c}\u{6210}\u{ff1b}\u{5f53}\u{524d}\u{6a21}\u{578b}\u{672a}\u{5207}\u{6362}\u{ff0c}\u{53ef}\u{7ee7}\u{7eed}\u{4e0b}\u{8f7d}\u{5176}\u{4ed6}\u{6743}\u{91cd}\u{6216}\u{9009}\u{62e9}\u{52a0}\u{8f7d}", "Download complete; current model unchanged. Download more weights or select Load.")),
            ConfirmKind::RecallMode(mode) => {
                retrieval_action(&["agent-config", "--set", &format!("recall_mode={mode}")]).map(
                    |_| {
                        t(
                            "\u{53ec}\u{56de}\u{6a21}\u{5f0f}\u{5df2}\u{4fdd}\u{5b58}\u{ff0c}\u{4e0b}\u{6b21}\u{67e5}\u{8be2}\u{751f}\u{6548}",
                            "Recall mode saved; applies to the next query",
                        )
                    },
                )
            }
            ConfirmKind::InstallEngines => host_command(&["model", "install-engines"]).map(|v| {
                format!(
                    "{}: {}",
                    t("\u{63a8}\u{7406}\u{5f15}\u{64ce}", "Providers"),
                    v["summary"]["providers"].as_array().map(|providers|
                        providers.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()
                )
            }),
            ConfirmKind::ProbeEngine => retrieval_action(&["model", "probe"]).and_then(probe_notice),
            ConfirmKind::Workspace(mode) => set_workspace(&mode),
            ConfirmKind::Autosync(on) => {
                let flag = if on { "true" } else { "false" };
                host_command(&["config", "--autosync", flag]).map(|_| t("\u{81ea}\u{52a8}\u{540c}\u{6b65}\u{5df2}\u{4fdd}\u{5b58}", "Auto-sync saved"))
            }
            ConfirmKind::Update(tool, spec) => run_package_update(&tool, &spec),
        };
        if changing_model {
            MODEL_REVISION.fetch_add(1, Ordering::AcqRel);
        }
        let message = match result {
            Ok(text) => text,
            Err(error) => error,
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(message);
        }
    });
}
