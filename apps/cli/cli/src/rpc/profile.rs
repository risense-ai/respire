use super::*;

pub fn stop_if_running() -> Result<()> {
    let _takeover = crate::runtime_policy::takeover_lock()?;
    if crate::autostart::stop_managed_locked()? {
        return Ok(());
    }
    if let Some(health) = probe_runtime()? {
        stop_occupant(health.pid)?;
    }
    Ok(())
}

/// Restart after an authenticated drain, or after verifying the port and library are free.
pub(crate) fn restart_runtime(timeout: Duration) -> Result<Value> {
    crate::runtime_policy::require_host("runtime restart")?;
    let takeover = crate::runtime_policy::takeover_lock()?;
    // An unavailable health response is not permission to discover or kill a PID.
    let health = probe_runtime().context(
        "runtime restart blocked: owner health is unavailable; no process was forced to exit",
    )?;
    // Verify the replacement before retiring any state or stopping a service.
    resolve_runtime_executable()?;
    // A pre-existing maintenance operation still needs its original receipt.
    let allow_missing_receipt = !crate::autostart::background_paused();
    let independent = health.is_none() && crate::autostart::prepare_cold_restart_locked()?;
    let maintenance = if independent {
        None
    } else {
        Some(crate::autostart::restart_maintenance_locked()?)
    };
    respire_host_control::lifecycle_io::migrate_legacy_session_under_takeover(&takeover)?;
    crate::autostart::restart_stage("restart_probe");
    let stopped_pid = health.as_ref().map(|value| value.pid);
    match health.as_ref() {
        Some(health) if !health.inference_stalled() && crate::autostart::managed_enabled()? => {
            crate::autostart::restart_stage("restart_managed");
            crate::autostart::drain_managed_for_restart(health, allow_missing_receipt)?;
        }
        health => {
            if let Some(health) = health {
                crate::autostart::restart_stage("restart_stop");
                drain_occupant_for_restart(health, timeout, allow_missing_receipt)?;
            }
            crate::autostart::restart_stage("restart_start");
        }
    }
    ensure_daemon_locked()?;
    let health = crate::net_rpc::health()?;
    if !respire_client_host::settings::off_mode()? && health.credential_state != "required" {
        verify_open_store(&respire_client_host::config::control_roots()?.1)?;
    }
    if let Some(mut maintenance) = maintenance {
        maintenance.finish()?;
        maintenance.finish_handoff()?;
    }
    let ready = crate::net_rpc::health()?;
    crate::autostart::restart_stage("restart_ready");
    Ok(json!({"state":"ready", "stopped_pid":stopped_pid, "pid":ready.pid,
        "forced":false, "data_dir":ready.data_dir, "version":ready.bin}))
}

/// Host profile changes release the old library before starting its successor.
pub fn recover_interrupted_login() -> Result<()> {
    recover_login_for(LoginRecoveryCaller::OrdinaryRequest)
}

/// Account control resumes its own operation, but first recovers old journals.
pub fn recover_legacy_login_before_account_control() -> Result<()> {
    recover_login_for(LoginRecoveryCaller::AccountControl)
}

#[derive(Clone, Copy)]
pub(super) enum LoginRecoveryCaller {
    OrdinaryRequest,
    AccountControl,
}

pub(super) fn needs_legacy_login_recovery(
    state: respire_protocol::host_control::HostLoginRecoveryState,
    caller: LoginRecoveryCaller,
) -> Result<bool> {
    use respire_protocol::host_control::HostLoginRecoveryState;
    match state {
        HostLoginRecoveryState::Clear => Ok(false),
        HostLoginRecoveryState::LegacyPending => Ok(true),
        HostLoginRecoveryState::OperationPending => match caller {
            LoginRecoveryCaller::AccountControl => Ok(false),
            LoginRecoveryCaller::OrdinaryRequest => anyhow::bail!(
                "authentication recovery requires the original operation; query its result before continuing"),
        },
    }
}

pub(super) fn recover_login_for(caller: LoginRecoveryCaller) -> Result<()> {
    respire_client_host::config::control_roots()?;
    // Presence is only a launch hint; Runtime alone parses the private journal.
    if !respire_client_host::paths::client_config_path()
        .with_file_name(".login-recovery.json").try_exists()? {
        return Ok(());
    }
    let takeover = crate::runtime_policy::takeover_lock()?;
    if !needs_legacy_login_recovery(crate::autostart::login_recovery_status(&takeover)?, caller)? {
        return Ok(());
    }
    let mut maintenance =
        crate::autostart::with_explicit_recovery(crate::autostart::begin_maintenance_locked)?;
    if let Some(health) = probe_runtime()? {
        maintenance.drain_runtime(&health)?;
    }
    let recovered = crate::autostart::recover_legacy_login_locked(&takeover, &mut maintenance)?;
    maintenance.resume_managed_runtime()?;
    maintenance.finish()?;
    maintenance.finish_handoff()?;
    if recovered {
        eprintln!(
            "Recovered an interrupted login; the original account and configuration were restored."
        );
    }
    Ok(())
}

