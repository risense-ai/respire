//! Shared ONNX session policy for BGE-M3.
//!
//! Explicit engine selection. Failures never switch to a different engine.

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[cfg(feature = "cpu-owned-ort-experiment")]
mod cpu_owned;
#[cfg(feature = "cpu-owned-ort-experiment")]
pub(crate) use cpu_owned::CpuOwnedGroup;
#[cfg(all(test, feature = "cpu-owned-ort-experiment"))]
#[path = "onnx/cpu_owned_tests.rs"]
mod cpu_owned_tests;

use std::sync::atomic::{AtomicU64, Ordering};
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static RESET_TRANSITION_HOOK: std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn arm_reset_transition_hook(entered: std::sync::mpsc::Sender<()>, release: std::sync::mpsc::Receiver<()>) -> Result<()> {
    let mut hook = RESET_TRANSITION_HOOK.lock().map_err(|_| anyhow!("reset test hook poisoned"))?;
    anyhow::ensure!(hook.is_none(), "reset test hook already armed");
    *hook = Some((entered, release));
    Ok(())
}

#[cfg(test)]
pub(crate) fn reset_transition_hook() {
    let hook = RESET_TRANSITION_HOOK.lock().ok().and_then(|mut hook| hook.take());
    if let Some((entered, release)) = hook {
        let _ = entered.send(());
        let _ = release.recv_timeout(Duration::from_secs(10));
    }
}

#[cfg(test)]
pub(crate) fn current_generation_for_test() -> u64 { GENERATION.load(Ordering::Acquire) }

// The caller holds BGE's canonical shared-slot lock across retirement and
// this advance. Session construction takes that same lock before capturing it.
pub(crate) fn advance_model_generation() { GENERATION.fetch_add(1, Ordering::AcqRel); }

// Explicit fixture control; absent from every default/release SDK build.
#[cfg(feature = "native-fault-tests")]
static TEST_FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(feature = "native-fault-tests")]
static TEST_STAGE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(feature = "native-fault-tests")]
pub fn arm_native_fault(mode: u64) -> Result<()> {
    let cpu_panic = {
        #[cfg(feature = "cpu-owned-ort-experiment")]
        { mode == 4 && cpu_owned::active() }
        #[cfg(not(feature = "cpu-owned-ort-experiment"))]
        { false }
    };
    anyhow::ensure!(matches!(mode, 1..=3) || cpu_panic, "invalid native fixture mode");
    TEST_FAULT.compare_exchange(0, mode as u8, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| anyhow!("native fixture already armed"))?;
    TEST_STAGE.store(1, Ordering::Release);
    Ok(())
}

/// Intra-op threads for every inference session. Must stay 1.
pub const INTRA_OP_THREADS: usize = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Npu,
    Gpu,
    #[default]
    Cpu,
}

impl Engine {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "npu" => Ok(Self::Npu),
            "gpu" => Ok(Self::Gpu),
            "cpu" => Ok(Self::Cpu),
            _ => Err(anyhow!(
                "engine must be npu, gpu, or cpu; automatic mode is no longer supported"
            )),
        }
    }
}

/// Host-supplied inference settings; no file or environment discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceOptions {
    pub engine: Engine,
    pub execution_timeout_secs: u64,
    pub cpu_owned_ort_experiment: bool,
    pub intra_threads: usize,
    pub background_intra_threads: usize,
    pub host_execution_provider: bool,
}

