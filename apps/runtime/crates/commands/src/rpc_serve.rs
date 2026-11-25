//! Server face of the runtime RPC surface.
//!
//! Owns the request queue (Job/admission), the dispatch loop and its worker
//! threads, the health body and the loopback HTTP wiring used by both the
//! CLI-hosted runtime and the standalone `rsrs-runtime` binary. The CLI binary
//! re-imports these items so `crate::rpc::...` call sites keep resolving.
//!
//! Boundaries:
//! - depends on `respire_app` (app-core) and `respire_protocol`, never on the
//!   `respire` CLI package;
//! - host-only services (argv execution, MCP bridge, managed registration)
//!   arrive through `host_hooks`.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use respire_protocol::{
    RuntimeIdentity, RuntimeRequestScope, RuntimeStopOutcome, RuntimeStopReceipt,
    RuntimeUnfinishedOperation,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// ---- Runtime identity (split-era health extension) ----

static RUNTIME_IDENTITY: OnceLock<RuntimeIdentity> = OnceLock::new();

/// Publish the serving binary's identity; the first call wins. The health body
/// and every RPC reply report it for diagnostics.
pub fn set_runtime_identity(identity: RuntimeIdentity) -> bool {
    RUNTIME_IDENTITY.set(identity).is_ok()
}

