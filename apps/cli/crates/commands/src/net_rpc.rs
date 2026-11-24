//! Loopback HTTP client for the local runtime (`127.0.0.1:15169`).
//! Current loopback runtimes need no token; pre-1.0.10 runtimes require the
//! existing host token on the initial request. Non-loopback HTTP access uses
//! `RSRS_RPC_TOKEN` then `<data_dir>/runtime/token` on the host.

use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::runtime_error::{OperationContext, RuntimeError};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_RPC_HOST: &str = "127.0.0.1";
pub const DEFAULT_RPC_PORT: u16 = 15169;
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_POLLS: usize = 40;
const STOP_WAIT: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Health {
    pub server: String,
    pub bin: String,
    #[serde(default)]
    pub runtime_version: String,
    pub pid: u32,
    pub v: u32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub data_dir: String,
    #[serde(default)]
    pub inference: Value,
    #[serde(default)]
    pub rpc: Value,
    #[serde(default)]
    pub store_ready: bool,
    #[serde(default)]
    pub snapshot_restore_id: Option<String>,
    #[serde(default)]
    pub installation_root: String,
    #[serde(default)]
    pub installation_id: String,
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub process_start_id: String,
    #[serde(default)]
    pub image_path: String,
    #[serde(default)]
    pub image_sha256: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub managed_id: Option<String>,
    #[serde(default)]
    pub credential_state: String,
}

impl Health {
    pub fn inference_stalled(&self) -> bool {
        self.inference["host_recovery_required"] == true
    }
}

pub fn rpc_port() -> u16 {
    if let Ok(raw) = respire_client_host::env::var("RSRS_RPC_PORT") {
        let trimmed = raw.trim();
        if let Ok(port) = trimmed.parse::<u16>() {
            if port != 0 {
                return port;
            }
        }
    }
    DEFAULT_RPC_PORT
}

pub fn rpc_base_url() -> String {
    format!("http://{DEFAULT_RPC_HOST}:{}", rpc_port())
}

pub fn no_autostart() -> bool {
    crate::runtime_policy::client_only()
}

/// Stable path of the installed CLI binary. Runtime process identity checks
/// compare a suspect process image against this location (previously provided
/// by the CLI's mcp module; the transport owns the same rule in one place).
pub fn stable_bin_path() -> PathBuf {
    let name = if cfg!(windows) { "rsrs.exe" } else { "rsrs" };
    respire_client_host::paths::bin_dir().join(name)
}

pub fn token_path() -> PathBuf {
    respire_client_host::paths::main_data_dir()
        .join("runtime")
        .join("token")
}

#[cfg(test)]
fn load_token() -> Option<String> {
    read_token().ok().flatten()
}

fn read_token() -> Result<Option<String>> {
    if let Ok(raw) = respire_client_host::env::var("RSRS_RPC_TOKEN") {
        let trimmed = raw.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }
    }
    if let Some(token) = read_token_file(&token_path())? {
        return Ok(Some(token));
    }
    read_token_file(
        &respire_client_host::paths::legacy_token_data_dir()
            .join("runtime")
            .join("token"),
    )
}

fn read_token_file(path: &std::path::Path) -> Result<Option<String>> {
    respire_client_host::credential::read_token_file(path)
        .map_err(|error| RuntimeError::TokenUnreadable(format!("{error:#}")).into())
}

fn check_connection() -> Result<()> {
    check_connection_to(SocketAddr::from(([127, 0, 0, 1], rpc_port())))
}

pub fn check_connection_to(addr: SocketAddr) -> Result<()> {
    // Windows may take slightly over two seconds to report loopback refusal.
    // A shorter deadline misclassifies a stopped service as a network timeout.
    match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            Err(RuntimeError::Unavailable.into())
        }
        Err(error) => Err(RuntimeError::Transport(error.to_string()).into()),
    }
}

pub fn port_is_open() -> bool {
    port_is_open_on(rpc_port())
}

pub fn port_is_open_on(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

pub fn pid_listening_on(port: u16) -> Option<u32> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("netstat")
            .args(["-ano", "-p", "tcp"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let needle = format!(":{}", port);
        for line in text.lines() {
            let line = line.trim();
            if !line.contains("LISTENING")
                || !line
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|address| address.ends_with(&needle))
            {
                continue;
            }
            let pid = line.split_whitespace().last()?.parse().ok()?;
            if pid != 0 {
                return Some(pid);
            }
        }
        None
    }
    #[cfg(unix)]
    {
        let out = std::process::Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.trim().parse().ok())
    }
}

pub fn pid_is_respire(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // Accept both the combined legacy binary and the split rsrs-runtime child.
    #[cfg(windows)]
    {
        respire_spawn::process_identity(pid)
            .ok()
            .flatten()
            .and_then(|value| value.executable.file_name().map(|name| name.to_owned()))
            .is_some_and(|name| {
                name.to_str().is_some_and(|name| {
                    name.eq_ignore_ascii_case("rsrs.exe")
                        || name.eq_ignore_ascii_case("rsrs-runtime.exe")
                })
            })
    }
    #[cfg(target_os = "linux")]
    {
        // Use the same procfs authority as runtime_process_identity, without
        // assuming GNU ps options in minimal musl userspaces.
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_owned()))
            .and_then(|name| {
                name.to_str().map(|name| {
                    name.strip_suffix(" (deleted)").unwrap_or(name) == "rsrs"
                        || name.strip_suffix(" (deleted)").unwrap_or(name) == "rsrs-runtime"
                })
            })
            .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        darwin_runtime_name(std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()).is_ok()
    }
}

