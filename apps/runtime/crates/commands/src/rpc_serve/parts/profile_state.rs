fn profile_state() -> Value {
    PROFILE_SWITCH_STATE
        .lock()
        .map(|v| v.clone())
        .unwrap_or_else(|_| json!({"state":"failed","error":"switch state lock poisoned"}))
}

pub(crate) fn model_assets_stable() -> bool {
    !PROFILE_SWITCH_RUNNING.load(Ordering::Acquire)
        && PROFILE_SWITCH_STATE.lock().is_ok_and(|state| state["semantic"] != "unverified")
}

#[cfg(test)]
pub(crate) fn hold_sync_fixture() -> Result<std::sync::MutexGuard<'static, ()>> {
    PROFILE_ADMISSION.lock().map_err(|_| anyhow::anyhow!("model admission lock poisoned"))
}
fn set_profile_state(value: Value) {
    if let Ok(mut slot) = PROFILE_SWITCH_STATE.lock() {
        *slot = value;
    }
}
fn start_profile_switch(
    choice: respire_app::embedding::Choice,
    request: &RpcRequest,
    identity: &OperationIdentity,
) -> Result<RpcResponse> {
    anyhow::ensure!(
        !respire_app::service::service_paused() && !crate::host_hooks::background_paused(),
        "model switching is paused by service mode"
    );
    start_profile_switch_target(respire_app::embedding::select(choice)?, request, identity)
}
fn start_profile_switch_target(
    mut target: respire_app::core_sdk::profiles::Selection,
    request: &RpcRequest,
    identity: &OperationIdentity,
) -> Result<RpcResponse> {
    anyhow::ensure!(
        !respire_app::service::service_paused() && !crate::host_hooks::background_paused(),
        "model switching is paused by service mode"
    );
    target.validate()?;
    let task_id = identity.operation_id.clone();
    let scope = identity.scope.clone();
    let epoch = scope_generation(&scope).map_err(anyhow::Error::msg)?;
    let completion_admission = begin_admission(task_id.clone(), "model_switch")
        .context("runtime is draining and rejected new business")?;
    let library = respire_app::service::data_dir();
    let admission = PROFILE_ADMISSION
        .lock()
        .map_err(|_| anyhow::anyhow!("model admission lock poisoned"))?;
    // The RPC still holds the write gate. Releasing it wakes waiters before rebuilding.
    anyhow::ensure!(
        PROFILE_SWITCH_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "another model switch is running"
    );
    let operation = match crate::model_progress::Operation::reserve_task(task_id.clone()) {
        Ok(operation) => operation,
        Err(error) => {
            PROFILE_SWITCH_RUNNING.store(false, Ordering::Release);
            return Err(error);
        }
    };
    let queued = (|| -> Result<()> {
        check_sync_context(epoch)?;
        let store = respire_app::service::open_store()?;
        anyhow::ensure!(
            library == respire_app::service::data_dir(),
            "model switch library changed before acceptance"
        );
        store.queue_selection(&target, &task_id)
    })();
    if let Err(error) = queued {
        PROFILE_SWITCH_RUNNING.store(false, Ordering::Release);
        return Err(error);
    }
    drop(admission);
    let pending = json!({"id":task_id,"state":"pending","target":target.model,"active_changed":false,"semantic":"busy"});
    set_profile_state(pending.clone());
    let mut response = status_response(request);
    response.operation_id = Some(task_id.clone());
    response.envelope = Some(ResultEnvelope::new(
        &request.method, OutputStatus::Pending, pending, Vec::new(),
    ));
    // Publish Pending before the worker can fail or finish, including before it takes the gate.
    remember(&identity.storage_id, &response);
    let reply = deferred_reply(request, identity, completion_admission);
    let method = request.method.clone();
    let spawn = std::thread::Builder::new().name("respire-model-switch".into()).stack_size(16 * 1024 * 1024).spawn(move || {
        struct Finish;
        impl Drop for Finish { fn drop(&mut self) { PROFILE_SWITCH_RUNNING.store(false,Ordering::Release); } }
        let _finish = Finish;
        operation.enter_thread();
        let _operation = operation;
        let _scope = crate::model_progress::TaskScope::new(Some(task_id.clone()));
        let result = (|| -> Result<Value> {
            crate::model_progress::update("switch-verify",&target.model,0,None)?;
            let waiting = Instant::now();
            while RUNNING.load(Ordering::Acquire).saturating_sub(RUNNING_STATUS.load(Ordering::Acquire)) > 0 || CLASSIFY_RUNNING.load(Ordering::Acquire) || sync_running() {
                check_sync_context(epoch)?;
                crate::model_progress::check()?;
                anyhow::ensure!(waiting.elapsed() < Duration::from_secs(1800),"model switch timed out draining in-flight requests");
                std::thread::sleep(Duration::from_millis(25));
            }
            let _identity = SCOPE_EXECUTION.read()
                .map_err(|_| anyhow::anyhow!("scope_mismatch: identity fence poisoned"))?;
            let gate = shared_exclusive();
            let _write = gate.acquire(true);
            validate_job_scope(&scope).map_err(anyhow::Error::msg)?;
            check_sync_context(epoch)?;
            anyhow::ensure!(library == respire_app::service::data_dir(), "model switch library changed");
            let store = respire_app::service::open_store()?;
            let vault = respire_app::auth::ModelSwitchVault::from_session(crate::engine::build_session()?);
            run_model_switch(
                &store, &vault, &task_id, &mut target,
                || {
                    check_sync_context(epoch)?;
                    anyhow::ensure!(library == respire_app::service::data_dir(), "model switch library changed");
                    Ok(())
                },
                |_, _, _| Ok(()),
                |final_commit, action| {
                    if final_commit {
                        crate::model_progress::commit_final(action)
                    } else {
                        check_sync_context(epoch)?;
                        crate::model_progress::commit(action)
                    }
                },
            )
        })();
        let final_state = record_switch_result(&task_id, &result);
        let completion = result.map(|_| crate::output_state::Captured {
            exit: 0,
            envelope: ResultEnvelope::new(&method, OutputStatus::Ok, final_state, Vec::new()),
        }).map_err(|error| format!("{error:#}"));
        let _ = reply.send(completion);
        kick_index();
    });
    if let Err(error) = spawn {
        PROFILE_SWITCH_RUNNING.store(false, Ordering::Release);
        set_profile_state(
            json!({"state":"failed","active_changed":false,"error":error.to_string()}),
        );
        return Err(error.into());
    }
    Ok(response)
}

