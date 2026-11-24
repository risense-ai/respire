use super::*;

fn run_classify_config(
    backend: Option<&str>,
    api_base: Option<&str>,
    model: Option<&str>,
    set: bool,
    key_stdin: bool,
) -> Result<()> {
    let value = if set {
        let selected = match backend {
            Some(backend) => backend.to_owned(),
            None => respire_client_host::classification::load_classify_backend()?
                .unwrap_or_else(|| "jev".to_owned()),
        };
        classify_config::configure(&selected, api_base, model, key_stdin)?
    } else {
        if key_stdin || model.is_some() {
            anyhow::bail!("--key-stdin and --model require classify-config --set");
        }
        classify_config::status(backend, api_base)?
    };
    emit_result(ResultEnvelope::new(
        "classify-config",
        OutputStatus::Ok,
        value,
        Vec::new(),
    ))
}

pub(super) fn run(args: Cli) -> Result<()> {
    if let Some(Command::UpdateCheck { force, clear }) = args.command.as_ref() {
        set_json_mode(args.json || respire::env::var("RSRS_JSON")
            .is_ok_and(|value| value == "1" || value == "true"));
        return run_update_check(*force, *clear);
    }
    // The binary's main wires the host hooks before dispatch; test builds and
    // in-process capture callers reach run() without main(), so wire them
    // idempotently here too (the first installation always wins).
    let _ = install_cli_host_hooks();
    if matches!(args.command.as_ref(), Some(Command::AgentConfig { set: None })) {
        set_json_mode(args.json || respire_client_host::env::var("RSRS_JSON")
            .is_ok_and(|value| value == "1" || value == "true"));
        return run_agent_config_read();
    }
    if let Some(Command::Doctor {
        remote,
        check_update,
        fix,
    }) = args.command.as_ref()
    {
        set_json_mode(
            args.json || respire::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"),
        );
        let _progress = progress::Scope::start(args.progress_id.clone(), !json_mode(), true);
        return doctor::run(*remote, *check_update, *fix);
    }
    if let Some(Command::Runtime { command }) = args.command.as_ref() {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        return autostart::execute(command);
    }
    if let Some(Command::SuperReset { super_pass }) = args.command.as_ref() {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        // The private adapter owns Paused recovery; never wrap it in change_profile.
        return run_super_reset(super_pass.as_deref());
    }
    if let Some(Command::Migrate {
        source: Some(source),
        account: Some(account),
        vault: false,
        ..
    }) = args.command.as_ref()
    {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        // This private transaction owns Paused recovery, before generic login/profile gates.
        return emit_result(autostart::control_migrate_profile(source, account)?);
    }
    // Static CLI support is safe to inspect even on a fresh or inaccessible
    // profile. It makes no statement about account rights or model readiness.
    if matches!(args.command, Some(Command::Capabilities)) {
        return run_capabilities();
    }
    if matches!(
        args.command.as_ref(),
        Some(Command::Keygen { .. } | Command::Register { .. } | Command::Login { .. })
    ) {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        // Runtime distinguishes legacy journals from operation-owned recovery.
        // Recover only the former; account control must retain its original receipt.
        // Keygen prepares its source before identity binding and rejects legacy journals explicitly.
        if !runtime_policy::client_only() && !matches!(args.command.as_ref(), Some(Command::Keygen { .. })) {
            rpc::recover_legacy_login_before_account_control()?;
        }
        match args.command.as_ref() {
            Some(Command::Keygen { pass, force }) => return run_keygen(pass.as_deref(), *force),
            Some(Command::Register {
                addr,
                user,
                pass,
                super_pass,
            }) => {
                return run_register(
                    addr.as_deref(),
                    user.as_deref(),
                    pass.as_deref(),
                    super_pass.as_deref(),
                );
            }
            Some(Command::Login {
                addr,
                user,
                pass,
                super_pass,
                interactive,
                oauth,
                dashboard,
                no_open,
                migrate_local,
            }) => {
                return login::run(
                    addr.as_deref(),
                    user.as_deref(),
                    pass.as_deref(),
                    super_pass.as_deref(),
                    *interactive,
                    *oauth,
                    dashboard.as_deref(),
                    *no_open,
                    *migrate_local,
                );
            }
            _ => unreachable!("account command matched above"),
        }
    }
    if matches!(args.command.as_ref(), Some(Command::Space { action, .. })
        if matches!(action.as_str(), "invite" | "join" | "kick" | "create" | "remove"))
        || matches!(args.command.as_ref(), Some(Command::Account { action, .. }) if action == "remove")
    {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        // Membership owns its original Paused receipt, before generic profile/login recovery.
        return host_command(&args).context("private space host handler is missing")?;
    }
    // Legacy recovery owns its Paused transaction before ordinary requests hit the fence.
    if !runtime_policy::client_only()
        && !matches!(args.command, Some(Command::V | Command::MemoryRevision))
    {
        rpc::recover_interrupted_login()?;
    }
    if let Some(Command::AgentConfig { set: Some(kv) }) = args.command.as_ref() {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        // The original O owns recovery before the generic Paused/request gate.
        return emit_result(autostart::control_agent_config(kv)?);
    }
    autostart::check_managed_request()?;
    if matches!(
        args.command.as_ref(),
        Some(Command::Migrate {
            source: None,
            account: None,
            vault: false,
            ..
        })
    ) {
        runtime_policy::require_host("explicit legacy migration")?;
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        let value = profile_discovery::list_legacy_profiles()?;
        let mut envelope =
            ResultEnvelope::new("migrate", OutputStatus::Ok, value.clone(), Vec::new());
        envelope.details = value;
        return emit_result(envelope);
    }
    // Listings need host metadata only; switches own their config transaction.
    // Never enter the legacy change_profile -> run_local transaction for them.
    if matches!(args.command.as_ref(), Some(Command::Account { action, .. })
        if action != "remove")
        || matches!(args.command.as_ref(), Some(Command::Space { action, .. }) if matches!(action.as_str(), "list" | "use"))
    {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        return host_command(&args).context("profile switch host handler is missing")?;
    }
    if matches!(
        args.command.as_ref(),
        Some(Command::Secret { .. } | Command::KeysExport { .. })
    ) {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        return match args.command.as_ref() {
            Some(Command::Secret { reveal }) => run_secret(*reveal),
            Some(Command::KeysExport { out }) => run_keys_export(out.as_deref()),
            _ => unreachable!("private secret command matched above"),
        };
    }
    if DIRECT_MODE.load(Ordering::Relaxed)
        && matches!(
            args.command,
            Some(Command::Metadata {
                command: metadata::Command::Status
            })
        )
    {
        runtime_policy::require_host("direct metadata status")?;
        set_json_mode(
            args.json || respire::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"),
        );
        return emit_result(autostart::control_offline_read(
            respire_protocol::metadata_control::HostOfflineRead::MetadataStatus,
        )?);
    }
    if let Some(Command::Metadata {
        command: command @ (metadata::Command::Enable { .. } | metadata::Command::Disable),
    }) = args.command.as_ref()
    {
        set_json_mode(
            args.json || respire::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true"),
        );
        return emit_result(autostart::control_metadata(
            command,
            DIRECT_MODE.load(Ordering::Relaxed),
        )?);
    }
    if let Some(Command::Snapshot {
        cmd: SnapshotCmd::Restore { id, confirm },
    }) = args.command.as_ref()
    {
        runtime_policy::require_host("snapshot restore")?;
        anyhow::ensure!(
            *confirm,
            "snapshot restore requires --confirm; it replaces local state and quarantines sync"
        );
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        return emit_result(autostart::snapshot_restore_control(id)?);
    }
    if let Some(Command::Snapshot {
        cmd: SnapshotCmd::Recover { confirm },
    }) = args.command.as_ref()
    {
        runtime_policy::require_host("snapshot recovery")?;
        anyhow::ensure!(*confirm, "snapshot recover requires --confirm");
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        return emit_result(autostart::snapshot_recover_control()?);
    }
    if let Some(Command::Model {
        action:
            ModelAction::InstallM3 {
                mirror,
                model,
                for_engine,
            },
    }) = args.command.as_ref()
    {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        let _task = respire_commands::model_progress::TaskScope::new(args.model_task_id.clone());
        let intent = respire_protocol::ModelInstallIntent {
            mirror: mirror.clone(),
            model: model.clone(),
            for_engine: for_engine.clone(),
        };
        let (assets, skipped) = autostart::control_prepare_model(&intent)?;
        let directory = assets
            .assets
            .iter()
            .find(|asset| {
                matches!(
                    asset.name.as_str(),
                    "bge-m3-tokenizer" | "profile-tokenizer"
                )
            })
            .and_then(|asset| asset.path.as_deref())
            .and_then(|path| std::path::Path::new(path).parent())
            .context("Runtime model receipt is missing its tokenizer directory")?;
        let mut result = ResultEnvelope::new("model install-m3", OutputStatus::Ok,
            serde_json::json!({"dir":directory,"skipped":skipped,"activated":false,"model":"m3"}),
            vec![OutputItem::new("model", OutputStatus::Ok,
                "Runtime verified model files and fresh backend loading; run rsrs reembed to rebuild an old index")]);
        result.details = serde_json::to_value(&assets)?;
        return emit_result(result);
    }
    if let Some(Command::Model {
        action: ModelAction::InstallGranite { model, mirror },
    }) = args.command.as_ref()
    {
        set_json_mode(
            args.json
                || respire_client_host::env::var("RSRS_JSON")
                    .is_ok_and(|value| value == "1" || value == "true"),
        );
        let _task = respire_commands::model_progress::TaskScope::new(args.model_task_id.clone());
        let report = respire_commands::model_granite::install_granite(model, mirror.as_deref())?;
        let configuration = report.configuration()?;
        let result = ResultEnvelope::new(
            "model install-granite",
            OutputStatus::Ok,
            serde_json::json!({
                "dir": report.dir,
                "profile": report.profile,
                "model": model,
                "skipped": report.skipped,
                "activated": false,
                "configuration": configuration,
            }),
            vec![OutputItem::new(
                "model",
                OutputStatus::Ok,
                "Granite files verified. Use the JSON configuration with rsrs model configure, then run rsrs model switch.",
            )],
        );
        return emit_result(result);
    }
    run_routed(args)
}

#[path = "dispatch/routed.rs"]
mod routed;
use routed::run_routed;
