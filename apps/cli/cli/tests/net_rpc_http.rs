//! HTTP RPC against the official standalone `rsrs-runtime` fixture.
//! The CLI remains a client; neither fixture substitutes for Runtime G1 acceptance.
//! Isolated DATA_DIR + ephemeral port. Safe for GitHub Actions.
//! Do not bind the developer machine's default 15169.

use std::fs;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod support;
#[path = "../../test-support/loopback_http.rs"]
mod loopback_http;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rsrs")
}

fn runtime_bin() -> Result<std::path::PathBuf, String> {
    let runtime = std::env::var_os("RSRS_RUNTIME_BIN")
        .ok_or("supply RSRS_RUNTIME_BIN with the official standalone Runtime fixture")?;
    let runtime = std::path::PathBuf::from(runtime)
        .canonicalize()
        .map_err(|err| err.to_string())?;
    if runtime == std::path::Path::new(bin()).canonicalize().map_err(|err| err.to_string())? {
        return Err("CLI shim cannot substitute for the standalone Runtime fixture".into());
    }
    Ok(runtime)
}

fn pick_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    Ok(listener.local_addr().map_err(|err| err.to_string())?.port())
}

fn fixture_health(root: &std::path::Path) -> serde_json::Value {
    serde_json::json!({"server":"respire","bin":"http-fixture","pid":1,"v":1,
        "installation_root":root,"account_id":"fixture-account",
        "instance_id":"fixture-instance","generation":1})
}

fn check_fixture_operation(body: &serde_json::Value) -> Result<(), String> {
    if body["scope"] != serde_json::json!({"account_id":"fixture-account",
        "instance_id":"fixture-instance","generation":1})
        || body["operation_id"].as_str().is_none_or(str::is_empty)
        || body["operation_id"] != body["idempotency_key"]
        || body["operation_id"] == body["id"]
    {
        return Err(format!("unbound fixture request: {body}"));
    }
    Ok(())
}

struct Runtime {
    child: Child,
    dir: tempfile::TempDir,
    port: u16,
    stderr_path: std::path::PathBuf,
    identity: serde_json::Value,
    host_control: bool,
}

