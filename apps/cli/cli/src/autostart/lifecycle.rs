//! One host lifecycle owner. A durable maintenance marker fences queued/direct
//! requests until the replacement store has been verified; failure stays paused.
use super::{
    control_path, disabled_status, hash, identity, manager, private_write, read_registration,
    AutostartCommand, Platform, Registration, RuntimeCommand, ENVIRONMENT,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};

mod snapshot_control;
pub(crate) use snapshot_control::{snapshot_recover_control, snapshot_restore_control};
// Runtime snapshot authority must use this limit; its generic reader caps at 64 KiB.
pub(super) const MAINTENANCE_MARKER_MAX_BYTES: usize = 16 * 1024;

static MANAGED_ID: OnceLock<String> = OnceLock::new();
static CREDENTIAL_STATE: AtomicU8 = AtomicU8::new(0);
thread_local! {
    static ACTIVE: RefCell<Option<String>> = const { RefCell::new(None) };
    static RECOVERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Phase {
    Paused,
    Starting,
    Verified,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    id: String,
    token: String,
    phase: Phase,
    expected_profile: Option<PathBuf>,
    expected_version: String,
    owner_pid: u32,
    owner_birth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot: Option<snapshot_control::SnapshotMarkerBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_config: Option<Value>,
}

fn marker_path(root: &Path) -> PathBuf {
    control_path(root).with_file_name("maintenance.json")
}
fn marker_bytes(root: &Path) -> Result<Option<Vec<u8>>> {
    let path = marker_path(root);
    let meta = match fs::symlink_metadata(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink()
            && meta.len() <= MAINTENANCE_MARKER_MAX_BYTES as u64,
        "invalid runtime maintenance marker"
    );
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(MAINTENANCE_MARKER_MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAINTENANCE_MARKER_MAX_BYTES, "runtime maintenance marker too large");
    Ok(Some(bytes))
}
fn marker(root: &Path) -> Result<Option<Marker>> {
    let Some(bytes) = marker_bytes(root)? else { return Ok(None); };
    let value: Marker = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        value.schema == 1 && uuid::Uuid::parse_str(&value.token).is_ok(),
        "invalid runtime maintenance marker"
    );
    Ok(Some(value))
}

fn controller_alive(value: &Marker) -> Result<bool> {
    Ok(manager::process_birth(value.owner_pid)?.as_deref() == Some(value.owner_birth.as_str()))
}

pub(super) fn save(config: &mut Registration) -> Result<()> {
    let path = control_path(&config.installation);
    let bytes = serde_json::to_vec_pretty(config)?;
    let result = private_write(&path, &bytes, config.digest.as_deref());
    // Namespace fsync can fail after publication. Preserve the observed identity
    // while propagating that error; the separate maintenance marker stays put.
    if fs::read(&path).is_ok_and(|actual| actual == bytes) {
        config.digest = Some(hash(&bytes));
    }
    result
}

pub(super) fn ensure_control_endpoint(config: &Registration) -> Result<()> {
    let registered = config
        .environment
        .get("RSRS_RPC_PORT")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .context("registered runtime port is missing or invalid")?;
    anyhow::ensure!(registered==crate::net_rpc::rpc_port(),
        "host port differs from registered runtime port {registered}; use the registered RSRS_RPC_PORT to stop/remove the old registration before enabling new settings");
    Ok(())
}

pub(super) fn owned(config: &Registration) -> Result<()> {
    anyhow::ensure!(
        config.owner == manager::owner()?,
        "runtime registration belongs to another OS user"
    );
    let bin = config
        .environment
        .get("RSRS_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.installation.join("bin"));
    let expected = bin.join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" });
    anyhow::ensure!(
        config.executable == expected,
        "runtime registration has an unexpected executable"
    );
    Ok(())
}

pub(crate) fn note_credential_access(available: bool) {
    CREDENTIAL_STATE.store(if available { 1 } else { 2 }, Ordering::Release);
}
pub(crate) fn credential_state() -> &'static str {
    match CREDENTIAL_STATE.load(Ordering::Acquire) {
        1 => "ready",
        2 => "unavailable",
        _ => "unknown",
    }
}
pub(crate) fn managed_enabled() -> Result<bool> {
    Ok(read_registration(&respire_client_host::paths::main_data_dir())?.is_some_and(|config| config.enabled))
}
pub(crate) fn record_exit(success: bool) {
    if managed_id().is_some() {
        if let Ok(Some(config)) = read_registration(&respire_client_host::paths::main_data_dir()) {
            if event(&config, if success { "exit_ok" } else { "exit_error" }).is_err() {
                eprintln!("Managed runtime event could not be recorded.");
            }
        }
    }
}

pub(crate) fn managed_id() -> Option<&'static str> {
    MANAGED_ID.get().map(String::as_str)
}
pub(crate) fn current_boot_token() -> Option<String> {
    ACTIVE.with(|v| v.borrow().clone())
}

pub(crate) fn check_direct_access() -> Result<()> {
    let root = respire_client_host::paths::main_data_dir();
    if let Some(value) = marker(&root)? {
        anyhow::ensure!(ACTIVE.with(|v|v.borrow().as_deref()==Some(value.token.as_str())),
            "runtime maintenance is incomplete; use the host runtime restart or the indicated snapshot/login recovery before accessing the library");
    }
    Ok(())
}

pub(crate) fn check_managed_request() -> Result<()> {
    check_direct_access()
}

pub(crate) fn verify_managed_boot() -> Result<()> {
    let root = respire_client_host::paths::main_data_dir();
    let config = read_registration(&root)?;
    if let Some(id) = managed_id() {
        let config = config
            .as_ref()
            .context("managed runtime registration disappeared")?;
        anyhow::ensure!(
            config.id == id && config.enabled && !config.manually_stopped,
            "managed runtime startup is disabled or manually stopped"
        );
    }
    if let Some(value) = marker(&root)? {
        anyhow::ensure!(
            controller_alive(&value)?,
            "maintenance controller exited; explicit host runtime restart is required"
        );
        let token = std::env::var("RSRS_RUNTIME_BOOT_TOKEN").ok();
        let permitted = managed_id() == Some(value.id.as_str())
            || token.as_deref() == Some(value.token.as_str());
        anyhow::ensure!(
            permitted && matches!(value.phase, Phase::Starting | Phase::Verified),
            "runtime maintenance has not authorized a replacement process"
        );
        anyhow::ensure!(
            value.expected_version == env!("CARGO_PKG_VERSION")
                && value.expected_profile.as_deref()
                    == Some(respire_client_host::config::control_roots()?.1.canonicalize()?.as_path()),
            "replacement runtime version/profile differs from maintenance plan"
        );
    }
    Ok(())
}

include!("lifecycle/maintenance.rs");

include!("lifecycle/maintenance_recovery.rs");

include!("lifecycle/runtime_owner.rs");

include!("lifecycle/registration.rs");

include!("lifecycle/commands.rs");

include!("lifecycle/foreground.rs");

#[cfg(test)]
mod tests;