pub fn load_or_create_token() -> Result<String> {
    crate::runtime_policy::require_host("creating runtime credentials")?;
    if let Some(existing) = read_token()? {
        if respire_client_host::env::var("RSRS_RPC_TOKEN").is_ok() {
            return Ok(existing);
        }
        return Ok(existing);
    }
    let generated = respire_client_host::credential::generate_runtime_token()?;
    persist_token(&generated)?;
    Ok(generated)
}

pub fn persist_token(token: &str) -> Result<()> {
    let path = token_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut file =
        fs::File::create(&path).with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(token.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn origin_ok(origin: &str, bound_origin: &str) -> bool {
    let origin = origin.trim();
    if origin.is_empty() {
        return true;
    }
    let bound = bound_origin.trim_end_matches('/');
    origin == bound
        || bound
            .rsplit_once(':')
            .is_some_and(|(_, port)| origin == format!("http://localhost:{port}"))
        || (bound.starts_with("http://")
            && bound
                .strip_suffix(":80")
                .is_some_and(|canonical| origin == canonical || origin == "http://localhost"))
}

pub fn is_our_health(value: &Value) -> bool {
    value.get("server").and_then(|v| v.as_str()) == Some("respire")
        && value.get("bin").and_then(|v| v.as_str()).is_some()
}

pub use respire_client_host::version_cmp;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(HTTP_TIMEOUT)
        .redirects(0)
        .build()
}

/// Preserve existing credentials on the first request, including health and stop.
/// New loopback servers ignore the header. Never replay a failed request.
fn send_loopback(
    request: ureq::Request,
    send: impl Fn(ureq::Request) -> std::result::Result<ureq::Response, ureq::Error>,
) -> Result<ureq::Response> {
    let request = match read_token()? {
        Some(token) => request.set("Authorization", &format!("Bearer {token}")),
        None => request,
    };
    let response = send(request).map_err(crate::runtime_error::http)?;
    if !(200..300).contains(&response.status()) {
        return Err(
            RuntimeError::Transport(format!("unexpected HTTP {}", response.status())).into(),
        );
    }
    Ok(response)
}

pub fn health() -> Result<Health> {
    check_connection()?;
    let url = format!("{}/api/health", rpc_base_url());
    let resp = send_loopback(
        agent().get(&url).timeout(Duration::from_secs(2))
            .set("X-Respire-Health", "runtime-v1"),
        |request| request.call(),
    )?;
    let parsed: Value = resp.into_json().context("runtime health is not JSON")?;
    if !is_our_health(&parsed) {
        bail!("port is not a rsrs runtime");
    }
    serde_json::from_value(parsed).context("runtime health shape mismatch")
}

pub fn request_stop() -> Result<()> {
    request_stop_with_timeout(HTTP_TIMEOUT)
}

pub fn request_stop_with_timeout(timeout: Duration) -> Result<()> {
    request_stop_operation(timeout).map(|_| ())
}

pub fn request_stop_operation(timeout: Duration) -> Result<String> {
    request_stop_for(&health()?, timeout)
}

pub fn request_stop_for(health: &Health, timeout: Duration) -> Result<String> {
    crate::runtime_policy::require_host("runtime shutdown")
        .context("runtime stop host policy rejected")?;
    anyhow::ensure!(
        !health.installation_id.is_empty()
            && !health.instance_id.is_empty()
            && !health.account_id.is_empty()
            && health.generation > 0,
        "runtime stop identity is incomplete"
    );
    let expected = json!({
        "installation_id": health.installation_id,
        "instance_id": health.instance_id,
        "account_id": health.account_id,
        "generation": health.generation,
    })
    .to_string();
    let url = format!("{}/api/runtime/stop", rpc_base_url());
    let response = send_loopback(
        agent()
            .post(&url)
            .timeout(timeout)
            .set("X-Respire-Lifecycle", "coordinator-v1"),
        |request| request.send_string(&expected),
    ).context("runtime stop HTTP exchange failed")?;
    let receipt: Value = response
        .into_json()
        .context("invalid stop acknowledgement")?;
    anyhow::ensure!(
        receipt["ok"] == true
            && receipt["state"] == "accepted"
            && receipt["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
        "runtime did not accept a traceable stop operation"
    );
    receipt["operation_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .context("stop acknowledgement lacks operation ID")
}

mod stop_receipt;
pub use stop_receipt::{final_stop_receipt, read_final_stop_receipt, validate_final_stop_receipt};

mod call;
pub use call::{request_scope, rpc_exec, rpc_method, rpc_result};

pub fn wait_until_down() -> Result<()> {
    for _ in 0..STOP_POLLS {
        if !port_is_open() {
            return Ok(());
        }
        std::thread::sleep(STOP_WAIT);
    }
    bail!("runtime did not exit after stop")
}

/// A closed listener does not prove that the library holder has exited.
pub fn wait_until_exited(pid: u32) -> Result<()> {
    for _ in 0..STOP_POLLS {
        if !process_is_running(pid)? {
            return Ok(());
        }
        std::thread::sleep(STOP_WAIT);
    }
    bail!("runtime listener closed but process {pid} has not exited")
}

mod process;
pub use process::{kill_pid, kill_runtime, process_is_running, runtime_process_identity, RuntimeProcessIdentity};

pub fn ensure_token_file() -> Result<PathBuf> {
    let token = load_or_create_token()?;
    let _ = token;
    Ok(token_path())
}

#[cfg(test)]
mod tests;