impl InferenceOptions {
    pub fn from_config(config: &serde_json::Value) -> Result<Self> {
        let engine = Engine::parse(config["engine"].as_str()
            .ok_or_else(|| anyhow!("host inference configuration missing engine"))?)?;
        let execution_timeout_secs = config["execution_timeout_secs"].as_u64()
            .ok_or_else(|| anyhow!("host inference configuration missing execution_timeout_secs"))?;
        anyhow::ensure!(execution_timeout_secs > 0, "execution timeout must be positive");
        let cpu_owned_ort_experiment = match config.get("cpu_owned_ort_experiment") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(|| anyhow!("cpu_owned_ort_experiment must be boolean"))?,
        };
        anyhow::ensure!(!cpu_owned_ort_experiment || cfg!(feature = "cpu-owned-ort-experiment"), "CPU owned ORT experiment not enabled in this SDK build");
        anyhow::ensure!(!cpu_owned_ort_experiment || engine == Engine::Cpu, "CPU owned ORT experiment requires CPU; accelerator policy unchanged");
        let background_intra_threads = match config.get("background_intra_threads") {
            None => INTRA_OP_THREADS,
            Some(value) => {
                let value = value.as_u64().filter(|value| (1..=16).contains(value))
                    .ok_or_else(|| anyhow!("background_intra_threads must be an integer from 1 to 16"))?;
                value as usize
            }
        };
        let host_execution_provider = config.get("host_execution_provider").map(|value| value.as_bool().ok_or_else(|| anyhow!("host_execution_provider must be boolean"))).transpose()?.unwrap_or(false);
        Ok(Self { engine, execution_timeout_secs, cpu_owned_ort_experiment, host_execution_provider,
            intra_threads: INTRA_OP_THREADS, background_intra_threads })
    }

    pub(crate) fn for_background(self) -> Self {
        Self { intra_threads: self.background_intra_threads, ..self }
    }

    fn limit(self) -> Duration { Duration::from_secs(self.execution_timeout_secs) }
}

/// Release shared compatibility sessions. Actual configuration belongs to the host.
pub fn reset_sessions() -> Result<()> {
    anyhow::ensure!(!inference_stalled() && !crate::bge::BgeEmbedder::sessions_stalled(), "inference unresponsive; host runtime recovery required before resetting the model");
    let _permit = crate::inference_queue::QUEUE.acquire(crate::inference_queue::WAIT_LIMIT)?;
    let _background = crate::inference_queue::BACKGROUND_QUEUE.acquire(crate::inference_queue::WAIT_LIMIT)?;
    crate::bge::BgeEmbedder::drop_shared()?;
    Ok(())
}

fn initialize_native_policy() {
    static INITIALIZED: std::sync::Once = std::sync::Once::new();
    INITIALIZED.call_once(|| {
        // Configure before any native environment exists. A host-preconfigured environment stays intact.
        let _ = ort::init().with_telemetry(false).commit();
    });
}

/// Borrow the existing native runtime for host-owned backend registration.
/// The host owns path resolution and loading; borrowed pointers remain valid for this callback.
pub fn with_native_environment<T>(callback: impl FnOnce(*mut std::ffi::c_void, *const std::ffi::c_void) -> Result<T>) -> Result<T> {
    use ort::AsPointer;
    initialize_native_policy();
    let environment = ort::environment::Environment::current()?;
    callback(environment.ptr().cast_mut().cast(), std::ptr::from_ref(ort::api()).cast())
}

/// In-process symbol addresses let the host identify the runtime actually used.
pub fn native_runtime_identity() -> serde_json::Value {
    let api = ort::api();
    serde_json::json!({"kind":"onnxruntime", "api_version":ort::MINOR_VERSION,
        "build_info":ort::info(), "api_address":format!("{api:p}"),
        "symbols":{
            "CreateSessionFromArray":format!("{:p}", api.CreateSessionFromArray as *const ()),
            "Run":format!("{:p}", api.Run as *const ())
        }})
}

fn common_builder(intra_threads: usize) -> Result<ort::session::builder::SessionBuilder> {
    initialize_native_policy();
    let builder = Session::builder().map_err(|e| anyhow!("ort session builder: {e}"))?;
    let builder = builder
        .with_no_environment_execution_providers()
        .map_err(|e| anyhow!("ort disable env eps: {e}"))?;
    let builder = builder
        .with_intra_threads(intra_threads)
        .map_err(|e| anyhow!("ort intra_threads: {e}"))?;
    let builder = builder
        .with_inter_threads(1)
        .map_err(|e| anyhow!("ort inter_threads: {e}"))?;
    let builder = builder
        .with_parallel_execution(false)
        .map_err(|e| anyhow!("ort sequential: {e}"))?;
    let builder = builder
        .with_intra_op_spinning(false)
        .map_err(|e| anyhow!("ort intra spinning: {e}"))?;
    Ok(builder)
}

#[cfg(test)]
fn create(onnx: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>) -> Result<(Session, String)> {
    create_with_profile(onnx, options, native_options, &crate::embedding::EmbeddingProfile::m3())
}

