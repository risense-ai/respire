fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let foot_height = if app.live.notice.is_empty() { 3 } else { 4 };
    let chunks =
        Layout::vertical([Constraint::Min(8), Constraint::Length(foot_height)]).split(area);
    let mut body = if !app.live.connected {
        let mut lines = vec![line(t(
            "\u{6b63}\u{5728}\u{8fde}\u{63a5} runtime / \u{521d}\u{59cb}\u{5316}\u{4e2d}\u{2026}",
            "Connecting to runtime / initializing...",
        ))];
        if !app.live.error.is_empty() {
            lines.push(fail_line(app.live.error.clone()));
        }
        lines.push(line(t(
            "\u{63a8}\u{7406}\u{5361}\u{4f4f}\u{53ef}\u{5728}\u{7ec8}\u{7aef}\u{8fd0}\u{884c}\u{ff1a}rsrs model reset-cpu",
            "If inference is stuck: rsrs model reset-cpu",
        )));
        lines
    } else {
        match app.page {
            Page::Home => home_body(app, chunks[0].width.saturating_sub(2) as usize),
            Page::Accounts => accounts_body(app),
            Page::Migration => migration_body(app),
            Page::Server => server_body(app),
            Page::Inject => Vec::new(),
            Page::Sync => sync_body(app),
            Page::Model => model_body(app),
            Page::Workspace => workspace_body(app),
            Page::Lang => lang_body(app),
            Page::Version => version_body(app),
            Page::Web => web_body(app),
        }
    };
    if matches!(app.overlay, Overlay::None) {
        keep_selection_visible(&mut body, chunks[0]);
    }
    let mut lines = vec![banner()];
    if let Some(mut extra) = overlay_lines(app) {
        if matches!(
            app.overlay,
            Overlay::ModelWeights(_) | Overlay::RecallFusion(_)
        ) {
            keep_selection_visible(&mut extra, chunks[0]);
        }
        // Confirmation and edit controls must not be clipped below a long page body.
        lines.extend(extra);
    } else {
        lines.extend(body);
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(crate::app_version::line()),
            )
            .wrap(Wrap { trim: false }),
        chunks[0],
    );
    match &app.overlay {
        Overlay::RecallApi(form) => form.draw(frame, chunks[0]),
        Overlay::RecallTest(form) => form.draw(frame, chunks[0]),
        _ => {}
    }
    if app.live.connected
        && matches!(app.page, Page::Inject)
        && matches!(app.overlay, Overlay::None)
    {
        draw_inject(frame, app, chunks[0]);
    }
    let foot = if matches!(app.overlay, Overlay::HostModel) {
        t("\u{7b49}\u{5f85}\u{5bbf}\u{4e3b}\u{7ed3}\u{679c}\u{ff1b}\u{4e0d}\u{652f}\u{6301}\u{53d6}\u{6d88}", "Waiting for host result; cancellation unavailable")
    } else if matches!(app.overlay, Overlay::ModelTask(_)) {
        t(
            "Esc / C \u{53d6}\u{6d88}\u{5f53}\u{524d}\u{6a21}\u{578b}\u{4efb}\u{52a1}",
            "Esc / C cancels the model task",
        )
    } else if matches!(
        app.overlay,
        Overlay::RecallApi(_) | Overlay::RecallFusion(_)
    ) {
        t(
            "Tab \u{5207}\u{5b57}\u{6bb5}    Ctrl+A \u{5168}\u{9009}    Ctrl+S \u{4fdd}\u{5b58}    Esc \u{653e}\u{5f03}",
            "Tab changes field    Ctrl+A selects all    Ctrl+S saves    Esc discards",
        )
    } else if matches!(app.overlay, Overlay::RecallTest(_)) {
        t(
            "\u{56de}\u{8f66}\u{6d4b}\u{8bd5}    Ctrl+A \u{5168}\u{9009}    PgUp/PgDn \u{6eda}\u{52a8}    Esc \u{8fd4}\u{56de}",
            "Enter tests    Ctrl+A selects all    PgUp/PgDn scroll    Esc returns",
        )
    } else if matches!(app.overlay, Overlay::Edit { .. }) {
        t(
            "\u{56de}\u{8f66}\u{4fdd}\u{5b58}    \u{7a7a}\u{683c}\u{6216} Esc \u{653e}\u{5f03}    \u{9000}\u{683c}\u{5220}\u{9664}",
            "Enter saves    Space or Esc cancels    Backspace deletes",
        )
    } else if zh() {
        "\u{65b9}\u{5411}\u{952e}\u{79fb}\u{52a8}    \u{6570}\u{5b57}\u{8fdb}\u{5165}    \u{56de}\u{8f66}\u{786e}\u{8ba4}    \u{7a7a}\u{683c}\u{52fe}\u{9009}    0 \u{6216} Esc \u{8fd4}\u{56de}    7 \u{5207}\u{6362}\u{8bed}\u{8a00}".to_owned()
    } else {
        "Arrows move    digits open    Enter confirms    Space checks    0 or Esc back    7 switches language".to_owned()
    };
    let note = if app.live.notice.is_empty() {
        foot
    } else {
        format!("{foot}\n{}", app.live.notice)
    };
    let foot_lines: Vec<Line<'static>> = note.lines().map(|text| line(text.to_owned())).collect();
    frame.render_widget(
        Paragraph::new(foot_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(t("\u{6309}\u{952e}", "Keys")),
        ),
        chunks[1],
    );
}

