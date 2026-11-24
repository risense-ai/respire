use super::*;

pub(super) fn run_routed(args: Cli) -> Result<()> {
    // Business RPC retains Runtime's post-ownership snapshot gate and bootstrap.
    // Only local host writes need this offline check before they change files.
    let host_snapshot_write = match args.command.as_ref() {
        Some(
            Command::AgentConfig { set: Some(_) }
            | Command::ClassifyConfig { set: true, .. }
            | Command::Inject {
                targets: false,
                preview: false,
                ..
            }
            | Command::Plugin {
                command: PluginCommand::Test { .. },
            },
        ) => true,
        Some(Command::Config {
            data_dir,
            addr,
            autosync,
            cure_auto,
            rpc_parallelism,
        }) => {
            data_dir.is_some()
                || addr.is_some()
                || autosync.is_some()
                || cure_auto.is_some()
                || rpc_parallelism.is_some()
        }
        Some(Command::Account { action, .. }) => action == "remove",
        Some(Command::Space { action, .. }) => matches!(action.as_str(), "create" | "remove"),
        _ => false,
    };
    if host_snapshot_write && !runtime_policy::client_only() {
        runtime_policy::require_host("local host write snapshot preflight")?;
        if runtime_fetch::read_manifest()?.is_none() {
            runtime_fetch::download_and_install(None, None)?;
        }
        autostart::control_offline_read(
            respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight,
        ).context("snapshot preflight unavailable or blocked; inspect snapshot recovery before continuing")?;
    }
    let _model_task = respire::model_progress::TaskScope::new(args.model_task_id.clone());
    set_json_mode(
        args.json || respire::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"),
    );
    if let Some(Command::Model { action }) = args.command.as_ref() {
        let request = match action {
            ModelAction::Catalog => Some(("model.catalog", Vec::new())),
            ModelAction::Configure { file } => {
                let value: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
                Some(("model.configure", vec![serde_json::to_string(&value)?]))
            }
            ModelAction::Switch {
                model,
                backend,
                engine,
                variant,
            } => Some((
                "model.switch",
                vec![serde_json::to_string(
                    &serde_json::json!({"model":model,"backend":backend,"engine":engine,"variant":variant}),
                )?],
            )),
            ModelAction::Progress => Some(("model.progress", Vec::new())),
            ModelAction::Resume => Some(("model.resume", Vec::new())),
            _ => None,
        };
        if let Some((method, values)) = request {
            anyhow::ensure!(
                !DIRECT_MODE.load(Ordering::Relaxed),
                "model profiles require the resident runtime; omit --direct"
            );
            let value = rpc::profile_request(method, values)?;
            return emit_result(serde_json::from_value(value)?);
        }
    }
    if matches!(args.command, Some(Command::MemoryRevision)) {
        if !DIRECT_MODE.load(Ordering::Relaxed) && runtime_policy::client_only() {
            return rpc::call_existing_from_argv();
        }
        runtime_policy::require_host("memory revision control")?;
        let result = autostart::control_memory_revision()?;
        // Keep private control evidence out of the public CLI output shape.
        return emit_result(ResultEnvelope::new(
            "memory-revision",
            OutputStatus::Ok,
            result.summary,
            Vec::new(),
        ));
    }
    // Host-only migration and repair dispatch must obey the memory gate too.
    if respire_client_host::settings::off_mode()?
        && matches!(args.command.as_ref(), Some(Command::Review { .. }))
    {
        set_exit_code(1);
        let mut result = ResultEnvelope::new(
            "review",
            OutputStatus::Fail,
            serde_json::json!({"saved":false}),
            Vec::new(),
        );
        result.details = serde_json::json!({"code":"feature_disabled","saved":false});
        result
            .errors
            .push("memory service is off; due state is unavailable".to_owned());
        return emit_result(result);
    }
    if respire_client_host::settings::off_mode()?
        && args
            .command
            .as_ref()
            .is_some_and(|command| !is_off_allowed(command))
    {
        if !json_mode() {
            return Ok(());
        }
        let command = match args.command.as_ref() {
            Some(Command::Recall { .. }) => "recall",
            Some(Command::Remember { .. }) => "remember",
            _ => "memory",
        };
        let mut result = ResultEnvelope::new(
            command,
            OutputStatus::Ok,
            serde_json::json!({"mode":"off", "skipped":true, "count":0}),
            Vec::new(),
        );
        result.details = serde_json::json!([]);
        return emit_result(result);
    }
    let display = !json_mode()
        && args.command.is_some()
        && !matches!(
            args.command,
            Some(Command::Mcp | Command::Login { .. } | Command::V)
        );
    let _progress = progress::Scope::start(
        args.progress_id.clone(),
        display,
        matches!(args.command, Some(Command::Sync | Command::Doctor { .. })),
    );
    if let Some(Command::Space {
        action, code, name, ..
    }) = args.command.as_ref()
    {
        if action == "join" {
            let code = code.as_deref().or(name.as_deref()).ok_or_else(|| {
                respire_client_host::input_error::InputError("join needs an invite code".into())
            })?;
            let invite = respire::space::parse_invite(code)?;
            respire::space::validate_space_name(
                invite["space"].as_str().context("invite space missing")?,
            )?;
        }
    }
    if let Some(Command::Migrate {
        source,
        account,
        vault,
        addr,
        user,
        pass,
        super_pass,
        secret_key,
        new_super,
    }) = args.command.as_ref()
    {
        if *vault || source.is_some() {
            respire_client_host::settings::ensure_writable()?;
        }
        runtime_policy::require_host("explicit legacy migration")?;
        if *vault {
            return login::migrate_vault(
                addr.as_deref(),
                user.as_deref(),
                pass.as_deref(),
                super_pass.as_deref(),
                secret_key.as_deref(),
                new_super.as_deref(),
            );
        }
        let value = match (source.as_deref(), account.as_deref()) {
            (Some(source), Some(account)) => {
                return emit_result(autostart::control_migrate_profile(source, account)?);
            }
            (None, None) => profile_discovery::list_legacy_profiles()?,
            _ => anyhow::bail!("migration requires both --source and --account"),
        };
        let mut envelope =
            ResultEnvelope::new("migrate", OutputStatus::Ok, value.clone(), Vec::new());
        envelope.details = value;
        return emit_result(envelope);
    }
    if matches!(args.command, Some(Command::V)) {
        return crate::app_version::emit(json_mode());
    }
    if let Some(Command::ClassifyConfig {
        backend,
        api_base,
        model,
        set,
        key_stdin,
    }) = args.command.as_ref()
    {
        runtime_policy::require_host("classification configuration")?;
        return run_classify_config(
            backend.as_deref(),
            api_base.as_deref(),
            model.as_deref(),
            *set,
            *key_stdin,
        );
    }
    if let Some(Command::Restart { timeout }) = args.command.as_ref() {
        let result = rpc::restart_runtime(std::time::Duration::from_secs(*timeout))?;
        return emit_result(ResultEnvelope::new(
            "restart",
            OutputStatus::Ok,
            result,
            Vec::new(),
        ));
    }
    if matches!(
        args.command,
        Some(Command::Model {
            action: ModelAction::InstallEngines
        })
    ) {
        runtime_policy::require_host("inference engine installation")?;
        progress::phase(
            "\u{5b89}\u{88c5}\u{63a8}\u{7406}\u{5f15}\u{64ce}\u{ff1b}\u{7b49}\u{5f85} Windows ML",
            "Installing inference providers; waiting for Windows ML",
        );
        return emit_result(autostart::control_install_engines()?);
    }
    if matches!(
        args.command,
        Some(Command::Model {
            action: ModelAction::ResetCpu
        })
    ) {
        runtime_policy::require_host("CPU recovery and runtime shutdown")?;
        return emit_result(autostart::control_reset_cpu()?);
    }
    if matches!(
        args.command,
        Some(Command::Model {
            action: ModelAction::UninstallM3
        })
    ) {
        runtime_policy::require_host("M3 model removal")?;
        return emit_result(autostart::control_uninstall_m3()?);
    }
    if let Some(Command::Web { no_open }) = args.command.as_ref() {
        if !no_open {
            crate::web::open_browser(crate::web::DASHBOARD_URL)?;
        }
        return emit_result(ResultEnvelope::new(
            "web",
            OutputStatus::Ok,
            serde_json::json!({"url":crate::web::DASHBOARD_URL,"opened":!no_open}),
            Vec::new(),
        ));
    }
    if matches!(args.command, Some(Command::Mcp)) {
        if DIRECT_MODE.load(Ordering::Relaxed) {
            anyhow::bail!("mcp uses the resident runtime; do not pass --direct");
        }
        return mcp::serve();
    }
    // Profile and agent changes own their private maintenance transactions.
    // Other host commands never belong to Runtime business RPC.
    if let Some(outcome) = host_command(&args) {
        return outcome;
    }
    if DIRECT_MODE.load(Ordering::Relaxed) {
        // Deprecated downgrade: --direct used to bypass the resident runtime.
        // For one release train it forwards to the local runtime instead.
        eprintln!("deprecated: --direct now forwards to the local runtime; the in-process bypass is removed in an upcoming release train");
        return rpc::call_from_argv();
    }
    if args.command.is_none() {
        if std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return shell::run();
        }
        eprintln!("{}", i18n::text("need_tty"));
        set_exit_code(2);
        return Ok(());
    }
    rpc::call_from_argv()
}
