fn deferred_reply(request: &RpcRequest, identity: &OperationIdentity, admission: AdmissionGuard) -> Reply {
    let mut request = request.clone();
    request.operation_id = Some(identity.operation_id.clone());
    let storage_id = identity.storage_id.clone();
    Reply::new(move |result| {
        let _admission = admission;
        let response = command_response(&request, result);
        remember(&storage_id, &response);
        INFLIGHT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&storage_id);
    })
}

fn enqueue_prepare(request: &RpcRequest, identity: &OperationIdentity) -> Result<()> {
    let tx = WRITE_TX
        .lock()
        .map_err(|_| anyhow::anyhow!("write queue lock poisoned"))?
        .clone()
        .context("write worker is not running")?;
    let admission = begin_admission(identity.operation_id.clone(), "model_prepare")
        .context("runtime is draining and rejected new business")?;
    let operation = crate::model_progress::Operation::reserve_task(identity.operation_id.clone())?;
    let wait = request
        .queue_wait_ms
        .map(Duration::from_millis)
        .unwrap_or_else(queue_wait)
        .min(queue_wait());
    let reply = deferred_reply(request, identity, admission);
    let deadline = Arc::new(crate::admission::QueueDeadline::new(wait));
    watch_queue(&deadline, &reply)?;
    let prepare = PrepareJob {
        job: Job {
            scope: identity.scope.clone(),
            args: Vec::new(),
            stop: false,
            reply,
            sync_context: None,
            deadline,
        },
        operation_id: identity.operation_id.clone(),
        operation,
    };
    let gate = shared_exclusive();
    gate.reserve();
    match tx.try_send(WriteJob::Prepare(prepare)) {
        Ok(()) => Ok(()),
        Err(error) => {
            gate.cancel_reservation();
            let (work, message) = match error {
                mpsc::TrySendError::Full(work) => (work, "write request queue is full"),
                mpsc::TrySendError::Disconnected(work) => (work, "write worker is gone"),
            };
            if let WriteJob::Prepare(prepare) = work {
                let _ = prepare.job.reply.send(Err(message.to_owned()));
            }
            bail!(message)
        }
    }
}

fn run_prepare(prepare: PrepareJob) {
    let PrepareJob {
        job,
        operation_id,
        operation,
    } = prepare;
    let scope_held = SCOPE_EXECUTION.read();
    let gate = shared_exclusive();
    let held = gate.acquire_reserved();
    WRITE_RUNNING.store(true, Ordering::Release);
    let result = (|| -> Result<crate::output_state::Captured> {
        scope_held
            .as_ref()
            .map_err(|_| anyhow::anyhow!("scope_mismatch: identity fence poisoned"))?;
        anyhow::ensure!(job.deadline.start(), QUEUE_EXPIRED);
        validate_job_scope(&job.scope).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!STOPPING.load(Ordering::Acquire), "runtime is stopping");
        anyhow::ensure!(
            !respire_app::service::service_paused() && !crate::host_hooks::background_paused(),
            "model preparation is paused by service mode"
        );
        let _task = crate::model_progress::TaskScope::new(Some(operation_id.clone()));
        operation.enter_thread();
        crate::model_progress::check()?;
        if let Some(selection) = respire_app::embedding::saved_selection()? {
            crate::model_assets::verify_selection_assets(&selection)?;
        } else {
            crate::model_assets::verify_m3_assets()?;
        }
        crate::model_progress::check()?;
        Ok(crate::output_state::Captured {
            exit: 0,
            envelope: ResultEnvelope::new(
                "model.prepare",
                OutputStatus::Ok,
                json!({"state":"completed", "operation_id":operation_id}),
                Vec::new(),
            ),
        })
    })();
    drop(held);
    drop(scope_held);
    drop(operation);
    if result.is_ok() {
        kick_index();
    }
    WRITE_RUNNING.store(false, Ordering::Release);
    let _ = job.reply.send(result.map_err(|error| format!("{error:#}")));
}

pub fn enqueue_job(
    tx: &Sender<Job>,
    args: Vec<String>,
    stop: bool,
    reply: Reply,
    wait: Duration,
    operation_id: String,
) -> std::result::Result<Arc<crate::admission::QueueDeadline>, String> {
    if !stop { validate_business_config()?; }
    enqueue_job_scoped(tx, args, stop, reply, wait, operation_id, current_scope())
}