fn keep_selection_visible(body: &mut Vec<Line<'static>>, area: Rect) {
    let height = area.height.saturating_sub(3) as usize;
    let width = usize::from(area.width.saturating_sub(2)).max(1);
    let rows = |line: &Line<'_>| line.width().max(1).div_ceil(width);
    let selected = body.iter().position(|line| {
        line.spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::REVERSED))
    });
    if let Some(selected) = selected {
        if body[..selected].iter().map(rows).sum::<usize>() >= height.saturating_sub(2) {
            let mut start = selected;
            let mut visible = rows(&body[selected]);
            while start > 0 && visible + rows(&body[start - 1]) <= height / 2 {
                start -= 1;
                visible += rows(&body[start]);
            }
            body.drain(..start);
        }
    }
}

fn home_body(app: &App, inner: usize) -> Vec<Line<'static>> {
    let live = &app.live;
    let phase = match live.phase.as_str() {
        "running" => t("\u{6b63}\u{5728}\u{540c}\u{6b65}", "syncing"),
        "ok" => t("\u{4e0a}\u{6b21}\u{6210}\u{529f}", "last ok"),
        "err" => t("\u{4e0a}\u{6b21}\u{5931}\u{8d25}", "last failed"),
        _ => t("\u{7a7a}\u{95f2}", "idle"),
    };
    let user = if live.user.is_empty() {
        t("\u{672a}\u{767b}\u{5f55}", "signed out")
    } else {
        live.user.clone()
    };
    let addr = if live.addr.is_empty() {
        crate::client_config::DEFAULT_SERVER_ADDR.to_owned()
    } else {
        live.addr.clone()
    };
    let stamp = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut lines = smi_table(
        &[
            [
                format!("CLI {}", crate::app_version::embedded()),
                runtime_version_label(live),
                stamp.clone(),
            ],
            [
                format!("rsrs  {stamp}"),
                format!("pid {}", live.pid),
                if live.connected {
                    t("runtime \u{5728}\u{8fd0}\u{884c}", "runtime up")
                } else {
                    t("runtime \u{672a}\u{8fde}\u{901a}", "runtime down")
                },
            ],
            [
                live.url.clone(),
                format!("{}  {user}", t("\u{6863}\u{6848}", "Account")),
                format!(
                    "{}  {}",
                    t("\u{5de5}\u{4f5c}\u{6a21}\u{5f0f}", "Service mode"),
                    workspace_label(&live.workspace)
                ),
            ],
            [
                format!("{}  {addr}", t("\u{670d}\u{52a1}\u{5668}", "Server")),
                format!("{}  {phase}", t("\u{540c}\u{6b65}", "Sync")),
                format!(
                    "{} {} / {} {}",
                    t("\u{672c}\u{5730}", "Local"),
                    live.local_alive,
                    t("\u{4e91}\u{7aef}", "Remote"),
                    live.remote_alive
                ),
            ],
            [
                format!(
                    "{} {}  {} {}",
                    t("\u{51b2}\u{7a81}", "Conflicts"),
                    live.conflicts,
                    t("\u{81ea}\u{52a8}", "Auto"),
                    if live.autosync {
                        t("\u{5f00}", "on")
                    } else {
                        t("\u{5173}", "off")
                    }
                ),
                format!(
                    "{} {}",
                    t("\u{5f85}\u{63a8}\u{9001}", "Pending"),
                    live.pending
                ),
                format!(
                    "{} {}  {} {}",
                    t("\u{4e0a}\u{6b21}\u{62c9}", "Pulled"),
                    live.pulled,
                    t("\u{63a8}", "pushed"),
                    live.pushed
                ),
            ],
            [
                format!(
                    "{} {}  {} {}  {} {}",
                    t("\u{6ce8}\u{5165}\u{65b0}", "fresh"),
                    live.fresh,
                    t("\u{8fc7}\u{671f}", "stale"),
                    live.stale,
                    t("\u{672a}\u{6ce8}\u{5165}", "missing"),
                    live.missing
                ),
                String::new(),
                String::new(),
            ],
        ],
        inner,
    );
    if !live.error.is_empty() {
        lines.push(fail_line(live.error.clone()));
    }
    let fails: Vec<_> = live
        .doctor
        .iter()
        .filter(|(_, status, _)| status == "fail" || status == "warn")
        .cloned()
        .collect();
    if fails.is_empty() {
        lines.push(ok_line(format!(
            "doctor  {} {}",
            live.doctor.len(),
            t("\u{9879}\u{5df2}\u{8bfb}\u{53d6}", "checks read")
        )));
    } else {
        for (name, status, value) in fails.iter().take(6) {
            let shown = if status == "fail" {
                fail_line(format!("{name}  {value}"))
            } else {
                warn_line(format!("{name}  {value}"))
            };
            lines.push(shown);
        }
    }
    lines.push(line(String::new()));
    let labels = [
        t("1  \u{8d26}\u{6237}", "1  Accounts"),
        t("2  \u{670d}\u{52a1}\u{5668}", "2  Server"),
        t(
            "3  \u{6ce8}\u{5165}\u{4e0e}\u{63d0}\u{793a}\u{8bcd}",
            "3  Inject and prompts",
        ),
        t("4  \u{540c}\u{6b65}", "4  Sync"),
        t("5  \u{6a21}\u{578b}", "5  Models"),
        t("6  \u{5de5}\u{4f5c}\u{6a21}\u{5f0f}", "6  Service mode"),
        t("7  \u{8bed}\u{8a00}", "7  Language"),
        t("8  \u{7248}\u{672c}", "8  Version"),
        t("9  \u{6253}\u{5f00} Web", "9  Open Web"),
    ];
    for (index, label) in labels.iter().enumerate() {
        lines.push(choice(index == app.cursor, label.clone()));
    }
    lines.push(choice(app.cursor == 9, t("0  \u{9000}\u{51fa}", "0  Quit")));
    lines
}

