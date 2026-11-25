/// The durable endpoint uses the same process/account identity as live health.
pub(crate) fn endpoint_identity(url: &str) -> Result<Value> {
    validate_process_identity()?;
    let process = process_identity();
    let roots = respire_app::service::resolved_roots()?;
    Ok(json!({
        "schema_version": 2, "server": "respire", "v": PROTOCOL_V,
        "pid": std::process::id(), "bin": bin_version(), "url": url,
        "runtime_version": runtime_identity().runtime_version,
        "exe": process.image_path, "image_path": process.image_path,
        "image_sha256": process.image_sha256,
        "installation_root": roots.installation_root,
        "data_dir": roots.account_data_root,
        "installation_id": process.installation_id, "instance_id": process.instance_id,
        "process_start_id": process.process_start_id,
        "account_id": account_identity(), "generation": crate::rpc_worker::wire_generation(),
        "managed_id": crate::host_hooks::managed_id(),
    }))
}

pub fn startup_complete() -> bool {
    STARTUP_COMPLETE.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn mark_http_fixture_ready() {
    STARTUP_COMPLETE.store(true, Ordering::Release);
}

/// Borrow the authoritative local owner and scope while the serving identity
/// and unlocked vault are fenced. Callers must not re-enter session I/O.
pub(crate) fn with_verified_owner_scope<T>(
    action: impl FnOnce(&str, &std::path::Path, uuid::Uuid, u64, uuid::Uuid) -> T,
) -> Option<T> {
    try_verified_owner_scope(action).ok().flatten()
}

pub(crate) fn try_verified_owner_scope<T>(
    action: impl FnOnce(&str, &std::path::Path, uuid::Uuid, u64, uuid::Uuid) -> T,
) -> std::result::Result<Option<T>, ()> {
    let _identity = match SCOPE_EXECUTION.try_read() {
        Ok(identity) => identity,
        Err(std::sync::TryLockError::WouldBlock) => return Err(()),
        Err(std::sync::TryLockError::Poisoned(_)) => return Ok(None),
    };
    Ok(with_locked_owner_scope(action))
}

pub(crate) fn with_verified_owner_store_scope<T>(
    action: impl FnOnce(&str, &std::path::Path, uuid::Uuid, u64, uuid::Uuid, respire_app::auth::RuntimeLibrary<'_>) -> T,
) -> Option<T> {
    with_owner_store_fence(|| with_locked_library(action)).flatten()
}

// Long local model work retains the identity and store fences while releasing
// the vault mutex between checkpoints so operation status and cancel stay live.
pub(crate) fn with_owner_store_fence<T>(action: impl FnOnce() -> T) -> Option<T> {
    let _identity = SCOPE_EXECUTION.try_read().ok()?;
    let writer = crate::rpc_worker::shared_exclusive();
    let _foreground = writer.try_owner_foreground()?;
    Some(action())
}

fn with_locked_owner_scope<T>(
    action: impl FnOnce(&str, &std::path::Path, uuid::Uuid, u64, uuid::Uuid) -> T,
) -> Option<T> {
    with_locked_library(|owner, profile, instance, generation, epoch, _| {
        action(owner, profile, instance, generation, epoch)
    })
}

pub(crate) fn with_locked_library<T>(
    action: impl FnOnce(&str, &std::path::Path, uuid::Uuid, u64, uuid::Uuid, respire_app::auth::RuntimeLibrary<'_>) -> T,
) -> Option<T> {
    if !startup_complete() || !store_ready() || business_paused()
        || respire_app::service::off_mode()
    {
        return None;
    }
    validate_business_config().ok()?;
    crate::host_hooks::check_direct_access().ok()?;
    let profile = respire_app::service::data_dir().canonicalize().ok()?;
    respire_app::service::ensure_runtime_profile().ok()?;
    respire_app::snapshot::ensure_no_incomplete_restore(&profile).ok()?;
    let instance = uuid::Uuid::parse_str(&process_identity().instance_id).ok()?;
    let generation = crate::rpc_worker::wire_generation();
    respire_app::auth::with_runtime_library(&profile, |owner, vault_epoch, library| {
        action(owner, &profile, instance, generation, vault_epoch, library)
    }).ok().flatten()
}

pub(crate) fn v1_health_identity() -> Option<(&'static str, String)> {
    let status = match health_state() {
        "draining" | "stopping" | "stopped" => return None,
        _ if !startup_complete() => "starting",
        "ready" => "ready",
        _ => "degraded",
    };
    Some((status, process_identity().instance_id))
}

pub(crate) fn v1_host_account_status() -> Result<Value> {
    let _identity = SCOPE_EXECUTION.try_read()
        .map_err(|_| anyhow::anyhow!("runtime identity is changing"))?;
    anyhow::ensure!(startup_complete() && v1_health_identity().is_some(), "runtime identity unavailable");
    validate_business_config().map_err(anyhow::Error::msg)?;
    crate::host_hooks::check_direct_access()?;
    respire_app::service::ensure_runtime_profile()?;
    let profile = respire_app::service::data_dir().canonicalize()?;
    respire_app::login_transaction::host_account_status(&profile)
}

pub fn health_body() -> Value {
    let roots = match respire_app::service::resolved_roots()
        .and_then(|roots| serde_json::to_value(roots).map_err(anyhow::Error::from))
    {
        Ok(roots) => roots,
        Err(_) => return json!({
            "server": "respire", "v": PROTOCOL_V, "state": "error",
            "error": {"code": "roots_resolution_failed"},
        }),
    };
    let provider_status = match respire_app::credential_io::provider_status() {
        Ok(status) => status,
        Err(_) => return json!({"server":"respire", "v":PROTOCOL_V, "state":"error",
            "error":{"code":"provider_state_unavailable"}}),
    };
    let inference = inference_health_status();
    let (queue_cancelled, timings) = crate::admission::timings();
    let (queued, executing) = crate::admission::occupancy();
    let identity = runtime_identity();
    let process = process_identity();
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut body = json!({
        "server": "respire",
        "bin": bin_version(),
        "pid": std::process::id(),
        "state": health_state(),
        "v": PROTOCOL_V,
        "url": current_url(),
        "exe": exe,
        "data_dir": roots["account_data_root"],
        "resolved_roots": roots,
        "store_ready": STORE_READY.load(Ordering::Acquire),
        "installation_root": roots["installation_root"],
        "managed_id": crate::host_hooks::managed_id(),
        "installation_id": process.installation_id,
        "instance_id": process.instance_id,
        "process_start_id": process.process_start_id,
        "image_path": process.image_path,
        "image_sha256": process.image_sha256,
        "account_id": account_identity(),
        "generation": crate::rpc_worker::wire_generation(),
        "protocol_range": {"minimum": PROTOCOL_V, "maximum": PROTOCOL_V},
        "capabilities": ["business_executor", "mcp_server"],
        "credential_state": if STORE_READY.load(Ordering::Acquire) { "available" } else { "required" },
        "snapshot_restore_id": SNAPSHOT_BOOT_ID.lock().ok().and_then(|slot|slot.clone()),
        "recall_statistics": recall_stats_status(),
        "retrieval_index": index_status(),
        "inference": inference,
        // Split-era identity: the extension lives under the `runtime` key per
        // the protocol contract; the flattened runtime_version keeps the
        // existing Health consumers (pin checks, shell banner) working.
        "runtime": identity.health_extension_json(),
        "control": {
            "state": control_state(),
            "operation_id": STOP_OPERATION.get(),
            "admitted": admitted_list(),
            "background": busy_background(),
            "unfinished": stop_unfinished(),
        },
        "runtime_version": identity.runtime_version,
        "rpc": {
            "available_parallelism": cpu_count(),
            "effective_parallelism": WORKERS.load(Ordering::Acquire),
            "ordinary_running": RUNNING.load(Ordering::Acquire).saturating_sub(LIGHT_RUNNING.load(Ordering::Acquire)),
            "lightweight_running": LIGHT_RUNNING.load(Ordering::Acquire),
            "lightweight_capacity": 2,
            "queue_wait_timeout_secs": queue_wait().as_secs(),
            "queue_cancelled": queue_cancelled,
            "queued_or_cancelled_pending_removal": queued,
            "executing_until_reply": executing,
            "recent_timing_micros": timings,
            "timing_scope": "queue admission to execution start; command execution to reply consumption",
        },
    });
    body["startup_complete"] = json!(startup_complete());
    body["runtime_unlock_schemas"] = json!([1, respire_protocol::credentials::RUNTIME_UNLOCK_SCHEMA_V2]);
    body["provider_status"] = json!(provider_status);
    body
}

fn write_stop_receipt(directory: &std::path::Path, receipt: &RuntimeStopReceipt) -> Result<()> {
    anyhow::ensure!(
        !receipt.instance_id.is_empty()
            && receipt.instance_id.len() <= 128
            && receipt
                .instance_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
        "invalid stop receipt instance ID"
    );
    let temporary = directory.join(format!(".stop-{}.tmp", uuid::Uuid::new_v4()));
    let target = directory.join(format!("stop-{}.json", receipt.instance_id));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(receipt)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &target)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        if let Err(error) = std::fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("stop receipt staging cleanup failed: {error}");
            }
        }
    }
    result
}

