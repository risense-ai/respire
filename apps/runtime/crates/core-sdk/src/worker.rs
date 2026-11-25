//! A host-owned process isolates synchronous native calls. It never owns SQLite.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const CANDIDATE_FRAME_LIMIT: usize = 64 * 1024 * 1024;
const HEARTBEAT_LIMIT: Duration = Duration::from_secs(10);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
// Load + native queue + execution budgets, plus cancellation grace.
/// Maintenance ops (full index rebuild) may need far more than the default;
/// `RSRS_CORE_CALL_LIMIT_SECS` overrides per process without changing the default.
fn call_limit() -> Duration {
    crate::inference_policy::call_limit(
        crate::env::var("RSRS_CORE_CALL_LIMIT_SECS").ok().as_deref(),
        CALL_LIMIT,
    )
}
#[cfg(not(feature = "native-fault-tests"))]
const CALL_LIMIT: Duration = Duration::from_secs(365);
#[cfg(feature = "native-fault-tests")]
const CALL_LIMIT: Duration = Duration::from_secs(10);
static ENABLED: AtomicBool = AtomicBool::new(false);
static STOPPING: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static NEXT: AtomicU64 = AtomicU64::new(0);
static PROCESS: Mutex<Option<Arc<Process>>> = Mutex::new(None);
static ACTIVE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "native-fault-tests")]
static TEST_START_FAILURE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(feature = "native-fault-tests")]
static TEST_CLEANUP_FAILURE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(feature = "native-fault-tests")]
static TEST_RETIRE_QUEUED_CALLBACK: AtomicBool = AtomicBool::new(false);

struct Pending {
    sender: mpsc::Sender<Value>,
    started: Instant,
}

#[derive(serde::Serialize)]
struct RequestFrame<'a> {
    id: u64,
    operation: &'a str,
    payload: &'a Value,
    index_root: Option<std::path::PathBuf>,
    transport: bool,
    selection: Option<crate::profiles::Selection>,
}

#[derive(serde::Serialize)]
struct ResponseFrame<'a> {
    id: u64,
    #[serde(flatten)]
    body: ResponseBody<'a>,
}

#[derive(serde::Serialize)]
#[serde(untagged)]
enum ResponseBody<'a> {
    Result { result: &'a Value },
    Error { error: String },
    Callback { callback: &'a Value },
    CallbackResult { callback_result: &'a Value },
    CallbackError { callback_error: String },
}

#[derive(serde::Serialize)]
struct HeartbeatFrame<'a> {
    heartbeat: &'a Value,
}

struct Process {
    child: Mutex<Child>,
    input: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, Pending>>,
    heartbeat: Mutex<(Instant, Value, Option<Instant>)>,
    retired: AtomicBool,
    reaped: AtomicBool,
    pid: u32,
    generation: u64,
    active_callbacks: AtomicUsize,
    foreground: bool,
}

struct CallbackGuard<'a>(&'a Process);
impl Drop for CallbackGuard<'_> {
    fn drop(&mut self) {
        self.0.active_callbacks.fetch_sub(1, Ordering::AcqRel);
        ACTIVE_CALLBACKS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Call only in the resident host after acquiring its library lock.
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}
pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn status() -> Result<Value> {
    let process = PROCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("Core worker state poisoned"))?
        .clone();
    let Some(process) = process else {
        return Ok(
            json!({"phase":"idle", "host_recovery_required":false, "worker_generation":generation(),
                "host_transport_callbacks_active":ACTIVE_CALLBACKS.load(Ordering::Acquire)}),
        );
    };
    let mut status = process
        .heartbeat
        .lock()
        .map_err(|_| anyhow::anyhow!("Core worker heartbeat poisoned"))?
        .1
        .clone();
    status["worker_generation"] = json!(process.generation);
    status["host_transport_callbacks_active"] = json!(ACTIVE_CALLBACKS.load(Ordering::Acquire));
    status["worker_host_callbacks_active"] =
        json!(process.active_callbacks.load(Ordering::Acquire));
    status["worker_pending_calls"] = json!(process
        .pending
        .lock()
        .map_err(|_| anyhow::anyhow!("Core worker pending poisoned"))?
        .len());
    status["worker_call_deadline_secs"] = json!(call_limit().as_secs());
    status["worker_pid"] = json!(process.pid);
    status["worker_retired"] = json!(process.retired.load(Ordering::Acquire));
    status["worker_reaped"] = json!(process.reaped.load(Ordering::Acquire));
    if process.retired.load(Ordering::Acquire) && !process.reaped.load(Ordering::Acquire) {
        status["phase"] = json!("worker_retiring");
        status["host_recovery_required"] = json!(true);
    }
    if process.reaped.load(Ordering::Acquire) {
        let previous = status.clone();
        status = json!({"phase":"worker_recovery_pending", "host_recovery_required":false,
            "host_transport_callbacks_active":ACTIVE_CALLBACKS.load(Ordering::Acquire),
            "worker_generation":generation(),"retired_worker":previous});
    }
    Ok(status)
}