pub(crate) fn runtime_identity() -> RuntimeIdentity {
    RUNTIME_IDENTITY.get().cloned().unwrap_or(RuntimeIdentity {
        runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

#[derive(Clone)]
struct ProcessIdentity {
    installation_id: String,
    instance_id: String,
    process_start_id: String,
    image_path: String,
    image_sha256: String,
}

static PROCESS_IDENTITY: OnceLock<ProcessIdentity> = OnceLock::new();

/// Install the consumed launch nonce before any process identity is published.
pub fn set_launch_instance_id(instance_id: &str) -> Result<()> {
    let nonce = uuid::Uuid::parse_str(instance_id)
        .map_err(|_| anyhow::anyhow!("invalid runtime launch instance ID"))?;
    anyhow::ensure!(nonce.get_version_num() == 4 && nonce.to_string() == instance_id,
        "runtime launch instance ID must be a canonical random UUID");
    let executable = std::env::current_exe()?.canonicalize()?;
    let installation = respire_app::service::main_data_dir().canonicalize()?;
    let identity = ProcessIdentity {
        installation_id: sha256_hex(installation.display().to_string().as_bytes()),
        instance_id: instance_id.to_owned(),
        process_start_id: os_process_start_identity()
            .context("runtime process start identity is unavailable")?,
        image_path: executable.display().to_string(),
        image_sha256: sha256_file(&executable)
            .context("runtime executable digest is unavailable")?,
    };
    PROCESS_IDENTITY.set(identity)
        .map_err(|_| anyhow::anyhow!("runtime process identity was already published"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &std::path::Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn os_process_start_identity() -> Option<String> {
    let pid = std::process::id();
    #[cfg(windows)]
    {
        return respire_spawn::process_birth(pid)
            .ok()
            .flatten()
            .map(|birth| format!("windows-filetime-{birth}"));
    }
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields = raw.rsplit_once(')')?.1;
        let state = fields.split_whitespace().next()?;
        if state == "Z" {
            return None;
        }
        let birth = fields.split_whitespace().nth(19)?;
        return Some(format!("linux-starttime-{birth}"));
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("lstart=")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let birth = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return (!birth.is_empty()).then(|| format!("darwin-lstart-{birth}"));
    }
}

fn process_identity() -> ProcessIdentity {
    PROCESS_IDENTITY
        .get_or_init(|| {
            let exe = std::env::current_exe().unwrap_or_default();
            let installation = respire_app::service::main_data_dir();
            ProcessIdentity {
                installation_id: sha256_hex(installation.display().to_string().as_bytes()),
                instance_id: uuid::Uuid::new_v4().to_string(),
                process_start_id: os_process_start_identity().unwrap_or_default(),
                image_path: exe.display().to_string(),
                image_sha256: sha256_file(&exe).unwrap_or_default(),
            }
        })
        .clone()
}

pub fn validate_process_identity() -> Result<()> {
    let identity = process_identity();
    anyhow::ensure!(
        !identity.process_start_id.is_empty()
            && !account_identity().is_empty()
            && std::path::Path::new(&identity.image_path).is_absolute()
            && identity.image_sha256.len() == 64
            && identity
                .image_sha256
                .bytes()
                .all(|ch| ch.is_ascii_hexdigit()),
        "runtime cannot establish its OS process and executable identity"
    );
    Ok(())
}

fn account_identity() -> String {
    let data_dir = respire_app::service::data_dir();
    let account = respire_app::service::session_user_of_dir(&data_dir);
    let source = if account.is_empty() {
        match data_dir.canonicalize() {
            Ok(profile) => profile.display().to_string(),
            Err(_) => return String::new(),
        }
    } else {
        account
    };
    sha256_hex(source.as_bytes())
}

/// Version string this serving process reports in RPC replies and health.
fn bin_version() -> &'static str {
    RUNTIME_IDENTITY
        .get()
        .map(|identity| identity.runtime_version.as_str())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Publish the identity of a host binary that serves in-process as the
/// split-transition shim (the CLI exe behind `--runtime-internal`): it reports
/// its own version for diagnostics. First publication still wins.
pub fn publish_shim_identity(version: &'static str) -> bool {
    set_runtime_identity(RuntimeIdentity {
        runtime_version: version.to_owned(),
    })
}

// Worker-face state (worker flag, sync context, generation, index work,
// write gate, recall statistics) lives in the shared rpc_worker module.
use crate::i18n;
use crate::output::{Item as OutputItem, ResultEnvelope, Status as OutputStatus};
use crate::rpc_worker::{
    check_sync_context, command_name, current_url, index_has_work, index_loop, index_status,
    is_context_query, is_status, kick_index, mark_worker, needs_inference, recall_stats_status,
    runtime_dir, shared_exclusive, sync_local, write_endpoint, RecallStats,
    GENERATION, INDEX_CV, INDEX_ON, MAX_PENDING_STATS, PROTOCOL_V, STATS_ENQUEUED,
    STATS_FAILED, STATS_PENDING, STATS_PERSISTED, STATS_REJECTED, STOPPING, SYNC_CONTEXT, WEB_URL,
};

pub static RUNNING: AtomicUsize = AtomicUsize::new(0);
static RUNNING_STATUS: AtomicUsize = AtomicUsize::new(0);
pub static LIGHT_RUNNING: AtomicUsize = AtomicUsize::new(0);
pub static WORKERS: AtomicUsize = AtomicUsize::new(1);
static STORE_READY: AtomicBool = AtomicBool::new(cfg!(test));

pub(crate) fn store_ready() -> bool {
    STORE_READY.load(Ordering::Acquire)
}

/// The HTTP transport authenticates this separate endpoint before parsing secrets.
/// Unlock input never enters operation journals, progress, argv or result caches.
pub(crate) fn unlock_runtime(input: respire_protocol::credentials::RuntimeUnlockRequest)
    -> Result<respire_protocol::credentials::RuntimeUnlockReceipt> {
    use respire_protocol::credentials::{RuntimeUnlockReceipt, RuntimeUnlockRequest};
    let schema_version = input.schema_version();
    let provider_status = {
        let _scope = SCOPE_EXECUTION.write().map_err(|_| anyhow::anyhow!("identity fence poisoned"))?;
        let (instance_id, generation) = match &input {
            RuntimeUnlockRequest::V1(input) => (&input.instance_id, input.generation),
            RuntimeUnlockRequest::V2(input) => (&input.instance_id, input.generation),
        };
        anyhow::ensure!(!STOPPING.load(Ordering::Acquire)
            && instance_id == &process_identity().instance_id
            && generation == crate::rpc_worker::wire_generation(), "unlock runtime scope mismatch");
        anyhow::ensure!(!respire_app::service::off_mode(), "runtime service is disabled");
        match input {
            RuntimeUnlockRequest::V1(input) => respire_app::auth::supply_runtime_session(input)?,
            RuntimeUnlockRequest::V2(input) => respire_app::auth::supply_runtime_session_v2(input)?,
        }
        respire_app::credential_io::provider_status()?
    };
    let until = Instant::now() + Duration::from_secs(10);
    while !store_ready() {
        anyhow::ensure!(!STOPPING.load(Ordering::Acquire) && Instant::now() < until,
            "runtime unlock did not reach library readiness");
        respire_app::auth::load_local_session()?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(RuntimeUnlockReceipt { schema_version, store_ready: true, provider_status })
}
static SNAPSHOT_BOOT_ID: Mutex<Option<String>> = Mutex::new(None);
pub static JOB_TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
static DISPATCH_THREAD: OnceLock<std::thread::Thread> = OnceLock::new();
pub const QUEUE_EXPIRED: &str = "request queue deadline exceeded; command was not started";
static WRITE_TX: Mutex<Option<mpsc::SyncSender<WriteJob>>> = Mutex::new(None);
static WRITE_RUNNING: AtomicBool = AtomicBool::new(false);
static CLASSIFY_RUNNING: AtomicBool = AtomicBool::new(false);
static SYNC_KICK: Mutex<SyncSchedule> = Mutex::new(SyncSchedule {
    due: None,
    manual: VecDeque::new(),
    backoff: 0,
    blocked_generation: None,
    last_success: None,
});
struct SyncSchedule {
    due: Option<Instant>,
    manual: VecDeque<ManualSync>,
    backoff: u32,
    blocked_generation: Option<usize>,
    last_success: Option<u64>,
}

enum ManualSync {
    Legacy(Job, usize, i64),
    V1(Box<crate::runtime_http::PendingSync>),
}

impl ManualSync {
    fn reject(self, error: &str) {
        if let Self::Legacy(job, _, _) = self {
            let _ = job.reply.send(Err(error.into()));
        }
    }
}

impl SyncSchedule {
    fn record_success(&mut self, synchronized: bool) {
        self.backoff = 0;
        self.blocked_generation = None;
        if synchronized {
            self.last_success = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|value| value.as_secs());
        }
    }
}
static SYNC_CV: Condvar = Condvar::new();
pub static SYNC_RUNNING: AtomicBool = AtomicBool::new(false);
static FLIGHT_ON: AtomicBool = AtomicBool::new(false);
struct CachedResponse {
    operation_id: String,
    response: RpcResponse,
}

struct IdempotencyRecord {
    operation_id: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct OperationIdentity {
    scope: RuntimeRequestScope,
    operation_id: String,
    storage_id: String,
    storage_key: String,
    fingerprint: String,
}

static CACHE: Mutex<Vec<CachedResponse>> = Mutex::new(Vec::new());
static IDEMPOTENCY: std::sync::LazyLock<Mutex<HashMap<String, IdempotencyRecord>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static OPERATION_KEYS: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static INFLIGHT: std::sync::LazyLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

#[derive(Debug)]
enum Admission {
    New(AdmissionGuard),
    Replay(RpcResponse),
    InProgress,
    Unknown,
}

static CONTROL_STATE: Mutex<&'static str> = Mutex::new("running");
static STARTUP_COMPLETE: AtomicBool = AtomicBool::new(false);
static STOP_OPERATION: OnceLock<String> = OnceLock::new();
static STOP_UNFINISHED: Mutex<Vec<Admitted>> = Mutex::new(Vec::new());
const FORCE_DRAIN_GRACE: Duration = Duration::from_secs(4);
static STOP_FORCE_REQUESTED: Mutex<Option<Instant>> = Mutex::new(None);
static STOP_FORCE_TRIGGERED: AtomicBool = AtomicBool::new(false);
// Order: scope lock, write gate (except short control), ADMISSIONS, then ledger.
// Stop takes scope + ADMISSIONS; generation advances take ADMISSIONS without upgrading scope.
static SCOPE_EXECUTION: std::sync::RwLock<()> = std::sync::RwLock::new(());
static ADMISSIONS: std::sync::LazyLock<Mutex<Vec<Admitted>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Clone)]
struct Admitted {
    operation_id: String,
    kind: &'static str,
}

#[derive(Debug)]
pub(crate) struct AdmissionGuard(String);

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if let Ok(mut admitted) = ADMISSIONS.lock() {
            if let Some(index) = admitted.iter().position(|item| item.operation_id == self.0) {
                admitted.remove(index);
            }
        }
    }
}

pub(crate) fn begin_admission(operation_id: String, kind: &'static str) -> Option<AdmissionGuard> {
    let mut admitted = ADMISSIONS.lock().ok()?;
    if business_paused() {
        return None;
    }
    admitted.push(Admitted {
        operation_id: operation_id.clone(),
        kind,
    });
    Some(AdmissionGuard(operation_id))
}

fn business_paused() -> bool {
    STOPPING.load(Ordering::Acquire) || crate::host_hooks::background_paused()
}

fn core_config_method(method: &str) -> bool {
    use respire_protocol::core_config::{APPLY_METHOD, READ_METHOD, VALIDATE_METHOD};
    matches!(method, APPLY_METHOD | READ_METHOD | VALIDATE_METHOD)
}

fn core_config_response(request: &RpcRequest) -> RpcResponse {
    use respire_protocol::core_config::*;
    let result = (|| -> Result<CoreConfigReceipt> {
        anyhow::ensure!(request.args.len() == 1, "core configuration requires one JSON argument");
        let params: CoreConfigParams = serde_json::from_str(&request.args[0])?;
        anyhow::ensure!(params.schema_version == 1, "core configuration schema version mismatch");
        crate::host_hooks::verify_core_config_maintenance(params.maintenance.as_ref())?;
        match request.method.as_str() {
            READ_METHOD => {
                anyhow::ensure!(params.candidate.is_none() && params.expected_revision.is_none(),
                    "read does not accept a candidate or expected revision");
                respire_app::core_config::read()
            }
            VALIDATE_METHOD => {
                anyhow::ensure!(params.expected_revision.is_none(), "validate does not accept expected revision");
                respire_app::core_config::validate(params.candidate.context("candidate required")?)
            }
            APPLY_METHOD => respire_app::core_config::apply(
                params.candidate.context("candidate required")?,
                params.expected_revision.context("expected revision required")?),
            _ => anyhow::bail!("unknown core configuration method"),
        }
    })();
    match result {
        Ok(receipt) => match serde_json::to_value(receipt) {
            Ok(summary) => {
                let mut response = status_response(request);
                response.operation_id = request.operation_id.clone();
                response.envelope = Some(ResultEnvelope::new(&request.method, OutputStatus::Ok, summary, Vec::new()));
                response
            }
            Err(error) => error_response(request, "core_config_outcome_unknown", &error.to_string()),
        },
        // apply performs every fallible check before its single in-memory state replacement.
        Err(error) => error_response(request, "core_config_rejected", &format!("{error:#}")),
    }
}

pub(crate) fn maintenance_control_method(method: &str) -> bool {
    core_config_method(method) || matches!(method, "runtime.status" | "runtime.stop" | "runtime.result" | "model.control" | "cli.progress")
}

pub(crate) fn admitted_list() -> Value {
    let items = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
    Value::Array(
        items
            .iter()
            .map(|item| {
                json!({
                    "operation_id": item.operation_id,
                    "kind": item.kind,
                })
            })
            .collect(),
    )
}

fn unfinished_admitted() -> Vec<Admitted> {
    ADMISSIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn busy_background() -> Vec<&'static str> {
    let mut busy = Vec::new();
    if WRITE_RUNNING.load(Ordering::Acquire) {
        busy.push("write");
    }
    if CLASSIFY_RUNNING.load(Ordering::Acquire) {
        busy.push("classify");
    }
    if sync_running() {
        busy.push("sync");
    }
    if index_has_work() {
        busy.push("index");
    }
    if PROFILE_SWITCH_RUNNING.load(Ordering::Acquire) {
        busy.push("model_switch");
    }
    busy
}

enum WriteJob {
    Command(Job),
    Prepare(PrepareJob),
    RecallStats(RecallStats, usize, AdmissionGuard),
}

struct PrepareJob {
    job: Job,
    operation_id: String,
    operation: crate::model_progress::Operation,
}

pub fn enqueue_recall_stats_local(stats: RecallStats, generation: usize) {
    if respire_app::service::workspace_mode() != "normal" {
        return;
    }
    if STATS_PENDING
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
            (pending < MAX_PENDING_STATS).then_some(pending + 1)
        })
        .is_err()
    {
        STATS_REJECTED.fetch_add(1, Ordering::AcqRel);
        eprintln!("recall statistics rejected: pending statistics limit reached");
        return;
    }
    let queued = (|| -> Result<()> {
        check_sync_context(generation)?;
        let tx = WRITE_TX
            .lock()
            .map_err(|_| anyhow::anyhow!("write queue lock poisoned"))?
            .clone()
            .ok_or_else(|| anyhow::anyhow!("write worker is not running"))?;
        let gate = shared_exclusive();
        gate.reserve();
        let Some(admission) = begin_admission(uuid::Uuid::new_v4().to_string(), "background_write")
        else {
            gate.cancel_reservation();
            return Err(anyhow::anyhow!("runtime is draining"));
        };
        match tx.try_send(WriteJob::RecallStats(stats, generation, admission)) {
            Ok(()) => Ok(()),
            Err(error) => {
                gate.cancel_reservation();
                match error {
                    mpsc::TrySendError::Full(_) => bail!("write request queue is full"),
                    mpsc::TrySendError::Disconnected(_) => bail!("write worker is gone"),
                }
            }
        }
    })();
    match queued {
        Ok(()) => {
            STATS_ENQUEUED.fetch_add(1, Ordering::AcqRel);
        }
        Err(error) => {
            STATS_PENDING.fetch_sub(1, Ordering::AcqRel);
            STATS_REJECTED.fetch_add(1, Ordering::AcqRel);
            eprintln!("recall statistics rejected: {error:#}");
        }
    }
}

