pub fn index_yield_to_foreground(generation: usize) -> Result<()> {
    anyhow::ensure!(
        !respire_app::service::service_paused(),
        "background index paused by service mode"
    );
    // A separate background Session no longer waits for foreground inference.
    // Context and cancellation checks still occur between every committed source.
    check_sync_context(generation)?;
    crate::model_progress::check()
}

pub fn diary_commit(generation: usize, action: &mut dyn FnMut() -> Result<()>) -> Result<()> {
    let gate = shared_exclusive();
    let _write = gate.acquire(false);
    check_sync_context(generation)?;
    // Account/mode transitions use the write gate; lifecycle transitions use
    // takeover. Never block on takeover while its owner waits for INDEX_RUNNING.
    let _lifecycle = crate::runtime_policy::try_takeover_lock()?;
    crate::model_progress::commit(|| {
        check_sync_context(generation)?;
        anyhow::ensure!(
            !respire_app::service::service_paused(),
            "background index paused by service mode"
        );
        action()
    })
}

/// The database's source-checked missing artifacts are the resumable work queue.
/// Model preparation runs on this thread without taking the foreground write gate.
pub fn index_loop() {
    mark_worker();
    if let Err(error) = index_loop_inner() {
        eprintln!("background retrieval index worker stopped: {error:#}");
    }
    INDEX_RUNNING.store(false, Ordering::Release);
    INDEX_ON.store(false, Ordering::Release);
}

pub fn index_has_work() -> bool {
    INDEX_RUNNING.load(Ordering::Acquire) || INDEX_WORK.lock().is_ok_and(|work| work.requested)
}