/// Listener readiness alone is insufficient for a snapshot commit: the actor
/// must have opened, checked and bound the actual restored database first.
pub fn verify_snapshot_store(root: &std::path::Path, id: &str) -> Result<()> {
    verify_runtime_store(root, Some(id))
}
pub fn verify_open_store(root: &std::path::Path) -> Result<()> {
    verify_runtime_store(root, None)
}
pub(super) fn verify_runtime_store(root: &std::path::Path, id: Option<&str>) -> Result<()> {
    for _ in 0..START_POLLS {
        let health = crate::net_rpc::health()?;
        anyhow::ensure!(
            !health.data_dir.is_empty()
                && crate::mcp::same_path(std::path::Path::new(&health.data_dir), root),
            "snapshot runtime profile changed during verification"
        );
        if health.store_ready
            && id.is_none_or(|id| health.snapshot_restore_id.as_deref() == Some(id))
        {
            return Ok(());
        }
        std::thread::sleep(START_WAIT);
    }
    bail!("restored runtime did not confirm an opened and verified snapshot store")
}

pub fn change_profile(change: impl FnOnce() -> Result<()>) -> Result<()> {
    change_profile_with_rollback(change, || Ok(()))
}

/// Asset maintenance restores a running owner without starting an idle runtime.
pub fn change_host_assets(change: impl FnOnce() -> Result<()>) -> Result<()> {
    change_profile_inner(change, || Ok(()), || Ok(()), false)
}

pub fn change_profile_with_rollback(
    change: impl FnOnce() -> Result<()>,
    rollback: impl FnOnce() -> Result<()>,
) -> Result<()> {
    change_profile_with_verification(change, rollback, || Ok(()))
}

pub fn change_profile_with_verification(
    change: impl FnOnce() -> Result<()>,
    rollback: impl FnOnce() -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    change_profile_inner(change, rollback, verify, true)
}

pub(super) fn change_profile_inner(
    change: impl FnOnce() -> Result<()>,
    rollback: impl FnOnce() -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
    start_runtime: bool,
) -> Result<()> {
    crate::progress::phase("\u{534f}\u{8c03}\u{8d26}\u{6237}\u{64cd}\u{4f5c}\u{ff1b}\u{7b49}\u{5f85}\u{5f53}\u{524d} runtime \u{91ca}\u{653e}", "Coordinating account operation; waiting for current runtime to release");
    let _takeover = crate::runtime_policy::takeover_lock()?;
    let original_runtime = probe_runtime()?;
    anyhow::ensure!(
        original_runtime.is_some() || !endpoint_path().exists(),
        "profile change blocked: recorded owner has no verifiable health/drain receipt"
    );
    let was_running = original_runtime.is_some();
    let mut maintenance = crate::autostart::begin_maintenance_locked()?;
    let config_path = respire_client_host::paths::client_config_path();
    let original = match std::fs::read(&config_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let backup = if let Some(bytes) = original.as_ref() {
        let parent = config_path
            .parent()
            .context("configuration has no parent directory")?;
        let path = parent.join(format!(
            ".client-before-switch-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Some(path)
    } else {
        None
    };
    if let Some(health) = original_runtime.as_ref() {
        maintenance.drain_runtime(health)?;
    }
    let changed = change().and_then(|_| {
        if !start_runtime && !was_running {
            return Ok(());
        }
        crate::progress::phase("\u{542f}\u{52a8} runtime\u{ff1b}\u{6838}\u{5bf9}\u{8d26}\u{6237}\u{548c}\u{8def}\u{5f84}", "Starting runtime; verifying account and path");
        ensure_daemon_locked()
    }).and_then(|_| verify());
    if let Err(error) = changed {
        let cleanup = if start_runtime || was_running {
            probe_runtime().and_then(|health| match health {
                Some(health) => maintenance.drain_runtime(&health),
                None => Ok(()),
            })
        } else {
            Ok(())
        };
        let mut failures = Vec::new();
        if let Err(error) = &cleanup {
            failures.push(format!("target runtime cleanup failed: {error:#}"));
        }
        if let Err(error) = rollback() {
            failures.push(format!("account session rollback failed: {error:#}"));
        }
        // Startup failure must restore the original selection and all API fields,
        // including the distinction between an absent config and an empty one.
        let restored = match original {
            Some(bytes) => {
                let temporary = config_path.with_file_name(format!(
                    ".client-restore-{}.json",
                    uuid::Uuid::new_v4().simple()
                ));
                (|| -> Result<()> {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = options.open(&temporary)?;
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                    drop(file);
                    std::fs::rename(&temporary, &config_path)?;
                    Ok(())
                })()
            }
            None => match std::fs::remove_file(&config_path) {
                Ok(()) => Ok(()),
                Err(remove) if remove.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(remove) => Err(remove.into()),
            },
        };
        if let Err(error) = &restored {
            failures.push(format!("configuration rollback failed: {error:#}"));
        }
        if was_running && cleanup.is_ok() && restored.is_ok() {
            if let Err(error) = ensure_daemon_locked() {
                failures.push(format!("original runtime restart failed: {error:#}"));
            }
        }
        if !failures.is_empty() {
            if let Some(path) = backup {
                failures.push(format!("original configuration backup: {}", path.display()));
            }
            return Err(error.context(failures.join("; ")));
        }
        if let Some(path) = backup {
            if let Err(cleanup) = std::fs::remove_file(&path) {
                return Err(error.context(format!(
                    "configuration restored; backup retained at {}: {cleanup}",
                    path.display()
                )));
            }
        }
        maintenance.finish()?;
        maintenance.finish_handoff()?;
        return Err(error);
    }
    if let Some(path) = backup {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!(
                "profile verified; configuration backup retained at {}: {error}",
                path.display()
            );
        }
    }
    maintenance.finish()?;
    maintenance.finish_handoff()?;
    Ok(())
}
