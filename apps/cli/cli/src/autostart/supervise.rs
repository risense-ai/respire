//! Foreground supervisor started by the sign-in registration. It owns and
//! restarts the separate `rsrs-runtime` child process; the runtime binary
//! itself never registers, restarts or updates anything.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::{lifecycle, read_registration, Registration};

#[path = "launch_authorization.rs"]
mod launch_authorization;
pub(crate) use launch_authorization::{run_detached_plugin_host, spawn_detached_runtime};
mod auth_control;
pub(crate) use launch_authorization::shared_host::{accounts as rpc_accounts, keys as rpc_keys};
pub(crate) mod offline_keygen;
pub(crate) use launch_authorization::migration_control::control_migrate_profile;
pub(crate) use launch_authorization::metadata_control::{control_metadata, control_offline_read, discover_legacy_snapshot};
pub(crate) use launch_authorization::agent_config_control::{control_agent_config, control_core_config_result, control_core_config_rpc, CoreConfigUnknown};
pub(crate) use launch_authorization::agent_config_control::{apply_startup_core_config, startup_core_config_ready};
pub(crate) use launch_authorization::key_control::{
    control_super_reset, control_super_reset_step, control_key_result, control_key_rollback,
    control_fivekeys,
    KeyRecoveryRequired,
};
pub(crate) use launch_authorization::identity_write_control::{control_logout, control_session_revoke, IdentityWriteRecoveryRequired};
pub(crate) use launch_authorization::space_control::{control_space_invite, control_space_join, control_space_kick};
pub(crate) use launch_authorization::profile_maintenance::{control_account_remove, control_space_remove};
pub(crate) use auth_control::{control_account_result, control_account_rollback,
    control_auth_result, control_keygen, control_login_step, control_register};
pub(crate) use launch_authorization::secret_control::{control_secret, control_keys_export, control_session_list};
pub(super) use launch_authorization::control::{installed_auth_runtime, verify_account_target};
#[cfg(feature = "client2-host-prototype")]
pub(super) use launch_authorization::control::identity as handoff_identity;
#[cfg(feature = "client2-host-prototype")]
pub(super) use launch_authorization::model_control::{installed_model_runtime, verify_model_owner};
pub(crate) use launch_authorization::control::{login_recovery_status, recover_legacy_login_locked};
pub(crate) use launch_authorization::model_control::control_prepare_model;
pub(crate) use launch_authorization::model_maintenance::{
    control_install_engines, control_reset_cpu, control_uninstall_m3,
};
pub(crate) use launch_authorization::config_control::{run_config_control, run_profile_config_control, run_space_config_control};
pub(crate) use launch_authorization::exchange_control as exchange_snapshot_control;
pub(crate) use launch_authorization::prepare_grant_for_identity as prepare_snapshot_grant;
pub(crate) use launch_authorization::control::{
    validate_account_receipt as validate_auth_account_receipt, AccountOutcomeUnknown, AuthOutcomeUnknown,
};
#[cfg(test)]
use launch_authorization::control::{
    account_request as prepare_auth_account_request, AccountControl as AuthAccountControl,
};

/// Delay between a failed child attempt and the next start.
const RESTART_DELAY: Duration = Duration::from_secs(60);
/// Child uptime after which the consecutive-failure counter resets.
const STABLE_UPTIME_RESET: Duration = Duration::from_secs(120);
/// Consecutive short-lived failures that trip the circuit breaker.
const MAX_CONSECUTIVE_FAILURES: u8 = 3;