pub fn sync_running() -> bool {
    SYNC_RUNNING.load(Ordering::Acquire)
}

// One in-flight background notification; the inherited pipe reader owns all acknowledgements.
#[derive(Default)]
pub(crate) struct HostPluginChannel {
    state: Mutex<HostPluginState>,
    changed: Condvar,
}

#[derive(Default)]
struct HostPluginState {
    pending: Option<String>,
    result: Option<respire_protocol::host_plugin::HostPluginEventResult>,
    closed: Option<String>,
}

static HOST_PLUGIN_CHANNEL: OnceLock<Arc<HostPluginChannel>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct HostPluginFailure(pub(crate) anyhow::Error);

impl std::fmt::Display for HostPluginFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for HostPluginFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(self.0.as_ref()) }
}

pub(crate) fn background_sync_retryable(error: &anyhow::Error) -> bool {
    !error.is::<HostPluginFailure>() && crate::sync_flow::is_retriable_sync_error(error)
}

impl HostPluginChannel {
    pub(crate) fn exchange(&self, send: impl FnOnce(&[u8]) -> Result<()>, pulled: u64, pushed: u64) -> Result<Vec<String>> {
        use respire_protocol::host_plugin::*;
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.state.lock().map_err(|_| anyhow::anyhow!("host plugin state poisoned"))?;
            if let Some(error) = &state.closed {
                anyhow::bail!("host plugin channel closed: {error}");
            }
            anyhow::ensure!(state.pending.is_none(), "host plugin event is already awaiting acknowledgement");
            state.pending = Some(id.clone());
        }
        let request = HostPluginEventRequest { plugin_event: HOST_PLUGIN_SCHEMA_V, id,
            event: HostPluginEvent::PostSync, data: HostPluginSyncData { pulled, pushed } };
        let written = (|| -> Result<()> {
            let bytes = serde_json::to_vec(&request)?;
            anyhow::ensure!(bytes.len() <= respire_protocol::HOST_CONTROL_MAX_FRAME_BYTES, "host plugin frame exceeds protocol limit");
            send(&bytes)
        })();
        if let Err(error) = written {
            self.close(format!("host plugin request delivery failed: {error:#}"))?;
        }
        let mut state = self.state.lock().map_err(|_| anyhow::anyhow!("host plugin state poisoned"))?;
        loop {
            if let Some(error) = &state.closed {
                let error = error.clone();
                state.pending = None;
                anyhow::bail!("{error}");
            }
            if let Some(result) = state.result.take() {
                state.pending = None;
                return match result {
                    HostPluginEventResult::Completed { warnings } => Ok(warnings),
                    HostPluginEventResult::Failed { error } => Err(anyhow::anyhow!("host post-sync plugin failed: {error}")),
                };
            }
            state = self.changed.wait(state).map_err(|_| anyhow::anyhow!("host plugin state poisoned"))?;
        }
    }

    pub(crate) fn acknowledge(&self, ack: respire_protocol::host_plugin::HostPluginEventAck) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| anyhow::anyhow!("host plugin state poisoned"))?;
        anyhow::ensure!(ack.plugin_event_ack == respire_protocol::host_plugin::HOST_PLUGIN_SCHEMA_V
            && state.pending.as_deref() == Some(ack.id.as_str()) && state.result.is_none()
            && state.closed.is_none(), "unexpected host plugin acknowledgement");
        state.result = Some(ack.result);
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn close(&self, error: String) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| anyhow::anyhow!("host plugin state poisoned"))?;
        state.closed = Some(error);
        self.changed.notify_all();
        Ok(())
    }
}

