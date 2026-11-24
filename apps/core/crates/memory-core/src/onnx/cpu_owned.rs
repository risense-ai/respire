//! Opt-in local CPU experiment. Native owners never leave a scoped lane.
use super::*;
use std::sync::{Arc, atomic::AtomicBool, mpsc};

#[cfg(test)]
pub(crate) static FAIL_LOAD_LANE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
pub(crate) static FAIL_SPAWN_LANE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
pub(crate) static FAIL_OWNER_SPAWN: AtomicBool = AtomicBool::new(false);
static OWNERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static OWNER_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LANES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(super) fn status() -> serde_json::Value {
    serde_json::json!({"owners":OWNERS.load(Ordering::Acquire), "model_bytes":OWNER_BYTES.load(Ordering::Acquire),
        "native_lanes":LANES.load(Ordering::Acquire), "lane_execution":"parallel", "backing":"Core-owned immutable buffer"})
}

struct Accounting { owner_bytes: Option<usize> }
impl Drop for Accounting {
    fn drop(&mut self) {
        if let Some(bytes) = self.owner_bytes {
            OWNER_BYTES.fetch_sub(bytes, Ordering::AcqRel);
            OWNERS.fetch_sub(1, Ordering::AcqRel);
        } else { LANES.fetch_sub(1, Ordering::AcqRel); }
    }
}

struct LaneUnwind<'a> { stalled: &'a AtomicBool }
impl Drop for LaneUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.stalled.store(true, Ordering::Release);
            INFERENCE_STALLED.store(true, Ordering::Release);
        }
    }
}

struct ExitSignal<'a> { sender: &'a mpsc::Sender<()> }
impl Drop for ExitSignal<'_> { fn drop(&mut self) { let _ = self.sender.send(()); } }

fn spawn_lane<'scope, 'env, F>(scope: &'scope std::thread::Scope<'scope, 'env>, id: u8, run: F)
    -> std::io::Result<std::thread::ScopedJoinHandle<'scope, Result<()>>>
where F: FnOnce() -> Result<()> + Send + 'scope {
    #[cfg(test)]
    if FAIL_SPAWN_LANE.compare_exchange(id, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        return Err(std::io::Error::other("intentional scoped lane spawn failure"));
    }
    std::thread::Builder::new().name(format!("respire-cpu-lane-{id}")).spawn_scoped(scope, run)
}

enum Command {
    Embed { ids: Vec<i64>, mask: Vec<i64>, types: Vec<i64>, response: mpsc::Sender<Result<Vec<f32>>> },
    Stop,
    #[cfg(test)]
    Panic,
}

pub(super) fn active() -> bool { OWNERS.load(Ordering::Acquire) == 1 && LANES.load(Ordering::Acquire) == 2 }

pub(crate) struct CpuOwnedGroup {
    foreground: Arc<mpsc::SyncSender<Command>>,
    background: Arc<mpsc::SyncSender<Command>>,
    owner: Option<std::thread::JoinHandle<Result<()>>>,
}

// Sealed immutable backing; moving either variant preserves its allocation.
enum ModelBacking { BorrowedCopy(Box<[u8]>), Uploaded(Vec<u8>) }
impl std::ops::Deref for ModelBacking {
    type Target = [u8];
    fn deref(&self) -> &[u8] { match self { Self::BorrowedCopy(bytes) => bytes, Self::Uploaded(bytes) => bytes } }
}

impl CpuOwnedGroup {
    pub(crate) fn load(model: &[u8], options: InferenceOptions, foreground_stalled: Arc<AtomicBool>, background_stalled: Arc<AtomicBool>) -> Result<Self> {
        anyhow::ensure!(options.engine == Engine::Cpu && options.cpu_owned_ort_experiment, "invalid CPU owned experiment configuration");
        anyhow::ensure!(model.get(4..8) == Some(&b"ORTM"[..]), "CPU owned experiment requires a self-contained ORT container");
        // Preserve the old borrowed ABI's single owned copy.
        Self::load_backing(ModelBacking::BorrowedCopy(Box::from(model)), options, foreground_stalled, background_stalled)
    }

    pub(crate) fn load_owned(model: Vec<u8>, options: InferenceOptions, foreground_stalled: Arc<AtomicBool>, background_stalled: Arc<AtomicBool>) -> Result<Self> {
        anyhow::ensure!(options.engine == Engine::Cpu && options.cpu_owned_ort_experiment, "invalid CPU owned experiment configuration");
        anyhow::ensure!(model.get(4..8) == Some(&b"ORTM"[..]), "CPU owned experiment requires a self-contained ORT container");
        Self::load_backing(ModelBacking::Uploaded(model), options, foreground_stalled, background_stalled)
    }