/// Registration entry point (`rsrs runtime supervise --data-dir <root>`).
/// Resolves the managed runtime, spawns `rsrs-runtime serve --supervised` and
/// keeps it alive with a bounded retry budget.
pub(crate) fn supervise(data_dir: &Path) -> Result<()> {
    let root = data_dir
        .canonicalize()
        .with_context(|| format!("invalid --data-dir {}", data_dir.display()))?;
    let config = read_registration(&root)?
        .context("no startup registration for this installation; enable runtime autostart first")?;
    lifecycle::owned(&config)?;
    let registered_root = PathBuf::from(
        config
            .environment
            .get("RSRS_DATA_DIR")
            .context("registered runtime root is missing")?,
    );
    anyhow::ensure!(
        registered_root.canonicalize()? == root,
        "registered runtime root belongs to another installation"
    );
    // Align this process with the registration environment before resolving
    // the runtime, exactly like the managed foreground run does.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| {
            key.starts_with("RSRS_") || key.starts_with("ONEMEMORY_") || key.starts_with("RESPIRE_")
        }) {
            std::env::remove_var(key);
        }
    }
    for (key, value) in &config.environment {
        std::env::set_var(key, value);
    }
    // OS-launched hosts must use the registered home's native credential store.
    std::env::set_var("HOME", &config.user_home);
    std::env::set_var("USERPROFILE", &config.user_home);
    // Runtime commands bypass the normal CLI bootstrap. Initialize only after
    // the registration has established this supervisor's owned data root.
    super::super::install_error_logging();
    // Parent-driven launches migrated under takeover before waiting for this child.
    respire_host_control::lifecycle_io::migrate_legacy_session()?;
    // The registered task is the foreground owner. A kill-on-close job keeps a
    // dead supervisor from orphaning the runtime child (Windows 8+ nested jobs
    // preserve the enclosing Task Scheduler job).
    #[cfg(windows)]
    respire_spawn::confine_supervisor_tree()
        .context("cannot establish owned supervisor process tree")?;
    // Unix delivery forwards the signal to the child and exits without
    // counting the shutdown against the retry budget.
    #[cfg(unix)]
    let stopped = {
        let stopped = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTERM, stopped.clone())?;
        signal_hook::flag::register(signal_hook::consts::SIGINT, stopped.clone())?;
        stopped
    };
    #[cfg(not(unix))]
    let stopped = Arc::new(AtomicBool::new(false));
    let mut failures: u8 = 0;
    loop {
        if !supervise_may_run(&root, &config)? {
            return Ok(());
        }
        let started = Instant::now();
        let binary = match resolve_child_plan() {
            Ok(binary) => binary,
            Err(error) => {
                respire_logging::error(format_args!("runtime resolution failed"));
                eprintln!("runtime resolution failed: {error:#}");
                if error.is::<crate::runtime_fetch::ManagedRuntimeRepairRequired>() {
                    lifecycle::record_supervise_event(&config, "exit_error");
                    return Err(error);
                }
                if !failure_backoff(&config, &root, &mut failures, Duration::ZERO)? {
                    return Err(error);
                }
                continue;
            }
        };
        lifecycle::record_supervise_event(&config, "starting");
        let arguments: Vec<std::ffi::OsString> = vec![
            std::ffi::OsString::from("serve"),
            std::ffi::OsString::from("--data-dir"),
            registered_root.clone().into_os_string(),
            std::ffi::OsString::from("--supervised"),
        ];
        let outcome = spawn_authorized_runtime(&binary, &arguments, Some(&config.id))
            .map(|(mut child, _instance_id)| wait_child(&mut child, &stopped));
        let uptime = started.elapsed();
        match outcome {
            // Signalled shutdown: the supervisor exits without retrying.
            Ok(Ok(None)) => return Ok(()),
            Ok(Ok(Some(status))) if status.success() => {
                lifecycle::record_supervise_event(&config, "exit_ok");
                return Ok(());
            }
            Ok(Ok(Some(_))) => {
                if !failure_backoff(&config, &root, &mut failures, uptime)? {
                    return Ok(());
                }
            }
            Ok(Err(error)) => {
                respire_logging::error(format_args!(
                    "runtime child supervision failed"
                ));
                eprintln!("runtime child supervision failed: {error:#}");
                if !failure_backoff(&config, &root, &mut failures, uptime)? {
                    return Ok(());
                }
            }
            Err(error) => {
                respire_logging::error(format_args!(
                    "runtime child could not be started"
                ));
                eprintln!("runtime child could not be started: {error:#}");
                if !failure_backoff(&config, &root, &mut failures, uptime)? {
                    return Ok(());
                }
            }
        }
    }
}

/// Both detached and supervised children use the same single-use startup grant.
pub(crate) fn spawn_authorized_runtime(
    binary: &Path,
    arguments: &[std::ffi::OsString],
    managed_id: Option<&str>,
) -> Result<(std::process::Child, String)> {
    launch_authorization::spawn(binary, arguments, managed_id)
}

pub(crate) fn control_memory_revision() -> Result<respire_commands::output::ResultEnvelope> {
    launch_authorization::control_memory_revision()
}