fn read_frame(input: &mut impl Read) -> Result<Value> {
    read_frame_bounded(input, usize::MAX)
}

fn read_frame_bounded(input: &mut impl Read, maximum: usize) -> Result<Value> {
    let mut length = [0; 8];
    input.read_exact(&mut length)?;
    let length =
        usize::try_from(u64::from_le_bytes(length)).context("Core worker frame length overflow")?;
    anyhow::ensure!(
        (1..=maximum).contains(&length),
        "invalid Core worker frame length"
    );
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .context("Core worker frame allocation failed")?;
    bytes.resize(length, 0);
    input.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).context("invalid Core worker frame")
}

fn write_frame<T: serde::Serialize + ?Sized>(output: &mut impl Write, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    output.write_all(&(bytes.len() as u64).to_le_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}

struct CandidateJson(Vec<u8>);
impl Write for CandidateJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > CANDIDATE_FRAME_LIMIT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("candidate frame exceeds byte budget"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_candidate_frame<T: serde::Serialize + ?Sized>(
    output: &mut impl Write,
    value: &T,
) -> Result<()> {
    let mut bytes = CandidateJson(Vec::new());
    serde_json::to_writer(&mut bytes, value)?;
    output.write_all(&(bytes.0.len() as u64).to_le_bytes())?;
    output.write_all(&bytes.0)?;
    output.flush()?;
    Ok(())
}

impl Process {
    fn begin_callback(&self) -> Result<CallbackGuard<'_>> {
        // Linearize callback dispatch with retirement. Once admitted, a host
        // transport remains observable until its own timeout/result; no replay.
        let _pending = self
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Core worker pending poisoned"))?;
        anyhow::ensure!(
            self.foreground
                && !self.retired.load(Ordering::Acquire)
                && self.generation == generation(),
            "retired Core callback discarded before host transport; not replayed"
        );
        self.active_callbacks.fetch_add(1, Ordering::AcqRel);
        ACTIVE_CALLBACKS.fetch_add(1, Ordering::AcqRel);
        Ok(CallbackGuard(self))
    }
    fn send<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<()> {
        anyhow::ensure!(
            !self.retired.load(Ordering::Acquire),
            "Core worker retired; command was not replayed"
        );
        let mut input = self
            .input
            .lock()
            .map_err(|_| anyhow::anyhow!("Core worker input poisoned"))?;
        let input = input.as_mut().context("Core worker pipe closed")?;
        if self.foreground {
            write_frame(input, value)
        } else {
            write_candidate_frame(input, value)
        }
    }

    fn retire(&self, reason: &str) -> Result<()> {
        // The Child handle, rather than an endpoint PID, identifies the owned process.
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("Core worker pending poisoned"))?;
            if !self.retired.swap(true, Ordering::AcqRel) {
                if self.foreground {
                    GENERATION.fetch_add(1, Ordering::AcqRel);
                }
                for (id, pending) in pending.drain() {
                    let _ = pending.sender.send(json!({"id":id,"error":format!("Core worker retired: {reason}; command was not replayed")}));
                }
            }
        }
        let mut child = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("Core worker child poisoned"))?;
        if self.reaped.load(Ordering::Acquire) {
            return Ok(());
        }
        #[cfg(feature = "native-fault-tests")]
        if TEST_CLEANUP_FAILURE.load(Ordering::Acquire) == 1 {
            bail!("test-only injected owned-child kill failure");
        }
        if child.try_wait()?.is_none() {
            child
                .kill()
                .context("terminate unresponsive owned Core worker")?;
        }
        #[cfg(feature = "native-fault-tests")]
        if TEST_CLEANUP_FAILURE.load(Ordering::Acquire) == 2 {
            bail!("test-only injected owned-child reap confirmation failure");
        }
        let started = Instant::now();
        while child.try_wait()?.is_none() {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(5),
                "owned Core worker did not exit; replacement prohibited"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.reaped.store(true, Ordering::Release);
        self.input
            .lock()
            .map_err(|_| anyhow::anyhow!("Core worker input poisoned"))?
            .take();
        Ok(())
    }
}

