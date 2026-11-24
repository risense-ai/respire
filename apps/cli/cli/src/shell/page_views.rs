fn accounts_body(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![line(t(
        "\u{9009}\u{62e9}\u{8d26}\u{6237}\u{540e}\u{56de}\u{8f66}\u{5207}\u{6362}\u{ff1b}\u{65b0}\u{8d26}\u{6237}\u{4f7f}\u{7528}\u{767b}\u{5f55}\u{5165}\u{53e3}\u{3002}",
        "Enter switches account; use Sign in for a new account.",
    ))];
    if app.live.accounts.is_empty() {
        lines.push(line(t(
            "\u{6ca1}\u{6709}\u{8bfb}\u{5230}\u{8d26}\u{6237}",
            "No accounts read",
        )));
    }
    for (index, row) in app.live.accounts.iter().enumerate() {
        let mark = if row.current {
            t("\u{5f53}\u{524d}", "current")
        } else {
            t("    ", "       ")
        };
        let user = if row.user.is_empty() {
            t("\u{672a}\u{767b}\u{5f55}", "signed out")
        } else {
            row.user.clone()
        };
        lines.push(choice(
            index == app.cursor,
            format!("{mark}  {}  {user}", row.name),
        ));
    }
    lines.push(choice(
        app.cursor == app.live.accounts.len(),
        t(
            "\u{8fc1}\u{79fb}\u{65e7}\u{7248}\u{672c}",
            "Migrate old version",
        ),
    ));
    lines.push(choice(
        app.cursor == app.live.accounts.len() + 1,
        t(
            "\u{767b}\u{5f55}\u{ff08}OAuth / \u{5bc6}\u{7801}\u{4e0e} TOTP\u{ff09}",
            "Sign in (OAuth / password and TOTP)",
        ),
    ));
    lines.push(choice(
        app.cursor == app.live.accounts.len() + 2,
        t("0  \u{8fd4}\u{56de}", "0  Back"),
    ));
    lines
}

fn migration_body(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![line(t("\u{9009}\u{62e9}\u{6765}\u{6e90}\u{76ee}\u{5f55}\u{548c}\u{65e7}\u{8d26}\u{6237}\u{ff0c}\u{56de}\u{8f66}\u{786e}\u{8ba4}\u{ff1b}\u{4e0d}\u{4f1a}\u{8986}\u{76d6}\u{5df2}\u{6709}\u{5e93}\u{3002}",
        "Choose the source directory and legacy account. Existing libraries are preserved."))];
    for (index, row) in app.migration_profiles.iter().enumerate() {
        let state = if row["migrated_to"].is_null() {
            ""
        } else {
            " [migrated]"
        };
        lines.push(choice(
            index == app.cursor,
            format!(
                "{} | {} \u{2192} {}{state}",
                row["source"].as_str().unwrap_or(""),
                row["user"].as_str().unwrap_or(""),
                row["account"].as_str().unwrap_or("")
            ),
        ));
    }
    if app.migration_profiles.is_empty() {
        lines.push(line(t(
            "\u{6ca1}\u{6709}\u{53ef}\u{8fc1}\u{79fb}\u{7684}\u{65e7}\u{5e93}",
            "No legacy libraries found",
        )));
    }
    lines.push(line(t("0  \u{8fd4}\u{56de}", "0  Back")));
    lines
}

fn server_body(app: &App) -> Vec<Line<'static>> {
    vec![
        choice(
            app.cursor == 0,
            format!(
                "{}: {}",
                t("\u{4fee}\u{6539}\u{5730}\u{5740}", "Edit address"),
                app.live.addr
            ),
        ),
        choice(app.cursor == 1, t("0  \u{8fd4}\u{56de}", "0  Back")),
    ]
}

