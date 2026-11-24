fn run_foreground(id: Option<&str>, root: Option<&Path>, supervised_child: bool) -> Result<()> {
    anyhow::ensure!(
        !supervised_child || (cfg!(windows) && id.is_some()),
        "supervised-child requires a Windows managed registration"
    );
    if let Some(id) = id {
        let root = root.context("managed runtime needs its explicit installation root")?;
        let config = read_registration(root)?.context("managed runtime registration missing")?;
        owned(&config)?;
        anyhow::ensure!(config.id == id, "managed runtime identity mismatch");
        if !config.enabled || config.manually_stopped {
            return Ok(());
        }
        anyhow::ensure!(
            crate::mcp::same_path(&std::env::current_exe()?, &config.executable),
            "managed startup must use the configured stable native executable"
        );
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|key| {
                key.starts_with("RSRS_")
                    || key.starts_with("ONEMEMORY_")
                    || key.starts_with("RESPIRE_")
            }) {
                std::env::remove_var(key);
            }
        }
        for (key, value) in &config.environment {
            std::env::set_var(key, value);
        }
        std::env::set_var("HOME", &config.user_home);
        std::env::set_var("USERPROFILE", &config.user_home);
        #[cfg(windows)]
        if !supervised_child {
            return supervise_windows_runtime(&config);
        }
        if let Some(value) = marker(&config.installation)? {
            if value.phase == Phase::Paused || !controller_alive(&value)? {
                event(&config, "paused")?;
                return Ok(());
            }
        }
        if !record_attempt(&config, now_seconds()?)? {
            event(&config, "crash_limit")?;
            return Ok(());
        }
        event(&config, "starting")?;
        MANAGED_ID
            .set(id.to_owned())
            .map_err(|_| anyhow::anyhow!("managed runtime already initialized"))?;
    } else {
        anyhow::ensure!(
            root.is_none(),
            "installation-root is only accepted for a managed registration"
        );
        anyhow::ensure!(!read_registration(&respire_client_host::paths::main_data_dir())?.is_some_and(|config|config.enabled),"the service manager owns this runtime; use runtime start instead of an unmanaged foreground instance");
        check_direct_access()?;
    }
    verify_managed_boot()?;
    respire_host_control::lifecycle_io::migrate_legacy_session()?;
    let stopped = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        signal_hook::flag::register(signal_hook::consts::SIGTERM, stopped.clone())?;
        signal_hook::flag::register(signal_hook::consts::SIGINT, stopped.clone())?;
    }
    let binary = crate::rpc::resolve_runtime_executable()?.canonicalize()?;
    let image_sha256 = hash(&fs::read(&binary)?);
    let mut arguments: Vec<std::ffi::OsString> = vec![
        "serve".into(),
        "--data-dir".into(),
        respire_client_host::paths::main_data_dir().into_os_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        crate::net_rpc::rpc_port().to_string().into(),
    ];
    if managed_id().is_some() {
        arguments.push("--supervised".into());
    }
    let (mut child, instance_id) =
        super::supervise::spawn_authorized_runtime(&binary, &arguments, managed_id())?;
    let pid = child.id();
    let birth = manager::process_birth(pid)?;
    let mut drain: Option<std::thread::JoinHandle<Result<()>>> = None;
    let mut drain_requested = false;
    let mut credentials_checked = false;
    let mut configuration_checked = false;
    let result = (|| -> Result<()> {
        loop {
            if let Some(status) = child.try_wait().context("cannot reap foreground Runtime")? {
                if let Some(drain) = drain.take() {
                    drain.join().map_err(|_| anyhow::anyhow!("Runtime drain thread panicked"))??;
                }
                crate::set_exit_code(status.code().unwrap_or(1));
                return Ok(());
            }
            if (!credentials_checked || !configuration_checked) && !stopped.load(Ordering::Acquire) {
                if let Ok(Some(health)) = crate::rpc::probe_runtime() {
                    if health.pid == pid && health.instance_id == instance_id {
                        if !configuration_checked {
                            configuration_checked = super::supervise::configure_background_owner(&health)?;
                        }
                        if !credentials_checked {
                            credentials_checked = true;
                            if respire_host_control::lifecycle_io::unlock_if_needed(&health).is_err() {
                                eprintln!("host could not unlock Runtime; waiting for explicit host credentials");
                            }
                        }
                    }
                }
            }
            if drain.as_ref().is_some_and(|drain| drain.is_finished()) {
                if let Some(drain) = drain.take() {
                    drain.join().map_err(|_| anyhow::anyhow!("Runtime drain thread panicked"))??;
                }
            }
            if stopped.load(Ordering::Acquire) && !drain_requested {
                drain_requested = true;
                let binary = binary.clone();
                let image_sha256 = image_sha256.clone();
                let instance_id = instance_id.clone();
                let birth = birth.clone();
                // Reap on this thread while drain verifies process exit and the final receipt.
                drain = Some(std::thread::spawn(move || -> Result<()> {
                    let birth = birth.context("foreground Runtime exited before its birth was captured")?;
                    let deadline = Instant::now() + Duration::from_secs(10);
                    let health = loop {
                        if let Some(health) = crate::rpc::probe_runtime()? {
                            break health;
                        }
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "foreground Runtime has no verifiable health; no process was forced to exit"
                        );
                        std::thread::sleep(Duration::from_millis(50));
                    };
                    let process = crate::net_rpc::runtime_process_identity(pid)?;
                    anyhow::ensure!(
                        health.pid == pid
                            && health.instance_id == instance_id
                            && crate::mcp::exe_matches_dest(&process.executable, &binary)
                            && (process.started_at == birth || process.started_at == health.process_start_id)
                            && manager::process_birth(pid)?.as_deref() == Some(birth.as_str())
                            && crate::mcp::exe_matches_dest(&health.exe, &binary)
                            && crate::mcp::exe_matches_dest(&health.image_path, &binary)
                            && health.image_sha256.eq_ignore_ascii_case(&image_sha256)
                            && health.managed_id.as_deref() == managed_id(),
                        "foreground Runtime ownership changed; no drain requested"
                    );
                    crate::rpc::drain_occupant(&health, Duration::from_secs(120))
                }));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    if result.is_err() && child.try_wait()?.is_none() {
        child.kill().context("stop owned foreground Runtime after configuration failure")?;
        child.wait().context("reap owned foreground Runtime after configuration failure")?;
    }
    if managed_id().is_some() {
        let config = read_registration(&respire_client_host::paths::main_data_dir())?
            .context("managed runtime registration disappeared")?;
        event(
            &config,
            if result.is_ok() {
                "exit_ok"
            } else {
                "exit_error"
            },
        )?;
    }
    result
}

#[cfg(any(windows, test))]
fn retry_supervised_child(failures: &mut u8, uptime: Duration) -> bool {
    if uptime >= Duration::from_secs(120) {
        *failures = 0;
    }
    *failures = failures.saturating_add(1);
    *failures < 3
}

#[cfg(windows)]
fn supervisor_may_run(expected: &Registration) -> Result<bool> {
    let current = read_registration(&expected.installation)?
        .context("supervised registration disappeared")?;
    anyhow::ensure!(
        current.id == expected.id
            && current.owner == expected.owner
            && current.executable == expected.executable,
        "supervised registration identity changed"
    );
    if !current.enabled || current.manually_stopped {
        return Ok(false);
    }
    if let Some(value) = marker(&expected.installation)? {
        return Ok(value.phase != Phase::Paused
            && controller_alive(&value)?
            && value.id == expected.id
            && value.expected_version == env!("CARGO_PKG_VERSION"));
    }
    Ok(true)
}

#[cfg(windows)]
fn supervise_windows_runtime(config: &Registration) -> Result<()> {
    // Task Scheduler remains the current-user logon owner, but does not
    // reliably retry terminated Exec actions. Keep its action in the foreground
    // and supervise a foreground child; never open the library in this parent.
    respire_spawn::confine_supervisor_tree()
        .context("cannot establish owned supervisor process tree")?;
    let mut failures = 0;
    loop {
        if !supervisor_may_run(config)? {
            return Ok(());
        }
        let mut command = std::process::Command::new(&config.executable);
        command
            .args([
                "runtime",
                "run",
                "--managed",
                &config.id,
                "--installation-root",
            ])
            .arg(&config.installation)
            .arg("--supervised-child")
            .env_remove("RSRS_RUNTIME_BOOT_TOKEN")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let started = Instant::now();
        let status = match respire_spawn::spawn_host_control(&mut command) {
            Ok(mut child) => Some(
                child
                    .wait()
                    .context("cannot confirm owned runtime child exit")?,
            ),
            Err(_) => None,
        };
        if status.is_some_and(|status| status.success()) {
            return Ok(());
        }
        event(config, "exit_error")?;
        if !supervisor_may_run(config)? {
            return Ok(());
        }
        if !retry_supervised_child(&mut failures, started.elapsed()) {
            event(config, "crash_limit")?;
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if !supervisor_may_run(config)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}
