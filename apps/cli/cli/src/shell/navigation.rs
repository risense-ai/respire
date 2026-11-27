fn handle(app: &mut App, code: KeyCode) -> bool {
    if matches!(app.overlay, Overlay::SavingWeights | Overlay::HostModel) {
        return false;
    }
    if matches!(app.overlay, Overlay::ModelWeights(_)) {
        if code == KeyCode::Char('7') {
            toggle_lang(app);
            return false;
        }
        model_weights_key(app, code);
        return false;
    }
    if code == KeyCode::Esc {
        if !matches!(app.overlay, Overlay::None) {
            app.overlay = Overlay::None;
            return false;
        }
        return go_back(app);
    }
    if code == KeyCode::Char('7') && !matches!(app.overlay, Overlay::Edit { .. }) {
        toggle_lang(app);
        return false;
    }
    if !app.live.connected {
        return code == KeyCode::Char('0');
    }
    if matches!(app.overlay, Overlay::Edit { .. }) {
        return edit_key(app, code);
    }
    if matches!(app.overlay, Overlay::Confirm { .. }) {
        return confirm_key(app, code);
    }
    match app.page {
        Page::Home => home_key(app, code),
        Page::Accounts => list_key(app, code, app.live.accounts.len() + 2, |app, code| {
            account_key(app, code)
        }),
        Page::Migration => list_key(app, code, app.migration_profiles.len(), migration_key),
        Page::Server => server_key(app, code),
        Page::Inject => list_key(app, code, app.live.inject.len() + 2, |app, code| {
            inject_key(app, code)
        }),
        Page::Sync => sync_key(app, code),
        Page::Model => list_key(
            app,
            code,
            15 + model_engines().len() + usize::from(cfg!(windows)),
            |app, code| model_key(app, code),
        ),
        Page::Workspace => workspace_key(app, code),
        Page::Lang => list_key(app, code, 2, |app, code| lang_key(app, code)),
        Page::Version => version_key(app, code),
        Page::Web => web_key(app, code),
    }
}

fn go_back(app: &mut App) -> bool {
    if matches!(app.page, Page::Home) {
        return true;
    }
    app.page = Page::Home;
    false
}

fn toggle_lang(app: &mut App) {
    let next = if zh() { Lang::En } else { Lang::Zh };
    match i18n::set_lang(next) {
        Ok(()) => app.set_notice(t(
            "\u{8bed}\u{8a00}\u{5df2}\u{5207}\u{6362}",
            "Language saved",
        )),
        Err(error) => app.set_notice(error.to_string()),
    }
}

fn home_key(app: &mut App, code: KeyCode) -> bool {
    let n = 10;
    match code {
        KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down => app.cursor = (app.cursor + 1).min(n - 1),
        KeyCode::Char('0') => return true,
        KeyCode::Char(ch) if ch.is_ascii_digit() => {
            let number = (ch as u8 - b'0') as usize;
            if (1..=9).contains(&number) {
                open_home(app, number - 1);
            }
        }
        KeyCode::Enter if app.cursor == 9 => return true,
        KeyCode::Enter => open_home(app, app.cursor),
        _ => {}
    }
    false
}

fn open_home(app: &mut App, index: usize) {
    app.cursor = 0;
    app.workspace_draft = match app.live.workspace.as_str() {
        "readonly" => 1,
        "off" => 2,
        _ => 0,
    };
    app.autosync_draft = app.live.autosync;
    app.page = match index {
        0 => Page::Accounts,
        1 => Page::Server,
        2 => Page::Inject,
        3 => Page::Sync,
        4 => Page::Model,
        5 => Page::Workspace,
        6 => Page::Lang,
        7 => Page::Version,
        _ => Page::Web,
    };
}

fn list_key(
    app: &mut App,
    code: KeyCode,
    content: usize,
    mut on_enter: impl FnMut(&mut App, KeyCode),
) -> bool {
    let total = content + 1;
    match code {
        KeyCode::Char('0') => {
            app.page = Page::Home;
            false
        }
        KeyCode::Up => {
            app.cursor = app.cursor.saturating_sub(1);
            false
        }
        KeyCode::Down => {
            if total > 0 {
                app.cursor = (app.cursor + 1).min(total - 1);
            }
            false
        }
        KeyCode::Enter if app.cursor == content => {
            app.page = Page::Home;
            false
        }
        other if app.cursor < content => {
            on_enter(app, other);
            false
        }
        _ => false,
    }
}

