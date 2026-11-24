use super::*;

pub(crate) fn host_command(args: &Cli) -> Option<Result<()>> {
    match args.command.as_ref() {
        Some(command @ Command::Extract { .. }) => {
            match respire_commands::cli_def::extract_uses_tui(command) {
                Ok(true) => Some(run_extract_tui()),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        }
        Some(Command::AgentConfig { set: None }) => Some(run_agent_config_read()),
        Some(Command::AgentConfig { set: Some(kv) }) => {
            Some(autostart::control_agent_config(kv).and_then(emit_result))
        }
        Some(Command::Session { command }) => Some(run_session(command)),
        Some(Command::SuperReset { super_pass }) => Some(run_super_reset(super_pass.as_deref())),
        Some(Command::Plugin { command }) => Some(run_plugin(command)),
        Some(Command::Passport) if !runtime_policy::client_only() => Some(run_passport()),
        Some(Command::Prompt) => {
            let text = respire::inject::INSTRUCTIONS_MD;
            Some(emit_result(ResultEnvelope::new(
                "prompt",
                OutputStatus::Ok,
                serde_json::json!({"instructions":text}),
                vec![OutputItem::new("instructions", OutputStatus::Ok, text)],
            )))
        }
        Some(Command::Keygen { pass, force }) => Some(run_keygen(pass.as_deref(), *force)),

        Some(Command::Inject {
            targets,
            id,
            remove,
            all,
            preview,
            expected,
            tui,
        }) => {
            // Default interactive: no args + real TTY -> TUI; client child/script (no tty) -> original full inject, same behavior
            let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            let bare = !*targets && id.is_none() && !*remove && !*preview && expected.is_none();
            if *tui || (bare && !*all && interactive) {
                return Some(respire::inject_tui::run());
            }
            return Some(run_inject(
                *targets,
                id.as_deref(),
                *remove,
                *all,
                *preview,
                expected.as_deref(),
            ));
        }

        Some(Command::Logout { full }) => {
            Some(autostart::control_logout(*full).and_then(emit_result))
        }

        Some(Command::Account { action, name, yes }) => {
            Some(run_account(action, name.as_deref(), *yes))
        }

        Some(Command::Space {
            action,
            name,
            note,
            readonly,
            code,
            session,
            all,
            yes,
        }) => Some(run_space(
            action,
            name.as_deref(),
            note.as_deref(),
            *readonly,
            code.as_deref(),
            session.as_deref(),
            *all,
            *yes,
        )),

        Some(Command::KeysExport { out }) => Some(run_keys_export(out.as_deref())),

        Some(Command::Config {
            data_dir,
            addr,
            autosync,
            cure_auto,
            rpc_parallelism,
        }) => Some(run_config(
            data_dir.as_deref(),
            addr.as_deref(),
            *autosync,
            *cure_auto,
            *rpc_parallelism,
        )),

        Some(Command::Secret { reveal }) => Some(run_secret(*reveal)),

        Some(Command::Fivekeys {
            addr,
            user,
            pass,
            secret,
            kdf_salt,
            wrapped_urk,
            urk_nonce,
            super_pass,
        }) => Some(run_fivekeys(
            addr,
            user,
            pass,
            secret,
            kdf_salt,
            wrapped_urk,
            urk_nonce,
            super_pass,
        )),

        Some(Command::Register {
            addr,
            user,
            pass,
            super_pass,
        }) => Some(run_register(
            addr.as_deref(),
            user.as_deref(),
            pass.as_deref(),
            super_pass.as_deref(),
        )),
        _ => None,
    }
}

pub(super) fn run_agent_config_read() -> Result<()> {
    let mut config = respire_client_host::settings::agent_config_snapshot()?;
    let configured_heat = config.get("read_heat").map(|value|
        value.as_bool().context("read_heat must be true or false")).transpose()?;
    // Local resolution does not prove Core support or the resident Runtime's loaded state.
    config["read_heat_runtime"] = serde_json::json!({
        "scope":"host_configuration", "runtime_verified":false,
        "configured":configured_heat, "loaded":null,
        "effective":configured_heat.unwrap_or(true), "default":true, "pending_reload":null,
    });
    config["recall_fusion_runtime"] = recall_fusion::receipt(
        config.get("recall_fusion").cloned().unwrap_or_else(|| serde_json::json!({})))?;
    let rows = config.as_object().context("agent config must be an object")?.iter()
        .map(|(key, value)| OutputItem::new(key, OutputStatus::Ok, value.to_string())).collect();
    emit_result(ResultEnvelope::new("agent-config", OutputStatus::Ok, config, rows))
}

pub(super) fn run_extract_tui() -> Result<()> {
    runtime_policy::require_host("extract source picker")?;
    anyhow::ensure!(
        std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stdout()),
        "extract --tui needs a real terminal (stdin/stdout must both be tty)"
    );
    // Finish host lifecycle work before the UI enters raw terminal mode.
    rpc::ensure_daemon()?;
    respire::extract_tui::run(|full| {
        let response = rpc::call_method("cli.exec", full, false)?;
        anyhow::ensure!(
            response.ok && response.exit == 0,
            "extract sources failed: {}",
            response
                .error
                .as_deref()
                .unwrap_or("Runtime source query failed")
        );
        response
            .envelope
            .context("extract sources: Runtime response missing envelope")
    })
}

