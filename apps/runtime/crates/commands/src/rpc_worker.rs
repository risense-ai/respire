//! Runtime worker infrastructure shared by the CLI binary and the resident
//! runtime engine.
//!
//! The command-execution engine and its worker plumbing migrate here from the
//! CLI binary in steps of the runtime split. This module currently hosts the
//! argv classification rules shared by client timeout budgets and server
//! routing, plus the loopback endpoint record that both sides read and write.
//!
//! Boundaries:
//! - depends on `respire_app` (app-core) and `cli_def`, never on the `respire`
//!   CLI package;
//! - the CLI binary re-imports these items so `crate::rpc::...` paths and bare
//!   call sites keep resolving.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use clap::Parser;
use serde_json::{json, Value};

use crate::cli_def::{Cli, Command, SessionCommand};

pub const PROTOCOL_V: u32 = 1;

pub static WEB_URL: Mutex<String> = Mutex::new(String::new());

pub fn is_status(args: &[String]) -> bool {
    command_name(args) == Some("status")
}

/// Viewing account/configuration state must not invalidate ongoing sync or indexing.
pub fn is_context_query(args: &[String]) -> bool {
    if !matches!(
        command_name(args),
        Some("account" | "space" | "config" | "agent-config" | "session")
    ) {
        return false;
    }
    let Ok(cli) =
        Cli::try_parse_from(std::iter::once("rsrs".to_owned()).chain(args.iter().cloned()))
    else {
        return false;
    };
    match cli.command {
        Some(Command::Account { action, .. }) => action == "list",
        Some(Command::Space { action, .. }) => matches!(action.as_str(), "list" | "members"),
        Some(Command::Config {
            data_dir: None,
            addr: None,
            autosync: None,
            cure_auto: None,
            rpc_parallelism: None,
        })
        | Some(Command::AgentConfig { set: None })
        | Some(Command::Session {
            command: SessionCommand::List,
        }) => true,
        _ => false,
    }
}

pub const GLOBAL_VALUE_FLAGS: [&str; 2] = ["--progress-id", "--model-task-id"];

// Keep client timeout classification and server routing on the same argv rules.
// A tracked request ID is a value even when it spells a command such as sync.
pub fn command_words(args: &[String]) -> impl Iterator<Item = &str> + '_ {
    let mut values = args.iter();
    std::iter::from_fn(move || {
        while let Some(arg) = values.next() {
            if GLOBAL_VALUE_FLAGS.contains(&arg.as_str()) {
                let _ = values.next();
            } else if !arg.starts_with('-') {
                return Some(arg.as_str());
            }
        }
        None
    })
}

pub fn command_name(args: &[String]) -> Option<&str> {
    command_words(args).next()
}

pub fn diary_lexical_mode(args: &[String]) -> bool {
    // Values after -- are literal queries, never runtime routing flags.
    let options = &args[..args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len())];
    let modes: Vec<&str> = options
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            if arg == "--mode" {
                Some(options.get(index + 1).map(String::as_str).unwrap_or(""))
            } else {
                arg.strip_prefix("--mode=")
            }
        })
        .collect();
    modes == ["lexical"]
}

pub fn needs_inference(args: &[String]) -> bool {
    matches!(
        command_name(args),
        Some("remember" | "recall" | "related" | "reembed" | "classify" | "update")
    ) || (command_name(args) == Some("diary-search") && !diary_lexical_mode(args))
        || (command_name(args) == Some("model")
            && args
                .iter()
                .any(|arg| matches!(arg.as_str(), "probe" | "load")))
}

pub fn current_url() -> String {
    WEB_URL
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

pub fn runtime_dir_path() -> PathBuf {
    runtime_dir()
}

pub fn runtime_dir() -> PathBuf {
    respire_app::service::main_data_dir().join("runtime")
}

pub fn endpoint_path() -> PathBuf {
    runtime_dir().join("endpoint.json")
}

// Both publication and receipt lookup reject linked or unbounded local records.
fn endpoint_record_bytes(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(!metadata.file_type().is_symlink(), "endpoint path contains a symlink");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "endpoint path contains a reparse point");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file() && metadata.len() <= 65536, "invalid endpoint/receipt file");
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 65536, "endpoint/receipt file is too large");
    Ok(Some(bytes))
}

#[derive(Clone)]
pub(crate) struct EndpointPublication {
    path: PathBuf,
    bytes: Vec<u8>,
    identity: Value,
    receipt: PathBuf,
    receipt_bytes: Option<Vec<u8>>,
}