fn account_key(app: &mut App, code: KeyCode) {
    if code != KeyCode::Enter {
        return;
    }
    if app.cursor == app.live.accounts.len() + 1 {
        app.login_requested = true;
        return;
    }
    if app.cursor == app.live.accounts.len() {
        match crate::profile_discovery::list_legacy_profiles() {
            Ok(value) => {
                app.migration_profiles = value["profiles"]
                    .as_array()
                    .map(|profiles| {
                        profiles
                            .iter()
                            .filter(|profile| profile["migrated_to"].is_null())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                app.cursor = 0;
                app.page = Page::Migration;
            }
            Err(error) => app.set_notice(format!("{error:#}")),
        }
        return;
    }
    let Some(row) = app.live.accounts.get(app.cursor) else {
        return;
    };
    if row.current {
        app.set_notice(t(
            "\u{5df2}\u{7ecf}\u{662f}\u{5f53}\u{524d}\u{8d26}\u{6237}",
            "Already the active account",
        ));
        return;
    }
    let name = row.name.clone();
    ask(
        app,
        t(
            &format!("\u{5207}\u{6362}\u{5230}\u{8d26}\u{6237} {name}\u{ff1f}",),
            &format!("Switch to account {name}?"),
        ),
        ConfirmKind::Switch(name),
    );
}

fn migration_key(app: &mut App, code: KeyCode) {
    if code != KeyCode::Enter {
        return;
    }
    let Some(row) = app.migration_profiles.get(app.cursor) else {
        return;
    };
    if !row["migrated_to"].is_null() {
        app.set_notice(t("\u{8be5}\u{6765}\u{6e90}\u{5df2}\u{8fc1}\u{79fb}\u{ff0c}\u{4e0d}\u{4f1a}\u{91cd}\u{590d}\u{590d}\u{5236}", "Already migrated; no duplicate copy"));
        return;
    }
    let (Some(id), Some(account)) = (row["source_id"].as_str(), row["account"].as_str()) else {
        return;
    };
    let id = id.to_owned();
    let account = account.to_owned();
    ask(app, t(&format!("\u{5c06}\u{6240}\u{9009}\u{65e7}\u{8d26}\u{6237}\u{590d}\u{5236}\u{4e3a} {account}\u{ff1f}\u{5f53}\u{524d}\u{8d26}\u{6237}\u{4fdd}\u{6301}\u{4e0d}\u{53d8}\u{3002}"),
        &format!("Copy the selected legacy account into {account}? Current account stays active.")),
        ConfirmKind::Migrate(id, account));
}

fn server_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('0') => app.page = Page::Home,
        KeyCode::Up => app.cursor = 0,
        KeyCode::Down => app.cursor = 1,
        KeyCode::Enter if app.cursor == 1 => app.page = Page::Home,
        KeyCode::Enter => {
            app.overlay = Overlay::Edit {
                buf: app.live.addr.clone(),
                cursor: app.live.addr.chars().count(),
            };
        }
        _ => {}
    }
    false
}

