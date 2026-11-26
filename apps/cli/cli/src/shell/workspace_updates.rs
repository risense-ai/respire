fn set_workspace(mode: &str) -> Result<String, String> {
    host_command(&["agent-config", "--set", &format!("workspace_mode={mode}")])?;
    Ok(t(
        "\u{5de5}\u{4f5c}\u{6a21}\u{5f0f}\u{5df2}\u{4fdd}\u{5b58}",
        "Service mode saved",
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Manager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Direct,
}

fn manager_name(manager: Manager) -> &'static str {
    match manager {
        Manager::Npm => "npm",
        Manager::Pnpm => "pnpm",
        Manager::Yarn => "yarn",
        Manager::Bun => "bun",
        Manager::Direct => "direct",
    }
}

fn tool_on_path(name: &str) -> bool {
    let path = match respire::env::var_os("PATH") {
        Some(path) => path,
        None => return false,
    };
    let file = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&path).any(|dir| dir.join(&file).is_file() || dir.join(name).is_file())
}

fn tools_on_path() -> Vec<Manager> {
    let mut tools = Vec::new();
    let agent = respire::env::var("npm_config_user_agent")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let prefer = if agent.starts_with("pnpm") || exe.contains(".pnpm") || exe.contains("pnpm") {
        Some(Manager::Pnpm)
    } else if agent.starts_with("yarn") || exe.contains("yarn") {
        Some(Manager::Yarn)
    } else if agent.starts_with("bun") || exe.contains("\\bun\\") || exe.contains("/bun/") {
        Some(Manager::Bun)
    } else if agent.starts_with("npm")
        || exe.contains("node_modules")
        || exe.contains("\\npm\\")
        || exe.contains("/npm/")
    {
        Some(Manager::Npm)
    } else {
        None
    };
    for manager in [Manager::Npm, Manager::Pnpm, Manager::Yarn, Manager::Bun] {
        if tool_on_path(manager_name(manager)) {
            tools.push(manager);
        }
    }
    if let Some(prefer) = prefer {
        if let Some(index) = tools.iter().position(|item| *item == prefer) {
            tools.swap(0, index);
        }
    }
    if tools.is_empty() {
        tools.push(Manager::Direct);
    }
    tools
}

fn channel_spec() -> String {
    if env!("CARGO_PKG_VERSION").contains("-dev") {
        "rsrs-cli@dev".to_owned()
    } else {
        "rsrs-cli@latest".to_owned()
    }
}

fn check_versions(app: &mut App) {
    app.set_notice(t(
        "\u{6b63}\u{5728}\u{68c0}\u{67e5} npm \u{4e0a}\u{7684}\u{7248}\u{672c}",
        "Checking versions on npm",
    ));
    let slot = Arc::clone(&app.outcome);
    let shared = Arc::clone(&app.shared);
    std::thread::spawn(move || {
        let current = env!("CARGO_PKG_VERSION");
        let message = match respire::update_check::fetch_channel_choices() {
            Some((latest, dev)) => {
                let dev_channel = current.contains("-dev");
                let spec = if dev_channel { &dev } else { &latest }.install_spec(dev_channel);
                let (latest, dev) = (latest.version, dev.version);
                let remote = if current.contains("-dev") {
                    dev.as_str()
                } else {
                    latest.as_str()
                };
                let newer =
                    !remote.is_empty() && respire::update_check::is_newer(remote, current);
                let line = format!(
                    "{} {current}    latest {latest}    dev {dev}{}",
                    if zh() { "\u{5f53}\u{524d}" } else { "current" },
                    if newer {
                        if zh() {
                            format!("    \u{6709}\u{65b0}\u{7248}\u{672c} {remote}")
                        } else {
                            format!("    update available {remote}")
                        }
                    } else if zh() {
                        "    \u{5df2}\u{662f}\u{8fd9}\u{6761}\u{901a}\u{9053}\u{4e0a}\u{7684}\u{6700}\u{65b0}\u{7248}".to_owned()
                    } else {
                        "    already the newest on this channel".to_owned()
                    }
                );
                if let Ok(mut guard) = shared.lock() {
                    guard.ver_line = line.clone();
                    guard.update_spec = spec;
                }
                line
            }
            None => t(
                "\u{6ca1}\u{6709}\u{8bfb}\u{5230} npm \u{7248}\u{672c}\u{ff0c}\u{68c0}\u{67e5}\u{7f51}\u{7edc}\u{540e}\u{518d}\u{8bd5}",
                "Could not read npm versions. Check the network and try again",
            ),
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(message);
        }
    });
}