impl EndpointPublication {
    pub(crate) fn revoke_failed_start(
        &self,
        owner: &respire_app::lock::LibraryLock,
    ) -> Result<()> {
        let url = self.identity["url"].as_str().context("published endpoint URL missing")?;
        let (identity, receipt, receipt_bytes) = endpoint_body(url)?;
        self.revoke_matching(owner, &endpoint_path(), &identity, &receipt, &receipt_bytes)
    }

    fn revoke_matching(
        &self,
        owner: &respire_app::lock::LibraryLock,
        path: &std::path::Path,
        identity: &Value,
        receipt: &std::path::Path,
        receipt_bytes: &Option<Vec<u8>>,
    ) -> Result<()> {
        let directory = self.path.parent().context("published endpoint parent missing")?;
        anyhow::ensure!(owner.protects(directory)?, "startup endpoint ownership lock changed");
        anyhow::ensure!(
            path == self.path && receipt == self.receipt && identity == &self.identity
                && receipt_bytes == &self.receipt_bytes,
            "startup endpoint path/identity/launch receipt changed; record retained"
        );
        anyhow::ensure!(
            endpoint_record_bytes(&self.receipt)? == self.receipt_bytes
                && endpoint_record_bytes(&self.path)?.as_deref() == Some(self.bytes.as_slice()),
            "startup endpoint or launch receipt bytes changed; record retained"
        );
        // Only the live publisher can revoke this exact publication, never a dead owner.
        std::fs::remove_file(&self.path)?;
        respire_spawn::durable::sync_directory(directory)?;
        Ok(())
    }
}

/// Revoke only the live publisher's current generation after admission is shut
/// down. Unlike failed startup, normal serving may have republished its scope.
/// Never derive authority from the bytes found in the endpoint file.
pub(crate) fn revoke_current_endpoint(
    owner: &respire_app::lock::LibraryLock,
    url: &str,
) -> Result<()> {
    let path = endpoint_path();
    let (identity, receipt, receipt_bytes) = endpoint_body(url)?;
    let expected = current_publication(path.clone(), identity, receipt, receipt_bytes)?;
    // Re-read actual identity and the claimed receipt before the final byte CAS.
    let (identity, receipt, receipt_bytes) = endpoint_body(url)?;
    expected.revoke_matching(owner, &path, &identity, &receipt, &receipt_bytes)
}

fn current_publication(path: PathBuf, identity: Value, receipt: PathBuf,
    receipt_bytes: Option<Vec<u8>>) -> Result<EndpointPublication> {
    let bytes = serde_json::to_vec_pretty(&identity)?;
    Ok(EndpointPublication { path, bytes, identity, receipt, receipt_bytes })
}

