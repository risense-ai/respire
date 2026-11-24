//! Loopback HTTP client for the local runtime (`127.0.0.1:15169`).
//! Current loopback runtimes need no token; pre-1.0.10 runtimes require the
//! existing host token on the initial request. Non-loopback HTTP access uses
//! `RSRS_RPC_TOKEN` then `<data_dir>/runtime/token` on the host.

use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::runtime_error::RuntimeError;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_RPC_HOST: &str = "127.0.0.1";
pub const DEFAULT_RPC_PORT: u16 = 15169;
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(all(test, unix))]
const STOP_POLLS: usize = 40;
#[cfg(all(test, unix))]
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
    if let Ok(raw) = respire_app::env::var("RSRS_RPC_PORT") {
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

pub fn token_path() -> PathBuf {
    crate::rpc_worker::runtime_dir_path().join("token")
}

#[cfg(test)]
fn load_token() -> Option<String> {
    read_token().ok().flatten()
}

pub(crate) fn read_token() -> Result<Option<String>> {
    if let Ok(raw) = respire_app::env::var("RSRS_RPC_TOKEN") {
        let trimmed = raw.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }
    }
    if let Some(token) = read_token_file(&token_path())? {
        return Ok(Some(token));
    }
    read_token_file(
        &respire_app::service::data_dir()
            .join("runtime")
            .join("token"),
    )
}

fn read_token_file(path: &std::path::Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(RuntimeError::TokenUnreadable(format!("{}: {error}", path.display())).into())
        }
    }
}

fn check_connection() -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], rpc_port()));
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

pub fn port_is_open_on(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Query the local TCP table without treating a failed query as an absent listener.
#[cfg(windows)]
pub fn windows_listener_pid_on(port: u16) -> std::io::Result<Option<u32>> {
    let out = std::process::Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("netstat TCP listener query failed: {}", out.status)));
    }
    windows_listener_pid_from_netstat(&out.stdout, port)
}

#[cfg(windows)]
fn windows_listener_pid_from_netstat(stdout: &[u8], port: u16) -> std::io::Result<Option<u32>> {
    let needle = format!(":{port}");
    let mut owner = None;
    for line in stdout.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii_start();
        // Localized headings use the OEM code page; TCP data rows use ASCII fields.
        if line.split(|byte| byte.is_ascii_whitespace()).next() != Some(&b"TCP"[..]) {
            continue;
        }
        let text = std::str::from_utf8(line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let fields: Vec<_> = text.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid TCP table row"));
        }
        let pid = fields[4].parse::<u32>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if fields[3] == "LISTENING" {
            if pid == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "TCP listener PID is zero"));
            }
            if fields[1].ends_with(&needle) && owner.is_none() {
                owner = Some(pid);
            }
        }
    }
    Ok(owner)
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
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output();
        let Ok(out) = out else {
            return false;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let name = text.trim();
        name == "rsrs"
            || name.ends_with("/rsrs")
            || name == "rsrs-runtime"
            || name.ends_with("/rsrs-runtime")
    }
}

pub fn load_or_create_token() -> Result<String> {
    crate::runtime_policy::require_host("creating runtime credentials")?;
    if let Some(existing) = read_token()? {
        if respire_app::env::var("RSRS_RPC_TOKEN").is_ok() {
            return Ok(existing);
        }
        return Ok(existing);
    }
    let generated = respire_app::memory::crypto::random_hex(32);
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

pub fn version_cmp(ours: &str, theirs: &str) -> std::cmp::Ordering {
    fn nums(s: &str) -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let a = nums(ours);
    let b = nums(theirs);
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

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
        agent().get(&url).timeout(Duration::from_secs(2)),
        |request| request.call(),
    )?;
    let parsed: Value = resp.into_json().context("runtime health is not JSON")?;
    if !is_our_health(&parsed) {
        bail!("port is not a rsrs runtime");
    }
    serde_json::from_value(parsed).context("runtime health shape mismatch")
}

pub fn instance_is_running(health: &Health) -> Result<bool> {
    anyhow::ensure!(
        health.pid > 0 && !health.process_start_id.is_empty(),
        "runtime process identity is incomplete"
    );
    #[cfg(windows)]
    {
        return Ok(respire_spawn::process_birth(health.pid)?
            .is_some_and(|birth| health.process_start_id == format!("windows-filetime-{birth}")));
    }
    #[cfg(target_os = "linux")]
    {
        let raw = match fs::read_to_string(format!("/proc/{}/stat", health.pid)) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("cannot inspect runtime process identity"),
        };
        let fields = raw.rsplit_once(')').context("invalid process identity")?.1;
        if fields.split_whitespace().next() == Some("Z") {
            return Ok(false);
        }
        let birth = fields
            .split_whitespace()
            .nth(19)
            .context("process birth is missing")?;
        return Ok(health.process_start_id == format!("linux-starttime-{birth}"));
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("ps")
            .args([
                "-p",
                &health.pid.to_string(),
                "-o",
                "lstart=",
                "-o",
                "stat=",
            ])
            .output()
            .context("cannot inspect runtime process identity")?;
        let text = String::from_utf8(output.stdout)?;
        if output.status.code() == Some(1) && text.trim().is_empty() {
            return Ok(false);
        }
        anyhow::ensure!(
            output.status.success(),
            "cannot inspect runtime process identity"
        );
        let (birth, state) = text
            .trim()
            .rsplit_once(char::is_whitespace)
            .context("invalid process identity")?;
        return Ok(!state.starts_with('Z')
            && health.process_start_id == format!("darwin-lstart-{}", birth.trim()));
    }
}