fn start_host_plugin_reader() -> Result<()> {
    match std::env::var("RSRS_HOST_PLUGIN_CHANNEL") {
        Err(std::env::VarError::NotPresent) => return Ok(()),
        Ok(value) if value == "1" => {},
        _ => anyhow::bail!("invalid host plugin channel selection"),
    }
    let channel = Arc::new(HostPluginChannel::default());
    HOST_PLUGIN_CHANNEL.set(channel.clone()).map_err(|_| anyhow::anyhow!("host plugin reader already initialized"))?;
    std::thread::Builder::new().name("host-plugin-input".into()).spawn(move || {
        let result = (|| -> Result<()> {
            let mut input = std::io::stdin().lock();
            loop {
                let mut length = [0_u8; 4];
                input.read_exact(&mut length).context("host plugin parent channel closed")?;
                let length = u32::from_be_bytes(length) as usize;
                anyhow::ensure!(length > 0 && length <= respire_protocol::HOST_CONTROL_MAX_FRAME_BYTES,
                    "invalid host plugin acknowledgement frame size");
                let mut bytes = vec![0_u8; length];
                input.read_exact(&mut bytes)?;
                channel.acknowledge(serde_json::from_slice(&bytes)?)?;
            }
        })();
        if let Err(error) = result {
            let message = format!("host plugin channel failed: {error:#}");
            eprintln!("{message}");
            if let Err(error) = channel.close(message) {
                eprintln!("host plugin channel cleanup failed: {error:#}");
            }
            STOPPING.store(true, Ordering::Release);
            set_control_state("draining");
            wake_dispatcher();
            SYNC_CV.notify_all();
            INDEX_CV.notify_all();
        }
    }).context("start host plugin reader")?;
    Ok(())
}

pub(crate) fn deliver_post_sync(pulled: usize, pushed: usize) -> Result<()> {
    // No initialized channel means this host did not subscribe to plugin events.
    let Some(channel) = HOST_PLUGIN_CHANNEL.get() else { return Ok(()); };
    let result = (|| -> Result<()> {
        // Release stdout before waiting: it is shared with other Runtime diagnostics.
        let warnings = channel.exchange(|bytes| {
            let mut output = std::io::stdout().lock();
            output.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
            output.write_all(bytes)?;
            output.flush()?;
            Ok(())
        }, u64::try_from(pulled)?, u64::try_from(pushed)?)?;
        for warning in warnings {
            eprintln!("command=sync status=warn mode=auto hook={warning}");
        }
        Ok(())
    })();
    result.map_err(|error| HostPluginFailure(error).into())
}