fn endpoint_body(url: &str) -> Result<(Value, PathBuf, Option<Vec<u8>>)> {
    let dir = runtime_dir();
    let mut body = crate::rpc_serve::endpoint_identity(url)?;
    let instance = body["instance_id"].as_str().context("endpoint instance missing")?;
    let nonce = uuid::Uuid::parse_str(instance)?;
    anyhow::ensure!(nonce.get_version_num() == 4 && nonce.to_string() == instance, "invalid endpoint instance");
    let receipt = dir.join("autostart/launch").join(format!("launch-{instance}.claimed"));
    let receipt_bytes = endpoint_record_bytes(&receipt)?;
    if let Some(bytes) = receipt_bytes.as_ref() {
        body["launch_receipt_sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
    }
    Ok((body, receipt, receipt_bytes))
}

pub fn write_endpoint(url: &str) -> Result<()> {
    publish_endpoint(url).map(|_| ())
}

pub(crate) fn publish_endpoint(url: &str) -> Result<EndpointPublication> {
    let dir = runtime_dir();
    let (body, receipt, receipt_bytes) = endpoint_body(url)?;
    let bytes = serde_json::to_vec_pretty(&body)?;
    let target = endpoint_path();
    let previous = endpoint_record_bytes(&target)?;
    std::fs::create_dir_all(&dir)?;
    let temporary = dir.join(format!(".endpoint-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        anyhow::ensure!(endpoint_record_bytes(&target)? == previous, "endpoint changed before publication");
        respire_spawn::durable::replace_file(&temporary, &target)?;
        Ok(())
    })();
    if result.is_err() && temporary.try_exists()? {
        std::fs::remove_file(&temporary)?;
    }
    result?;
    Ok(EndpointPublication { path: target, bytes, identity: body, receipt, receipt_bytes })
}

pub fn endpoint_pid() -> Option<u32> {
    let text = std::fs::read_to_string(endpoint_path()).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    data.get("pid")?
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(test)]
mod endpoint_publication_tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_worker/endpoint_publication_tests.rs")); }

// Runtime worker entrypoints.
// Sync-context, write gate, background index worker and recall-statistics
// plumbing, verbatim from their extraction source apart from rebinding
// app-core paths rebind to respire_app and CLI-host services go through host_hooks.

thread_local! {
    static WORKER: Cell<bool> = const { Cell::new(false) };
    pub static SYNC_CONTEXT: Cell<Option<(usize, i64)>> = const { Cell::new(None) };
}

pub static STOPPING: AtomicBool = AtomicBool::new(false);
pub static STATS_PENDING: AtomicUsize = AtomicUsize::new(0);
pub static STATS_ENQUEUED: AtomicUsize = AtomicUsize::new(0);
pub static STATS_PERSISTED: AtomicUsize = AtomicUsize::new(0);
pub static STATS_REJECTED: AtomicUsize = AtomicUsize::new(0);
pub static STATS_FAILED: AtomicUsize = AtomicUsize::new(0);
pub const MAX_PENDING_STATS: usize = 16;
pub static EXCLUSIVE: OnceLock<Arc<WriteGate>> = OnceLock::new();
pub static GENERATION: AtomicUsize = AtomicUsize::new(0);
pub static INDEX_ON: AtomicBool = AtomicBool::new(false);
pub static INDEX_RUNNING: AtomicBool = AtomicBool::new(false);
pub static INDEX_CV: Condvar = Condvar::new();
pub static INDEX_WORK: Mutex<IndexWork> = Mutex::new(IndexWork {
    requested: false,
    create_batch: None,
    state: "idle",
    error: None,
});

pub struct IndexWork {
    pub(crate) requested: bool,
    create_batch: Option<(Instant, Instant)>,
    pub(crate) state: &'static str,
    pub(crate) error: Option<String>,
}
#[derive(Default)]
pub struct GateState {
    held: bool,
    foreground: usize,
}
#[derive(Default)]
pub struct WriteGate {
    state: Mutex<GateState>,
    changed: Condvar,
}
pub struct WriteGuard<'a>(&'a WriteGate);
impl WriteGate {
    pub(crate) fn reserve(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .foreground += 1;
    }
    pub(crate) fn cancel_reservation(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.foreground -= 1;
        self.changed.notify_all();
    }
    pub fn acquire_reserved(&self) -> WriteGuard<'_> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.held {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.foreground -= 1;
        state.held = true;
        WriteGuard(self)
    }
    // Scoped HTTP never waits behind an existing writer from the dispatch
    // loop. Preserve the priority of every previously reserved legacy writer.
    pub(crate) fn try_owner_foreground(&self) -> Result<Option<WriteGuard<'_>>, ()> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(()),
            Err(std::sync::TryLockError::Poisoned(_)) => return Ok(None),
        };
        if state.held || state.foreground != 0 {
            return Err(());
        }
        state.held = true;
        Ok(Some(WriteGuard(self)))
    }

    pub(crate) fn acquire_foreground_unless(&self, cancelled: impl Fn() -> bool) -> Option<WriteGuard<'_>> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.foreground += 1;
        loop {
            if cancelled() {
                state.foreground -= 1;
                self.changed.notify_all();
                return None;
            }
            if !state.held {
                state.foreground -= 1;
                state.held = true;
                return Some(WriteGuard(self));
            }
            state = self.changed.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub fn acquire(&self, foreground: bool) -> WriteGuard<'_> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if foreground {
            state.foreground += 1;
        }
        while state.held || (!foreground && state.foreground > 0) {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if foreground {
            state.foreground -= 1;
        }
        state.held = true;
        WriteGuard(self)
    }
}
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.held = false;
        self.0.changed.notify_all();
    }
}
pub fn worker_active() -> bool {
    WORKER.with(|flag| flag.get())
}

pub fn set_worker_active(on: bool) {
    WORKER.with(|flag| flag.set(on));
}

pub fn mark_worker() {
    WORKER.with(|flag| flag.set(true));
}

pub struct RecallStats {
    pub request_id: String,
    pub query: String,
    pub project: String,
    pub candidates: Vec<String>,
    pub scores: Vec<f32>,
}