fn spawn_control_thread(
    name: &'static str,
    run: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(feature = "native-fault-tests")]
    {
        let kind = if name == "respire-core-replies" { 1 } else { 2 };
        if TEST_START_FAILURE
            .compare_exchange(kind, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Err(std::io::Error::other(
                "test-only injected control thread start failure",
            ));
        }
    }
    std::thread::Builder::new().name(name.into()).spawn(run)
}

fn process() -> Result<Arc<Process>> {
    let mut slot = PROCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("Core worker state poisoned"))?;
    anyhow::ensure!(
        !STOPPING.load(Ordering::Acquire),
        "Core worker host is stopping; command was not submitted"
    );
    if let Some(process) = slot
        .as_ref()
        .filter(|process| !process.retired.load(Ordering::Acquire))
    {
        return Ok(Arc::clone(process));
    }
    if let Some(process) = slot.as_ref() {
        // A failed cleanup must not permanently strand the retained child handle.
        process.retire("retry previous worker cleanup").context(
            "previous Core worker has not exited; replacement prohibited; command was not submitted",
        )?;
    }
    start_process(&mut slot, None, None)
}

fn start_process(
    slot: &mut Option<Arc<Process>>,
    candidate_root: Option<&std::path::Path>,
    fixture_executable: Option<&std::path::Path>,
) -> Result<Arc<Process>> {
    let foreground = candidate_root.is_none();
    let mut command = spawn_config::command(candidate_root, fixture_executable)?;
    let mut child = command.spawn().context("start owned Core worker")?;
    let input = child.stdin.take().context("missing Core worker stdin")?;
    let mut output = child.stdout.take().context("missing Core worker stdout")?;
    let pid = child.id();
    let process = Arc::new(Process {
        child: Mutex::new(child),
        input: Mutex::new(Some(input)),
        pending: Mutex::new(HashMap::new()),
        heartbeat: Mutex::new((
            Instant::now(),
            json!({"phase":"starting","host_recovery_required":false}),
            None,
        )),
        retired: AtomicBool::new(false),
        reaped: AtomicBool::new(false),
        pid,
        generation: if foreground { generation() } else { 0 },
        active_callbacks: AtomicUsize::new(0),
        foreground,
    });
    // Publish ownership before any subsequent fallible thread initialization.
    *slot = Some(Arc::clone(&process));
    let reader = Arc::clone(&process);
    spawn_control_thread("respire-core-replies", move || loop {
        let reply = if reader.foreground {
            read_frame(&mut output)
        } else {
            read_frame_bounded(&mut output, CANDIDATE_FRAME_LIMIT)
        };
        let mut frame = match reply {
            Ok(frame) => frame,
            Err(error) => {
                if let Err(retire) = reader.retire(&format!("IPC closed: {error:#}")) {
                    eprintln!("Core recovery failed: {retire:#}");
                }
                break;
            }
        };
        if reader.retired.load(Ordering::Acquire) {
            break;
        }
        if let Some(status) = frame.get("heartbeat") {
            if let Ok(mut heartbeat) = reader.heartbeat.lock() {
                let stalled = status["host_recovery_required"].as_bool() == Some(true);
                heartbeat.0 = Instant::now();
                heartbeat.1 = frame["heartbeat"].take();
                heartbeat.2 = if stalled {
                    heartbeat.2.or(Some(Instant::now()))
                } else {
                    None
                };
            }
        } else if let Some(id) = frame["id"].as_u64() {
            let is_callback = frame.get("callback").is_some();
            if let Ok(mut pending) = reader.pending.lock() {
                if let Some(pending) = pending.get(&id) {
                    let _ = pending.sender.send(frame);
                }
                if !is_callback {
                    pending.remove(&id);
                }
            }
        }
    })
    .map_err(|error| {
        let cleanup = process.retire("reply reader could not start");
        anyhow::anyhow!(
            "reply reader start failed: {error}; cleanup: {cleanup:?}; worker ownership retained"
        )
    })?;
    let supervisor = Arc::clone(&process);
    spawn_control_thread("respire-core-supervisor", move || {
        while !supervisor.retired.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
            let expired = match supervisor.pending.lock() {
                Ok(pending) => pending
                    .values()
                    .any(|pending| pending.started.elapsed() >= call_limit()),
                Err(_) => true,
            };
            let reason = if expired {
                Some("Core call deadline exceeded")
            } else {
                match supervisor.heartbeat.lock() {
                    Ok(heartbeat) if heartbeat.0.elapsed() > HEARTBEAT_LIMIT => {
                        Some("worker heartbeat timed out")
                    }
                    Ok(heartbeat)
                        if heartbeat
                            .2
                            .is_some_and(|since| since.elapsed() >= CANCEL_GRACE) =>
                    {
                        Some("native call did not return after cancellation")
                    }
                    Ok(_) => None,
                    Err(_) => Some("worker heartbeat poisoned"),
                }
            };
            if let Some(reason) = reason {
                if let Err(error) = supervisor.retire(reason) {
                    eprintln!("Core recovery failed: {error:#}");
                }
            }
        }
    })
    .map_err(|error| {
        let cleanup = process.retire("supervisor could not start");
        anyhow::anyhow!(
            "supervisor start failed: {error}; cleanup: {cleanup:?}; worker ownership retained"
        )
    })?;
    Ok(process)
}