// The caller holds SYNC_KICK while starting the worker and queueing work.
fn ensure_sync_worker() -> bool {
    if FLIGHT_ON
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return true;
    }
    if std::thread::Builder::new()
        .name("respire-sync".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(flight_loop)
        .is_err()
    {
        FLIGHT_ON.store(false, Ordering::Release);
        eprintln!("command=sync status=warn error=failed to start sync worker");
        return false;
    }
    true
}

pub(crate) fn enqueue_v1_sync(sync: crate::runtime_http::PendingSync) -> Result<()> {
    let mut state = SYNC_KICK.lock().map_err(|_| anyhow::anyhow!("sync queue lock poisoned"))?;
    anyhow::ensure!(!STOPPING.load(Ordering::Acquire) && state.manual.len() < 64,
        "sync queue unavailable");
    anyhow::ensure!(ensure_sync_worker(), "sync worker unavailable");
    state.manual.push_back(ManualSync::V1(Box::new(sync)));
    SYNC_CV.notify_one();
    Ok(())
}

/// Notifications are hints; the durable outbox holds the work.
pub fn kick_autosync() {
    let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
    if state.due.is_none() {
        state.due = Some(Instant::now() + Duration::from_secs(3));
    }
    ensure_sync_worker();
    SYNC_CV.notify_one();
}

fn enqueue_manual(job: Job) {
    let captured = job
        .sync_context
        .ok_or_else(|| anyhow::anyhow!("missing sync request context"));
    match captured {
        Ok((generation, boundary)) => {
            let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
            if state.manual.len() >= 64 {
                let _ = job.reply.send(Err("sync request queue is full".into()));
                return;
            }
            ensure_sync_worker();
            state.manual.push_back(ManualSync::Legacy(job, generation, boundary));
            SYNC_CV.notify_one();
        }
        Err(error) => {
            let _ = job.reply.send(Err(format!("{error:#}")));
        }
    }
}

fn automatic_sync_boundary(generation: usize, enabled: bool) -> Result<Option<i64>> {
    // A startup timer may fire even with autosync disabled. Do not take the
    // shared writer gate just to discover there is no automatic work to do.
    if !enabled {
        return Ok(None);
    }
    sync_local(generation, || {
        anyhow::ensure!(
            !respire_app::service::service_paused(),
            "background sync paused by service mode"
        );
        crate::engine::build_local()?.outgoing_boundary()
    }).map(Some)
}