fn run_passport() -> Result<()> {
    runtime_policy::require_host("passport agent inventory")?;
    let mut args = vec!["passport".to_owned()];
    if let Some(id) = progress::remote_id() {
        args.extend(["--progress-id".to_owned(), id]);
    }
    let mut result: ResultEnvelope = serde_json::from_value(rpc::query_json(args)?)?;
    if result.command == "memory"
        && result.summary["mode"] == "off"
        && result.summary["skipped"] == true
    {
        return emit_result(result);
    }
    anyhow::ensure!(
        result.command == "passport",
        "Runtime did not return a passport result"
    );
    let summary = result
        .summary
        .as_object_mut()
        .ok_or_else(|| anyhow!("Runtime passport summary missing"))?;
    let agents = match respire::inject::targets() {
        Ok(targets) => {
            let count = targets
                .iter()
                .filter(|target| matches!(target.state, "fresh" | "stale"))
                .count();
            summary.insert("agents".into(), serde_json::json!(count));
            OutputItem::new("agents", OutputStatus::Ok, count.to_string())
        }
        Err(error) => {
            summary.insert("agents".into(), serde_json::Value::Null);
            let message = format!("host agent inventory: {error:#}");
            result.errors.push(message.clone());
            result.status = OutputStatus::Fail;
            OutputItem::new("agents", OutputStatus::Fail, message)
        }
    };
    if let Some(row) = result.items.iter_mut().find(|row| row.name == "agents") {
        *row = agents;
    } else {
        result.items.push(agents);
    }
    emit_result(result)
}

pub(super) fn run_super_reset(super_pass: Option<&str>) -> Result<()> {
    use respire_protocol::key_control::{HostKeyInput, HostKeyOutcome, HostKeyResult};
    runtime_policy::require_host("super password rotation")?;
    let mut input = HostKeyInput {
        password: None,
        super_password: super_pass.map(str::to_owned),
        ..HostKeyInput::default()
    };
    let mut supplied = std::collections::BTreeSet::new();
    if super_pass.is_some() {
        supplied.insert("super_password".to_owned());
    }
    let mut envelope = autostart::control_super_reset(super_pass)?;
    loop {
        if !matches!(envelope.status, OutputStatus::Pending) {
            return emit_result(envelope);
        }
        let receipt: HostKeyResult = serde_json::from_value(envelope.summary.clone())
            .map_err(|_| anyhow!("invalid key continuation receipt"))?;
        envelope.actions.push(format!(
            "runtime key-result --operation-id {} (use the original selected profile)",
            receipt.operation_id,
        ));
        // Scripts receive the original O without reading stdin or admitting another reset.
        if receipt.outcome != HostKeyOutcome::AwaitingInput
            || json_mode()
            || !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return emit_result(envelope);
        }
        let requested: std::collections::BTreeSet<_> = receipt.required_inputs.iter().collect();
        anyhow::ensure!(
            !requested.is_empty()
                && requested.len() == receipt.required_inputs.len()
                && requested
                    .iter()
                    .all(|field| matches!(field.as_str(), "password" | "super_password")),
            "Runtime requested invalid key continuation inputs"
        );
        if requested
            .iter()
            .any(|field| supplied.contains(field.as_str()))
        {
            return emit_result(envelope);
        }
        let continuation = (|| -> Result<ResultEnvelope> {
            for field in &receipt.required_inputs {
                let prompt = match field.as_str() {
                    "password" => "original v1 login password: ",
                    "super_password" => "super password (A3-...): ",
                    _ => anyhow::bail!("unsupported key continuation input"),
                };
                let value = respire::prompt::ask_secret(prompt)?;
                anyhow::ensure!(
                    !value.is_empty(),
                    "key input is empty; original operation retained"
                );
                match field.as_str() {
                    "password" => input.password = Some(value),
                    "super_password" => input.super_password = Some(value),
                    _ => anyhow::bail!("unsupported key continuation input"),
                }
                supplied.insert(field.clone());
            }
            autostart::control_super_reset_step(
                None,
                HostKeyInput {
                    password: input.password.clone(),
                    super_password: input.super_password.clone(),
                    ..HostKeyInput::default()
                },
                Some(&receipt),
            )
        })();
        envelope = continuation.map_err(|error| {
            error.context(autostart::KeyRecoveryRequired {
                operation_id: receipt.operation_id,
                source_profile: receipt.source_profile,
            })
        })?;
    }
}