fn inject_key(app: &mut App, code: KeyCode) {
    if code != KeyCode::Char(' ') && code != KeyCode::Enter {
        return;
    }
    if INJECT_REVISION.load(Ordering::Acquire) % 2 != 0 {
        return;
    }
    let batch = app.cursor < 2;
    let remove = app.cursor == 1;
    let selected = if batch {
        app.live
            .inject
            .iter()
            .filter(|row| row.checked)
            .collect::<Vec<_>>()
    } else {
        app.live.inject.get(app.cursor - 2).into_iter().collect()
    };
    let ids: Vec<(String, bool)> = selected
        .iter()
        .map(|row| (row.id.clone(), if batch { remove } else { row.checked }))
        .collect();
    if ids.is_empty() {
        app.set_notice(t(
            "\u{6ca1}\u{6709}\u{5df2}\u{5b89}\u{88c5}\u{7684}\u{63d0}\u{793a}\u{8bcd}",
            "No installed prompts",
        ));
        return;
    }
    INJECT_REVISION.fetch_add(1, Ordering::AcqRel);
    app.set_notice(t(
        "\u{6b63}\u{5728}\u{5904}\u{7406}\u{63d0}\u{793a}\u{8bcd}",
        "Updating prompts",
    ));
    let slot = Arc::clone(&app.outcome);
    let shared = Arc::clone(&app.shared);
    std::thread::spawn(move || {
        let mut succeeded = 0;
        let mut errors = Vec::new();
        for (id, remove) in &ids {
            let result = if *remove {
                host_command(&["inject", "--remove", "--id", id])
            } else {
                host_command(&["inject", "--id", id])
            }
            .and_then(inject_result);
            match result {
                Ok(_) => succeeded += 1,
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        match read_inject_rows() {
            Ok(rows) => {
                if let Ok(mut guard) = shared.lock() {
                    set_inject_rows(&mut guard, rows);
                }
            }
            Err(error) => errors.push(error),
        }
        let mut message = t(
            &format!("\u{63d0}\u{793a}\u{8bcd}\u{5904}\u{7406}\u{5b8c}\u{6210}\u{ff1a}\u{6210}\u{529f} {succeeded}/{}", ids.len()),
            &format!("Prompts finished: {succeeded}/{} succeeded", ids.len()),
        );
        if !errors.is_empty() {
            message.push_str(&format!("; {}", errors.join("; ")));
        }
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(message);
        }
        INJECT_REVISION.fetch_add(1, Ordering::Release);
    });
}

fn inject_result(envelope: Value) -> Result<Value, String> {
    if envelope["status"] != "ok" {
        return Err(envelope["errors"].to_string());
    }
    Ok(envelope)
}

fn read_inject_rows() -> Result<Vec<InjectRow>, String> {
    let envelope = inject_result(host_command(&["inject", "--targets"])?)?;
    let rows = envelope["details"]
        .as_array()
        .ok_or("Missing inject targets")?;
    Ok(rows
        .iter()
        .map(|row| {
            let state = row["state"].as_str().unwrap_or("").to_owned();
            InjectRow {
                checked: state == "fresh" || state == "stale",
                id: row["id"].as_str().unwrap_or("").to_owned(),
                name: row["name"].as_str().unwrap_or("").to_owned(),
                seen: row["likely_installed"].as_bool().unwrap_or(false),
                state,
            }
        })
        .collect())
}

fn set_inject_rows(live: &mut Live, rows: Vec<InjectRow>) {
    live.fresh = rows.iter().filter(|row| row.state == "fresh").count();
    live.stale = rows.iter().filter(|row| row.state == "stale").count();
    live.missing = rows.len() - live.fresh - live.stale;
    live.inject = rows;
}

fn sync_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('0') => app.page = Page::Home,
        KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down => app.cursor = (app.cursor + 1).min(2),
        KeyCode::Left | KeyCode::Right if app.cursor == 1 => {
            app.autosync_draft = !app.autosync_draft
        }
        KeyCode::Enter if app.cursor == 2 => app.page = Page::Home,
        KeyCode::Enter if app.cursor == 0 => ask(
            app,
            t(
                "\u{73b0}\u{5728}\u{540c}\u{6b65}\u{4e00}\u{6b21}\u{ff1f}",
                "Sync once now?",
            ),
            ConfirmKind::Sync,
        ),
        KeyCode::Enter if app.cursor == 1 => {
            let on = app.autosync_draft;
            ask(
                app,
                t(
                    if on {
                        "\u{6253}\u{5f00}\u{81ea}\u{52a8}\u{540c}\u{6b65}\u{ff1f}"
                    } else {
                        "\u{5173}\u{95ed}\u{81ea}\u{52a8}\u{540c}\u{6b65}\u{ff1f}"
                    },
                    if on {
                        "Turn auto-sync on?"
                    } else {
                        "Turn auto-sync off?"
                    },
                ),
                ConfirmKind::Autosync(on),
            );
        }
        _ => {}
    }
    false
}