fn create_with_profile(onnx: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>, profile: &crate::embedding::EmbeddingProfile) -> Result<(Session, String)> {
    let engine = options.engine;
    let limit = options.limit();
    let mut builder = common_builder(options.intra_threads)?;
    let mut label = "CPU".to_owned();
    let mut host_configured = false;
    if options.host_execution_provider {
        use ort::AsPointer;
        let configure = native_options.ok_or_else(|| anyhow!("host execution provider requires a native options callback"))?;
        let environment = ort::environment::Environment::current()?;
        configure(builder.ptr().cast_mut().cast(), environment.ptr().cast_mut().cast(), std::ptr::null())?;
        label = "Host".to_owned();
        host_configured = true;
    } else {
    match engine {
        Engine::Cpu => {
            builder = builder
                .with_execution_providers([ort::ep::CPU::default().build().error_on_failure()])
                .map_err(|e| anyhow!("{e}"))?;
            #[cfg(windows)]
            {
                let env = ort::environment::Environment::current()?;
                if let Some(cpu) = env
                    .devices()
                    .find(|d| d.ep().is_ok_and(|ep| ep == "CPUExecutionProvider"))
                {
                    if let Some(configure) = native_options {
                        use ort::AsPointer;
                        configure(builder.ptr().cast_mut().cast(), env.ptr().cast_mut().cast(), cpu.ptr().cast())?;
                        host_configured = true;
                    } else {
                        builder = builder.with_devices([cpu], None).map_err(|e| anyhow!("{e}"))?;
                    }
                };
            }
        }
        Engine::Npu | Engine::Gpu => {
            #[cfg(windows)]
            {
                if engine == Engine::Gpu {
                    // DirectML serves every DX12 GPU (Win10 1809+) from the DML ORT
                    // build. Host device enumeration stays exclusive to NPU vendors.
                    use ort::ep::DirectML;
                    builder = builder
                        .with_execution_providers([DirectML::default().build().error_on_failure()])
                        .map_err(|e| anyhow!("{e}"))?;
                    label = "Gpu: DmlExecutionProvider".to_owned();
                    builder = builder.with_memory_pattern(false).map_err(|e| anyhow!("{e}"))?;
                    // The host still owns common options such as profiling.
                    // Leave the generic callback below enabled with a null device:
                    // DirectML is already registered, so it must not register it twice.
                } else {
                    use ort::memory::DeviceType;
                    let env = ort::environment::Environment::current()?;
                    let device = env
                        .devices()
                        .filter(|d| {
                            d.hardware_device().ty() == DeviceType::NPU
                                && !d.ep().is_ok_and(|ep| ep.ends_with(".AUTO"))
                        })
                        .next()
                        .ok_or_else(|| anyhow!("no registered {engine:?} execution provider"))?;
                    label = format!("{engine:?}: {}", device.ep()?);
                    builder = builder
                        .with_disable_cpu_fallback()
                        .map_err(|e| anyhow!("{e}"))?
                        .with_dimension_override("batch_size", 1)
                        .map_err(|e| anyhow!("{e}"))?
                        .with_dimension_override("sequence_length", profile.max_tokens as i64)
                        .map_err(|e| anyhow!("{e}"))?
                        .with_memory_pattern(false)
                        .map_err(|e| anyhow!("{e}"))?;
                    if let Some(configure) = native_options {
                        use ort::AsPointer;
                        configure(builder.ptr().cast_mut().cast(), env.ptr().cast_mut().cast(), device.ptr().cast())?;
                        host_configured = true;
                    } else {
                        builder = builder.with_devices([device], None).map_err(|e| anyhow!("{e}"))?;
                    }
                }
            }
            #[cfg(target_os = "macos")]
            {
                use ort::ep::{
                    coreml::{ComputeUnits, ModelFormat},
                    CoreML,
                };
                let units = if engine == Engine::Npu {
                    ComputeUnits::CPUAndNeuralEngine
                } else {
                    ComputeUnits::CPUAndGPU
                };
                // Restrict CoreML to the selected accelerator family.
                // CoreML still controls CPU placement of individual operations.
                label = format!("CoreML {units:?}");
                builder = builder
                    .with_execution_providers([CoreML::default()
                        .with_compute_units(units)
                        .with_model_format(ModelFormat::MLProgram)
                        .build()
                        .error_on_failure()])
                    .map_err(|e| anyhow!("{e}"))?;
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            {
                return Err(anyhow!("{engine:?} is not supported on this platform"));
            }
        }
    }
    }
    if !host_configured {
        if let Some(configure) = native_options {
            use ort::AsPointer;
            let environment = ort::environment::Environment::current()?;
            configure(builder.ptr().cast_mut().cast(), environment.ptr().cast_mut().cast(), std::ptr::null())?;
        }
    }
    // ORT 1.20 has no load cancellation; the host terminates timed-out workers.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    let canceler = builder.canceler();
    let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timer_expired = std::sync::Arc::clone(&expired);
    let (finished, waiting) = std::sync::mpsc::channel::<()>();
    let timer = std::thread::Builder::new().name("respire-model-load-deadline".into())
        .spawn(move || {
            if matches!(waiting.recv_timeout(limit), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                timer_expired.store(true, Ordering::Release);
                INFERENCE_STALLED.store(true, Ordering::Release);
                #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
                if let Err(error) = canceler.cancel() { eprintln!("ONNX model load cancellation failed: {error}"); }
            }
        }).context("start model loading deadline watchdog")?;
    NATIVE_STAGE.store(1, Ordering::Release);
    let started = std::time::Instant::now();
    let loaded = builder.commit_from_memory(onnx);
    let exceeded = started.elapsed() >= limit;
    NATIVE_STAGE.store(0, Ordering::Release);
    let _ = finished.send(());
    timer.join().map_err(|_| anyhow!("model loading deadline watchdog panicked"))?;
    INFERENCE_STALLED.store(false, Ordering::Release);
    anyhow::ensure!(!exceeded && !expired.load(Ordering::Acquire), "model loading timed out; no session accepted; retry the command");
    let session = loaded
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("load host-supplied model bytes with {label}"))?;
    Ok((session, label))
}

