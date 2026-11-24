fn status() -> Result<Value> {
    let mut value = disabled_status()?;
    let root = respire_client_host::paths::main_data_dir();
    if let Some(config) = read_registration(&root)? {
        owned(&config)?;
        let actual = manager::inspect(&config)?;
        value["registered"] = json!(actual.registered);
        value["enabled"] = json!(config.enabled && actual.enabled);
        value["registration"] = serde_json::to_value(actual)?;
        value["maintenance"] = json!(marker(&root)?.is_some());
    }
    match crate::rpc::probe_runtime() {
        Ok(Some(health)) => {
            if health.installation_root.is_empty()
                || !crate::mcp::same_path(Path::new(&health.installation_root), &root)
            {
                value["runtime"] = json!({"running":null,"state":if health.installation_root.is_empty() {"legacy_identity_unavailable"} else {"another_installation"},"version":health.bin});
                return Ok(value);
            }
            value["runtime"] = json!({"running":true,"pid":health.pid,"version":health.bin,"data_dir":health.data_dir,"store_ready":health.store_ready,"inference":health.inference,"credential_state":if health.credential_state.is_empty() {"unknown"} else {health.credential_state.as_str()},"network_state":"not_probed"});
        }
        Ok(None) => value["runtime"] = json!({"running":false}),
        Err(_) => value["runtime"] = json!({"running":null,"state":"unreachable_or_unverified"}),
    }
    Ok(value)
}

fn emit(command: &str, summary: Value) -> Result<()> {
    crate::emit_result(crate::output::ResultEnvelope::new(
        command,
        crate::OutputStatus::Ok,
        summary,
        Vec::new(),
    ))
}

fn verify_key_custody(operation_id: &str, source: &Path) -> Result<crate::output::ResultEnvelope> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin().take(65537).read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("private custody input unreadable"))?;
    anyhow::ensure!(bytes.len() <= 65536, "private custody input exceeds the control limit");
    let expected: respire_protocol::key_control::HostKeyInput = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid private custody input"))?;
    respire_host_control::KeyCoordinator::verify_custody(operation_id, source, &expected)
}