fn unconfirmed_model_load(operation: &str, error: anyhow::Error) -> anyhow::Error {
    if operation == "host_load_selection" {
        error.context("load outcome unconfirmed; settings may already be saved; previous model recovery unconfirmed; request was not replayed; inspect runtime state and follow host recovery guidance")
    } else {
        error
    }
}

pub(crate) fn execute(
    operation: &str,
    payload: &Value,
    mut transport: Option<&mut dyn FnMut(&Value) -> Result<Value>>,
) -> Result<Value> {
    let process = process()?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::channel();
    {
        let mut pending = process
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Core worker pending poisoned"))?;
        anyhow::ensure!(
            !process.retired.load(Ordering::Acquire),
            "Core worker retired before submission"
        );
        pending.insert(
            id,
            Pending {
                sender,
                started: Instant::now(),
            },
        );
    }
    let root = crate::business::index_root().ok();
    if let Err(error) = process.send(&RequestFrame {
        id,
        operation,
        payload,
        index_root: root,
        transport: transport.is_some(),
        selection: crate::profiles::current()?,
    }) {
        let _ = process.retire(&format!("IPC write failed: {error:#}"));
        return Err(unconfirmed_model_load(operation, error));
    }
    loop {
        let mut frame = receiver
            .recv()
            .context("Core worker reply unavailable; command was not replayed")
            .map_err(|error| unconfirmed_model_load(operation, error))?;
        if let Some(request) = frame.get("callback") {
            #[cfg(feature = "native-fault-tests")]
            if TEST_RETIRE_QUEUED_CALLBACK.swap(false, Ordering::AcqRel) {
                process.retire("test-only retire callback queued before dispatch")?;
            }
            let _callback = process.begin_callback()?;
            let result = match transport.as_mut() {
                Some(transport) => transport(request),
                None => Err(anyhow::anyhow!("unexpected Core worker transport callback")),
            };
            anyhow::ensure!(!process.retired.load(Ordering::Acquire) && process.generation == generation(),
                "host transport already started and was not cancelled; its result was discarded after Core retirement; business outcome unavailable; not replayed");
            let body = match &result {
                Ok(value) => ResponseBody::CallbackResult {
                    callback_result: value,
                },
                Err(error) => ResponseBody::CallbackError {
                    callback_error: format!("{error:#}"),
                },
            };
            let frame = ResponseFrame { id, body };
            if let Err(error) = process.send(&frame) {
                let _ = process.retire("host callback IPC failed");
                return Err(error);
            }
        } else if let Some(error) = frame["error"].as_str() {
            let error = anyhow::anyhow!("{error}");
            return Err(if process.retired.load(Ordering::Acquire) {
                unconfirmed_model_load(operation, error)
            } else {
                error
            });
        } else {
            if process.retired.load(Ordering::Acquire) || process.generation != generation() {
                return Err(unconfirmed_model_load(operation, anyhow::anyhow!(
                    "Core worker generation changed; late response discarded; command was not replayed")));
            }
            return frame
                .get_mut("result")
                .map(Value::take)
                .context("Core worker result missing")
                .map_err(|error| unconfirmed_model_load(operation, error));
        }
    }
}

/// Stop before releasing the host's LibraryLock. Replacement never overlaps a retired child.
pub fn shutdown() -> Result<()> {
    STOPPING.store(true, Ordering::Release);
    let candidate_result = candidate::shutdown();
    let process = PROCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("Core worker state poisoned"))?
        .clone();
    if let Some(process) = process {
        process.retire("host shutdown")?;
    }
    candidate_result
}

mod candidate;
mod spawn_config;
pub use candidate::CandidateWorker;

mod server;
pub use server::run;

#[cfg(feature = "native-fault-tests")]
mod fixtures;
#[cfg(feature = "native-fault-tests")]
pub use fixtures::fixture_control;

#[cfg(test)]
mod tests;