fn final_stop_receipt(operation_id: &str, success: bool) -> RuntimeStopReceipt {
    let process = process_identity();
    let unfinished = STOP_UNFINISHED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .map(|item| RuntimeUnfinishedOperation {
            operation_id: item.operation_id.clone(),
            result: respire_protocol::RuntimeUnfinishedOutcome::Unknown,
        })
        .collect::<Vec<_>>();
    RuntimeStopReceipt {
        schema_version: 1,
        protocol_version: PROTOCOL_V,
        operation_id: operation_id.to_owned(),
        installation_id: process.installation_id,
        instance_id: process.instance_id,
        pid: std::process::id(),
        process_start_id: process.process_start_id,
        image_path: process.image_path,
        image_sha256: process.image_sha256,
        runtime_version: runtime_identity().runtime_version,
        account_id: account_identity(),
        generation: crate::rpc_worker::wire_generation(),
        state: stop_outcome(
            success,
            STOP_FORCE_TRIGGERED.load(Ordering::Acquire),
            unfinished.is_empty(),
        ),
        unfinished,
    }
}

fn should_revoke_clean_stop(clean: bool, forced: bool) -> bool {
    clean && !forced
}

#[cfg(test)]
#[test]
fn endpoint_cleanup_is_only_for_clean_unforced_shutdown() {
    assert!(should_revoke_clean_stop(true, false));
    assert!(!should_revoke_clean_stop(false, false));
    assert!(!should_revoke_clean_stop(true, true));
    assert!(!should_revoke_clean_stop(false, true));
    assert_eq!(stop_outcome(false, false, true), RuntimeStopOutcome::Failed);
}