    fn load_backing(model: ModelBacking, options: InferenceOptions, foreground_stalled: Arc<AtomicBool>, background_stalled: Arc<AtomicBool>) -> Result<Self> {
        let generation = GENERATION.load(Ordering::Acquire);
        EXECUTION_LIMIT_SECS.store(options.execution_timeout_secs, Ordering::Release);
        let (foreground, foreground_rx) = mpsc::sync_channel(1);
        let (background, background_rx) = mpsc::sync_channel(1);
        let foreground = Arc::new(foreground);
        let background = Arc::new(background);
        // Count-only control handles let the coordinator stop a partially started group.
        let stop_foreground = Arc::clone(&foreground);
        let stop_background = Arc::clone(&background);
        let (ready, ready_rx) = mpsc::channel();
        #[cfg(test)]
        if FAIL_OWNER_SPAWN.swap(false, Ordering::AcqRel) { anyhow::bail!("intentional CPU owner spawn failure"); }
        #[cfg(test)]
        let backing_address = model.as_ptr() as usize;
        let owner = std::thread::Builder::new().name("respire-cpu-model-owner".into()).spawn(move || {
            #[cfg(test)]
            assert_eq!(model.as_ptr() as usize, backing_address, "owner move must preserve backing allocation");
            OWNERS.fetch_add(1, Ordering::AcqRel);
            OWNER_BYTES.fetch_add(model.len(), Ordering::AcqRel);
            let accounting = Accounting { owner_bytes: Some(model.len()) };
            // The unique immutable backing lives on this stack until both native lane sessions are gone.
            let result = std::thread::scope(|scope| -> Result<()> {
                let (fg_ready, fg_wait) = mpsc::channel();
                let (exited, exited_wait) = mpsc::channel();
                let exited = Arc::new(exited);
                let fg_exited = Arc::clone(&exited);
                let fg = match spawn_lane(scope, 1, || lane(&model, foreground_rx, fg_ready, options, generation, foreground_stalled, 1, fg_exited)) {
                    Ok(lane) => lane,
                    Err(error) => { let _ = ready.send(Err(anyhow!("foreground lane spawn failed: {error}"))); return Ok(()); }
                };
                match fg_wait.recv().context("foreground initialization channel closed")? {
                    Ok(()) => (),
                    Err(error) => { let _ = ready.send(Err(error)); return join_lane(fg); }
                }
                // Serialize construction to bound transient load memory, not inference.
                let (bg_ready, bg_wait) = mpsc::channel();
                let bg_exited = Arc::clone(&exited);
                let bg = match spawn_lane(scope, 2, || lane(&model, background_rx, bg_ready, options.for_background(), generation, background_stalled, 2, bg_exited)) {
                    Ok(lane) => lane,
                    Err(error) => {
                        let _ = stop_foreground.send(Command::Stop);
                        let cleanup = join_lane(fg);
                        let _ = ready.send(Err(anyhow!("background lane spawn failed: {error}")));
                        return cleanup;
                    }
                };
                // Always notify the caller before joining the still-running foreground,
                // including a background panic/disconnect before its ready message.
                let initialized = match bg_wait.recv() {
                    Ok(result) => result,
                    Err(error) => Err(anyhow!("background initialization channel closed: {error}")),
                };
                let _ = ready.send(initialized);
                // Any lane exit (including panic) stops its sibling. No polling or
                // dependence on a caller reaching Group::drop to break initialization.
                let _ = exited_wait.recv();
                let _ = stop_foreground.send(Command::Stop);
                let _ = stop_background.send(Command::Stop);
                // Caller owns Stop senders, including the failed-construction path.
                let fg_result = join_lane(fg);
                let bg_result = join_lane(bg);
                fg_result.and(bg_result)
            });
            drop(model);
            drop(accounting);
            result
        }).context("start CPU model owner")?;
        let group = Self { foreground, background, owner: Some(owner) };
        ready_rx.recv().context("CPU model owner initialization ended")??;
        Ok(group)
    }

    pub(crate) fn embed(&self, background: bool, ids: Vec<i64>, mask: Vec<i64>, types: Vec<i64>) -> Result<Vec<f32>> {
        let (response, received) = mpsc::channel();
        let lane = if background { &self.background } else { &self.foreground };
        lane.send(Command::Embed { ids, mask, types, response }).map_err(|_| anyhow!("CPU model lane retired"))?;
        received.recv().context("CPU model lane ended without an embedding")?
    }
}