pub fn sync_scheduler_status() -> Value {
    let state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
    let blocked = state.blocked_generation == Some(GENERATION.load(Ordering::Acquire));
    let phase = if sync_running() {
        "running"
    } else if blocked {
        "blocked"
    } else if state.backoff > 0 {
        "backoff"
    } else if state.due.is_some() {
        "scheduled"
    } else {
        "idle"
    };
    json!({"state":phase, "next_run_ms":state.due.map(|due| due.saturating_duration_since(Instant::now()).as_millis() as u64),
        "manual_waiters":state.manual.len(), "last_success_unix":state.last_success})
}

pub fn context_changed() {
    if let Err(error) = respire_app::resident::clear() {
        eprintln!("resident catalog reset failed: {error:#}");
    }
    let mut state = SYNC_KICK.lock().unwrap_or_else(|e| e.into_inner());
    state.blocked_generation = None;
    state.backoff = 0;
    state.due = Some(Instant::now() + Duration::from_secs(3));
    drop(state);
    SYNC_CV.notify_one();
    kick_index();
}

static PROFILE_SWITCH_RUNNING: AtomicBool = AtomicBool::new(false);

static PROFILE_ADMISSION: Mutex<()> = Mutex::new(());
static PROFILE_SWITCH_STATE: Mutex<Value> = Mutex::new(Value::Null);


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/profile_state.rs"));
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/model_switch.rs"));
// The unfinished typed_sync.rs draft is intentionally not wired: it reopens SQLite.

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/job.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/dispatch_validated.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/deferred_reply.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/endpoint_identity.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/dispatch_loop_inner.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/parts/remember.rs"));