pub struct InferenceSession {
    session: Session,
    generation: u64,
    execution_limit: Duration,
    pub selected: String,
    pub stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    profile: crate::embedding::EmbeddingProfile,
    names: crate::embedding::OnnxNames,
}

/// Keep the native output owner alive while pooling borrows its tensor.
pub(crate) enum InferenceOutput {
    Native(ort::value::DynValue),
    /// Non-ORT backends return materialized row-major f32 output.
    Owned { shape: Vec<i64>, data: Vec<f32> },
}

impl InferenceOutput {
    pub(crate) fn tensor(&self) -> Result<(&[i64], &[f32])> {
        match self {
            Self::Native(value) => {
                let (shape, values) = value.try_extract_tensor::<f32>()?;
                Ok((shape, values))
            }
            Self::Owned { shape, data } => Ok((shape, data)),
        }
    }
}

static INFERENCE_STALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// Includes temporary CPU reference sessions; each expired run clears only its own count.
static STALLED_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static NATIVE_STAGE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
pub(crate) static EXECUTION_LIMIT_SECS: AtomicU64 = AtomicU64::new(120);

/// An expired native run may still be executing if its provider ignores cancellation.
pub fn inference_stalled() -> bool {
    INFERENCE_STALLED.load(Ordering::Acquire) || STALLED_RUNS.load(Ordering::Acquire) > 0
}

