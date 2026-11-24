pub fn run() -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        anyhow::bail!(i18n::text("need_tty"));
    }
    // Finish host startup before terminal rendering. Polling never takes over a
    // runtime, and host actions capture their diagnostics instead of drawing them.
    crate::rpc::ensure_daemon()?;
    enable_raw_mode()?;
    let mut terminal = setup()?;
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(Live::empty()));
    let worker_stop = Arc::clone(&stop);
    let worker_shared = Arc::clone(&shared);
    std::thread::spawn(move || refresh_loop(worker_stop, worker_shared));
    let worker_stop = Arc::clone(&stop);
    let worker_shared = Arc::clone(&shared);
    std::thread::spawn(move || refresh_details_loop(worker_stop, worker_shared));
    let worker_stop = stop.clone();
    let worker_shared = shared.clone();
    std::thread::spawn(move || refresh_inference_loop(worker_stop, worker_shared));
    let mut app = App::new(Arc::clone(&shared));
    let result = loop {
        if let Overlay::RecallTest(form) = &mut app.overlay {
            form.poll();
        }
        if let Overlay::RecallFusion(form) = &mut app.overlay {
            form.poll();
        }
        let api_saved = match &mut app.overlay {
            Overlay::RecallApi(form) => form.poll_saved(),
            _ => false,
        };
        if api_saved {
            app.overlay = Overlay::None;
            app.set_notice(t(
                "API \u{914d}\u{7f6e}\u{5df2}\u{4fdd}\u{5b58}\u{ff1b}\u{9009}\u{62e9}\u{9ad8}\u{8d28}\u{91cf}\u{6a21}\u{5f0f}\u{540e}\u{751f}\u{6548}",
                "API saved; select High quality to enable it",
            ));
        }
        if let Ok(mut slot) = app.outcome.lock() {
            if let Some(message) = slot.take() {
                // An older background action must not dismiss an in-flight save.
                if !matches!(app.overlay, Overlay::SavingWeights | Overlay::HostModel) {
                    app.overlay = Overlay::None;
                    app.set_notice(message);
                }
            }
        }
        if let Ok(mut slot) = app.model_outcome.lock() {
            if let Some(message) = slot.take() {
                app.overlay = Overlay::None;
                app.set_notice(message);
            }
        }
        if let Ok(guard) = shared.lock() {
            app.live = guard.clone();
        }
        if let Err(error) = terminal.draw(|frame| draw(frame, &app)) {
            break Err(error);
        }
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Paste(text) = &event {
            match &mut app.overlay {
                Overlay::RecallApi(form) => form.paste(text),
                Overlay::RecallFusion(form) => form.paste(text),
                Overlay::ModelMirror(input) => input.paste(text),
                Overlay::RecallTest(form) => form.paste(text),
                _ => {}
            }
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if let Overlay::ModelMirror(input) = &mut app.overlay {
            if key.code == KeyCode::Esc {
                app.overlay = Overlay::None;
            } else if key.code == KeyCode::Enter {
                let value = input.text.trim().to_owned();
                let result = respire_commands::model_download::validate_mirror(&value)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        retrieval_action(&[
                            "agent-config",
                            "--set",
                            &format!("model_mirror={value}"),
                        ])
                        .map(|_| ())
                    });
                match result {
                    Ok(()) => {
                        app.overlay = Overlay::None;
                        app.set_notice(t(
                            "\u{4e0b}\u{8f7d}\u{6e90}\u{5df2}\u{4fdd}\u{5b58}",
                            "Download source saved",
                        ));
                        if let Some(id) = app.live.model_progress["id"]
                            .as_str()
                            .filter(|_| app.live.model_progress["active"] == true)
                        {
                            let id = id.to_owned();
                            ask(&mut app, t("\u{53d6}\u{6d88}\u{5f53}\u{524d}\u{4efb}\u{52a1}\u{ff0c}\u{4f7f}\u{7528}\u{6240}\u{9009}\u{4e0b}\u{8f7d}\u{6e90}\u{91cd}\u{65b0}\u{4e0b}\u{8f7d}\u{ff1f}", "Cancel the current task and restart using the selected source?"), ConfirmKind::RestartM3(id, value));
                        }
                    }
                    Err(error) => app.set_notice(error),
                }
            } else if matches!(key.code, KeyCode::Up | KeyCode::Down) {
                let presets = respire_commands::model_download::MIRRORS;
                let index = presets.iter().position(|v| *v == input.text).unwrap_or(0);
                let next = if key.code == KeyCode::Down {
                    (index + 1) % presets.len()
                } else {
                    (index + presets.len() - 1) % presets.len()
                };
                *input = text_input::Input::new(presets[next].to_owned());
            } else {
                input.key(key);
            }
            continue;
        }
        if let Overlay::RecallFusion(form) = &mut app.overlay {
            if form.key(key) {
                app.overlay = Overlay::None;
            }
            continue;
        }
        if let Overlay::RecallApi(form) = &mut app.overlay {
            if key.code == KeyCode::Esc && !form.saving() {
                app.overlay = Overlay::None;
            } else {
                form.key(key);
            }
            continue;
        }
        if let Overlay::RecallTest(form) = &mut app.overlay {
            if key.code == KeyCode::Esc {
                app.overlay = Overlay::None;
            } else {
                form.key(key);
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            continue;
        }
        if handle(&mut app, key.code) {
            break Ok(());
        }
        if app.login_requested {
            app.login_requested = false;
            // Give secret prompts sole ownership of the normal terminal, then redraw the TUI.
            restore(&mut terminal)?;
            disable_raw_mode()?;
            let login = crate::login::run(None, None, None, None, true, false, None, false, false);
            enable_raw_mode()?;
            terminal = match setup() {
                Ok(terminal) => terminal,
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(error);
                }
            };
            app.set_notice(match login {
                Ok(()) => t("\u{767b}\u{5f55}\u{6210}\u{529f}\u{ff1b}\u{8d26}\u{6237}\u{5df2}\u{9a8c}\u{8bc1}", "Signed in; account verified"),
                Err(error) => format!("{error:#}"),
            });
        }
    };
    stop.store(true, Ordering::Relaxed);
    restore(&mut terminal)?;
    disable_raw_mode()?;
    result.map_err(anyhow::Error::from)
}