fn table_metrics(inner: usize) -> (usize, usize, usize) {
    let preferred = 28usize;
    let cols = if inner >= preferred * 3 + 4 {
        3
    } else if inner >= preferred * 2 + 3 {
        2
    } else {
        1
    };
    let gutters = cols + 1;
    let cell = if cols == 3 {
        preferred
    } else {
        inner.saturating_sub(gutters).clamp(12, preferred)
    };
    let width = cols * cell + gutters;
    (cols, cell, width)
}

fn clip_cell(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

fn smi_table(rows: &[[String; 3]], inner: usize) -> Vec<Line<'static>> {
    let (cols, cell, _) = table_metrics(inner);
    let mut flat = Vec::new();
    for row in rows {
        for item in row {
            flat.push(item.clone());
        }
    }
    let mut out = Vec::new();
    let rule = |fill: char| {
        let mut text = String::from("+");
        for _ in 0..cols {
            text.push_str(&fill.to_string().repeat(cell));
            text.push('+');
        }
        line(text)
    };
    out.push(rule('-'));
    for (index, chunk) in flat.chunks(cols).enumerate() {
        let mut text = String::from("|");
        for item in chunk {
            text.push_str(&clip_cell(item, cell));
            text.push('|');
        }
        for _ in 0..(cols - chunk.len()) {
            text.push_str(&clip_cell("", cell));
            text.push('|');
        }
        out.push(line(text));
        out.push(rule(if index == 0 { '=' } else { '-' }));
    }
    out
}

fn web_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('0') => app.page = Page::Home,
        KeyCode::Up => app.cursor = 0,
        KeyCode::Down => app.cursor = 1,
        KeyCode::Enter if app.cursor == 1 => app.page = Page::Home,
        KeyCode::Enter => {
            let url = crate::web::DASHBOARD_URL;
            if let Err(error) = crate::web::open_browser(url) {
                app.set_notice(error.to_string());
                return false;
            }
            app.set_notice(t(
                &format!("\u{5df2}\u{6253}\u{5f00} {url}"),
                &format!("Opened {url}"),
            ));
        }
        _ => {}
    }
    false
}

fn web_body(app: &App) -> Vec<Line<'static>> {
    let url = crate::web::DASHBOARD_URL;
    vec![
        line(format!("{}: {url}", t("\u{7528}\u{6237}\u{540e}\u{53f0}", "User dashboard"))),
        line(t(
            "\u{56de}\u{8f66}\u{7528}\u{7cfb}\u{7edf}\u{6d4f}\u{89c8}\u{5668}\u{6253}\u{5f00}\u{3002}",
            "Enter opens it in the system browser.",
        )),
        choice(app.cursor == 0, t("\u{6253}\u{5f00} Web", "Open Web")),
        choice(app.cursor == 1, t("0  \u{8fd4}\u{56de}", "0  Back")),
    ]
}

fn banner() -> Line<'static> {
    let zh_on = zh();
    let on = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let off = Style::default().fg(Color::DarkGray);
    Line::from(vec![
        Span::raw(t("\u{8bed}\u{8a00}  ", "Language  ")),
        Span::styled("\u{4e2d}\u{6587}", if zh_on { on } else { off }),
        Span::raw("  "),
        Span::styled("English", if zh_on { off } else { on }),
        Span::raw(t(
            "      7 \u{7acb}\u{5373}\u{5207}\u{6362}",
            "      7 switches now",
        )),
    ])
}