pub(crate) fn execute(command: &RuntimeCommand) -> Result<()> {
    match command {
        RuntimeCommand::CoreConfigResult { context, source_profile } => {
            return crate::emit_result(super::supervise::control_core_config_result(context, source_profile)?);
        }
        RuntimeCommand::KeyResult { operation_id, source_profile, verify_custody } => {
            if *verify_custody {
                let source = source_profile.as_deref().context("custody verification requires its original source")?;
                return crate::emit_result(verify_key_custody(operation_id, source)?);
            }
            return crate::emit_result(super::control_key_result(operation_id, source_profile.as_deref())?);
        }
        RuntimeCommand::KeyRollback { operation_id, confirm } => {
            anyhow::ensure!(*confirm, "key rollback requires explicit --confirm");
            return crate::emit_result(super::control_key_rollback(operation_id)?);
        }
        RuntimeCommand::AuthResult { operation_id, force } => {
            return crate::emit_result(super::control_auth_result(operation_id, *force)?);
        }
        RuntimeCommand::AccountResult { operation_id, source_profile, register } => {
            return crate::emit_result(super::control_account_result(
                operation_id, source_profile, *register,
            )?);
        }
        RuntimeCommand::AccountRollback { operation_id, source_profile, register, confirm } => {
            anyhow::ensure!(*confirm, "account rollback requires explicit --confirm");
            return crate::emit_result(super::control_account_rollback(
                operation_id, source_profile, *register,
            )?);
        }
        _ => {}
    }
    if let RuntimeCommand::Result { context } = command {
        let response: crate::rpc::RpcResponse =
            serde_json::from_value(crate::net_rpc::rpc_result(context)?)
                .context("invalid runtime.result response")?;
        if let Some(code @ ("scope_required" | "scope_mismatch")) = response.code.as_deref() {
            bail!(
                "{code}: {}",
                response
                    .error
                    .as_deref()
                    .unwrap_or("runtime.result scope rejected")
            );
        }
        if let Some(envelope) = response.envelope {
            crate::emit_result(envelope)?;
            crate::set_exit_code(response.exit);
            return Ok(());
        }
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "runtime.result returned an empty response".to_owned())
        );
    }
    if matches!(
        command,
        RuntimeCommand::Status
            | RuntimeCommand::Autostart {
                command: AutostartCommand::Status
            }
    ) {
        return emit("runtime.autostart.status", status()?);
    }
    crate::runtime_policy::require_host("runtime lifecycle")?;
    match command {
        RuntimeCommand::Run {
            managed,
            installation_root,
            supervised_child,
        } => run_foreground(
            managed.as_deref(),
            installation_root.as_deref(),
            *supervised_child,
        ),
        RuntimeCommand::Stop => {
            let stopped = crate::rpc::probe_runtime()?;
            crate::rpc::stop_if_running()?;
            let result = match stopped {
                Some(health) => crate::net_rpc::final_stop_receipt(&health, None)?,
                None => json!({"state":"stopped", "already_stopped":true}),
            };
            emit("runtime.stop", result)
        }
        RuntimeCommand::Restart => {
            let value = crate::rpc::restart_runtime(Duration::from_secs(10))?;
            emit("runtime.restart", value)
        }
        RuntimeCommand::Start => {
            let ownership = crate::runtime_policy::takeover_lock()?;
            if !super::supervise::offline_keygen::start_explicit(&ownership)? {
                start_explicit_locked(&ownership)?;
            }
            emit("runtime.start", status()?)
        }
        RuntimeCommand::Unlock => {
            let health = crate::net_rpc::health()?;
            let receipt = respire_host_control::lifecycle_io::supply_runtime_credentials(&health)?
                .context("Runtime is not ready for V2 credential delivery")?;
            emit("runtime.unlock", json!({"store_ready":receipt.store_ready,
                "credential_state":"available", "provider_status":receipt.provider_status}))
        }
        RuntimeCommand::Autostart {
            command: AutostartCommand::Enable { now },
        } => enable(*now),
        RuntimeCommand::Autostart {
            command: AutostartCommand::Disable { stop },
        } => disable(*stop, false),
        RuntimeCommand::Autostart {
            command: AutostartCommand::Remove { stop },
        } => disable(*stop, true),
        RuntimeCommand::Install {
            file,
            version,
            mirror,
            channel,
        } => install_runtime("runtime.install", file.as_deref(), version.as_deref(), mirror.as_deref(), *channel),
        RuntimeCommand::List => emit("runtime.list", runtime_inventory()?),
        RuntimeCommand::Clean => {
            let removed = crate::runtime_fetch::clean_old()?;
            emit("runtime.clean", json!({"removed": removed}))
        }
        RuntimeCommand::Update { channel } => install_runtime("runtime.update", None, None, None, *channel),
        RuntimeCommand::Supervise { data_dir } => {
            return super::supervise::supervise(data_dir);
        }
        RuntimeCommand::CoreConfigResult { .. }
        | RuntimeCommand::KeyResult { .. }
        | RuntimeCommand::KeyRollback { .. }
        | RuntimeCommand::AuthResult { .. }
        | RuntimeCommand::AccountResult { .. }
        | RuntimeCommand::AccountRollback { .. } => {
            bail!("authentication recovery command was not handled before lifecycle setup")
        }
        _ => bail!("unsupported runtime lifecycle command"),
    }
}

/// `runtime install`: channel download by default, or an offline archive with
/// `--file`/`--version`, with an optional explicit `--mirror` origin.
fn install_runtime(command: &str, file: Option<&Path>, version: Option<&str>, mirror: Option<&str>,
    channel: Option<respire_commands::autostart::RuntimeChannel>) -> Result<()> {
    let (binary, source) = match (file, version) {
        (Some(archive), Some(explicit_version)) => {
            anyhow::ensure!(mirror.is_none(), "--mirror is only accepted for downloads");
            (crate::runtime_fetch::install_from_file(archive, explicit_version)?, "file")
        }
        (Some(_), None) => bail!("--file requires --version"),
        (None, explicit_version) => {
            if let Some(origin) = mirror {
                respire_commands::model_download::validate_mirror(origin)?;
            }
            (crate::runtime_fetch::install_published_runtime(explicit_version, mirror, channel)?, "download")
        }
    };
    let selected_version = crate::runtime_fetch::read_manifest()?
        .context("Runtime selection missing after installation")?.version;
    emit(command, json!({"source":source,"version":selected_version,
        "binary":binary,"cli_binary":crate::mcp::stable_bin_path(),"restarted":false}))
}

