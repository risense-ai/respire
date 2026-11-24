pub(super) fn registered_runtime_image(config: &Registration, migrated_image: Option<&str>) -> Result<PathBuf> {
    let root = config.installation.canonicalize()?;
    let registered_root = PathBuf::from(
        config
            .environment
            .get("RSRS_DATA_DIR")
            .context("registered runtime root is missing")?,
    );
    anyhow::ensure!(
        registered_root.canonicalize()? == root
            && respire_client_host::paths::main_data_dir().canonicalize()? == root,
        "registered runtime root belongs to another installation"
    );
    if let Some(image) = migrated_image {
        if let Some(runtime) = crate::runtime_fetch::verified_migrated_image(image)? {
            return Ok(runtime);
        }
    }
    // The registered supervisor clears runtime overrides before selecting its child.
    let runtime =
        if let Some(managed) = crate::runtime_fetch::managed_runtime_path()? {
            managed
        } else {
            config
                .executable
                .parent()
                .context("supervisor executable parent missing")?
                .join(crate::runtime_fetch::RUNTIME_BINARY)
        };
    let runtime = runtime
        .canonicalize()
        .context("selected Runtime is unavailable")?;
    anyhow::ensure!(runtime.is_file(), "selected Runtime is not a file");
    Ok(runtime)
}

pub(super) fn verify_owned_runtime(config: &Registration, health: &crate::net_rpc::Health) -> Result<String> {
    let root = config.installation.canonicalize()?;
    let runtime = registered_runtime_image(config, Some(&health.image_path))?;
    anyhow::ensure!(
        runtime != config.executable.canonicalize()?
            && runtime != std::env::current_exe()?.canonicalize()?
            && crate::mcp::exe_matches_dest(&health.exe, &runtime)
            && crate::mcp::exe_matches_dest(&health.image_path, &runtime)
            && !health.data_dir.is_empty()
            && Path::new(&health.data_dir).canonicalize()? == respire_client_host::config::control_roots()?.1.canonicalize()?
            && !health.installation_root.is_empty()
            && Path::new(&health.installation_root).canonicalize()? == root
            && health.installation_id == hash(root.display().to_string().as_bytes())
            && health.managed_id.as_deref() == Some(config.id.as_str())
            && health.pid > 0,
        "runtime identity/profile does not match this startup registration; no lifecycle action taken"
    );
    let birth =
        manager::process_birth(health.pid)?.context("owned runtime process already exited")?;
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    anyhow::ensure!(
        health.process_start_id == format!("{prefix}{birth}"),
        "owned runtime process birth does not match health"
    );
    Ok(birth)
}

fn raw_stop_owned(config: &Registration) -> Result<()> {
    owned(config)?;
    ensure_control_endpoint(config)?;
    let manager_state = manager::inspect(config)?;
    // A closed listener can still belong to a draining child in the supervisor's job.
    let Some(health) = crate::rpc::probe_runtime()
        .context("owned runtime health is unavailable; manager stop is blocked")?
    else {
        anyhow::ensure!(
            !manager_state.running && !crate::rpc::endpoint_path().exists(),
            "owned runtime has no verifiable health/drain proof; manager stop is blocked"
        );
        return Ok(());
    };
    verify_owned_runtime(config, &health)?;
    crate::rpc::drain_occupant(&health, Duration::from_secs(60))?;
    stop_drained_supervisor(config)
}

// Call only after drain_occupant has verified the child's final receipt and exit.
pub(super) fn stop_drained_supervisor(config: &Registration) -> Result<()> {
    manager::stop(config)?;
    anyhow::ensure!(
        !manager::inspect(config)?.running && !crate::rpc::endpoint_path().try_exists()?,
        "drained runtime supervisor is still active or its endpoint changed; replacement is blocked"
    );
    Ok(())
}