fn enqueue_job_scoped(
    tx: &Sender<Job>,
    args: Vec<String>,
    stop: bool,
    reply: Reply,
    wait: Duration,
    operation_id: String,
    scope: RuntimeRequestScope,
) -> std::result::Result<Arc<crate::admission::QueueDeadline>, String> {
    if !stop && business_paused() {
        return Err("runtime_maintenance: runtime maintenance rejected queued business".to_owned());
    }
    if !stop { validate_business_config()?; }
    if !stop
        && !respire_app::service::off_mode()
        && needs_inference(&args)
        && inference_health_status()["host_recovery_required"] == true
    {
        return Err("inference is unresponsive after cancellation; host runtime recovery is required; request was not queued".into());
    }
    let slots = WORKERS.load(Ordering::Acquire).max(1);
    if !stop
        && is_status(&args)
        && LIGHT_RUNNING.load(Ordering::Acquire) >= 2
        && RUNNING
            .load(Ordering::Acquire)
            .saturating_sub(LIGHT_RUNNING.load(Ordering::Acquire))
            >= slots
    {
        let mut envelope = ResultEnvelope::new(
            "status",
            OutputStatus::Warn,
            json!({"state": "busy", "workers": slots}),
            Vec::new(),
        );
        envelope.errors.push(i18n::text("busy").to_owned());
        reply.send(Ok(crate::output_state::Captured { exit: 2, envelope }))?;
        return Ok(Arc::new(crate::admission::QueueDeadline::new(
            Duration::ZERO,
        )));
    }
    let sync_context = if !stop && !respire_app::service::off_mode()
        && matches!(
            command_name(&args),
            Some(
                "sync"
                    | "sync-conflicts"
                    | "sync-resolve"
                    | "sync-history"
                    | "sync-restore"
                    | "classify"
            )
        ) {
        // Use the caller's wire generation; the sync worker takes its snapshot later.
        Some((scope_generation(&scope)?, -1))
    } else {
        None
    };
    let deadline = Arc::new(crate::admission::QueueDeadline::new(wait));
    watch_queue(&deadline, &reply).map_err(|error| error.to_string())?;
    let admission = {
        let _scope_held = SCOPE_EXECUTION.read().map_err(|_| "scope_mismatch: identity fence poisoned".to_owned())?;
        let gate = shared_exclusive();
        let _held = gate.acquire(true);
        let mut admitted = ADMISSIONS.lock().map_err(|_| "admission_lock_poisoned".to_owned())?;
        if stop {
            validate_scope(Some(&scope), &current_scope()).map_err(|(code, _)| code)?;
        } else {
            validate_job_scope(&scope)?;
        }
        admitted.push(Admitted {
            operation_id: operation_id.clone(),
            kind: if stop { "stop" } else { "rpc" },
        });
        AdmissionGuard(operation_id)
    };
    let reply = Reply::new(move |result| {
        let _ = reply.send(result);
        drop(admission);
    });
    tx.send(Job {
        scope,
        args,
        stop,
        reply,
        sync_context,
        deadline: Arc::clone(&deadline),
    })
    .map_err(|_| "runtime worker is gone".to_owned())?;
    wake_dispatcher();
    Ok(deadline)
}

pub fn submit(
    tx: &Sender<Job>,
    args: Vec<String>,
    stop: bool,
    operation_id: String,
) -> CommandResult {
    // Internal MCP callers have no external scope: capture once, never at execution.
    validate_business_config()?;
    submit_scoped(tx, args, stop, operation_id, Some(current_scope()))
}

fn submit_scoped(
    tx: &Sender<Job>,
    args: Vec<String>,
    stop: bool,
    operation_id: String,
    scope: Option<RuntimeRequestScope>,
) -> CommandResult {
    let scope = scope.ok_or_else(|| "scope_required".to_owned())?;
    let (reply_tx, reply_rx) = mpsc::channel();
    let deadline = enqueue_job_scoped(
        tx,
        args,
        stop,
        Reply::new(move |result| {
            let _ = reply_tx.send(result);
        }),
        queue_wait(),
        operation_id,
        scope,
    )?;
    let wait = queue_wait();
    match reply_rx.recv_timeout(wait) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("runtime worker dropped the reply".to_owned())
        }
        Err(mpsc::RecvTimeoutError::Timeout) if deadline.cancel() => Err(QUEUE_EXPIRED.to_owned()),
        // Execution won admission. Especially for writes, preserve the actual
        // commit outcome instead of returning an ambiguous client timeout.
        Err(mpsc::RecvTimeoutError::Timeout) => reply_rx
            .recv()
            .map_err(|_| "runtime worker dropped the reply".to_owned())?,
    }
}

