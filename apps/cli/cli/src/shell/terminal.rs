fn workspace_label(mode: &str) -> String {
    match mode {
        "readonly" => t("\u{53ea}\u{8bfb}\u{670d}\u{52a1}", "Read-only service"),
        "off" => t("\u{7981}\u{7528}\u{670d}\u{52a1}", "Disabled service"),
        _ => t("\u{6b63}\u{5e38}\u{670d}\u{52a1}", "Normal service"),
    }
}

fn overlay_lines(app: &App) -> Option<Vec<Line<'static>>> {
    match &app.overlay {
        Overlay::None => None,
        Overlay::ModelWeights(form) => Some(form.lines(&app.live.model_settings)),
        Overlay::RecallFusion(form) => Some(form.lines()),
        Overlay::HostModel => Some(vec![
            line(t("\u{7b49}\u{5f85}\u{5bbf}\u{4e3b}\u{5b50}\u{8fdb}\u{7a0b}\u{8fd4}\u{56de}\u{7ed3}\u{679c}", "Waiting for the host subprocess result")),
            line(t("\u{5b9e}\u{65f6}\u{8fdb}\u{5ea6}\u{4e0d}\u{53ef}\u{7528}\u{ff1b}\u{6b64}\u{5904}\u{4e0d}\u{652f}\u{6301}\u{53d6}\u{6d88}", "Live progress unavailable; cancellation is not supported here")),
        ]),
        Overlay::SavingWeights => Some(vec![
            line(match app.live.model_progress["phase"].as_str() {
                Some("verify") => t("\u{6b63}\u{5728}\u{6821}\u{9a8c}\u{56fa}\u{5b9a}\u{5927}\u{5c0f}\u{3001}SHA-256 \u{548c}\u{6a21}\u{578b}\u{8eab}\u{4efd}", "Validating pinned size, SHA-256 and model identity"),
                Some("load") => t("\u{6b63}\u{5728}\u{5b9e}\u{9645}\u{52a0}\u{8f7d}\u{548c}\u{8bd5}\u{63a8}\u{7406}\u{ff1b}\u{6210}\u{529f}\u{540e}\u{4fdd}\u{5b58}", "Loading and running trial inference; saving after success"),
                _ => t("\u{7b49}\u{5f85}\u{52a0}\u{8f7d}\u{4e8b}\u{52a1}\u{5b8c}\u{6210}\u{ff1b}\u{4e0d}\u{4f1a}\u{4e0b}\u{8f7d}", "Waiting for model load transaction; no downloads"),
            }),
            line(t("\u{8bf7}\u{7b49}\u{5f85}\u{ff1b}\u{52a0}\u{8f7d}\u{671f}\u{95f4}\u{4e0d}\u{63a5}\u{53d7}\u{53d6}\u{6d88}", "Please wait; loading cannot be cancelled")),
        ]),
        Overlay::ModelMirror(input) => {
            let mut lines = vec![line(t("BGE-M3 \u{4e0b}\u{8f7d}\u{6e90}", "BGE-M3 download source"))];
            lines.extend(respire_commands::model_download::MIRRORS.iter().map(|mirror| choice(input.text == *mirror, (*mirror).to_owned())));
            lines.push(input.line(70, true, false));
            lines.push(line(t("\u{4e0a}\u{4e0b}\u{952e}\u{9009}\u{62e9}\u{ff0c}\u{4e5f}\u{53ef}\u{8f93}\u{5165}\u{81ea}\u{5b9a}\u{4e49}\u{5730}\u{5740}\u{ff1b}\u{56de}\u{8f66}\u{4fdd}\u{5b58}\u{ff0c}Esc \u{8fd4}\u{56de}\u{3002}", "Up/down selects; custom URL supported; Enter saves, Esc returns.")));
            lines.push(line(t("\u{8fd0}\u{884c}\u{4e2d}\u{7684}\u{4e0b}\u{8f7d}\u{4e0d}\u{4f1a}\u{76f4}\u{63a5}\u{6362}\u{6e90}\u{ff1b}\u{4fdd}\u{5b58}\u{540e}\u{53ef}\u{53d6}\u{6d88}\u{5e76}\u{91cd}\u{65b0}\u{4e0b}\u{8f7d}\u{3002}", "An active download keeps its source; cancel and restart after saving.")));
            Some(lines)
        }
        Overlay::RecallApi(_) | Overlay::RecallTest(_) => Some(Vec::new()),
        Overlay::ModelTask(task) => Some(task.lines()),
        Overlay::Edit { buf, cursor } => {
            let mut shown = String::new();
            for (index, ch) in buf.chars().enumerate() {
                if index == *cursor {
                    shown.push('|');
                }
                shown.push(ch);
            }
            if *cursor >= buf.chars().count() {
                shown.push('|');
            }
            Some(vec![
                line(String::new()),
                line(t("\u{670d}\u{52a1}\u{5668}\u{5730}\u{5740}", "Server address")),
                line(shown),
            ])
        }
        Overlay::Confirm { text, cursor, .. } => Some(vec![
            line(String::new()),
            line(text.clone()),
            choice(*cursor == 0, t("1  \u{786e}\u{8ba4}", "1  Confirm")),
            choice(*cursor == 1, t("2  \u{53d6}\u{6d88}", "2  Cancel")),
        ]),
    }
}

fn line(text: String) -> Line<'static> {
    Line::from(text)
}

fn choice(on: bool, text: String) -> Line<'static> {
    let prefix = if on { "> " } else { "  " };
    let style = if on {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(Span::styled(format!("{prefix}{text}"), style))
}

fn ok_line(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(Color::Green)))
}

fn warn_line(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(Color::Yellow)))
}

fn fail_line(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(Color::Red)))
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let result = (|| -> Result<_> {
        execute!(stdout(), EnterAlternateScreen, event::EnableBracketedPaste)?;
        Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
    })();
    match result {
        Ok(terminal) => Ok(terminal),
        Err(mut error) => {
            let restore = execute!(
                stdout(),
                event::DisableBracketedPaste,
                LeaveAlternateScreen,
                crossterm::cursor::Show
            );
            let raw_mode = disable_raw_mode();
            if let Err(cleanup) = restore {
                error = error.context(format!("terminal restoration failed: {cleanup}"));
            }
            if let Err(cleanup) = raw_mode {
                error = error.context(format!("disabling raw mode failed: {cleanup}"));
            }
            Err(error)
        }
    }
}

fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    execute!(
        terminal.backend_mut(),
        event::DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    Ok(terminal.show_cursor()?)
}