pub fn index_loop_inner() -> Result<()> {
    let mut model_wait_started: Option<Instant> = None;
    loop {
        if !crate::rpc_serve::store_ready() || respire_app::service::service_paused() || crate::host_hooks::background_paused() {
            if STOPPING.load(Ordering::Acquire) {
                let mut work = INDEX_WORK
                    .lock()
                    .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
                work.requested = false;
                work.state = "stopped";
                return Ok(());
            }
            let mut work = INDEX_WORK
                .lock()
                .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
            work.state = "paused";
            let _paused = INDEX_CV
                .wait_timeout(work, Duration::from_secs(1))
                .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
            continue;
        }
        {
            let mut work = INDEX_WORK
                .lock()
                .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
            while !STOPPING.load(Ordering::Acquire) {
                if !work.requested {
                    work = INDEX_CV.wait(work)
                        .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
                } else if let Some(delay) = work.create_delay(Instant::now()) {
                    work = INDEX_CV.wait_timeout(work, delay)
                        .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?.0;
                } else {
                    break;
                }
            }
            if STOPPING.load(Ordering::Acquire) {
                work.state = "stopped";
                work.requested = false;
                return Ok(());
            }
            work.requested = false;
            work.create_batch = None;
            work.state = "running";
        }
        INDEX_RUNNING.store(true, Ordering::Release);
        let generation = GENERATION.load(Ordering::Acquire);
        let result = (|| -> Result<bool> {
            check_sync_context(generation)?;
            let store = respire_app::service::open_store()?;
            let model = store.retrieval_model()?;
            let pending = store.index_missing(&model)?;
            let diary_pending = respire_app::diary::needs_rebuild(&store)?;
            // An indexed library still needs the CPU container for its next query.
            // Missing assets are reported; only the CLI may install them.
            let active_profile = store.active_selection()?;
            let cpu_upgrade = active_profile.is_none()
                && model == "m3"
                && respire_app::MemoryTransport::count(&store)? > 0
                && crate::model_assets::cpu_model_preparation_needed()?;
            if !pending && !diary_pending && !cpu_upgrade {
                return Ok(true);
            }
            let Some(_operation) = crate::model_progress::Operation::try_begin_background_index()?
            else {
                return Ok(false);
            };
            index_yield_to_foreground(generation)?;
            if diary_pending
                && !pending
                && !cpu_upgrade
                && respire_app::diary::clear_if_empty(&store, |action| {
                    diary_commit(generation, action)
                })?
            {
                return Ok(true);
            }
            if let Some(selection) = active_profile.as_ref() {
                crate::model_progress::update("verify", &selection.model, 0, None)?;
                crate::model_assets::verify_selection_assets(selection)?;
            } else if model == "m3" {
                crate::model_progress::update("verify", "BGE-M3", 0, None)?;
                crate::model_assets::verify_m3_assets()?;
            }
            check_sync_context(generation)?;
            if !pending && !diary_pending {
                return Ok(true);
            }
            let keys = crate::engine::build_session()?;
            let embedder = respire_app::memory::bge::BgeEmbedder::load_model(&model)?;
            while pending {
                let rebuilt =
                    store.rebuild_index_with_progress(&keys, &embedder, &model, |done, total| {
                        index_yield_to_foreground(generation)?;
                        crate::model_progress::update(
                            "index",
                            &model,
                            done as u64,
                            Some(total as u64),
                        )
                    });
                match rebuilt {
                    Ok(_) => break,
                    Err(error)
                        if error
                            .downcast_ref::<respire_app::transport::local::IndexSourceChanged>()
                            .is_some() =>
                    {
                        // Keep this Operation across source changes: cancellation and
                        // the 30-minute idle budget apply to the whole resumed task.
                        crate::model_progress::check()?;
                        let mut work = INDEX_WORK
                            .lock()
                            .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
                        work.error = Some(format!("{error:#}"));
                        drop(work);
                        for _ in 0..5 {
                            check_sync_context(generation)?;
                            crate::model_progress::check()?;
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            respire_app::diary::rebuild_with_progress(
                &store,
                &keys,
                &embedder,
                || {
                    index_yield_to_foreground(generation)?;
                    crate::model_progress::check()
                },
                |action| diary_commit(generation, action),
                |done| crate::model_progress::update("index", "diary-fragments", done as u64, None),
            )?;
            check_sync_context(generation)?;
            Ok(true)
        })();
        INDEX_RUNNING.store(false, Ordering::Release);
        let mut work = INDEX_WORK
            .lock()
            .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?;
        match result {
            Ok(true) => {
                work.state = "ready";
                work.error = None;
                model_wait_started = None;
            }
            Ok(false) => {
                let started = model_wait_started.get_or_insert_with(Instant::now);
                if started.elapsed() >= Duration::from_secs(1800) {
                    work.state = "failed";
                    work.requested = false;
                    work.error = Some(
                        "background indexing timed out waiting for another model operation".into(),
                    );
                    model_wait_started = None;
                    continue;
                }
                work.state = "waiting_model";
                work.requested = true;
                work = INDEX_CV
                    .wait_timeout(work, Duration::from_secs(1))
                    .map_err(|_| anyhow::anyhow!("index work lock poisoned"))?
                    .0;
            }
            Err(error)
                if error
                    .downcast_ref::<crate::model_progress::OperationStopped>()
                    .is_some() =>
            {
                model_wait_started = None;
                work.state = "paused";
                work.requested = false;
                work.error = Some(format!("{error:#}"));
            }
            Err(error) if generation != GENERATION.load(Ordering::Acquire) => {
                model_wait_started = None;
                work.state = "scheduled";
                work.requested = true;
                work.error = Some(format!("{error:#}"));
            }
            Err(error)
                if error
                    .downcast_ref::<crate::model_assets::AssetsUnavailable>()
                    .is_some() =>
            {
                model_wait_started = None;
                work.state = "missing";
                work.error = Some(format!("{error:#}"));
            }
            Err(error) => {
                model_wait_started = None;
                work.state = "failed";
                work.error = Some(format!("{error:#}"));
                eprintln!("background retrieval indexing failed: {error:#}");
            }
        }
        drop(work);
    }
}

/// An explicit direct command releases its library lock before starting this work.
pub fn request_index() -> Result<Value> {
    if worker_active() {
        kick_index();
        return Ok(index_status());
    }
    let response = crate::host_hooks::call_runtime_method("index.prepare", Vec::new(), true)?;
    anyhow::ensure!(response.ok, "{}", response.error.unwrap_or_default());
    response
        .envelope
        .map(|envelope| envelope.summary)
        .ok_or_else(|| anyhow::anyhow!("runtime did not acknowledge background indexing"))
}

// ---- Recall-statistics enqueue bridge ----
// The write queue itself (WriteJob/WRITE_TX) still lives beside the runtime
// Job/admission machinery in the CLI binary; the engine reaches it through
// this installed hook so the shared recall path has one call spelling.

pub type EnqueueRecallStats = fn(RecallStats, usize);

static RECALL_STATS_ENQUEUE: OnceLock<EnqueueRecallStats> = OnceLock::new();

/// Install the host write-queue enqueue implementation. First installer wins.
pub fn install_recall_stats_enqueue(hook: EnqueueRecallStats) -> bool {
    RECALL_STATS_ENQUEUE.set(hook).is_ok()
}

/// Statistics are best effort; never wait for queue capacity on the recall reply path.
pub fn queue_recall_stats(stats: RecallStats, generation: usize) {
    match RECALL_STATS_ENQUEUE.get() {
        Some(enqueue) => enqueue(stats, generation),
        None => {
            STATS_REJECTED.fetch_add(1, Ordering::AcqRel);
            eprintln!("recall statistics rejected: recall statistics queue is not installed");
        }
    }
}