impl Drop for CpuOwnedGroup {
    fn drop(&mut self) {
        let _ = self.foreground.send(Command::Stop);
        let _ = self.background.send(Command::Stop);
        if let Some(owner) = self.owner.take() {
            match owner.join() {
                Ok(Ok(())) => (),
                Ok(Err(error)) => eprintln!("CPU model owner teardown failed: {error:#}"),
                Err(_) => eprintln!("CPU model owner panicked during teardown"),
            }
        }
    }
}

fn join_lane(lane: std::thread::ScopedJoinHandle<'_, Result<()>>) -> Result<()> {
    lane.join().map_err(|_| anyhow!("CPU inference lane panicked"))?
}

fn create_borrowed(model: &[u8], options: InferenceOptions) -> Result<ort::session::InMemorySession<'_>> {
    let mut builder = common_builder(options.intra_threads)?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build().error_on_failure()])
        .map_err(|error| anyhow!("{error}"))?
        .with_prepacking(false).map_err(|error| anyhow!("{error}"))?
        .with_memory_pattern(false).map_err(|error| anyhow!("{error}"))?;
    for (key, value) in [("session.load_model_format", "ORT"), ("session.use_ort_model_bytes_directly", "1"),
        ("session.use_ort_model_bytes_for_initializers", "1"), ("session.inter_op.allow_spinning", "0")] {
        // The builder recovery payload is not Send/Sync; only its message leaves this lane.
        builder = builder.with_config_entry(key, value).map_err(|error| anyhow!("{error}"))?;
    }
    // ORT 1.20 has no load cancellation; the host terminates timed-out workers.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    let canceler = builder.canceler();
    let expired = Arc::new(AtomicBool::new(false));
    // Count-only watchdog ownership; no model data or builder crosses threads.
    let timer_expired = Arc::clone(&expired);
    let (finished, waiting) = mpsc::channel();
    let limit = options.limit();
    let timer = std::thread::Builder::new().name("respire-cpu-load-deadline".into()).spawn(move || {
        if matches!(waiting.recv_timeout(limit), Err(mpsc::RecvTimeoutError::Timeout)) {
            timer_expired.store(true, Ordering::Release);
            INFERENCE_STALLED.store(true, Ordering::Release);
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            if let Err(error) = canceler.cancel() { eprintln!("CPU model load cancellation failed: {error}"); }
        }
    }).context("start CPU model loading deadline")?;
    NATIVE_STAGE.store(1, Ordering::Release);
    let started = std::time::Instant::now();
    let loaded = builder.commit_from_memory_directly(model);
    let exceeded = started.elapsed() >= limit;
    NATIVE_STAGE.store(0, Ordering::Release);
    let _ = finished.send(());
    timer.join().map_err(|_| anyhow!("CPU model loading deadline panicked"))?;
    INFERENCE_STALLED.store(false, Ordering::Release);
    anyhow::ensure!(!exceeded && !expired.load(Ordering::Acquire), "CPU model loading timed out; no session accepted");
    loaded.context("load shared Core-owned ORT model")
}

fn lane(model: &[u8], requests: mpsc::Receiver<Command>, ready: mpsc::Sender<Result<()>>, options: InferenceOptions,
    generation: u64, stalled: Arc<AtomicBool>, lane_id: u8, exited: Arc<mpsc::Sender<()>>) -> Result<()> {
    // Declared before native objects, so the exit signal follows their actual Drop.
    let _exit = ExitSignal { sender: &exited };
    // An unexpected lane panic requires whole-worker recovery, never a half-group retry.
    let _unwind = LaneUnwind { stalled: &stalled };
    #[cfg(test)]
    if FAIL_LOAD_LANE.compare_exchange(lane_id, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        let _ = ready.send(Err(anyhow!("intentional CPU lane {lane_id} construction failure")));
        return Ok(());
    }
    #[cfg(not(test))]
    let _ = lane_id;
    // SessionBuilder (which may not be Send) is created entirely in this lane.
    let mut session = match create_borrowed(model, options) {
        Ok(session) => session,
        Err(error) => { let _ = ready.send(Err(error)); return Ok(()); }
    };
    LANES.fetch_add(1, Ordering::AcqRel);
    let accounting = Accounting { owner_bytes: None };
    ready.send(Ok(())).map_err(|_| anyhow!("CPU model initialization caller ended"))?;
    let mut pooled_cls = Vec::<f32>::with_capacity(1024);
    while let Ok(command) = requests.recv() {
        match command {
            Command::Stop => break,
            #[cfg(test)]
            Command::Panic => panic!("intentional real scoped lane panic"),
            Command::Embed { ids, mask, types, response } => {
                let result = embed_local(&mut session, &ids, &mask, &types, options, generation, &stalled, &mut pooled_cls);
                let _ = response.send(result);
            }
        }
    }
    // No Session/inner Arc/DynValue/IoBinding is retained outside this stack.
    drop(session);
    drop(accounting);
    Ok(())
}

