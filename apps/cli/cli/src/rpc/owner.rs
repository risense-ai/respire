use super::*;

pub(crate) fn endpoint_path() -> PathBuf {
    respire_client_host::paths::main_data_dir().join("runtime/endpoint.json")
}

pub(super) fn endpoint_pid() -> Option<u32> {
    let data: Value = serde_json::from_slice(&std::fs::read(endpoint_path()).ok()?).ok()?;
    data.get("pid")?.as_u64().and_then(|pid| u32::try_from(pid).ok())
}

pub(crate) fn probe_runtime() -> Result<Option<crate::net_rpc::Health>> {
    match crate::net_rpc::health() {
        Ok(health) => Ok(Some(health)),
        Err(error) if matches!(error.downcast_ref::<crate::runtime_error::RuntimeError>(),
            Some(crate::runtime_error::RuntimeError::Unavailable)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Recovery must not wait on RPC, model locks or a library lock.
pub(crate) fn force_stop_for_reset() -> Result<Option<u32>> {
    let port = crate::net_rpc::rpc_port();
    let Some(pid) = crate::net_rpc::pid_listening_on(port) else {
        if crate::net_rpc::port_is_open() {
            bail!("CPU saved, but cannot identify runtime on port {port}");
        }
        return Ok(None);
    };
    if pid == std::process::id() || !crate::net_rpc::pid_is_respire(pid) {
        bail!("CPU saved; refusing to terminate non-runtime process {pid} on port {port}");
    }
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    #[cfg(unix)]
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    if !status.success() {
        bail!("CPU saved, but failed to terminate runtime {pid}");
    }
    crate::net_rpc::wait_until_down().context("CPU saved, but runtime port is still occupied")?;
    if endpoint_pid() == Some(pid) && endpoint_path().exists() {
        std::fs::remove_file(endpoint_path())?;
    }
    Ok(Some(pid))
}

pub(crate) fn ensure_daemon() -> Result<()> {
    if crate::runtime_policy::client_only() {
        crate::net_rpc::health()?;
        return Ok(());
    }
    if let Some(health) = probe_runtime()? {
        if reusable_business_runtime(&health)? {
            if !health.store_ready {
                let (_, profile) = respire_client_host::config::control_roots()?;
                if profile.join("session.json").try_exists()? {
                    respire_host_control::lifecycle_io::unlock_if_needed(&health)?;
                }
            }
            return Ok(());
        }
        if runtime_acceptable(&health)? {
            if crate::autostart::startup_core_config_ready(&health)? {
                respire_host_control::lifecycle_io::unlock_if_needed(&health)?;
                return Ok(());
            }
            // Only unfinished startup configuration needs serialization with setters.
            let _takeover = crate::runtime_policy::takeover_lock()?;
            let health = probe_runtime()?.context("Runtime owner exited before core configuration")?;
            anyhow::ensure!(runtime_acceptable(&health)?, "Runtime owner changed before core configuration");
            crate::autostart::apply_startup_core_config(&health, None)?;
            respire_host_control::lifecycle_io::unlock_if_needed(&health)?;
            return Ok(());
        }
    }
    let takeover = crate::runtime_policy::takeover_lock()?;
    if let Some(health) = probe_runtime()? {
        if reusable_business_runtime(&health)? {
            return Ok(());
        }
    }
    crate::autostart::prepare_cold_start_locked()?;
    respire_host_control::lifecycle_io::migrate_legacy_session_under_takeover(&takeover)?;
    ensure_daemon_locked()
}

/// Business RPC can reuse a scoped service without selecting its executable.
pub(super) fn reusable_business_runtime(health: &crate::net_rpc::Health) -> Result<bool> {
    if !(health.store_ready || health.credential_state == "required")
        || health.inference_stalled() || health.installation_root.is_empty()
        || health.data_dir.is_empty() || health.v != PROTOCOL_V
    {
        return Ok(false);
    }
    let (root, profile) = respire_client_host::config::control_roots()?;
    if !crate::mcp::same_path(std::path::Path::new(&health.installation_root), &root)
        || !crate::mcp::same_path(std::path::Path::new(&health.data_dir), &profile)
    {
        return Ok(false);
    }
    crate::net_rpc::request_scope(health)?;
    Ok(true)
}

pub(crate) fn runtime_acceptable(health: &crate::net_rpc::Health) -> Result<bool> {
    let override_bin = respire::env::var("RSRS_RUNTIME_BIN")
        .ok()
        .map(|raw| std::path::PathBuf::from(raw.trim()))
        .filter(|path| !path.as_os_str().is_empty());
    let managed = if override_bin.is_none() {
        crate::runtime_fetch::managed_runtime_path()?
    } else {
        None
    };
    let portable = std::env::current_exe()?
        .parent()
        .map(|directory| directory.join(crate::runtime_fetch::RUNTIME_BINARY))
        .filter(|path| path.is_file());
    let Some(expected) = override_bin.or(managed).or(portable) else {
        return Ok(false);
    };
    if crate::embedding::selection().is_none()
        && crate::mcp::exe_matches_dest(&health.exe, &std::env::current_exe()?)
    {
        return Ok(false);
    }
    Ok(!health.data_dir.is_empty()
        && crate::mcp::same_path(
            std::path::Path::new(&health.data_dir),
            &respire_client_host::config::control_roots()?.1,
        )
        && crate::autostart::may_reuse_runtime(health)?
        && health.v == PROTOCOL_V
        && crate::mcp::exe_matches_dest(&health.exe, &expected)
        && !health.inference_stalled())
}

pub(crate) fn ensure_daemon_locked() -> Result<()> {
    ensure_daemon_locked_with_expectation(respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
}

pub(crate) fn ensure_daemon_locked_with_expectation(
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation,
) -> Result<()> {
    ensure_daemon_locked_inner(expectation)?;
    // An active controller applies under Paused in Maintenance::finish instead.
    if crate::autostart::lifecycle_boot_token().is_none() {
        let owner = probe_runtime()?.context("started Runtime owner disappeared before core configuration")?;
        crate::autostart::apply_startup_core_config(&owner, None)?;
    }
    Ok(())
}

pub(super) fn ensure_daemon_locked_inner(
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation,
) -> Result<()> {
    if crate::autostart::ensure_managed_locked_with_expectation(expectation)? {
        return Ok(());
    }
    crate::autostart::check_direct_access()?;
    let current = probe_runtime()?;
    if let Some(health) = &current {
        if runtime_acceptable(health)? {
            expectation.confirm_credentials(health)?;
            return Ok(());
        }
    }
    // Resolve before stopping the resident owner: a missing or damaged release
    // must fail without replacing it with a CLI shim.
    let executable = resolve_runtime_executable()?;
    if current.is_none() {
        crate::runtime_fetch::archive_stopped_endpoint_locked()?;
    }
    if let Some(health) = current {
        if health.inference_stalled() {
            anyhow::ensure!(
                crate::net_rpc::pid_is_respire(health.pid),
                "cannot verify the unresponsive runtime process; no lifecycle action taken"
            );
            crate::progress::phase("\u{63a8}\u{7406}\u{65e0}\u{54cd}\u{5e94}\u{ff1b}\u{5bbf}\u{4e3b}\u{6b63}\u{5728}\u{6062}\u{590d} runtime", "Inference unresponsive; host recovering runtime");
        }
        stop_occupant(health.pid)?;
    }
    crate::autostart::authorize_detached_boot_locked()?;
    let arguments: Vec<std::ffi::OsString> = vec![
        "serve".into(),
        "--data-dir".into(),
        respire_client_host::paths::main_data_dir().into_os_string(),
    ];
    let (mut child, runtime_pid, instance_id) =
        crate::autostart::spawn_detached_runtime(&executable, &arguments)
            .context("failed to start detached Runtime")?;
    let result = wait_daemon_ready(&mut child, runtime_pid, &executable, &instance_id, expectation);
    if result.is_err() && child.try_wait()?.is_none() {
        // Do not leave a delayed startup racing the next lock holder.
        child
            .kill()
            .context("failed to stop unready host runtime")?;
        child
            .wait()
            .context("failed to reap unready host runtime")?;
    }
    result
}

pub(crate) fn stop_occupant(pid: u32) -> Result<()> {
    let health = crate::net_rpc::health()?;
    anyhow::ensure!(health.pid == pid, "runtime owner changed before shutdown");
    drain_occupant(&health, Duration::from_secs(60))
}

pub(crate) fn drain_occupant(health: &crate::net_rpc::Health, timeout: Duration) -> Result<()> {
    drain_occupant_for_restart(health, timeout, false)
}

pub(crate) fn drain_occupant_for_restart(
    health: &crate::net_rpc::Health,
    timeout: Duration,
    allow_missing_receipt: bool,
) -> Result<()> {
    let pid = health.pid;
    let (installation_root, profile) = respire_client_host::config::control_roots()?;
    anyhow::ensure!(
        crate::mcp::same_path(
            std::path::Path::new(&health.data_dir),
            &profile,
        ) && crate::mcp::same_path(
            std::path::Path::new(&health.installation_root),
            &installation_root,
        ),
        "runtime restart blocked: owner roots do not match this installation"
    );
    let birth = crate::autostart::process_birth(pid)?.context("runtime process already exited")?;
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    anyhow::ensure!(
        health.process_start_id == format!("{prefix}{birth}"),
        "runtime process start identity does not match health"
    );
    let operation_id = crate::net_rpc::request_stop_for(health, Duration::from_secs(120))
        .context("runtime drain stop request failed")?;
    let deadline = Instant::now() + timeout;
    loop {
        if crate::autostart::process_birth(pid)?.as_deref() != Some(birth.as_str()) {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "runtime drain is still pending; no process was forced to exit"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let receipt = crate::net_rpc::read_final_stop_receipt(health, Some(&operation_id))
        .context("runtime drain final receipt verification failed")?;
    if receipt.is_none() {
        anyhow::ensure!(allow_missing_receipt,
            "runtime drain final receipt verification failed: final runtime stop receipt is missing");
        anyhow::ensure!(respire_client_host::config::control_roots()? == (installation_root, profile),
            "runtime selection changed before verifying the stopped library");
        respire_host_control::lifecycle_io::verify_no_private_recovery_for_restart()?;
        crate::runtime_fetch::with_stopped_runtime_locked(|_| Ok(()))
            .context("Runtime exited without a final stop receipt, but its listener or locks could not be verified free")?;
        eprintln!("Runtime exited without a final stop receipt; its listener and library locks were verified free.");
        return Ok(());
    }
    crate::net_rpc::wait_until_down()
        .context("runtime drain listener did not close")?;
    let released = respire::lock::LibraryLock::acquire(
        std::path::Path::new(&health.data_dir),
        Duration::from_secs(15),
    )
    .context("stopped runtime has not released its library lock")?;
    // Keep the library lock until the verified dead owner's locator is removed.
    if endpoint_path().exists() {
        anyhow::ensure!(
            endpoint_pid() == Some(pid) && crate::autostart::process_birth(pid)?.is_none(),
            "runtime endpoint owner changed after drain; record was not removed"
        );
        std::fs::remove_file(endpoint_path())
            .context("runtime drain endpoint removal failed")?;
    }
    drop(released);
    Ok(())
}

pub(super) fn wait_daemon_ready(
    child: &mut std::process::Child,
    runtime_pid: u32,
    executable: &std::path::Path,
    instance_id: &str,
    expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation,
) -> Result<()> {
    let off_mode = respire_client_host::settings::off_mode()?;
    let mut last_store_ready = None;
    let deadline = Instant::now() + START_WAIT * START_POLLS as u32;
    while Instant::now() < deadline {
        std::thread::sleep(START_WAIT);
        if let Some(status) = child.try_wait()? {
            bail!("host runtime exited before readiness: {status} (off_mode={off_mode}, last_store_ready={last_store_ready:?})");
        }
        if let Some(health) = probe_runtime()? {
            last_store_ready = Some(health.store_ready);
            let birth = crate::autostart::process_birth(runtime_pid)?
                .context("starting runtime process exited")?;
            let birth_prefix = if cfg!(windows) {
                "windows-filetime-"
            } else if cfg!(target_os = "linux") {
                "linux-starttime-"
            } else {
                "darwin-lstart-"
            };
            anyhow::ensure!(
                health.pid == runtime_pid
                    && health.process_start_id == format!("{birth_prefix}{birth}")
                    && health.v == PROTOCOL_V
                    && health.generation > 0
                    && !health.account_id.is_empty()
                    && health.instance_id == instance_id
                    && crate::mcp::exe_matches_dest(&health.exe, executable)
                    && health.v == PROTOCOL_V
                    && !health.data_dir.is_empty()
                    && crate::mcp::same_path(
                        std::path::Path::new(&health.data_dir),
                        &respire_client_host::config::control_roots()?.1
                    ),
                "runtime owner changed during startup"
            );
            if health.store_ready || off_mode || health.credential_state == "required" {
                expectation.confirm_credentials(&health)?;
                return Ok(());
            }
        }
    }
    bail!("host runtime did not become ready (off_mode={off_mode}, last_store_ready={last_store_ready:?})")
}
