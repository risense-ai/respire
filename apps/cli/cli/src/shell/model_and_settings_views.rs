fn model_body(app: &App) -> Vec<Line<'static>> {
    let mirror = match selected_model_mirror() {
        Ok(value) => value,
        Err(error) => return vec![fail_line(format!("{error:#}"))],
    };
    let config = match respire_client_host::settings::read_agent_config() {
        Ok(value) => value,
        Err(error) => return vec![fail_line(format!("{error:#}"))],
    };
    let bge = doctor_value(&app.live, "embedder");
    let engine = &app.live.engine;
    let mut lines = vec![
        line(format!("BGE-M3  {bge}")),
        choice(
            app.cursor == 0,
            t("\u{5b89}\u{88c5}\u{6216}\u{6821}\u{9a8c} BGE-M3 \u{91cf}\u{5316}\u{6a21}\u{578b}\u{ff08}543 MiB\u{ff09}", "Install or verify BGE-M3 quantized (543 MiB)"),
        ),
        choice(
            app.cursor == 1,
            t("\u{5378}\u{8f7d} BGE-M3\u{ff08}\u{5220}\u{9664}\u{6587}\u{4ef6}\u{ff09}", "Uninstall BGE-M3 (delete files)"),
        ),
        choice(app.cursor == 2, format!("{}: {}", t("\u{4e0b}\u{8f7d}\u{6e90}\u{ff08}\u{56de}\u{8f66}\u{9009}\u{62e9}\u{ff09}", "Download source (Enter to choose)"), mirror)),
        line(format!(
            "{}: {engine} / {}",
            t("\u{5f53}\u{524d}\u{63a8}\u{7406}\u{8bbe}\u{7f6e}", "Inference setting"), app.live.model_settings["model"].as_str().unwrap_or("unknown")
        )),
    ];
    for (index, engine) in model_engines().iter().enumerate() {
        let label = match *engine {
            "npu" if cfg!(target_os = "macos") => "Apple CoreML (ANE / CPU)".to_owned(),
            "npu" => "NPU (Windows ML)".to_owned(),
            "gpu" => "GPU".to_owned(),
            _ => t("CPU\u{ff08}\u{9ed8}\u{8ba4}\u{ff09}", "CPU (default)"),
        };
        lines.push(choice(app.cursor == index + 3, label));
    }
    let mut index = 3 + model_engines().len();
    if cfg!(windows) {
        lines.push(choice(
            app.cursor == index,
            t(
                "\u{5b89}\u{88c5} Windows NPU \u{63a8}\u{7406}\u{5f15}\u{64ce}",
                "Install Windows NPU providers",
            ),
        ));
        index += 1;
    }
    lines.push(choice(
        app.cursor == index,
        t(
            "\u{8fd0}\u{884c}\u{63a8}\u{7406}\u{9a8c}\u{8bc1}",
            "Check inference",
        ),
    ));
    let mode = config["recall_mode"]
        .as_str()
        .unwrap_or("fast")
        .to_owned();
    lines.push(line(format!(
        "{}: {mode}",
        t("\u{53ec}\u{56de}\u{6a21}\u{5f0f}", "Recall mode")
    )));
    lines.push(choice(
        app.cursor == index + 1,
        t("\u{5feb}\u{901f}\u{ff08}\u{672c}\u{5730}\u{53ec}\u{56de}\u{ff0c}\u{9ed8}\u{8ba4}\u{ff09}", "Fast (local retrieval, default)"),
    ));
    lines.push(choice(
        app.cursor == index + 2,
        t(
            "\u{9ad8}\u{8d28}\u{91cf}\u{ff08}\u{6a21}\u{578b}\u{7b5b}\u{9009}\u{6807}\u{9898}\u{ff0c}\u{9700}\u{914d}\u{7f6e} API\u{ff09}",
            "High quality (model selects titles; API required)",
        ),
    ));
    let index_state = app.live.index["state"].as_str().unwrap_or("unknown");
    lines.push(line(format!(
        "{}: {}",
        t("\u{540e}\u{53f0}\u{7d22}\u{5f15}", "Background index"),
        index_state
    )));
    if let Some(error) = app.live.index["error"].as_str() {
        lines.push(fail_line(error.to_owned()));
    }
    if app.live.model_progress["active"] == true {
        lines.push(line(crate::output::model_task_text(
            &app.live.model_progress,
        )));
    }
    if let Some(status) = crate::progress::inference_progress_text(&app.live.inference) {
        if app.live.inference["host_recovery_required"] == true {
            lines.push(fail_line(status));
        } else {
            lines.push(line(status));
        }
    }
    if let Some(error) = app.live.inference["error"].as_str() {
        lines.push(fail_line(error.to_owned()));
    }
    lines.push(line(t(
        "\u{9ed8}\u{8ba4} CPU\u{ff1b}runtime \u{5185}\u{5171}\u{4eab}\u{6a21}\u{578b}\u{4e0e}\u{63a8}\u{7406}\u{ff1b}\u{5931}\u{8d25}\u{62a5}\u{9519}\u{ff0c}\u{4e0d}\u{5207}\u{6362}\u{5f15}\u{64ce}\u{3002}",
        "CPU by default; shared model and inference inside runtime; failures report errors without switching engines.",
    )));
    lines.push(line(t(
        "\u{5361}\u{4f4f}\u{65f6}\u{5728}\u{7ec8}\u{7aef}\u{8fd0}\u{884c}\u{ff1a}rsrs model reset-cpu",
        "If stuck, run in terminal: rsrs model reset-cpu",
    )));
    lines.push(choice(
        app.cursor == index + 3,
        t(
            "\u{914d}\u{7f6e}\u{9ad8}\u{8d28}\u{91cf}\u{53ec}\u{56de} API\u{ff08}\u{5730}\u{5740} / \u{6a21}\u{578b} / \u{5bc6}\u{94a5}\u{ff09}",
            "Configure recall API (URL / model / key)",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 4,
        t(
            "\u{6d4b}\u{8bd5}\u{5feb}\u{901f}\u{53ec}\u{56de}",
            "Test fast recall",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 5,
        t(
            "\u{6d4b}\u{8bd5}\u{9ad8}\u{8d28}\u{91cf}\u{53ec}\u{56de}",
            "Test high-quality recall",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 6,
        t(
            "\u{53d6}\u{6d88}\u{5f53}\u{524d}\u{6a21}\u{578b}\u{4efb}\u{52a1}",
            "Cancel current model task",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 7,
        t(
            "\u{4ece}\u{6240}\u{9009}\u{4e0b}\u{8f7d}\u{6e90}\u{91cd}\u{65b0}\u{4e0b}\u{8f7d}",
            "Restart download from selected source",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 8,
        t(
            "\u{63a8}\u{7406} / Recall Benchmark",
            "Inference / Recall Benchmark",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 9,
        t(
            "\u{9009}\u{62e9}\u{5f15}\u{64ce}\u{4e0e}\u{6a21}\u{578b}\u{6743}\u{91cd}\u{2026}",
            "Choose engine and model weights\u{2026}",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 10,
        t(
            crate::i18n::cli_zh::MENU,
            "Five-axis attention fusion settings…",
        ),
    ));
    lines.push(choice(
        app.cursor == index + 11,
        t("0  \u{8fd4}\u{56de}", "0  Back"),
    ));
    lines
}

fn workspace_body(app: &App) -> Vec<Line<'static>> {
    let labels = [
        t("\u{6b63}\u{5e38}\u{670d}\u{52a1}", "Normal service"),
        t("\u{53ea}\u{8bfb}\u{670d}\u{52a1}", "Read-only service"),
        t("\u{7981}\u{7528}\u{670d}\u{52a1}", "Disabled service"),
    ];
    vec![
        line(format!(
            "{}: {}",
            t("\u{5f53}\u{524d}", "Current"),
            workspace_label(&app.live.workspace)
        )),
        choice(
            app.cursor == 0,
            format!("{}  {}", t("\u{9009}\u{62e9}", "Choice"), labels[app.workspace_draft]),
        ),
        choice(app.cursor == 1, t("0  \u{8fd4}\u{56de}", "0  Back")),
        line(t(
            "\u{5de6}\u{53f3}\u{952e}\u{9009}\u{62e9}\u{ff0c}\u{56de}\u{8f66}\u{4fdd}\u{5b58}\u{ff1b}\u{5207}\u{6362}\u{8d26}\u{53f7}\u{540e}\u{4fdd}\u{6301}\u{3002}",
            "Left/right chooses. Enter saves for all accounts on this device.",
        )),
    ]
}

fn lang_body(app: &App) -> Vec<Line<'static>> {
    vec![
        choice(app.cursor == 0, t("1  \u{4e2d}\u{6587}", "1  Chinese")),
        choice(app.cursor == 1, t("2  English", "2  English")),
        choice(app.cursor == 2, t("0  \u{8fd4}\u{56de}", "0  Back")),
        line(t(
            "\u{4efb}\u{610f}\u{754c}\u{9762}\u{6309} 7 \u{4e5f}\u{4f1a}\u{7acb}\u{5373}\u{5207}\u{6362}\u{3002}\u{56de}\u{8f66}\u{5728}\u{8fd9}\u{91cc}\u{9009}\u{5b9a}\u{3002}",
            "7 also switches immediately from any screen. Enter selects here.",
        )),
    ]
}

fn version_body(app: &App) -> Vec<Line<'static>> {
    let tools = tools_on_path();
    let tool = tools
        .get(app.manager_pick)
        .copied()
        .unwrap_or(Manager::Direct);
    let spec = if app.live.update_spec.is_empty() {
        channel_spec()
    } else {
        app.live.update_spec.clone()
    };
    let status = if app.live.ver_line.is_empty() {
        format!(
            "{} {}",
            t("\u{5f53}\u{524d}", "Current"),
            env!("CARGO_PKG_VERSION")
        )
    } else {
        app.live.ver_line.clone()
    };
    vec![
        line(status),
        line(runtime_version_label(&app.live)),
        line(format!("{}: {}", t("\u{542f}\u{52a8}\u{65b9}\u{5f0f}", "Started as"), manager_name(detect_preferred()))),
        choice(app.cursor == 0, t("\u{68c0}\u{67e5}\u{6700}\u{65b0}\u{7248}", "Check the newest version")),
        choice(app.cursor == 1, format!("{} {}  {spec}", t("\u{66f4}\u{65b0}", "Update"), manager_name(tool))),
        choice(app.cursor == 2, t("0  \u{8fd4}\u{56de}", "0  Back")),
        line(t("\u{5de6}\u{53f3}\u{952e}\u{9009}\u{62e9}\u{7528}\u{54ea}\u{4e2a}\u{5305}\u{7ba1}\u{7406}\u{5668}\u{66f4}\u{65b0}\u{3002}\u{76f4}\u{63a5}\u{8fd0}\u{884c}\u{7684}\u{7a0b}\u{5e8f}\u{4e0d}\u{4f1a}\u{88ab}\u{8fd9}\u{6b21}\u{66f4}\u{65b0}\u{66ff}\u{6362}\u{3002}", "Left and right choose the package manager. A direct binary is not replaced by the update.")),
    ]
}

fn runtime_version_label(live: &Live) -> String {
    if live.runtime_version.is_empty() {
        t("Runtime \u{672a}\u{8fde}\u{63a5}", "Runtime not connected")
    } else {
        format!("Runtime {}", live.runtime_version)
    }
}

fn detect_preferred() -> Manager {
    tools_on_path().first().copied().unwrap_or(Manager::Direct)
}

fn doctor_value(live: &Live, name: &str) -> String {
    if name == "embedder" && live.model_progress["active"] == true {
        return t(
            "\u{6b63}\u{5728}\u{540e}\u{53f0}\u{51c6}\u{5907}\u{6a21}\u{578b}",
            "Preparing model in background",
        );
    }
    live.doctor
        .iter()
        .find(|(item, _, _)| item == name)
        .map(|(_, status, value)| {
            let value = if value.starts_with("BGE-M3 preparation/indexing ")
                || value.starts_with("Model operation in progress:")
            {
                t(
                    "\u{5df2}\u{5b89}\u{6392}\u{540e}\u{53f0}\u{51c6}\u{5907}",
                    "Background preparation scheduled",
                )
            } else if value.starts_with("BGE-M3 index pending;") {
                t(
                    "\u{7b49}\u{5f85}\u{540e}\u{53f0}\u{91cd}\u{5efa}",
                    "Waiting for background rebuild",
                )
            } else {
                value.clone()
            };
            format!("{status}  {value}")
        })
        .unwrap_or_else(|| t("\u{8fd8}\u{6ca1}\u{6709}\u{8bfb}\u{5230}", "not read yet"))
}