impl Runtime {
    fn host_command(&self, args: &[&str]) -> Result<std::process::Output, String> {
        let host = self.dir.path().join("host");
        let home = host.join("home");
        Command::new(self.dir.path().join("bin").join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" }))
            .args(args).current_dir(host.join("work"))
            .env("HOME", &home).env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", host.join("run"))
            .env("APPDATA", home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", home.join("AppData/Local"))
            .env("RSRS_DATA_DIR", self.dir.path()).env("RSRS_RPC_PORT", self.port.to_string())
            .env("RSRS_BIN_DIR", self.dir.path().join("bin"))
            .env_remove("RSRS_RUNTIME_BIN").env_remove("RSRS_CLIENT_ONLY")
            .env_remove("RSRS_NO_AUTOSTART").env_remove("RSRS_RPC_TOKEN")
            .env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN")
            .output().map_err(|err| err.to_string())
    }

    fn refresh_controlled_identity(&mut self) -> Result<(), String> {
        self.verify_stop_receipt(None)?;
        let health: serde_json::Value = ureq::get(&format!("http://127.0.0.1:{}/api/health", self.port))
            .set("X-Respire-Health", "runtime-v1")
            .timeout(Duration::from_secs(3)).call().map_err(|err| err.to_string())?
            .into_json().map_err(|err| err.to_string())?;
        for field in ["server", "v", "installation_id", "account_id", "bin", "runtime_version", "image_sha256"] {
            assert_eq!(health[field], self.identity[field], "replacement Runtime changed {field}");
        }
        for field in ["installation_root", "data_dir", "exe", "image_path"] {
            let actual = health[field].as_str().ok_or("replacement Runtime path missing")?;
            let expected = self.identity[field].as_str().ok_or("original Runtime path missing")?;
            assert_eq!(fs::canonicalize(actual).map_err(|err| err.to_string())?,
                fs::canonicalize(expected).map_err(|err| err.to_string())?);
        }
        assert_ne!(health["instance_id"], self.identity["instance_id"]);
        assert!(health["generation"].as_u64().is_some_and(|generation| generation > 0));
        for field in ["account_id", "instance_id", "process_start_id"] {
            assert!(health[field].as_str().is_some_and(|value| !value.is_empty()));
        }
        let pid = health["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok())
            .ok_or("replacement Runtime PID missing")?;
        let process = respire_commands::net_rpc::runtime_process_identity(pid).map_err(|err| err.to_string())?;
        let prefix = if cfg!(windows) { "windows-filetime-" }
            else if cfg!(target_os = "linux") { "linux-starttime-" } else { "darwin-lstart-" };
        assert_eq!(health["process_start_id"], format!("{prefix}{}", process.started_at));
        assert_eq!(fs::canonicalize(&process.executable).map_err(|err| err.to_string())?,
            fs::canonicalize(health["image_path"].as_str().ok_or("Runtime image missing")?)
                .map_err(|err| err.to_string())?);
        self.identity = health;
        Ok(())
    }

    fn check_health(&self, health: &serde_json::Value) {
        for field in ["pid", "bin", "runtime_version", "installation_id", "installation_root",
            "account_id", "instance_id", "generation", "process_start_id", "image_path", "image_sha256"] {
            assert_eq!(health[field], self.identity[field], "Runtime identity changed: {field}");
        }
        assert_eq!(health["pid"], self.child.id());
    }

    fn rpc_body(&self, id: &str, args: &[&str]) -> serde_json::Value {
        let operation = uuid::Uuid::new_v4().to_string();
        serde_json::json!({"v":1,"id":id,"method":"cli.exec","args":args,
            "operation_id":operation,"idempotency_key":operation,
            "scope":{"account_id":self.identity["account_id"],
                "instance_id":self.identity["instance_id"],"generation":self.identity["generation"]}})
    }

    fn rpc(&self, id: &str, args: &[&str], timeout: Duration) -> Result<serde_json::Value, String> {
        let body = self.rpc_body(id, args);
        let reply: serde_json::Value = ureq::post(&format!("http://127.0.0.1:{}/api/rpc", self.port))
            .timeout(timeout).send_json(body.clone()).map_err(|err| err.to_string())?
            .into_json().map_err(|err| err.to_string())?;
        for field in ["id", "operation_id", "idempotency_key", "scope"] {
            assert_eq!(reply[field], body[field], "unbound RPC reply: {field}");
        }
        assert_ne!(reply["code"], "scope_required", "{reply}");
        assert_ne!(reply["code"], "scope_mismatch", "{reply}");
        assert_eq!(reply["pid"], self.identity["pid"]);
        assert_eq!(reply["bin"], self.identity["bin"]);
        Ok(reply)
    }

    fn verify_stop_receipt(&self, operation: Option<&str>) -> Result<(), String> {
        let instance = self.identity["instance_id"].as_str().ok_or("Runtime instance missing")?;
        let receipt = fs::read(self.dir.path().join("runtime").join(format!("stop-{instance}.json")))
            .map_err(|err| err.to_string())?;
        let receipt = serde_json::from_slice(&receipt).map_err(|err| err.to_string())?;
        let health = serde_json::from_value(self.identity.clone()).map_err(|err| err.to_string())?;
        respire_commands::net_rpc::validate_final_stop_receipt(&health, operation, &receipt)
            .map_err(|err| err.to_string())
    }

    fn stop(&self) -> Result<String, String> {
        let expected = serde_json::json!({"installation_id":self.identity["installation_id"],
            "account_id":self.identity["account_id"],"instance_id":self.identity["instance_id"],
            "generation":self.identity["generation"]});
        let receipt: serde_json::Value = ureq::post(&format!("http://127.0.0.1:{}/api/runtime/stop", self.port))
            .timeout(Duration::from_secs(2)).set("X-Respire-Lifecycle", "coordinator-v1")
            .send_json(expected).map_err(|err| err.to_string())?
            .into_json().map_err(|err| err.to_string())?;
        assert_eq!(receipt["ok"], true, "{receipt}");
        assert_eq!(receipt["state"], "accepted", "{receipt}");
        assert!(receipt["request_id"].as_str().is_some_and(|id| !id.is_empty()));
        receipt["operation_id"].as_str().filter(|id| !id.is_empty())
            .map(str::to_owned).ok_or_else(|| "stop operation ID missing".into())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.host_control {
            // Private setters replace the original child. CLI stop verifies roots and OS birth
            // before draining the current owner, rather than killing a PID found by port.
            match self.host_command(&["--runtime-internal", "--stop", "--json"]) {
                Ok(output) if output.status.success() => {}
                _ => {
                    self.dir.disable_cleanup(true);
                    eprintln!("owned mode-test Runtime cleanup was not confirmed; retained {}", self.dir.path().display());
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_internal_runtime() -> Result<Runtime, String> {
    start_internal_runtime_with_mode("normal")
}

fn start_internal_runtime_with_mode(mode: &str) -> Result<Runtime, String> {
    start_internal_runtime_with_legacy_path(mode, false)
}

fn start_internal_runtime_with_legacy_path(
    mode: &str,
    legacy_path: bool,
) -> Result<Runtime, String> {
    start_runtime(mode, legacy_path, false)
}

fn start_scoped_runtime() -> Result<Runtime, String> {
    start_runtime("normal", false, false)
}

fn start_runtime(mode: &str, legacy_path: bool, host_control: bool) -> Result<Runtime, String> {
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    fs::write(
        dir.path().join("client.json"),
        serde_json::json!({"service_mode":mode}).to_string(),
    )
    .map_err(|err| err.to_string())?;
    if legacy_path {
        // A directory at the former socket path cannot be unlinked as a stale
        // socket or bound by the old listener. TCP startup must leave it alone.
        fs::create_dir_all(dir.path().join("runtime/rpc.sock")).map_err(|err| err.to_string())?;
        fs::write(
            dir.path().join("runtime/rpc.sock/owned-marker"),
            b"preserve",
        )
        .map_err(|err| err.to_string())?;
    }
    let port = pick_port()?;
    let stderr_path = dir.path().join("stderr.log");
    let stderr_file = fs::File::create(&stderr_path).map_err(|err| err.to_string())?;
    let mut runtime = runtime_bin()?;
    if host_control {
        let cli = dir.path().join("bin").join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" });
        fs::create_dir_all(cli.parent().ok_or("CLI fixture bin directory missing")?)
            .map_err(|err| err.to_string())?;
        fs::copy(bin(), &cli).map_err(|err| err.to_string())?;
        use sha2::Digest;
        let source_hash = sha2::Sha256::digest(fs::read(bin()).map_err(|err| err.to_string())?);
        let installed_hash = sha2::Sha256::digest(fs::read(&cli).map_err(|err| err.to_string())?);
        assert_eq!(installed_hash, source_hash, "installed CLI fixture differs from the tested binary");
        for path in ["host/work", "host/run", "host/home/.config", "host/home/.local/share",
            "host/home/.local/state", "host/home/.cache", "host/home/AppData/Roaming", "host/home/AppData/Local"] {
            fs::create_dir_all(dir.path().join(path)).map_err(|err| err.to_string())?;
        }
        support::install_runtime_fixture(dir.path()).map_err(|err| format!("{err:#}"))?;
        let version = runtime.parent().and_then(std::path::Path::file_name)
            .ok_or("Runtime fixture version directory missing")?;
        let filename = runtime.file_name().ok_or("Runtime fixture binary name missing")?;
        runtime = dir.path().join("bin/runtime").join(version).join(filename)
            .canonicalize().map_err(|err| err.to_string())?;
    }
    let version = Command::new(&runtime).arg("--version").output().map_err(|err| err.to_string())?;
    if !version.status.success() {
        return Err("standalone Runtime version query failed".into());
    }
    let version_text = String::from_utf8(version.stdout).map_err(|err| err.to_string())?;
    let version = version_text.trim().strip_prefix("rsrs-runtime ")
        .filter(|version| !version.is_empty()).ok_or("unexpected standalone Runtime version identity")?;
    use sha2::Digest;
    use std::io::Read;
    let mut image = fs::File::open(&runtime).map_err(|err| err.to_string())?;
    let mut digest = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = image.read(&mut buffer).map_err(|err| err.to_string())?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    let image_sha256 = format!("{:x}", digest.finalize());
    let mut child = Command::new(&runtime)
        .arg("serve")
        .args(["--port", &port.to_string()])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env("RSRS_BIN_DIR", dir.path().join("bin"))
        .env("RSRS_CORE_TEST_MODE", "1")
        .env("RSRS_NO_AUTOSYNC", "1")
        .env_remove("RSRS_NO_AUTOSTART")
        .env_remove("RSRS_CLIENT_ONLY")
        .env_remove("RSRS_RPC_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|err| err.to_string())?;
    let health_url = format!("http://127.0.0.1:{port}/api/health");
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < Duration::from_secs(20) {
        match ureq::get(&health_url).call() {
            Ok(resp) if resp.status() == 200 => {
                let identity = match resp.into_json::<serde_json::Value>() {
                    Ok(identity) => identity,
                    Err(err) => { last = err.to_string(); continue; }
                };
                let rt = Runtime { child, dir, port, stderr_path, identity, host_control };
                let health = &rt.identity;
                if health["server"] != "respire" || health["v"] != 1
                    || health["pid"] != rt.child.id() || health["bin"] != version
                    || health["runtime_version"] != version || health["image_sha256"] != image_sha256
                    || health["generation"].as_u64().is_none_or(|generation| generation == 0)
                    || ["installation_id", "account_id", "instance_id", "process_start_id"].iter()
                        .any(|field| health[*field].as_str().is_none_or(str::is_empty))
                {
                    return Err("standalone Runtime health identity does not match the owned fixture".into());
                }
                for field in ["exe", "image_path"] {
                    let path = health[field].as_str().ok_or("Runtime image path missing")?;
                    assert_eq!(fs::canonicalize(path).map_err(|err| err.to_string())?, runtime);
                }
                let root = health["installation_root"].as_str().ok_or("Runtime installation root missing")?;
                assert_eq!(fs::canonicalize(root).map_err(|err| err.to_string())?,
                    fs::canonicalize(rt.dir.path()).map_err(|err| err.to_string())?);
                return Ok(rt);
            }
            Ok(resp) => last = format!("status {}", resp.status()),
            Err(err) => last = err.to_string(),
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "health not ready: {last}; stderr={}",
        fs::read_to_string(&stderr_path).unwrap_or_default()
    ))
}

#[path = "net_rpc_http/native_modes.rs"]
mod native_modes;
#[path = "net_rpc_http/protocol.rs"]
mod protocol;
#[path = "net_rpc_http/client_transport.rs"]
mod client_transport;
#[path = "net_rpc_http/v1_discovery.rs"]
mod v1_discovery;