fn run_session(command: &SessionCommand) -> Result<()> {
    runtime_policy::require_host("session identity management")?;
    let revoke = match command {
        SessionCommand::List => None,
        SessionCommand::Revoke { id } => Some(id.as_str()),
    };
    let action = if revoke.is_some() { "revoke" } else { "list" };
    let details = match revoke {
        Some(id) => autostart::control_session_revoke(id)?,
        None => autostart::control_session_list()?,
    };
    let rows = details.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let items = rows
        .iter()
        .map(|row| {
            OutputItem::new(
                row["id"]
                    .as_str()
                    .or_else(|| row["session_id"].as_str())
                    .unwrap_or("session"),
                OutputStatus::Ok,
                row.to_string(),
            )
        })
        .collect();
    let mut result = ResultEnvelope::new(
        "session",
        OutputStatus::Ok,
        serde_json::json!({"action":action,"count":rows.len()}),
        items,
    );
    result.details = details;
    emit_result(result)
}

fn run_plugin(command: &PluginCommand) -> Result<()> {
    runtime_policy::require_host("plugin management")?;
    use respire_commands::hooks;
    let (_, profile) = respire_client_host::config::control_roots()?;
    let path = profile.join("plugins.json");
    match command {
        PluginCommand::List => {
            let cfg = hooks::read_config_at(&path)?;
            let mut items = Vec::new();
            for (event, hooks) in &cfg.hooks {
                for hook in hooks {
                    let policy = if hook.on_error.is_empty() {
                        "default"
                    } else {
                        &hook.on_error
                    };
                    items.push(OutputItem::new(
                        event,
                        OutputStatus::Ok,
                        format!(
                            "{} timeout={}ms on_error={policy}",
                            hook.cmd, hook.timeout_ms
                        ),
                    ));
                }
            }
            if items.is_empty() {
                items.push(OutputItem::new(
                    "hooks",
                    OutputStatus::Skip,
                    format!("none configured; file={}", path.display()),
                ));
            }
            let mut result = ResultEnvelope::new(
                "plugin-list",
                if cfg.hooks.is_empty() {
                    OutputStatus::Skip
                } else {
                    OutputStatus::Ok
                },
                serde_json::json!({
                    "config_path": path.to_string_lossy(),
                    "events": hooks::EVENT_NAMES,
                    "count": cfg.hooks.values().map(Vec::len).sum::<usize>(),
                }),
                items,
            );
            result.details = serde_json::to_value(&cfg.hooks)?;
            result
                .actions
                .push("plugin test <EVENT> --payload <JSON>".into());
            emit_result(result)
        }
        PluginCommand::Test { event, payload } => {
            let event = hooks::HookEvent::parse(event).ok_or_else(|| {
                anyhow!(
                    "unknown event \"{event}\"; available: {}",
                    hooks::EVENT_NAMES.join(" / ")
                )
            })?;
            let data = serde_json::from_str(payload)
                .map_err(|error| anyhow!("--payload is not JSON: {error}"))?;
            let verdict = hooks::fire_at(&path, event, data)?;
            let status = if verdict.blocked {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            };
            let mut result = ResultEnvelope::new(
                "plugin-test",
                status,
                serde_json::json!({"event":event.as_str(),"blocked":verdict.blocked,"warning_count":verdict.warnings.len()}),
                vec![OutputItem::new(
                    "event",
                    status,
                    if verdict.blocked {
                        format!("{} blocked: {}", event.as_str(), verdict.reason)
                    } else {
                        format!("{} allowed", event.as_str())
                    },
                )],
            );
            result.details = serde_json::json!({"event":event.as_str(),"blocked":verdict.blocked,"reason":verdict.reason,"warnings":verdict.warnings});
            if verdict.blocked {
                result.actions.push("review plugin policy".into());
            }
            emit_result(result)
        }
    }
}