fn draw_inject(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(t(
        "\u{6ce8}\u{5165}\u{4e0e}\u{63d0}\u{793a}\u{8bcd}",
        "Inject and prompts",
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(banner()), sections[0]);
    frame.render_widget(
        Paragraph::new(vec![
            choice(
                app.cursor == 0,
                t("\u{66f4}\u{65b0}\u{6240}\u{6709}\u{5df2}\u{5b89}\u{88c5}\u{63d0}\u{793a}\u{8bcd}", "Update all installed prompts"),
            ),
            choice(
                app.cursor == 1,
                t("\u{5378}\u{8f7d}\u{6240}\u{6709}\u{5df2}\u{5b89}\u{88c5}\u{63d0}\u{793a}\u{8bcd}", "Uninstall all installed prompts"),
            ),
        ]),
        sections[1],
    );
    let rows = app.live.inject.iter().map(|row| {
        let color = match row.state.as_str() {
            "fresh" => Color::Green,
            "stale" => Color::Yellow,
            _ => Color::DarkGray,
        };
        Row::new(vec![
            Cell::from(if row.checked { "[x]" } else { "[ ]" }),
            Cell::from(row.name.clone()),
            Cell::from(state_label(&row.state)).style(Style::default().fg(color)),
            Cell::from(if row.seen {
                t("\u{5df2}\u{68c0}\u{6d4b}\u{5230}", "detected")
            } else {
                t("\u{672a}\u{68c0}\u{6d4b}\u{5230}", "not detected")
            }),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new([
            t("\u{5b89}\u{88c5}", "On"),
            t("\u{76ee}\u{6807}", "Target"),
            t("\u{63d0}\u{793a}\u{8bcd}\u{72b6}\u{6001}", "Prompt status"),
            t("\u{5e94}\u{7528}\u{68c0}\u{6d4b}", "App detected"),
        ])
        .style(Style::default().bold()),
    )
    .block(Block::bordered())
    .column_spacing(2)
    .row_highlight_style(Style::default().bg(Color::DarkGray).bold())
    .highlight_symbol("> ");
    let selected = app
        .cursor
        .checked_sub(2)
        .filter(|index| *index < app.live.inject.len());
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, sections[2], &mut state);
    frame.render_widget(
        Paragraph::new(choice(
            app.cursor == app.live.inject.len() + 2,
            t("0  \u{8fd4}\u{56de}", "0  Back"),
        )),
        sections[3],
    );
}

fn state_label(state: &str) -> String {
    match state {
        "fresh" => t(
            "\u{63d0}\u{793a}\u{8bcd}\u{5df2}\u{66f4}\u{65b0}",
            "prompt fresh",
        ),
        "stale" => t("\u{63d0}\u{793a}\u{8bcd}\u{8fc7}\u{671f}", "prompt stale"),
        "none" | "absent" => t("\u{672a}\u{6ce8}\u{5165}", "not injected"),
        other => other.to_owned(),
    }
}

fn sync_body(app: &App) -> Vec<Line<'static>> {
    let phase = match app.live.phase.as_str() {
        "running" => t("\u{6b63}\u{5728}\u{540c}\u{6b65}", "syncing"),
        "ok" => t("\u{4e0a}\u{6b21}\u{6210}\u{529f}", "last ok"),
        "err" => t("\u{4e0a}\u{6b21}\u{5931}\u{8d25}", "last failed"),
        _ => t("\u{5c1a}\u{672a}\u{540c}\u{6b65}", "not synced yet"),
    };
    vec![
        line(format!(
            "{phase}    {}: {}  {}: {}  {}: {}",
            t("\u{62c9}", "pulled"),
            app.live.pulled,
            t("\u{63a8}", "pushed"),
            app.live.pushed,
            t("\u{51b2}\u{7a81}", "conflicts"),
            app.live.conflicts
        )),
        line(if app.live.error.is_empty() {
            t(
                "\u{51b2}\u{7a81}\u{53ea}\u{5728}\u{8fd9}\u{91cc}\u{67e5}\u{770b}\u{ff0c}\u{5408}\u{5e76}\u{4ecd}\u{7528}\u{547d}\u{4ee4}\u{3002}",
                "Conflicts are listed here. Merging stays a command.",
            )
        } else {
            app.live.error.clone()
        }),
        choice(app.cursor == 0, t("\u{624b}\u{52a8}\u{540c}\u{6b65}\u{4e00}\u{6b21}", "Sync once")),
        choice(
            app.cursor == 1,
            format!(
                "{}  {}",
                t("\u{81ea}\u{52a8}\u{540c}\u{6b65}", "Auto-sync"),
                if app.autosync_draft {
                    t("\u{5f00}", "on")
                } else {
                    t("\u{5173}", "off")
                }
            ),
        ),
        choice(app.cursor == 2, t("0  \u{8fd4}\u{56de}", "0  Back")),
        line(t(
            "\u{5de6}\u{53f3}\u{952e}\u{5207}\u{6362}\u{81ea}\u{52a8}\u{540c}\u{6b65}\u{ff0c}\u{56de}\u{8f66}\u{4fdd}\u{5b58}\u{3002}",
            "Left and right change auto-sync. Enter saves.",
        )),
    ]
}