fn mcp_request(args: Vec<String>, scope: RuntimeRequestScope) -> RpcRequest {
    RpcRequest {
        scope: Some(scope),
        v: PROTOCOL_V,
        id: uuid::Uuid::new_v4().to_string(),
        operation_id: Some(uuid::Uuid::new_v4().to_string()),
        idempotency_key: None,
        parent_operation_id: None,
        method: "cli.exec".to_owned(),
        args,
        queue_wait_ms: None,
        lifecycle: None,
    }
}

pub fn execute_json(args: Vec<String>) -> std::result::Result<Value, String> {
    let tx = {
        let guard = JOB_TX
            .get()
            .ok_or_else(|| "runtime worker is not running".to_owned())?
            .lock()
            .map_err(|_| "runtime worker lock poisoned".to_owned())?;
        guard.clone()
    };
    validate_business_config().map_err(|error| {
        json!({"error": error, "code": "invalid_config"}).to_string()
    })?;
    let request = mcp_request(args, current_scope());
    let operation_id = request.operation_id.clone();
    let scope = request.scope.clone();
    let (send, receive) = mpsc::channel();
    dispatch_deferred(request, &tx, move |response| {
        let _ = send.send(response);
    });
    let response = receive
        .recv()
        .map_err(|_| "runtime worker dropped the MCP receipt".to_owned())?;
    let mut envelope = match response.envelope.as_ref() {
        Some(envelope) => serde_json::to_value(envelope).map_err(|error| error.to_string())?,
        None => json!({"error":response.error, "code":response.code}),
    };
    envelope["request_id"] = json!(response.id);
    envelope["operation_id"] = json!(operation_id);
    envelope["scope"] = json!(scope);
    if !response.ok {
        return Err(envelope.to_string());
    }
    Ok(envelope)
}

pub fn wait_for_drain(_supervised: bool, mut busy: impl FnMut() -> bool) {
    while busy() {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn set_control_state(state: &'static str) {
    *CONTROL_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = state;
}

pub fn control_state() -> &'static str {
    *CONTROL_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn health_state() -> &'static str {
    match control_state() {
        "maintenance_requested" | "draining" => "draining",
        "resources_closing" | "forced_timeout" | "compatibility_timeout" => "stopping",
        "drained" => "stopped",
        "running" if !store_ready() => "locked",
        "running" => "ready",
        other => other,
    }
}

fn mark_stop_unfinished(mut items: Vec<Admitted>) {
    // RPC retention and queue execution each hold an admission guard for the
    // same wire operation. The durable receipt records that operation once.
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.operation_id.clone()));
    *STOP_UNFINISHED
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = items;
}

fn stop_unfinished() -> Value {
    let items = STOP_UNFINISHED
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    Value::Array(
        items
            .iter()
            .map(|item| {
                json!({
                    "operation_id": item.operation_id,
                    "result": "unknown",
                })
            })
            .collect(),
    )
}

fn stop_operation_status(request_id: &str, operation_id: &str) -> Value {
    json!({
        "request_id": request_id,
        "operation_id": operation_id,
        "state": control_state(),
        "admitted": admitted_list(),
        "background": busy_background(),
        "unfinished": stop_unfinished(),
    })
}

fn drain_busy() -> bool {
    drain_background_busy()
        || ADMISSIONS
            .lock()
            .map_or(true, |items| items.iter().any(|item| item.kind != "stop"))
}

fn drain_background_busy() -> bool {
    RUNNING.load(Ordering::Acquire) != 0
        || WRITE_RUNNING.load(Ordering::Acquire)
        || CLASSIFY_RUNNING.load(Ordering::Acquire)
        || sync_running()
        || index_has_work()
        || PROFILE_SWITCH_RUNNING.load(Ordering::Acquire)
}

fn validate_stop_force(expected: &Value, accepted: Option<&str>) -> Result<bool> {
    match expected.get("force") {
        None | Some(Value::Bool(false)) => Ok(false),
        Some(Value::Bool(true)) => {
            anyhow::ensure!(
                accepted.is_some_and(|id| expected["operation_id"].as_str() == Some(id)),
                "stop_operation_mismatch"
            );
            Ok(true)
        }
        Some(_) => bail!("invalid_stop_force"),
    }
}