pub fn inference_status() -> Result<serde_json::Value> {
    let (waiting, active) = crate::inference_queue::QUEUE.status()?;
    let (background_waiting, background_active) = crate::inference_queue::BACKGROUND_QUEUE.status()?;
    let recovery = inference_stalled() || crate::bge::BgeEmbedder::sessions_stalled();
    let phase = if recovery { "unresponsive" } else if NATIVE_STAGE.load(Ordering::Acquire) == 1 {
        "loading"
    } else if active || background_active { "running" } else { "idle" };
    let status = serde_json::json!({"queued":waiting + background_waiting,"active":active || background_active,"capacity":64,
        "foreground":{"queued":waiting,"active":active,"capacity":32},
        "background":{"queued":background_waiting,"active":background_active,"capacity":32},
        "phase":phase,
        "wait_timeout_secs":crate::inference_queue::WAIT_LIMIT.as_secs(),
        "execution_timeout_secs":EXECUTION_LIMIT_SECS.load(Ordering::Acquire),
        "host_recovery_required":recovery});
    let status = {
        let mut status = status;
        status["intra_threads"] = match crate::bge::BgeEmbedder::inference_thread_settings() {
            Some((foreground, background)) => serde_json::json!({"foreground":foreground,"background":background}),
            None => serde_json::Value::Null,
        };
        status
    };
    #[cfg(feature = "cpu-owned-ort-experiment")]
    let status = { let mut status = status; status["cpu_owned_ort_experiment"] = cpu_owned::status(); status };
    #[cfg(feature = "native-fault-tests")]
    let status = { let mut status = status; status["test_fault_stage"] = serde_json::json!(TEST_STAGE.load(Ordering::Acquire)); status };
    Ok(status)
}

pub fn commit_onnx_session_for(onnx: &[u8], options: InferenceOptions) -> Result<InferenceSession> {
    commit_onnx_session_with_options(onnx, options, None)
}

pub fn commit_onnx_session_with_options(onnx: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>) -> Result<InferenceSession> {
    commit_onnx_session_with_profile(onnx, options, native_options, &crate::embedding::EmbeddingProfile::m3(), &crate::embedding::OnnxNames::default())
}

pub fn commit_onnx_session_with_profile(onnx: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>, profile: &crate::embedding::EmbeddingProfile, names: &crate::embedding::OnnxNames) -> Result<InferenceSession> {
    let generation = GENERATION.load(Ordering::Acquire);
    EXECUTION_LIMIT_SECS.store(options.execution_timeout_secs, Ordering::Release);
    let (session, selected) = create_with_profile(onnx, options, native_options, profile).with_context(|| format!("selected {:?} engine failed to load; no engine fallback", options.engine))?;
    Ok(InferenceSession { session, generation, execution_limit: options.limit(), selected, profile:profile.clone(), names:names.clone(),
        stalled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) })
}

impl InferenceSession {
    #[cfg(test)]
    pub(crate) fn generation_for_test(&self) -> u64 { self.generation }
    /// Execute in the runtime on the selected engine and propagate native errors.
    pub(crate) fn run(
        &mut self,
        ids: &[i64],
        mask: &[i64],
        types: &[i64],
        batch: usize,
        seq: usize,
    ) -> Result<InferenceOutput> {
        let count = batch
            .checked_mul(seq)
            .ok_or_else(|| anyhow!("input shape overflow"))?;
        anyhow::ensure!(
            batch > 0
                && seq > 0
                && seq <= self.profile.max_tokens
                && ids.len() == count
                && mask.len() == count
                && types.len() == count,
            "invalid inference input shape"
        );
        anyhow::ensure!(
            self.generation == GENERATION.load(Ordering::Acquire),
            "inference engine changed; retry the command"
        );
        self.run_with_deadline(ids, mask, types, batch, seq, self.execution_limit)
    }

    fn run_with_deadline(&mut self, ids: &[i64], mask: &[i64], types: &[i64],
        batch: usize, seq: usize, limit: std::time::Duration) -> Result<InferenceOutput> {
        run_session_with_profile(&mut self.session, &self.selected, &self.stalled, ids, mask, types, batch, seq, limit, &self.names)
    }
}

// Native output is private to this module and its scoped CPU lane implementation.
fn run_session_with_deadline(session: &mut Session, selected: &str, session_stalled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ids: &[i64], mask: &[i64], types: &[i64], batch: usize, seq: usize, limit: Duration) -> Result<InferenceOutput> {
    run_session_with_profile(session, selected, session_stalled, ids, mask, types, batch, seq, limit, &crate::embedding::OnnxNames::default())
}