fn runtime_inventory() -> Result<Value> {
    let installed = crate::runtime_fetch::list_installed()?;
    let versions: Vec<Value> = installed
        .iter()
        .map(
            |entry| json!({"version": entry.version, "path": entry.path, "current": entry.current}),
        )
        .collect();
    Ok(json!({
        "channel": crate::runtime_fetch::default_runtime_channel_name(),
        "installed": versions,
    }))
}

fn enable(now: bool) -> Result<()> {
    crate::runtime_fetch::require_automatic_migration_safe()?;
    // A platform-rejected native path must not even create lifecycle state.
    manager::validate_executable(Platform::current()?, &crate::mcp::stable_bin_path())?;
    let _ownership = crate::runtime_policy::takeover_lock()?;
    respire_host_control::lifecycle_io::migrate_legacy_session_under_takeover(&_ownership)?;
    crate::rpc::resolve_runtime_executable()?;
    anyhow::ensure!(crate::runtime_fetch::managed_runtime_path()?.is_some(),
        "autostart requires a verified managed Runtime before registration; install the trusted release first");
    let mut config = match read_registration(&respire_client_host::paths::main_data_dir())? {
        Some(config) if !config.enabled && config.artifact_hashes.is_empty() => {
            owned(&config)?;
            anyhow::ensure!(
                !manager::inspect(&config)?.registered,
                "unknown remaining registration; refusing reconfiguration"
            );
            let mut fresh = new_registration()?;
            fresh.digest = config.digest;
            fresh
        }
        Some(config) => config,
        None => new_registration()?,
    };
    owned(&config)?;
    manager::inspect(&config)?;
    config.manually_stopped = false;
    let was_running = crate::rpc::probe_runtime()?.is_some();
    let mut guard = Maintenance::begin(config, false)?;
    let install = (|| -> Result<()> {
        raw_stop_owned(guard.config.as_ref().context("registration missing")?)?;
        crate::mcp::materialize_bin().map_err(anyhow::Error::msg)?;
        let config = guard.config.as_mut().context("registration missing")?;
        manager::install(config)?;
        reset_attempts(config)?;
        config.enabled = true;
        save(config)?;
        if now || was_running {
            start_candidate(&mut guard)?;
        }
        Ok(())
    })();
    if let Err(error) = install {
        if let Some(config) = guard.config.as_mut() {
            config.enabled = false;
            if let Err(save_error) = save(config) {
                return Err(error.context(format!("startup remains paused: {save_error}")));
            }
        }
        return Err(error.context("startup registration did not complete; runtime maintenance remains paused for explicit host recovery"));
    }
    guard.finish()?;
    emit("runtime.autostart.enable", status()?)
}

fn disable(stop: bool, remove: bool) -> Result<()> {
    let command = if remove {
        "runtime.autostart.remove"
    } else {
        "runtime.autostart.disable"
    };
    if read_registration(&respire_client_host::paths::main_data_dir())?.is_none() {
        return emit(command, status()?);
    }
    let _ownership = crate::runtime_policy::takeover_lock()?;
    let Some(mut config) = read_registration(&respire_client_host::paths::main_data_dir())? else {
        return emit(command, status()?);
    };
    owned(&config)?;
    let actual = manager::inspect(&config)?;
    anyhow::ensure!(
        !remove || stop || marker(&config.installation)?.is_none(),
        "interrupted maintenance requires remove --stop or explicit host runtime restart"
    );
    anyhow::ensure!(
        !remove || stop || !actual.running,
        "stop the owned runtime before removing its startup registration"
    );
    if stop {
        ensure_control_endpoint(&config)?;
        config.manually_stopped = true;
        save(&mut config)?;
        let mut guard = Maintenance::begin(config, true)?;
        raw_stop_owned(guard.config.as_ref().context("registration missing")?)?;
        guard.finish()?;
        config = read_registration(&respire_client_host::paths::main_data_dir())?
            .context("registration disappeared")?;
    }
    config.enabled = false;
    save(&mut config)?;
    manager::disable(&config)?;
    if remove {
        manager::remove(&config)?;
        config.artifact_hashes.clear();
        save(&mut config)?;
    }
    emit(
        if remove {
            "runtime.autostart.remove"
        } else {
            "runtime.autostart.disable"
        },
        status()?,
    )
}