pub(super) fn wait_ready(config: &Registration) -> Result<()> {
    wait_ready_with_expectation(config, respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
}

fn wait_ready_with_expectation(config: &Registration,
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation) -> Result<()> {
    owned(config)?;
    manager::inspect(config)?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(health) = crate::rpc::probe_runtime()? {
            verify_owned_runtime(config, &health)?;
            anyhow::ensure!(
                health.v == crate::rpc::PROTOCOL_V,
                "managed Runtime protocol mismatch"
            );
            if health.credential_state == "required" || health.store_ready
                || respire_client_host::settings::off_mode()? {
                expectation.confirm_credentials(&health)?;
                if current_boot_token().is_none() {
                    super::supervise::apply_startup_core_config(&health, None)?;
                }
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("owned runtime did not confirm store readiness; maintenance remains paused")
}

fn start_candidate(guard: &mut Maintenance) -> Result<()> {
    start_candidate_with_expectation(guard, respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
}

fn start_candidate_with_expectation(guard: &mut Maintenance,
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation) -> Result<()> {
    let config = guard
        .config
        .as_ref()
        .context("managed registration missing")?;
    anyhow::ensure!(
        config.executable == crate::mcp::stable_bin_path(),
        "service executable differs from host configuration; explicitly reconfigure startup first"
    );
    let executable = crate::mcp::materialize_bin().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        executable == config.executable,
        "managed native path changed unexpectedly"
    );
    let current = manager::inspect(config)?;
    anyhow::ensure!(current.registered,"startup registration is missing; upgrade never registers it automatically; run runtime autostart enable explicitly");
    guard.mark(Phase::Starting)?;
    manager::start(
        guard
            .config
            .as_ref()
            .context("managed registration missing")?,
    )?;
    wait_ready_with_expectation(
        guard
            .config
            .as_ref()
            .context("managed registration missing")?,
        expectation,
    )
}

pub(crate) fn may_reuse_runtime(health: &crate::net_rpc::Health) -> Result<bool> {
    if background_paused() {
        return Ok(false);
    }
    match read_registration(&respire_client_host::paths::main_data_dir())? {
        Some(config) if config.enabled => Ok(
            !config.manually_stopped && health.managed_id.as_deref() == Some(config.id.as_str())
        ),
        _ => Ok(true),
    }
}

pub(crate) fn ensure_managed_locked() -> Result<bool> {
    ensure_managed_locked_with_expectation(respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
}

pub(crate) fn ensure_managed_locked_with_expectation(
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation) -> Result<bool> {
    let Some(config) = read_registration(&respire_client_host::paths::main_data_dir())? else {
        return Ok(false);
    };
    if !config.enabled {
        return Ok(false);
    }
    owned(&config)?;
    if config.manually_stopped {
        if let Some(token) = current_boot_token() {
            let value = marker(&config.installation)?.context("manual account maintenance marker is missing")?;
            anyhow::ensure!(value.id == config.id && value.token == token
                && value.owner_pid == std::process::id() && controller_alive(&value)?
                && matches!(value.phase, Phase::Starting | Phase::Verified),
                "manual detached launch lacks this host controller's startup authority");
            // A foreground maintenance helper may verify a target without enabling the service.
            return Ok(false);
        }
    }
    anyhow::ensure!(
        !config.manually_stopped,
        "runtime was manually stopped; use runtime start explicitly"
    );
    crate::runtime_fetch::require_automatic_migration_safe()?;
    if ACTIVE.with(|v| v.borrow().is_some()) {
        let value = marker(&config.installation)?.context("active maintenance marker missing")?;
        let digest = hash(&fs::read(marker_path(&config.installation))?);
        let mut borrowed = Maintenance {
            installation: Some(config.installation.clone()),
            config: Some(config),
            marker: Some(value),
            marker_digest: Some(digest),
            completed: true,
        };
        let result = (|| {
            raw_stop_owned(borrowed.config.as_ref().context("registration missing")?)?;
            start_candidate_with_expectation(&mut borrowed, expectation)
        })();
        // This borrowed guard does not own completion or its caller's token.
        borrowed.config = None;
        borrowed.installation = None;
        result?;
        return Ok(true);
    }
    check_direct_access()?;
    if let Some(health) = crate::rpc::probe_runtime()? {
        if crate::rpc::runtime_acceptable(&health)?
            && health.managed_id.as_deref() == Some(config.id.as_str())
        {
            expectation.confirm_credentials(&health)?;
            return Ok(true);
        }
    }
    let mut guard = Maintenance::begin(config, false)?;
    raw_stop_owned(guard.config.as_ref().context("registration missing")?)?;
    start_candidate_with_expectation(&mut guard, expectation)?;
    guard.finish()?;
    Ok(true)
}

fn archive_stopped_registration_locked(root: &Path, config: &Registration) -> Result<()> {
    crate::runtime_fetch::with_stopped_runtime_locked(|canonical| {
        anyhow::ensure!(marker(root)?.is_none(), "Runtime maintenance still owns the registration");
        let state = manager::inspect(config)?;
        anyhow::ensure!(
            !state.registered && !state.loaded && !state.running,
            "Runtime still has an OS startup registration"
        );
        let path = control_path(root);
        anyhow::ensure!(
            path.parent().context("Runtime registration directory missing")?.canonicalize()?
                == canonical.join("runtime/autostart") && fs::symlink_metadata(&path)?.is_file(),
            "invalid Runtime registration path"
        );
        let archive = path.with_file_name(format!("registration.stopped-{}.json", uuid::Uuid::new_v4()));
        respire_spawn::durable::publish_new_file(&path, &archive)
            .with_context(|| format!("could not archive Runtime registration at {}", archive.display()))
    })
}

pub(crate) fn prepare_cold_start_locked() -> Result<()> {
    check_direct_access()?;
    if crate::rpc::probe_runtime()?.is_some() {
        return Ok(());
    }
    let root = respire_client_host::paths::main_data_dir();
    match read_registration(&root) {
        Ok(Some(config)) if config.enabled => {
            let state = manager::inspect(&config)?;
            if !state.registered && !state.loaded && !state.running {
                return archive_stopped_registration_locked(&root, &config);
            }
        }
        Err(error) => {
            return archive_stopped_registration_locked(&root, &new_registration()?)
                .with_context(|| format!("could not archive obsolete Runtime registration: {error:#}"));
        }
        Ok(_) => {}
    }
    crate::runtime_fetch::archive_stopped_endpoint_locked()
}

pub(crate) fn prepare_cold_restart_locked() -> Result<bool> {
    let root = respire_client_host::paths::main_data_dir();
    if marker(&root)?.is_some() {
        crate::runtime_fetch::archive_stopped_endpoint_locked()?;
        return Ok(false);
    }
    prepare_cold_start_locked()?;
    let Some(config) = read_registration(&root)?.filter(|config| !config.enabled) else {
        return Ok(false);
    };
    let state = manager::inspect(&config)?;
    anyhow::ensure!(!state.enabled && !state.running,
        "Runtime startup registration is still active; stop it with its owning host");
    crate::runtime_fetch::with_stopped_runtime_locked(|_| Ok(()))?;
    Ok(true)
}

fn start_explicit_locked(takeover: &respire_client_host::lock::LibraryLock) -> Result<()> {
    let root = respire_client_host::paths::main_data_dir();
    super::supervise::offline_keygen::reject_generic_recovery(&root)?;
    prepare_cold_start_locked()?;
    respire_host_control::lifecycle_io::migrate_legacy_session_under_takeover(takeover)?;
    let Some(mut config) = read_registration(&root)? else {
        return crate::rpc::ensure_daemon_locked();
    };
    owned(&config)?;
    manager::inspect(&config)?;
    if !config.enabled {
        // Explicit start clears the stop flag without re-enabling sign-in startup.
        if config.manually_stopped {
            config.manually_stopped = false;
            save(&mut config)?;
        }
        return crate::rpc::ensure_daemon_locked();
    }
    if !config.manually_stopped {
        if let Some(health) = crate::rpc::probe_runtime()? {
            if health.managed_id.as_deref() == Some(config.id.as_str())
                && crate::rpc::runtime_acceptable(&health)?
            {
                super::supervise::apply_startup_core_config(&health, None)?;
                respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup.confirm_credentials(&health)?;
                reset_attempts(&config)?;
                return Ok(());
            }
        }
    }
    // Fence OS-triggered launches before clearing the persistent stop flag.
    let mut guard = Maintenance::begin(config, false)?;
    let config = guard
        .config
        .as_mut()
        .context("start registration missing")?;
    config.manually_stopped = false;
    save(config)?;
    reset_attempts(config)?;
    raw_stop_owned(guard.config.as_ref().context("registration missing")?)?;
    start_candidate(&mut guard)?;
    guard.finish()
}

pub(crate) fn stop_managed_locked() -> Result<bool> {
    let Some(mut config) = read_registration(&respire_client_host::paths::main_data_dir())? else {
        return Ok(false);
    };
    if !config.enabled {
        return Ok(false);
    }
    owned(&config)?;
    manager::inspect(&config)?;
    ensure_control_endpoint(&config)?;
    config.manually_stopped = true;
    save(&mut config)?;
    let mut guard = Maintenance::begin(config, false)?;
    raw_stop_owned(
        guard
            .config
            .as_ref()
            .context("managed registration missing")?,
    )?;
    guard.finish()?;
    Ok(true)
}