fn stop_outcome(clean: bool, forced: bool, unfinished_empty: bool) -> RuntimeStopOutcome {
    if !clean {
        RuntimeStopOutcome::Failed
    } else if forced {
        RuntimeStopOutcome::ForcedTimeout
    } else if unfinished_empty {
        RuntimeStopOutcome::Drained
    } else {
        RuntimeStopOutcome::Failed
    }
}

fn stop_process(code: i32) -> ! {
    #[cfg(test)]
    {
        let _ = code;
        loop {
            std::thread::park();
        }
    }
    #[cfg(not(test))]
    {
        let mut exit_code = code;
        if let Err(error) = respire_app::core_sdk::worker::shutdown() {
            eprintln!(
                "runtime retained its library lock because Core worker cleanup failed: {error:#}"
            );
            exit_code = 1;
            while respire_app::core_sdk::worker::shutdown().is_err() {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        if let Err(error) = crate::runtime_http::shutdown() {
            eprintln!("runtime could not drain HTTP responses before exit: {error:#}");
            exit_code = 1;
        }
        if let Some(operation_id) = STOP_OPERATION.get() {
            let receipt = final_stop_receipt(operation_id, exit_code == 0);
            let directory = respire_app::service::main_data_dir().join("runtime");
            let written = std::fs::create_dir_all(&directory)
                .map_err(anyhow::Error::from)
                .and_then(|_| write_stop_receipt(&directory, &receipt));
            if receipt.state == RuntimeStopOutcome::ForcedTimeout {
                exit_code = 1;
            }
            if let Err(error) = written {
                eprintln!("runtime failed to persist its final stop receipt: {error:#}");
                exit_code = 1;
            }
        }
        crate::host_hooks::record_exit(exit_code == 0);
        std::process::exit(exit_code);
    }
}

/// Job slots for this process. `RSRS_RPC_PARALLELISM` wins, then
/// `client.json` `rpc_parallelism`, otherwise OS-reported available parallelism.
/// Explicit settings cannot exceed the CPUs available to this process.
/// Two additional lightweight slots keep show/list/status/revision responsive.
/// Simple writes prepare concurrently and use short source-checked commits.
/// Foreground and background ONNX sessions have independent serial queues.
pub fn worker_limit() -> usize {
    let available = cpu_count();
    if let Ok(raw) = respire_app::env::var("RSRS_RPC_PARALLELISM") {
        let raw = raw.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("cpu") {
            return cpu_count();
        }
        if let Ok(parsed) = raw.parse::<usize>() {
            if parsed == 0 {
                return cpu_count();
            }
            return parsed.clamp(1, available);
        }
    }
    if let Some(saved) = respire_app::service::rpc_parallelism_setting() {
        return saved.clamp(1, available);
    }
    cpu_count()
}

/// OS-reported parallelism for this process; the floor keeps a single-CPU
/// host usable when the metric is unavailable.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

fn queue_wait() -> Duration {
    Duration::from_secs(
        respire_app::env::var("RSRS_RPC_QUEUE_WAIT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(120)
            .clamp(1, 3600),
    )
}

pub fn is_exclusive(args: &[String]) -> bool {
    if is_context_query(args) {
        return false;
    }
    if command_name(args) == Some("update") {
        return false;
    }
    if command_name(args) == Some("remember")
        && !args.iter().any(|arg| {
            matches!(arg.as_str(), "--merge-ids" | "--supersedes" | "--see-also")
                || arg.starts_with("--merge-ids=")
                || arg.starts_with("--supersedes=")
                || arg.starts_with("--see-also=")
        })
    {
        return false;
    }
    matches!(
        command_name(args),
        Some(
            "remember"
                | "update"
                | "forget"
                | "restore"
                | "purge"
                | "attach"
                | "promote"
                | "demote"
                | "retitle"
                | "retitle-many"
                | "import"
                | "reembed"
                | "resort"
                | "split"
                | "tree-deepen"
                | "tree-cure"
                | "tree-float"
                | "sync"
                | "sync-conflicts"
                | "sync-resolve"
                | "sync-restore"
                | "sync-reset"
                | "repack"
                | "inject"
                | "keygen"
                | "account"
                | "space"
                | "fivekeys"
                | "super-reset"
                | "register"
                | "login"
                | "logout"
                | "config"
                | "agent-config"
                | "model"
                | "grant"
                | "session"
                | "query-log"
                | "doctor"
                | "root-create"
                | "share-import"
                | "backup"
                | "snapshot"
        )
    )
}

fn wake_dispatcher() {
    if let Some(thread) = DISPATCH_THREAD.get() {
        thread.unpark();
    }
}

// The unpark token survives a completion between try_recv and park_timeout.
// Keep the periodic check as a fallback for shutdown and queue expiry.
pub fn receive_dispatch_job<T>(
    rx: &Receiver<T>,
    wait: Duration,
) -> Result<T, mpsc::RecvTimeoutError> {
    match rx.try_recv() {
        Ok(job) => return Ok(job),
        Err(mpsc::TryRecvError::Disconnected) => return Err(mpsc::RecvTimeoutError::Disconnected),
        Err(mpsc::TryRecvError::Empty) => {}
    }
    std::thread::park_timeout(wait);
    rx.try_recv().map_err(|error| match error {
        mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
        mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
    })
}

struct RunningGuard {
    status: bool,
    light: bool,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::AcqRel);
        if self.light {
            LIGHT_RUNNING.fetch_sub(1, Ordering::AcqRel);
        }
        if self.status {
            RUNNING_STATUS.fetch_sub(1, Ordering::AcqRel);
        }
        wake_dispatcher();
    }
}

struct ClassifyGuard;
impl Drop for ClassifyGuard {
    fn drop(&mut self) {
        SYNC_CONTEXT.with(|context| context.set(None));
        CLASSIFY_RUNNING.store(false, Ordering::Release);
    }
}

struct StartupEndpoint {
    owner: Arc<Mutex<respire_app::lock::LibraryLock>>,
    publication: crate::rpc_worker::EndpointPublication,
}

impl StartupEndpoint {
    fn revoke_clean_stop(&self) -> Result<()> {
        let owner = self.owner.lock()
            .map_err(|_| anyhow::anyhow!("installation ownership lock poisoned"))?;
        crate::rpc_worker::revoke_current_endpoint(&owner, &current_url())
    }

    fn revoke(&self) {
        let result = self.owner.lock()
            .map_err(|_| anyhow::anyhow!("installation ownership lock poisoned"))
            .and_then(|owner| self.publication.revoke_failed_start(&owner));
        if let Err(error) = result {
            eprintln!("runtime failed to revoke its startup endpoint: {error:#}");
        }
    }
}

pub fn dispatch_loop(
    rx: Receiver<Job>,
    limit: usize,
    idle: Option<Duration>,
    _lock: respire_app::lock::LibraryLock,
) {
    dispatch_loop_inner(rx, limit, idle, _lock, None)
}

