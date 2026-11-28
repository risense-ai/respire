fn dispatch_loop_inner(
    rx: Receiver<Job>,
    limit: usize,
    idle: Option<Duration>,
    _lock: respire_app::lock::LibraryLock,
    startup: Option<StartupEndpoint>,
) {
    mark_worker();
    let _ = DISPATCH_THREAD.set(std::thread::current());
    #[cfg(not(test))]
    respire_app::core_sdk::worker::enable();
    // Keep WAL open across requests: closing the last connection can briefly lock
    // a concurrent opener during checkpoint/cleanup, even for read-only status.
    let mut _store = if respire_app::service::off_mode() || respire_app::auth::load_local_session().is_err() {
        None
    } else {
        Some(match respire_app::service::open_runtime_store(&_lock) {
            Ok((store, snapshot_id)) => {
                if let Ok(mut slot) = SNAPSHOT_BOOT_ID.lock() {
                    *slot = snapshot_id;
                }
                STORE_READY.store(true, Ordering::Release);
                store
            }
            Err(error) => {
                respire_logging::error(format_args!("runtime failed to initialize the library"));
                eprintln!("runtime failed to initialize the library: {error:#}");
                if let Some(startup) = startup.as_ref() {
                    startup.revoke();
                }
                stop_process(1);
            }
        })
    };
    respire_app::service::install_runtime_profile(respire_app::service::data_dir());
    if let Err(error) = respire_app::read_heat::reload() {
        respire_logging::error(format_args!("runtime failed to load read_heat configuration"));
        eprintln!("runtime failed to load read_heat configuration: {error:#}");
        stop_process(1);
    }
    let (classify_tx, classify_rx) = mpsc::sync_channel::<Job>(16);
    let classifier = std::thread::Builder::new()
        .name("respire-classify".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            mark_worker();
            while let Ok(job) = classify_rx.recv() {
                if STOPPING.load(Ordering::Acquire) {
                    let _ = job.reply.send(Err("runtime is stopping".into()));
                    continue;
                }
                if !job.deadline.start() {
                    let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                    continue;
                }
                let Some((generation, _)) = job.sync_context else {
                    let _ = job.reply.send(Err("missing classification context".into()));
                    continue;
                };
                let admission = PROFILE_ADMISSION
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if model_switch_blocks_args(&job.args) {
                    let _ = job.reply.send(Err("model_switch_busy".into()));
                    continue;
                }
                CLASSIFY_RUNNING.store(true, Ordering::Release);
                drop(admission);
                let _active = ClassifyGuard;
                SYNC_CONTEXT.with(|context| context.set(Some((generation, -1))));
                let result = capture_scoped(&job.scope, job.args);
                let _ = job.reply.send(result);
            }
        });
    if classifier.is_err() {
        respire_logging::error(format_args!("runtime failed to start the classifier worker"));
        stop_process(1);
    }
    let (write_tx, write_rx) = mpsc::sync_channel::<WriteJob>(256);
    if let Ok(mut installed) = WRITE_TX.lock() {
        *installed = Some(write_tx.clone());
    } else {
        respire_logging::error(format_args!("runtime failed to install the write queue"));
        eprintln!("runtime failed to install the write queue");
        stop_process(1);
    }
    let writer = std::thread::Builder::new()
        .name("respire-write".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            mark_worker();
            while let Ok(work) = write_rx.recv() {
                let job = match work {
                    WriteJob::Command(job) => job,
                    WriteJob::Prepare(prepare) => {
                        run_prepare(prepare);
                        continue;
                    }
                    WriteJob::RecallStats(stats, generation, _admission) => {
                        let result = if STOPPING.load(Ordering::Acquire) {
                            shared_exclusive().cancel_reservation();
                            Err(anyhow::anyhow!("runtime is stopping"))
                        } else {
                            WRITE_RUNNING.store(true, Ordering::Release);
                            let gate = shared_exclusive();
                            let _held = gate.acquire_reserved();
                            let result = check_sync_context(generation)
                                .and_then(|_| respire_app::service::open_store())
                                .and_then(|store| stats.persist(&store));
                            drop(_held);
                            WRITE_RUNNING.store(false, Ordering::Release);
                            result
                        };
                        STATS_PENDING.fetch_sub(1, Ordering::AcqRel);
                        match result {
                            Ok(()) => {
                                STATS_PERSISTED.fetch_add(1, Ordering::AcqRel);
                            }
                            Err(error) => {
                                STATS_FAILED.fetch_add(1, Ordering::AcqRel);
                                respire_logging::error(format_args!("recall statistics writeback failed"));
                                eprintln!("recall statistics writeback failed: {error:#}");
                            }
                        }
                        continue;
                    }
                };
                if STOPPING.load(Ordering::Acquire) {
                    shared_exclusive().cancel_reservation();
                    let _ = job.reply.send(Err("runtime is stopping".into()));
                    continue;
                }
                let admission = PROFILE_ADMISSION
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if model_switch_blocks_args(&job.args) {
                    shared_exclusive().cancel_reservation();
                    let _ = job.reply.send(Err("model_switch_busy".into()));
                    continue;
                }
                WRITE_RUNNING.store(true, Ordering::Release);
                drop(admission);
                let changing_context = !is_context_query(&job.args)
                    && matches!(command_name(&job.args), Some("login" | "logout" | "register" | "keygen" | "config" | "session" | "agent-config" | "sync-reset" | "account" | "space" | "fivekeys" | "super-reset"));
                let scope_write = if changing_context { Some(SCOPE_EXECUTION.write()) } else { None };
                let scope_read = if !changing_context { Some(SCOPE_EXECUTION.read()) } else { None };
                if scope_write.as_ref().is_some_and(|held| held.is_err())
                    || scope_read.as_ref().is_some_and(|held| held.is_err())
                {
                    shared_exclusive().cancel_reservation();
                    WRITE_RUNNING.store(false, Ordering::Release);
                    let _ = job.reply.send(Err("scope_mismatch: identity fence poisoned".into()));
                    continue;
                }
                let gate = shared_exclusive();
                let _held = gate.acquire_reserved();
                if !job.deadline.start() {
                    drop(_held);
                    WRITE_RUNNING.store(false, Ordering::Release);
                    let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                    continue;
                }
                if let Some(rejected) = readonly_sync_reset_rejection_locked(&job.scope, &job.args) {
                    drop(_held);
                    WRITE_RUNNING.store(false, Ordering::Release);
                    let _ = job.reply.send(rejected);
                    continue;
                }
                let captured = capture_scoped_locked(&job.scope, job.args);
                // A host-owned command was rejected before execution; its scope did not change.
                let changing_context = changing_context && !captured.as_ref().is_ok_and(|captured| {
                    captured.envelope.details.get("error_type").and_then(Value::as_str)
                        == Some("host_boundary")
                });
                let captured = captured.and_then(|captured| {
                    if changing_context {
                        advance_generation(scope_generation(&job.scope)?)
                            .map_err(|error| format!("{error:#}"))?;
                        SYNC_CV.notify_one();
                    }
                    Ok(captured)
                });
                drop(_held);
                kick_index();
                if changing_context {
                    context_changed();
                }
                WRITE_RUNNING.store(false, Ordering::Release);
                let _ = job.reply.send(captured);
            }
        });
    if writer.is_err() {
        respire_logging::error(format_args!("runtime failed to start the writer worker"));
        stop_process(1);
    }
    respire_app::service::install_autosync_notifier(kick_autosync);
    respire_app::service::install_index_notifier(kick_index);
    INDEX_ON.store(true, Ordering::Release);
    let indexer = std::thread::Builder::new()
        .name("respire-index".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(index_loop);
    if let Err(error) = indexer.as_ref() {
        respire_logging::error(format_args!("runtime failed to start the index worker"));
        eprintln!("runtime failed to start the index worker: {error}");
        stop_process(1);
    }
    kick_index();
    kick_autosync();
    STARTUP_COMPLETE.store(true, Ordering::Release);
    // Publish readiness after library and queue setup; revoke the endpoint on failure.
    if let Err(error) = crate::host_hooks::runtime_listener_ready() {
        eprintln!("runtime private readiness failed: {error:#}");
        if let Some(startup) = startup.as_ref() {
            startup.revoke();
        }
        stop_process(1);
    }
    let mut owner_dispatch = startup.as_ref()
        .and_then(|_| crate::runtime_http::start_owner_dispatch());
    let mut pending = VecDeque::<Job>::new();
    let mut logout = None;
    let mut idle_since = Instant::now();
    loop {
        if STOPPING.load(Ordering::Acquire) {
            break;
        }
        if _store.is_none() && !respire_app::service::off_mode()
            && respire_app::auth::load_local_session().is_ok()
        {
            match respire_app::service::open_runtime_store(&_lock) {
                Ok((store, snapshot_id)) => {
                    if let Ok(mut slot) = SNAPSHOT_BOOT_ID.lock() { *slot = snapshot_id; }
                    _store = Some(store);
                    STORE_READY.store(true, Ordering::Release);
                    context_changed();
                }
                Err(error) => {
                    eprintln!("runtime library initialization after unlock failed: {error:#}");
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    respire_logging::error(format_args!(
                        "runtime library initialization after unlock failed: {error:#}"
                    ));
                    let _ = respire_app::auth::clear_runtime_session();
                }
            }
        }
        if let Some(dispatch) = owner_dispatch.as_mut() {
            if let Some(work) = dispatch.tick(_store.as_ref(), &_lock) {
                logout = Some(work);
                break;
            }
        }
        match receive_dispatch_job(&rx, Duration::from_millis(10)) {
            Ok(job) => {
                if job.stop {
                    break;
                }
                if job.deadline.expired() {
                    let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                    continue;
                }
                if model_switch_blocks_args(&job.args) {
                    let _ = job.reply.send(Err("model_switch_busy".into()));
                    continue;
                }
                idle_since = Instant::now();
                let _scope_held = match SCOPE_EXECUTION.read() {
                    Ok(held) => held,
                    Err(_) => {
                        let _ = job.reply.send(Err("scope_mismatch: identity fence poisoned".into()));
                        continue;
                    }
                };
                {
                    let gate = shared_exclusive();
                    let _held = (!is_status(&job.args)).then(|| gate.acquire(true));
                    if let Err(error) = validate_job_scope(&job.scope) {
                        let _ = job.reply.send(Err(error));
                        continue;
                    }
                }
                if respire_app::service::off_mode() {
                    let parsed = crate::cli_def::Cli::try_parse_from(
                        std::iter::once("rsrs".to_owned()).chain(job.args.iter().cloned()),
                    );
                    if parsed.is_ok_and(|cli| {
                        cli.command
                            .as_ref()
                            .is_some_and(|command| !crate::gates::is_off_allowed(command))
                    }) {
                        if !job.deadline.start() {
                            let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                            continue;
                        }
                        let captured = capture_scoped_locked(&job.scope, job.args);
                        let _ = job.reply.send(captured);
                        continue;
                    }
                } else if _store.is_none() && !is_status(&job.args) && respire_app::auth::load_local_session().is_ok()
                    && !matches!(
                        command_name(&job.args),
                        Some("agent-config" | "memory-revision")
                    )
                {
                    let gate = shared_exclusive();
                    let _held = gate.acquire(true);
                    let opened = if !respire_app::service::service_paused() {
                        respire_app::service::initialize_runtime_store()
                    } else {
                        respire_app::service::open_store()
                    };
                    match opened {
                        Ok(store) => _store = Some(store),
                        Err(error) => {
                            let _ = job.reply.send(Err(format!("{error:#}")));
                            continue;
                        }
                    }
                }
                if matches!(
                    command_name(&job.args),
                    Some(
                        "sync"
                            | "sync-conflicts"
                            | "sync-resolve"
                            | "sync-history"
                            | "sync-restore"
                    )
                ) {
                    enqueue_manual(job);
                } else if command_name(&job.args) == Some("classify") {
                    match classify_tx.try_send(job) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(job)) => {
                            let _ = job
                                .reply
                                .send(Err("classification request queue is full".into()));
                        }
                        Err(mpsc::TrySendError::Disconnected(job)) => {
                            let _ = job.reply.send(Err("classification worker is gone".into()));
                        }
                    }
                } else if is_exclusive(&job.args) {
                    shared_exclusive().reserve();
                    match write_tx.try_send(WriteJob::Command(job)) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(WriteJob::Command(job))) => {
                            shared_exclusive().cancel_reservation();
                            let _ = job.reply.send(Err("write request queue is full".into()));
                        }
                        Err(mpsc::TrySendError::Disconnected(WriteJob::Command(job))) => {
                            shared_exclusive().cancel_reservation();
                            let _ = job.reply.send(Err("write worker is gone".into()));
                        }
                        Err(_) => unreachable!("only a command was submitted"),
                    }
                } else {
                    pending.push_back(job);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        pending.retain(|job| {
            if job.deadline.expired() {
                let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                false
            } else {
                true
            }
        });
        loop {
            let ordinary_available = RUNNING
                .load(Ordering::Acquire)
                .saturating_sub(LIGHT_RUNNING.load(Ordering::Acquire))
                < limit;
            let position = if LIGHT_RUNNING.load(Ordering::Acquire) < 2 {
                pending.iter().position(|job| {
                    matches!(
                        command_name(&job.args),
                        Some("show" | "list" | "status" | "memory-revision")
                    )
                })
            } else {
                None
            };
            let job = if let Some(position) = position {
                pending.remove(position)
            } else if ordinary_available {
                pending
                    .iter()
                    .position(|job| {
                        !matches!(
                            command_name(&job.args),
                            Some("show" | "list" | "status" | "memory-revision")
                        )
                    })
                    .and_then(|position| pending.remove(position))
            } else {
                None
            };
            let Some(job) = job else {
                break;
            };
            let admission = PROFILE_ADMISSION
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if model_switch_blocks_args(&job.args) {
                let _ = job.reply.send(Err("model_switch_busy".into()));
                continue;
            }
            let light_job = matches!(
                command_name(&job.args),
                Some("show" | "list" | "status" | "memory-revision")
            );
            if light_job {
                LIGHT_RUNNING.fetch_add(1, Ordering::AcqRel);
            }
            let status_job = is_status(&job.args);
            let incumbent_read = (!status_job).then(IncumbentRead::start);
            if status_job {
                RUNNING_STATUS.fetch_add(1, Ordering::AcqRel);
            }
            RUNNING.fetch_add(1, Ordering::AcqRel);
            drop(admission);
            let reply = job.reply.clone();
            let spawned = std::thread::Builder::new()
                .name("respire-read".into())
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    let _incumbent = incumbent_read;
                    let _slot = RunningGuard {
                        status: status_job,
                        light: light_job,
                    };
                    if !job.deadline.start() {
                        let _ = job.reply.send(Err(QUEUE_EXPIRED.into()));
                        return;
                    }
                    mark_worker();
                    let scope = job.scope.clone();
                    let revision_probe = command_name(&job.args) == Some("memory-revision");
                    let captured = capture_scoped(&scope, job.args);
                    if !revision_probe {
                        kick_index();
                    }
                    let result = captured.and_then(|captured| validate_job_scope(&scope).map(|_| captured));
                    let _ = job.reply.send(result);
                });
            if spawned.is_err() {
                RUNNING.fetch_sub(1, Ordering::AcqRel);
                if light_job {
                    LIGHT_RUNNING.fetch_sub(1, Ordering::AcqRel);
                }
                if status_job {
                    RUNNING_STATUS.fetch_sub(1, Ordering::AcqRel);
                }
                let _ = reply.send(Err("failed to start a read thread".into()));
            }
        }
        if let Some(idle) = idle {
            if RUNNING.load(Ordering::Acquire) > 0
                || WRITE_RUNNING.load(Ordering::Acquire)
                || CLASSIFY_RUNNING.load(Ordering::Acquire)
                || sync_running()
                || index_has_work()
                || !pending.is_empty()
            {
                idle_since = Instant::now();
            } else if idle_since.elapsed() >= idle {
                break;
            }
        }
    }
    {
        let _fence = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
        STOPPING.store(true, Ordering::Release);
    }
    drop(owner_dispatch);
    SYNC_CV.notify_all();
    INDEX_CV.notify_all();
    if let Ok(mut installed) = WRITE_TX.lock() {
        installed.take();
    }
    drop(write_tx);
    drop(classify_tx);
    for job in pending {
        let _ = job.reply.send(Err("runtime is stopping".into()));
    }
    let mut clean = true;
    if let Ok(writer) = writer {
        if writer.join().is_err() {
            eprintln!("write worker panicked during shutdown");
            clean = false;
        }
    }
    if let Ok(classifier) = classifier {
        if classifier.join().is_err() {
            eprintln!("classification worker panicked during shutdown");
            clean = false;
        }
    }
    if let Ok(indexer) = indexer {
        if indexer.join().is_err() {
            respire_logging::error(format_args!("background index worker panicked during shutdown"));
            eprintln!("background index worker panicked during shutdown");
            clean = false;
        }
    }
    wait_for_drain(crate::host_hooks::managed_id().is_some(), || {
        for job in rx.try_iter() {
            let _ = job.reply.send(Err("runtime is stopping".into()));
        }
        drain_busy()
    });
    {
        let _admitted = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
        set_control_state("resources_closing");
    }
    STORE_READY.store(false, Ordering::Release);
    if let Err(error) = respire_app::auth::clear_runtime_session() {
        eprintln!("runtime session cleanup failed: {error:#}");
        clean = false;
    }
    if let Err(error) = respire_app::core_sdk::worker::shutdown() {
        respire_logging::error(format_args!("Core worker shutdown failed"));
        eprintln!("Core worker shutdown failed: {error:#}");
        set_control_state("failed");
        // Keep ownership until stop_process confirms native-child cleanup.
        stop_process(1);
    }
    drop(_store);
    // Retain both library and installation ownership through endpoint CAS.
    // Failed/forced exits keep their evidence for exact-owner recovery.
    if should_revoke_clean_stop(clean, STOP_FORCE_TRIGGERED.load(Ordering::Acquire)) {
        if let Some(startup) = startup.as_ref() {
            if let Err(error) = startup.revoke_clean_stop() {
                eprintln!("runtime failed to revoke its final endpoint: {error:#}");
                clean = false;
            }
        }
    }
    if let Some(work) = logout {
        work.finish(&_lock, clean && !STOP_FORCE_TRIGGERED.load(Ordering::Acquire));
    }
    drop(_lock);
    if !STOP_FORCE_TRIGGERED.load(Ordering::Acquire) {
        mark_stop_unfinished(Vec::new());
    }
    set_control_state(if !clean {
        "failed"
    } else if STOP_FORCE_TRIGGERED.load(Ordering::Acquire) {
        "forced_timeout"
    } else {
        "drained"
    });
    stop_process(if clean { 0 } else { 1 });
}

fn result_state(response: Option<&RpcResponse>, pending: bool) -> &'static str {
    if pending {
        "pending"
    } else {
        match response {
            Some(response) if response.ok => "completed",
            Some(_) => "failed",
            None => "unknown",
        }
    }
}

pub fn cached(id: &str) -> Option<RpcResponse> {
    CACHE
        .lock()
        .ok()?
        .iter()
        .find(|entry| entry.operation_id == id)
        .map(|entry| entry.response.clone())
}