fn run_session_with_profile(session: &mut Session, selected: &str, session_stalled: &std::sync::Arc<std::sync::atomic::AtomicBool>, ids: &[i64], mask: &[i64], types: &[i64], batch: usize, seq: usize, limit: Duration, names: &crate::embedding::OnnxNames) -> Result<InferenceOutput> {
        #[cfg(feature = "native-fault-tests")]
        let fault = TEST_FAULT.swap(0, Ordering::AcqRel);
        #[cfg(feature = "native-fault-tests")]
        if fault == 4 {
            TEST_STAGE.store(2, Ordering::Release);
            panic!("intentional scoped CPU lane panic fixture");
        }
        #[cfg(feature = "native-fault-tests")]
        let limit = if fault != 0 { Duration::from_secs(2) } else { limit };
        let options = std::sync::Arc::new(ort::session::RunOptions::new()?);
        let cancel = std::sync::Arc::clone(&options);
        let stalled = std::sync::Arc::clone(session_stalled);
        let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let timer_expired = std::sync::Arc::clone(&expired);
        let (finished, waiting) = std::sync::mpsc::channel::<()>();
        let timer = std::thread::Builder::new().name("respire-inference-deadline".into())
            .spawn(move || {
                if matches!(waiting.recv_timeout(limit), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                    timer_expired.store(true, Ordering::Release);
                    stalled.store(true, Ordering::Release);
                    STALLED_RUNS.fetch_add(1, Ordering::AcqRel);
                    let terminated = cancel.terminate();
                    if let Err(error) = &terminated {
                        eprintln!("ONNX inference cancellation failed: {error}");
                    }
                    #[cfg(feature = "native-fault-tests")]
                    if fault != 0 { TEST_STAGE.store(if terminated.is_ok() { 3 } else { 5 }, Ordering::Release); }
                }
            }).context("start inference deadline watchdog")?;
        NATIVE_STAGE.store(2, Ordering::Release);
        let started = std::time::Instant::now();
        #[cfg(feature = "native-fault-tests")]
        if fault != 0 {
            TEST_STAGE.store(2, Ordering::Release);
            // Hold the real session/permit at Run's boundary, with its real
            // watchdog and RunOptions. This does not claim ORT itself hangs.
            if fault == 1 { loop { std::thread::park(); } }
            while TEST_STAGE.load(Ordering::Acquire) != 3 { std::thread::sleep(Duration::from_millis(5)); }
            if fault == 3 { std::thread::sleep(Duration::from_secs(20)); }
        }
        let result = run_local(session, selected, ids, mask, types, batch, seq, &options, names);
        let exceeded = started.elapsed() >= limit;
        NATIVE_STAGE.store(0, Ordering::Release);
        let _ = finished.send(());
        timer.join().map_err(|_| anyhow!("inference deadline watchdog panicked"))?;
        session_stalled.store(false, Ordering::Release);
        if expired.load(Ordering::Acquire) { STALLED_RUNS.fetch_sub(1, Ordering::AcqRel); }
        if exceeded || expired.load(Ordering::Acquire) {
            anyhow::bail!("inference execution timed out; no embedding produced; retry the command");
        }
        result.with_context(|| format!("{selected} inference failed; retry the command; inspect `rsrs doctor` if it persists"))
}

fn run_local(session: &mut Session, _label: &str, ids: &[i64], mask: &[i64], types: &[i64], batch: usize, seq: usize, options: &ort::session::RunOptions, names: &crate::embedding::OnnxNames) -> Result<InferenceOutput> {
    use ort::value::TensorRef;
    let ids = TensorRef::from_array_view(([batch, seq], ids))?;
    let mut inputs = ort::inputs![names.input_ids.clone() => ids];
    if let Some(name) = &names.attention_mask {
        if session.inputs().iter().any(|input| input.name() == name) {
            inputs.push((name.clone().into(), TensorRef::from_array_view(([batch, seq], mask))?.into()));
        }
    }
    if let Some(name) = &names.token_type_ids {
        if session.inputs().iter().any(|input| input.name() == name) {
            inputs.push((name.clone().into(), TensorRef::from_array_view(([batch, seq], types))?.into()));
        }
    }
    let output = session.run_with_options(inputs, options)?;
    let value = match &names.output {
        Some(name) => output.into_iter().find(|(key, _)| key == name).map(|(_, value)| value).context("configured embedding output missing")?,
        None => output.into_iter().next().map(|(_, value)| value).context("model produced no output tensor")?,
    };
    value.try_extract_tensor::<f32>()?;
    Ok(InferenceOutput::Native(value))
}

#[cfg(test)]
#[path = "onnx/session_tests.rs"]
mod tests;