fn force_stop_after_grace() {
    std::thread::sleep(FORCE_DRAIN_GRACE);
    let background_busy = drain_background_busy();
    {
        let admitted = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
        if control_state() != "draining"
            || (!background_busy && !admitted.iter().any(|item| item.kind != "stop"))
        {
            return;
        }
        // An admitted operation may already have committed. Retirement never proves abortion.
        mark_stop_unfinished(
            admitted.iter().filter(|item| item.kind != "stop").cloned().collect(),
        );
        STOP_FORCE_TRIGGERED.store(true, Ordering::Release);
        set_control_state("forced_timeout");
    }
    // Reap the owned native child to unblock callers, then retain the ordinary host joins/flush.
    if let Err(error) = respire_app::core_sdk::worker::shutdown() {
        eprintln!("explicit forced stop could not confirm Core worker cleanup: {error:#}");
    }
    wake_dispatcher();
    SYNC_CV.notify_all();
    INDEX_CV.notify_all();
}

pub fn stop_force_deadline_ms() -> Option<u64> {
    STOP_FORCE_REQUESTED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .map(|started| FORCE_DRAIN_GRACE.saturating_sub(started.elapsed()).as_millis() as u64)
}

pub(crate) fn advance_generation(generation: usize) -> Result<()> {
    // Callers may hold a scope read lock and the write gate: never upgrade it.
    let _fence = ADMISSIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime admission lock is poisoned"))?;
    anyhow::ensure!(!STOPPING.load(Ordering::Acquire), "runtime is stopping");
    GENERATION
        .compare_exchange(
            generation,
            generation.saturating_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| anyhow::anyhow!("sync context changed; pending work kept locally"))?;
    // Keep the durable scope at the actual wire generation, under the same fence.
    if JOB_TX.get().is_some() {
        write_endpoint(&current_url())?;
    }
    Ok(())
}

pub fn request_drain_exit(expected: &Value) -> Result<&'static str> {
    // Identity writers take the scope write lock; generation changes share ADMISSIONS.
    let _identity = SCOPE_EXECUTION.read().map_err(|_| anyhow::anyhow!("identity fence poisoned"))?;
    let operation_id;
    {
        let _admitted = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
        let process = process_identity();
        validate_stop_target(
            expected,
            &process,
            &account_identity(),
            crate::rpc_worker::wire_generation(),
        )?;
        let force = validate_stop_force(expected, STOP_OPERATION.get().map(String::as_str))?;
        if force {
            let mut requested = STOP_FORCE_REQUESTED
                .lock()
                .map_err(|_| anyhow::anyhow!("force stop state poisoned"))?;
            if requested.is_none() {
                let _controller = std::thread::Builder::new()
                    .name("runtime-force-stop".to_owned())
                    .spawn(force_stop_after_grace)
                    .context("start explicit force stop controller")?;
                *requested = Some(Instant::now());
            }
        }
        operation_id = STOP_OPERATION.get_or_init(|| uuid::Uuid::new_v4().to_string());
        if STOPPING.swap(true, Ordering::AcqRel) {
            return Ok(operation_id.as_str());
        }
        set_control_state("maintenance_requested");
    }
    set_control_state("draining");
    wake_dispatcher();
    SYNC_CV.notify_all();
    INDEX_CV.notify_all();
    Ok(operation_id.as_str())
}

fn validate_stop_target(
    expected: &Value,
    process: &ProcessIdentity,
    account: &str,
    generation: u64,
) -> Result<()> {
    anyhow::ensure!(
        expected["installation_id"] == process.installation_id
            && expected["instance_id"] == process.instance_id
            && expected["account_id"] == account
            && expected["generation"] == generation,
        "stop_identity_mismatch"
    );
    Ok(())
}

pub fn handle_http_rpc(body: &[u8]) -> Result<RpcResponse> {
    let request: RpcRequest = serde_json::from_slice(body).context("rpc body is not JSON")?;
    let tx = {
        let guard = JOB_TX
            .get()
            .ok_or_else(|| anyhow::anyhow!("runtime worker is not running"))?
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime worker lock poisoned"))?;
        guard.clone()
    };
    Ok(dispatch(request, &tx))
}

pub fn handle_http_rpc_deferred(
    body: &[u8],
    action: impl FnOnce(RpcResponse) + Send + 'static,
) -> Result<()> {
    let request: RpcRequest = serde_json::from_slice(body).context("rpc body is not JSON")?;
    let tx = JOB_TX
        .get()
        .ok_or_else(|| anyhow::anyhow!("runtime worker is not running"))?
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime worker lock poisoned"))?
        .clone();
    dispatch_deferred(request, &tx, action);
    Ok(())
}

fn inference_health_status() -> Value {
    if !store_ready() {
        return json!({"state":"locked","reason":"host_key_required"});
    }
    match respire_app::memory::onnx::inference_status() {
        Ok(status) => status,
        Err(error) => json!({"state":"error","error":format!("{error:#}")}),
    }
}