fn flight_loop() {
    mark_worker();
    loop {
        let manual = {
            let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if STOPPING.load(Ordering::Acquire) {
                    for job in state.manual.drain(..) {
                        job.reject("runtime is stopping");
                    }
                    FLIGHT_ON.store(false, Ordering::Release);
                    return;
                }
                if !store_ready() || crate::host_hooks::background_paused() {
                    state = SYNC_CV
                        .wait_timeout(state, Duration::from_millis(250))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    continue;
                }
                if let Some(job) = state.manual.pop_front() {
                    break Some(job);
                }
                let current_generation = GENERATION.load(Ordering::Acquire);
                if state.blocked_generation == Some(current_generation) {
                    state = SYNC_CV.wait(state).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                state.blocked_generation = None;
                let now = Instant::now();
                let due = state.due.get_or_insert(now + Duration::from_secs(300));
                if *due <= now {
                    state.due = None;
                    break None;
                }
                let wait = *due - now;
                state = SYNC_CV
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        };
        let admission = PROFILE_ADMISSION
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if PROFILE_SWITCH_RUNNING.load(Ordering::Acquire) {
            if let Some(job) = manual {
                job.reject("model_switch_busy");
            } else {
                SYNC_KICK
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .due = Some(Instant::now() + Duration::from_secs(1));
            }
            drop(admission);
            continue;
        }
        SYNC_RUNNING.store(true, Ordering::Release);
        drop(admission);
        if let Some(ManualSync::V1(sync)) = manual {
            if sync.run() {
                SYNC_KICK.lock().unwrap_or_else(|error| error.into_inner()).record_success(true);
            }
        } else if let Some(ManualSync::Legacy(job, generation, boundary)) = manual {
            let _scope_held = match SCOPE_EXECUTION.read() {
                Ok(held) => held,
                Err(_) => {
                    let _ = job.reply.send(Err("scope_mismatch: identity fence poisoned".into()));
                    SYNC_RUNNING.store(false, Ordering::Release);
                    continue;
                }
            };
            let boundary = if boundary < 0 {
                let gate = shared_exclusive();
                let held = gate.acquire(true);
                let snapshot = if job.deadline.expired() {
                    Err(anyhow::anyhow!(QUEUE_EXPIRED))
                } else {
                    validate_job_scope(&job.scope).map_err(anyhow::Error::msg)
                        .and_then(|_| check_sync_context(generation))
                        .and_then(|_| crate::engine::build_local())
                        .and_then(|store| store.outgoing_boundary())
                };
                drop(held);
                match snapshot {
                    Ok(boundary) => boundary,
                    Err(error) => {
                        let _ = job.reply.send(Err(format!("{error:#}")));
                        SYNC_RUNNING.store(false, Ordering::Release);
                        continue;
                    }
                }
            } else {
                boundary
            };
            if !job.deadline.start() {
                let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                SYNC_RUNNING.store(false, Ordering::Release);
                continue;
            }
            SYNC_CONTEXT.with(|context| context.set(Some((generation, boundary))));
            let result = capture_scoped_locked(&job.scope, job.args);
            if result.as_ref().is_ok_and(|captured| captured.exit == 0) {
                let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
                state.blocked_generation = None;
                state.backoff = 0;
            }
            let _ = job.reply.send(result.map_err(|error| format!("{error:#}")));
        } else {
            let generation = GENERATION.load(Ordering::Acquire);
            let enabled = respire_app::service::autosync_active()
                && respire_app::sync::remote_configured();
            let result = automatic_sync_boundary(generation, enabled).and_then(|boundary| {
                let Some(boundary) = boundary else { return Ok(false) };
                SYNC_CONTEXT.with(|context| context.set(Some((generation, boundary))));
                crate::engine::background_sync_once()
            });
            let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(synchronized) => state.record_success(synchronized),
                Err(error) if error.is::<HostPluginFailure>() => {
                    // Core sync committed before the host observer failed; retain normal scheduling.
                    state.record_success(true);
                    eprintln!("command=sync status=warn mode=auto core=completed host_plugin_error={error}");
                }
                Err(error)
                    if error
                        .downcast_ref::<respire_app::sync::SyncBoundaryChanged>()
                        .is_some() =>
                {
                    state.backoff = 0;
                    state.blocked_generation = None;
                    state.due = Some(Instant::now() + Duration::from_secs(3));
                    eprintln!("command=sync status=warn mode=auto error={error}");
                }
                Err(error) if generation != GENERATION.load(Ordering::Acquire) => {
                    state.backoff = 0;
                    eprintln!("command=sync status=warn mode=auto error={error}");
                }
                Err(error) => {
                    let transport = background_sync_retryable(&error);
                    let delay = if transport {
                        30_u64.saturating_mul(1 << state.backoff.min(4)).min(300)
                    } else {
                        300
                    };
                    state.backoff = state.backoff.saturating_add(1);
                    if transport {
                        state.due = Some(Instant::now() + Duration::from_secs(delay));
                    } else {
                        state.blocked_generation = Some(generation);
                        state.due = None;
                    }
                    eprintln!("command=sync status=warn mode=auto error={error}");
                }
            }
        }
        SYNC_CONTEXT.with(|context| context.set(None));
        SYNC_RUNNING.store(false, Ordering::Release);
        kick_index();
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeFlags {
    pub port: Option<u16>,
    pub host: String,
    pub status: bool,
    pub stop: bool,
}

pub use respire_protocol::RpcRequest;
pub type RpcResponse = respire_protocol::RpcResponse<ResultEnvelope>;