fn embed_local(session: &mut ort::session::InMemorySession<'_>, ids: &[i64], mask: &[i64], types: &[i64], options: InferenceOptions,
    generation: u64, stalled: &Arc<AtomicBool>, pooled_cls: &mut Vec<f32>) -> Result<Vec<f32>> {
    anyhow::ensure!(generation == GENERATION.load(Ordering::Acquire), "inference engine changed; retry the command");
    anyhow::ensure!(!ids.is_empty() && ids.len() <= 512 && mask.len() == ids.len() && types.len() == ids.len(), "invalid CPU inference input shape");
    let output = run_session_with_deadline(session, "CPU", stalled, ids, mask, types, 1, ids.len(), options.limit())?;
    let (shape, values) = output.tensor()?;
    anyhow::ensure!(shape.len() == 3 && shape[0] == 1 && shape[1] == ids.len() as i64 && shape[2] == 1024
        && values.len() == ids.len() * 1024, "invalid CPU embedding output shape: {shape:?}");
    pooled_cls.clear();
    pooled_cls.extend_from_slice(&values[..1024]);
    let norm = pooled_cls.iter().map(|value| value * value).sum::<f32>().sqrt();
    anyhow::ensure!(norm.is_finite() && norm > 1e-8, "invalid M3 embedding norm");
    for value in pooled_cls.iter_mut() { *value /= norm; }
    // Drop the native owner before returning. Only 4KiB Rust values cross lanes.
    drop(output);
    let mut result = Vec::with_capacity(1024);
    result.extend_from_slice(pooled_cls);
    Ok(result)
}

#[cfg(test)]
pub(super) fn verify_native_failure_cancel_and_recovery(model: &[u8], options: InferenceOptions) -> Result<()> {
    let mut session = create_borrowed(model, options)?;
    let stalled = Arc::new(AtomicBool::new(false));
    let invalid = run_session_with_deadline(&mut session, "CPU", &stalled, &[i64::MAX], &[1], &[0], 1, 1, options.limit())
        .err().context("invalid Gather input must fail")?;
    assert!(format!("{invalid:#}").contains("indices") || format!("{invalid:#}").contains("Gather"));
    let cancelled = ort::session::RunOptions::new()?;
    cancelled.terminate()?;
    let failure = run_local(&mut session, "CPU", &[0], &[1], &[0], 1, 1, &cancelled, &crate::embedding::OnnxNames::default()).err().context("cancelled Run must fail")?;
    assert!(format!("{failure:#}").contains("terminate"));
    let expired = run_session_with_deadline(&mut session, "CPU", &stalled, &[0; 512], &[1; 512], &[0; 512], 1, 512, Duration::ZERO)
        .err().context("expired Run must return no embedding")?;
    assert!(format!("{expired:#}").contains("timed out"));
    assert!(!inference_stalled());
    let mut pool = Vec::with_capacity(1024);
    assert_eq!(embed_local(&mut session, &[0], &[1], &[0], options, GENERATION.load(Ordering::Acquire), &stalled, &mut pool)?.len(), 1024);
    Ok(())
}

#[cfg(test)]
pub(super) fn verify_real_lane_panic_teardown(model: &[u8], options: InferenceOptions) -> Result<()> {
    let foreground_stalled = Arc::new(AtomicBool::new(false));
    let background_stalled = Arc::new(AtomicBool::new(false));
    let group = CpuOwnedGroup::load_owned(model.to_vec(), options, Arc::clone(&foreground_stalled), background_stalled)?;
    group.foreground.send(Command::Panic).map_err(|_| anyhow!("panic fixture lane closed early"))?;
    let started = std::time::Instant::now();
    while !group.owner.as_ref().is_some_and(|owner| owner.is_finished()) {
        anyhow::ensure!(started.elapsed() < Duration::from_secs(5), "panic must stop sibling and join without waiting for caller Drop");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(status()["owners"], 0);
    assert_eq!(status()["native_lanes"], 0);
    assert!(foreground_stalled.load(Ordering::Acquire));
    assert!(inference_stalled(), "real panic must require whole-worker recovery");
    drop(group);
    // Test-only cleanup in this unit process. Product recovery replaces the process.
    INFERENCE_STALLED.store(false, Ordering::Release);
    Ok(())
}