fn run_package_update(tool: &str, spec: &str) -> Result<String, String> {
    crate::runtime_policy::require_host("package upgrade").map_err(|error| error.to_string())?;
    let line = match tool {
        "npm" => format!("npm i -g {spec}"),
        "pnpm" => format!("pnpm add -g {spec}"),
        "yarn" => format!("yarn global add {spec}"),
        "bun" => format!("bun add -g {spec}"),
        _ => {
            return Err(t(
                "\u{6ca1}\u{6709}\u{53ef}\u{7528}\u{7684}\u{5305}\u{7ba1}\u{7406}\u{5668}",
                "No package manager is available",
            ))
        }
    };
    let mut cmd = if cfg!(windows) {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", &line]);
        cmd
    } else {
        let mut parts = line.split_whitespace();
        let program = parts.next().unwrap_or(tool);
        let mut cmd = std::process::Command::new(program);
        cmd.args(parts);
        cmd
    };
    let output = cmd.output().map_err(|error| format!("{error}"))?;
    if output.status.success() {
        return Ok(t(
            "\u{66f4}\u{65b0}\u{547d}\u{4ee4}\u{5df2}\u{5b8c}\u{6210}\u{3002}\u{91cd}\u{65b0}\u{6253}\u{5f00}\u{7a0b}\u{5e8f}\u{540e}\u{624d}\u{4f1a}\u{6362}\u{6210}\u{65b0}\u{7248}\u{672c}\u{3002}\u{5f53}\u{524d}\u{8fd9}\u{4e2a}\u{7a97}\u{53e3}\u{5982}\u{679c}\u{662f}\u{76f4}\u{63a5}\u{8fd0}\u{884c}\u{7684}\u{ff0c}\u{4e0d}\u{4f1a}\u{88ab}\u{6362}\u{6389}\u{3002}",
            "Update command finished. Reopen the program to use the new version. This window is unchanged if it was started as a direct binary.",
        ));
    }
    let err = String::from_utf8_lossy(&output.stderr);
    let out = String::from_utf8_lossy(&output.stdout);
    let text = if err.trim().is_empty() { out } else { err };
    Err(text.chars().take(400).collect())
}

fn sync_notice(value: &Value) -> String {
    let summary = &value["summary"];
    t(
        &format!(
            "\u{540c}\u{6b65}\u{7ed3}\u{675f}  \u{62c9} {}  \u{63a8} {}  \u{51b2}\u{7a81} {}",
            summary["pulled"].as_u64().unwrap_or(0),
            summary["pushed"].as_u64().unwrap_or(0),
            summary["conflicts"].as_i64().unwrap_or(0)
        ),
        &format!(
            "Sync finished  pulled {}  pushed {}  conflicts {}",
            summary["pulled"].as_u64().unwrap_or(0),
            summary["pushed"].as_u64().unwrap_or(0),
            summary["conflicts"].as_i64().unwrap_or(0)
        ),
    )
}

fn edit_key(app: &mut App, code: KeyCode) -> bool {
    let Overlay::Edit { buf, cursor } = &mut app.overlay else {
        return false;
    };
    match code {
        KeyCode::Char(' ') => app.overlay = Overlay::None,
        KeyCode::Enter => {
            let value = buf.trim().to_owned();
            app.overlay = Overlay::None;
            if value.is_empty() || !(value.starts_with("http://") || value.starts_with("https://"))
            {
                app.set_notice(t(
                    "\u{5730}\u{5740}\u{5fc5}\u{987b}\u{4ee5} http:// \u{6216} https:// \u{5f00}\u{5934}",
                    "Address must start with http:// or https://",
                ));
                return false;
            }
            let slot = Arc::clone(&app.outcome);
            app.set_notice(t(
                "\u{6b63}\u{5728}\u{4fdd}\u{5b58}\u{670d}\u{52a1}\u{5668}\u{5730}\u{5740}",
                "Saving the server address",
            ));
            std::thread::spawn(move || {
                let message = match host_command(&["config", "--addr", &value]) {
                    Ok(_) => t(
                        "\u{670d}\u{52a1}\u{5668}\u{5730}\u{5740}\u{5df2}\u{4fdd}\u{5b58}",
                        "Server address saved",
                    ),
                    Err(error) => error,
                };
                if let Ok(mut guard) = slot.lock() {
                    *guard = Some(message);
                }
            });
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                let remove_at = *cursor - 1;
                let mut next = String::new();
                for (index, ch) in buf.chars().enumerate() {
                    if index != remove_at {
                        next.push(ch);
                    }
                }
                *buf = next;
                *cursor -= 1;
            }
        }
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(buf.chars().count()),
        KeyCode::Char(ch) => {
            let mut next = String::new();
            let mut inserted = false;
            for (index, current) in buf.chars().enumerate() {
                if index == *cursor {
                    next.push(ch);
                    inserted = true;
                }
                next.push(current);
            }
            if !inserted {
                next.push(ch);
            }
            *buf = next;
            *cursor += 1;
        }
        _ => {}
    }
    false
}