impl Live {
    fn empty() -> Self {
        Self {
            inference: Value::Null,
            index: Value::Null,
            model_progress: Value::Null,
            engine: String::new(),
            model_settings: Value::Null,
            connected: false,
            error: String::new(),
            notice: String::new(),
            user: String::new(),
            addr: String::new(),
            autosync: true,
            workspace: "normal".to_owned(),
            local_alive: 0,
            local_total: 0,
            phase: "idle".to_owned(),
            pulled: 0,
            pushed: 0,
            remote_alive: 0,
            conflicts: 0,
            pending: 0,
            pid: 0,
            url: String::new(),
            runtime_version: String::new(),
            doctor: Vec::new(),
            inject: Vec::new(),
            accounts: Vec::new(),
            fresh: 0,
            stale: 0,
            missing: 0,
            ver_line: String::new(),
            update_spec: String::new(),
        }
    }

    fn clone(&self) -> Self {
        Self {
            inference: self.inference.clone(),
            index: self.index.clone(),
            model_progress: self.model_progress.clone(),
            engine: self.engine.clone(),
            model_settings: self.model_settings.clone(),
            connected: self.connected,
            error: self.error.clone(),
            notice: self.notice.clone(),
            user: self.user.clone(),
            addr: self.addr.clone(),
            autosync: self.autosync,
            workspace: self.workspace.clone(),
            local_alive: self.local_alive,
            local_total: self.local_total,
            phase: self.phase.clone(),
            pulled: self.pulled,
            pushed: self.pushed,
            remote_alive: self.remote_alive,
            conflicts: self.conflicts,
            pending: self.pending,
            pid: self.pid,
            url: self.url.clone(),
            runtime_version: self.runtime_version.clone(),
            doctor: self.doctor.clone(),
            inject: self.inject.iter().map(InjectRow::clone_row).collect(),
            accounts: self.accounts.iter().map(AccountRow::clone_row).collect(),
            fresh: self.fresh,
            stale: self.stale,
            missing: self.missing,
            ver_line: self.ver_line.clone(),
            update_spec: self.update_spec.clone(),
        }
    }
}

impl InjectRow {
    fn clone_row(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            state: self.state.clone(),
            seen: self.seen,
            checked: self.checked,
        }
    }
}

impl AccountRow {
    fn clone_row(&self) -> Self {
        Self {
            name: self.name.clone(),
            user: self.user.clone(),
            current: self.current,
        }
    }
}

impl App {
    fn new(shared: Arc<Mutex<Live>>) -> Self {
        Self {
            login_requested: false,
            migration_profiles: Vec::new(),
            page: Page::Home,
            cursor: 0,
            overlay: Overlay::None,
            live: Live::empty(),
            workspace_draft: 0,
            autosync_draft: true,
            manager_pick: 0,
            shared,
            outcome: Arc::new(Mutex::new(None)),
            model_outcome: Arc::new(Mutex::new(None)),
        }
    }

    fn set_notice(&self, text: impl Into<String>) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.notice = text.into();
        }
    }
}