/// Parent-driven startup completes configuration under its own maintenance lock.
pub(super) fn configure_background_owner(owner: &crate::net_rpc::Health) -> Result<bool> {
    let root = respire_client_host::config::control_roots()?.0;
    let marker = root.join("runtime/autostart/maintenance.json");
    if marker.try_exists()? { return Ok(false); }
    // A stop owner holds takeover while waiting for this supervisor to exit.
    let _takeover = match crate::runtime_policy::try_takeover_lock() {
        Ok(ownership) => ownership,
        Err(error) if matches!(error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)) => return Ok(false),
        Err(error) => return Err(error),
    };
    if marker.try_exists()? { return Ok(false); }
    // An external stop may finish after the caller's health probe; let it reap the child.
    let Some(current) = crate::rpc::probe_runtime()? else { return Ok(false); };
    anyhow::ensure!(current.pid == owner.pid && current.process_start_id == owner.process_start_id
        && crate::net_rpc::request_scope(&current)? == crate::net_rpc::request_scope(owner)?,
        "background core owner changed");
    apply_startup_core_config(&current, None)?;
    Ok(true)
}

/// Wait for the child, honoring a signal flag. `Ok(None)` means the supervisor
/// was signalled and the child has been stopped.
fn wait_child(
    child: &mut std::process::Child,
    stopped: &AtomicBool,
) -> Result<Option<std::process::ExitStatus>> {
    let mut credentials_checked = false;
    let mut configuration_checked = false;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if stopped.load(Ordering::Acquire) {
            let _ = child.kill();
            child
                .wait()
                .context("cannot reap the stopped runtime child")?;
            return Ok(None);
        }
        if !credentials_checked || !configuration_checked {
            if let Ok(Some(health)) = crate::rpc::probe_runtime() {
                if health.pid == child.id() {
                    if !configuration_checked {
                        match configure_background_owner(&health) {
                            Ok(checked) => configuration_checked = checked,
                            Err(error) => {
                                child.kill().context("stop owned Runtime after core configuration failure")?;
                                child.wait().context("reap owned Runtime after core configuration failure")?;
                                return Err(error);
                            }
                        }
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
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The supervisor only starts independent Runtime images. Legacy compatibility
/// remains available through the explicit CLI foreground entry.
fn resolve_child_plan() -> Result<PathBuf> {
    let binary = crate::rpc::resolve_runtime_executable()?;
    anyhow::ensure!(
        !crate::rpc::explicit_cli_runtime(&binary)?,
        "supervision requires the independent Runtime; use --runtime-internal explicitly for legacy CLI compatibility"
    );
    Ok(binary)
}

/// Record the failure, keep the restart budget bounded and sleep out the retry
/// delay while still honoring disablement. `Ok(false)` stops the supervisor.
fn failure_backoff(
    config: &Registration,
    root: &Path,
    failures: &mut u8,
    uptime: Duration,
) -> Result<bool> {
    lifecycle::record_supervise_event(config, "exit_error");
    if !supervise_may_run(root, config)? {
        return Ok(false);
    }
    if !supervise_retry(failures, uptime) {
        lifecycle::record_supervise_event(config, "crash_limit");
        return Ok(false);
    }
    let deadline = Instant::now() + RESTART_DELAY;
    while Instant::now() < deadline {
        if !supervise_may_run(root, config)? {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(true)
}

/// Continue only while the registration still exists unchanged, enabled and
/// not manually stopped. Identity changes fail loudly instead of silently
/// abandoning a runtime we no longer own.
fn supervise_may_run(root: &Path, expected: &Registration) -> Result<bool> {
    let Some(current) = read_registration(root)? else {
        return Ok(false);
    };
    anyhow::ensure!(
        current.id == expected.id
            && current.owner == expected.owner
            && current.executable == expected.executable,
        "supervised registration identity changed"
    );
    Ok(current.enabled && !current.manually_stopped)
}

/// Bounded retry budget: three consecutive short-lived failures trip the
/// circuit breaker; stable uptime resets the counter.
fn supervise_retry(failures: &mut u8, uptime: Duration) -> bool {
    if uptime >= STABLE_UPTIME_RESET {
        *failures = 0;
    }
    *failures = failures.saturating_add(1);
    *failures < MAX_CONSECUTIVE_FAILURES
}

#[cfg(test)]
mod tests;