impl RecallStats {
    pub fn persist(&self, store: &respire_app::transport::local::LocalStore) -> Result<()> {
        if respire_app::service::workspace_mode() != "normal" {
            return Ok(());
        }
        store.write_transaction(|| {
            store.log_query_request(
                &self.request_id,
                &self.query,
                &self.project,
                "",
                &self.candidates,
                &self.scores,
            )?;
            store.bump_recall(&self.candidates)?;
            store.bump_recall_pairs(&self.candidates)
        })
    }
}

pub fn recall_stats_status() -> Value {
    json!({
        "pending": STATS_PENDING.load(Ordering::Acquire),
        "enqueued": STATS_ENQUEUED.load(Ordering::Acquire),
        "persisted": STATS_PERSISTED.load(Ordering::Acquire),
        "rejected": STATS_REJECTED.load(Ordering::Acquire),
        "failed": STATS_FAILED.load(Ordering::Acquire),
    })
}

/// Statistics are best effort; never wait for queue capacity on the recall reply path.
pub fn shared_exclusive() -> Arc<WriteGate> {
    EXCLUSIVE
        .get_or_init(|| Arc::new(WriteGate::default()))
        .clone()
}

/// Shared with standalone startup checks so grant and serving generations match.
pub fn generation_on_wire(generation: usize) -> u64 {
    generation.saturating_add(1).max(1) as u64
}

pub(crate) fn wire_generation() -> u64 {
    generation_on_wire(GENERATION.load(Ordering::Acquire))
}

pub fn sync_generation() -> usize {
    SYNC_CONTEXT
        .with(|context| context.get().map(|v| v.0))
        .unwrap_or_else(|| GENERATION.load(Ordering::Acquire))
}

pub fn sync_boundary() -> Option<i64> {
    SYNC_CONTEXT.with(|context| {
        let value = context.get()?;
        context.set(Some((value.0, -1)));
        (value.1 >= 0).then_some(value.1)
    })
}

pub fn sync_local<T>(generation: usize, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let gate = shared_exclusive();
    let _held = gate.acquire(false);
    check_sync_context(generation)?;
    action()
}

/// Short local preparation/commit for commands whose Core call performs networking.
pub fn foreground_phase<T>(generation: usize, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let gate = shared_exclusive();
    let _held = gate.acquire(true);
    check_sync_context(generation)?;
    action()
}

pub fn invalidate_sync_boundary(generation: usize) -> Result<()> {
    check_sync_context(generation)?;
    crate::rpc_serve::advance_generation(generation)?;
    // The sync scheduler and resident catalog live beside the dispatch loop.
    crate::rpc_serve::context_changed();
    Ok(())
}

pub fn check_sync_context(generation: usize) -> Result<()> {
    respire_app::service::validate_client_config()?;
    respire_app::service::ensure_runtime_profile()?;
    if GENERATION.load(Ordering::Acquire) != generation {
        bail!("sync context changed; pending work kept locally");
    }
    let retained = crate::rpc_serve::active_execution_retained(generation)?;
    anyhow::ensure!(
        retained || !crate::host_hooks::background_paused(),
        "runtime maintenance pauses background work"
    );
    if !retained && STOPPING.load(Ordering::Acquire) {
        bail!("runtime is stopping");
    }
    Ok(())
}

pub fn index_status() -> Value {
    let state = match INDEX_WORK.lock() {
        Ok(state) => state,
        Err(_) => return json!({"state":"failed", "error":"index work lock poisoned"}),
    };
    json!({"state": state.state, "scheduled": state.requested,
        "worker_active": INDEX_ON.load(Ordering::Acquire),
        "running": INDEX_RUNNING.load(Ordering::Acquire), "error": state.error,
        "model_setup": (state.state == "failed").then_some("Automatic BGE-M3 preparation or indexing failed; inspect the reported error and download source settings.")})
}

pub fn kick_index() {
    schedule_index(false);
}

pub(crate) fn kick_index_after_create() {
    schedule_index(true);
}

fn schedule_index(batched: bool) {
    if STOPPING.load(Ordering::Acquire) {
        return;
    }
    {
        let mut work = match INDEX_WORK.lock() {
            Ok(work) => work,
            Err(_) => {
                eprintln!("background retrieval indexing failed: index work lock poisoned");
                return;
            }
        };
        work.schedule(Instant::now(), batched);
        if !INDEX_RUNNING.load(Ordering::Acquire) && work.error.is_none() {
            work.state = "scheduled";
        }
    }
    INDEX_CV.notify_one();
}


#[path = "rpc_worker/index_schedule.rs"]
mod index_schedule;
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_worker/parts/index_yield_to_foreground.rs"));