pub fn rpc_exec(args: Vec<String>) -> Result<Value> {
    rpc_method("cli.exec", args)
}

pub fn rpc_method(method: &str, args: Vec<String>) -> Result<Value> {
    rpc_method_with_timeout(method, args, None)
}

pub fn request_scope(health: &Health) -> Result<Value> {
    anyhow::ensure!(
        health.v == crate::rpc_worker::PROTOCOL_V
            && !health.account_id.is_empty()
            && !health.instance_id.is_empty()
            && health.generation > 0
            && !health.installation_root.is_empty(),
        "runtime request scope is incomplete or incompatible"
    );
    anyhow::ensure!(
        fs::canonicalize(&health.installation_root)?
            == fs::canonicalize(respire_app::service::main_data_dir())?,
        "runtime request belongs to another installation"
    );
    Ok(
        json!({"account_id":health.account_id,"instance_id":health.instance_id,"generation":health.generation}),
    )
}

fn rpc_timeout(method: &str, args: &[String], test_timeout: Option<Duration>) -> Duration {
    if let Some(timeout) = test_timeout {
        return timeout;
    }
    match method {
        "cli.progress" => return Duration::from_secs(1),
        "model.control" => return Duration::from_secs(3),
        "cli.exec" => {}
        _ => return HTTP_TIMEOUT,
    }
    // Native inference stays inside the runtime. Tracking IDs are values, not
    // command words, and longer write budgets never authorize a request replay.
    let command: Vec<_> = crate::rpc_worker::command_words(args).take(2).collect();
    if matches!(
        command.as_slice(),
        ["model", "activate"]
    ) || command
        .first()
        .is_some_and(|c| matches!(*c, "reembed" | "sync" | "import"))
    {
        Duration::from_secs(30 * 60)
    } else {
        HTTP_TIMEOUT
    }
}

pub(crate) fn rpc_method_with_timeout(
    method: &str,
    args: Vec<String>,
    test_timeout: Option<Duration>,
) -> Result<Value> {
    let scope = if matches!(
        method,
        "cli.progress" | "runtime.status" | "assets.status" | "model.catalog" | "model.progress"
    ) {
        None
    } else {
        Some(request_scope(&health()?)?)
    };
    let url = format!("{}/api/rpc", rpc_base_url());
    let request_id = uuid::Uuid::new_v4().to_string();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let body = json!({
        "v": crate::rpc_worker::PROTOCOL_V,
        "id": request_id,
        "operation_id": operation_id.clone(),
        "idempotency_key": operation_id,
        "scope": scope,
        "method": method,
        "args": args,
        "lifecycle": if method == "runtime.stop" { Some("coordinator-v1") } else { None },
    });
    let timeout = rpc_timeout(method, &args, test_timeout);
    let resp = send_loopback(agent().post(&url).timeout(timeout), |request| {
        request.send_json(body.clone())
    })
    .map_err(|error| {
        if method == "cli.exec"
            && matches!(
                error.downcast_ref::<RuntimeError>(),
                Some(RuntimeError::Transport(_))
            )
        {
            RuntimeError::OutcomeUnknown {
                request_id: body["id"].as_str().unwrap_or_default().to_owned(),
                operation_id: body["operation_id"].as_str().unwrap_or_default().to_owned(),
                detail: error.to_string(),
            }
            .into()
        } else {
            error
        }
    })?;
    let parsed: Value = resp.into_json().map_err(|error| {
        if method == "cli.exec" {
            anyhow::Error::from(RuntimeError::OutcomeUnknown {
                request_id: body["id"].as_str().unwrap_or_default().to_owned(),
                operation_id: body["operation_id"].as_str().unwrap_or_default().to_owned(),
                detail: format!("runtime reply unavailable: {error}"),
            })
        } else {
            anyhow::anyhow!("runtime rpc is not JSON: {error}")
        }
    })?;
    Ok(parsed)
}


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/net_rpc/parts/wait_until_exited.rs"));
