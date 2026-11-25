//! Real private-channel and serving-child checks against isolated source fixtures.
use anyhow::{Context, Result};
use respire_client_host::lock::LibraryLock;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[path = "../src/startup_control/wire.rs"]
mod wire;
use wire::{Acquire, Endpoint, Finish, Held, Released, SCHEMA};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir()
            .canonicalize()?
            .join(format!("startup-process-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        Ok(Self(root.canonicalize()?))
    }

    fn command(&self, port: u16) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rsrs-runtime"));
        command
            .env("RSRS_DATA_DIR", &self.0)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("RSRS_RPC_PORT", port.to_string())
            .env("RSRS_CORE_TEST_MODE", "1")
            .env_remove("RSRS_RUNTIME_BOOT_TOKEN")
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .env_remove("RSRS_M3_DIR")
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_CLIENT_ONLY")
            .env_remove("RSRS_NO_AUTOSTART")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }

    fn request(&self, profile: PathBuf) -> Result<Acquire> {
        let pid = std::process::id();
        Ok(Acquire {
            schema_version: SCHEMA,
            installation_root: self.0.clone(),
            profile,
            controller_pid: pid,
            controller_birth: process_birth(pid)?,
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("startup process fixture cleanup failed: {error}");
        }
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            if let Err(error) = self.0.kill() {
                eprintln!("fixture child cleanup failed: {error}");
            }
        }
        if let Err(error) = self.0.wait() {
            eprintln!("fixture child reap failed: {error}");
        }
    }
}

fn process_birth(pid: u32) -> Result<String> {
    #[cfg(windows)]
    {
        return respire_spawn::process_birth(pid)?
            .map(|value| value.to_string())
            .context("fixture birth");
    }
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        return Ok(stat
            .rsplit_once(')')
            .context("fixture stat")?
            .1
            .split_whitespace()
            .nth(19)
            .context("fixture birth")?
            .to_owned());
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .output()?;
        anyhow::ensure!(output.status.success(), "fixture process inspection failed");
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}

fn send(input: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    input.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
    input.write_all(&bytes)?;
    input.flush()?;
    Ok(())
}

fn receive<T: serde::de::DeserializeOwned>(output: &mut impl Read) -> Result<T> {
    let mut size = [0; 4];
    output.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    anyhow::ensure!(
        size > 0 && size <= wire::MAX_FRAME,
        "invalid fixture response frame"
    );
    let mut bytes = vec![0; size];
    output.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn wait(child: &mut Child) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        anyhow::ensure!(Instant::now() < deadline, "fixture child exit timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cold_launch_under_exclusion_waits_for_old_cli_and_verifies_child() -> Result<()> {
    let fixture = Fixture::new()?;
    let profile = fixture.0.join("accounts/synthetic");
    fs::write(
        fixture.0.join("client.json"),
        serde_json::to_vec(&json!({"data_dir": profile}))?,
    )?;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    let old_cli = LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO)?;
    let mut helper = Process(
        fixture
            .command(port)
            .arg("--startup-control-internal")
            .spawn()?,
    );
    let mut input = helper.0.stdin.take().context("helper input")?;
    let mut output = helper.0.stdout.take().context("helper output")?;
    send(&mut input, &fixture.request(profile.clone())?)?;
    // The compatible old host retains the exclusion while the helper is starting.
    std::thread::sleep(Duration::from_millis(300));
    assert!(helper.0.try_wait()?.is_none());
    drop(old_cli);
    let held: Held = receive(&mut output)?;
    assert_eq!(held.helper_pid, helper.0.id());
    assert_eq!(held.helper_birth, process_birth(helper.0.id())?);
    assert_eq!(held.profile, profile);
    assert!(LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO).is_err());
    drop(reservation);
    let mut runtime = Process(
        fixture
            .command(port)
            .args(["serve", "--host", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let health: Value = loop {
        anyhow::ensure!(
            runtime.0.try_wait()?.is_none(),
            "serving child exited before health"
        );
        match ureq::get(&format!("http://127.0.0.1:{port}/api/health"))
            .timeout(Duration::from_secs(2))
            .call()
        {
            Ok(response) => {
                let health: Value = response.into_json()?;
                anyhow::ensure!(
                    health["startup_complete"].is_boolean(),
                    "fixture startup status missing"
                );
                if health["startup_complete"] == true {
                    break health;
                }
                anyhow::ensure!(Instant::now() < deadline, "fixture startup timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(ureq::Error::Transport(error))
                if error.kind() == ureq::ErrorKind::ConnectionFailed =>
            {
                anyhow::ensure!(Instant::now() < deadline, "fixture health timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    };
    let endpoint: Endpoint = serde_json::from_value(json!({
        "pid": health["pid"], "process_start_id": health["process_start_id"],
        "image_path": health["image_path"], "image_sha256": health["image_sha256"],
        "instance_id": health["instance_id"], "account_id": health["account_id"],
        "generation": health["generation"], "url": health["url"],
    }))?;
    assert_eq!(endpoint.pid, runtime.0.id());
    assert_eq!(endpoint.account_id, held.account_id);
    send(
        &mut input,
        &Finish {
            guard_id: held.guard_id.clone(),
            endpoint: Some(endpoint.clone()),
        },
    )?;
    let released: Released = receive(&mut output)?;
    assert_eq!(released.endpoint, Some(endpoint));
    assert_eq!(released.guard_id, held.guard_id);
    assert!(wait(&mut helper.0)?.success());
    let _next_host = LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO)?;
    assert!(LibraryLock::acquire(&profile, Duration::ZERO).is_err());
    Ok(())
}

#[test]
fn eof_releases_exclusion_and_wrong_scope_is_rejected() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut helper = Process(
        fixture
            .command(19169)
            .arg("--startup-control-internal")
            .spawn()?,
    );
    let mut input = helper.0.stdin.take().context("helper input")?;
    send(&mut input, &fixture.request(fixture.0.clone())?)?;
    let _: Held = receive(helper.0.stdout.as_mut().context("helper output")?)?;
    drop(input);
    assert!(!wait(&mut helper.0)?.success());
    let guard = LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO)?;
    drop(guard);
    let mut helper = Process(
        fixture
            .command(19169)
            .arg("--startup-control-internal")
            .spawn()?,
    );
    let mut input = helper.0.stdin.take().context("helper input")?;
    send(
        &mut input,
        &fixture.request(fixture.0.join("wrong profile"))?,
    )?;
    assert!(!wait(&mut helper.0)?.success());
    assert!(!fixture.0.join("wrong profile").exists());
    Ok(())
}

#[test]
fn eof_cancels_a_wait_behind_the_old_cli() -> Result<()> {
    let fixture = Fixture::new()?;
    let _old_cli = LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO)?;
    let mut helper = Process(
        fixture
            .command(19169)
            .arg("--startup-control-internal")
            .spawn()?,
    );
    let mut input = helper.0.stdin.take().context("helper input")?;
    send(&mut input, &fixture.request(fixture.0.clone())?)?;
    drop(input);
    assert!(!wait(&mut helper.0)?.success());
    Ok(())
}

#[test]
fn profile_changed_by_old_cli_is_rejected_before_held_receipt() -> Result<()> {
    use std::io::BufRead;
    let fixture = Fixture::new()?;
    let old_cli = LibraryLock::acquire(&fixture.0.join("runtime/takeover"), Duration::ZERO)?;
    let mut helper = Process(
        fixture
            .command(19169)
            .arg("--startup-control-internal")
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut input = helper.0.stdin.take().context("helper input")?;
    send(&mut input, &fixture.request(fixture.0.clone())?)?;
    let mut diagnostics =
        std::io::BufReader::new(helper.0.stderr.take().context("helper diagnostics")?);
    let mut line = String::new();
    diagnostics.read_line(&mut line)?;
    anyhow::ensure!(
        line.contains("locked"),
        "helper did not wait for old host exclusion"
    );
    let other = fixture.0.join("accounts/other synthetic profile");
    fs::create_dir_all(&other)?;
    fs::write(
        fixture.0.join("client.json"),
        serde_json::to_vec(&json!({"data_dir":other}))?,
    )?;
    drop(old_cli);
    assert!(!wait(&mut helper.0)?.success());
    let mut output = Vec::new();
    helper
        .0
        .stdout
        .take()
        .context("helper output")?
        .read_to_end(&mut output)?;
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn private_cold_keygen_commits_and_queries_the_same_synthetic_receipt() -> Result<()> {
    use respire_protocol::{HOST_AUTH_KEYGEN_METHOD, HOST_AUTH_RESULT_METHOD};
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new()?;
    let operation = uuid::Uuid::new_v4().to_string();
    let (factor, _, salt, wrapped, nonce) = respire_app::auth::new_vault_v4()?;
    let backend = if cfg!(target_os = "linux") { "secret_service" } else { "native" };
    let material = json!({
        "super_password": factor, "kdf_salt": salt, "wrapped_urk": wrapped, "urk_nonce": nonce,
        "keyring_account": format!("login-{}", operation.replace('-', "")), "keyring_backend": backend,
    });
    let port = 64183;
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_rsrs-runtime")).canonicalize()?;
    let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let id = hash(fixture.0.display().to_string().as_bytes());
    let binary_digest = hash(&fs::read(&binary)?);
    let pid = std::process::id();
    let birth = process_birth(pid)?;
    let call = |method: &str| -> Result<Value> {
        let grant_id = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let issued = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        let grant = json!({
            "schema_version":1, "protocol_version":1, "nonce":grant_id, "instance_id":grant_id,
            "installation_root":fixture.0, "installation_id":id, "profile":fixture.0, "account_id":id,
            "sync_generation":0, "image_path":binary, "image_sha256":binary_digest, "supervisor_pid":pid,
            "supervisor_birth":birth, "managed_id":null, "registration_sha256":null, "maintenance":null,
            "port":port, "issued_at_ms":issued, "expires_at_ms":issued+60_000,
        });
        let capability = method == "control.auth.cold_keygen_capability";
        let binding = json!({"installation_root":fixture.0,"profile":fixture.0,
            "launch_id":grant_id,"expected_account_id":null});
        let request = if capability {
            json!({"v":1,"id":request_id,"method":method,
                "args":[json!({"schema_version":1,"binding":binding}).to_string()]})
        } else {
            json!({"v":1,"id":request_id,"operation_id":operation,"idempotency_key":operation,"method":method,
                "args":[json!({"schema_version":1,"binding":binding,"identity":{"kind":"absent"},
                    "force":false,"material":material}).to_string()]})
        };
        let mut child = Process(fixture.command(port).current_dir(&fixture.0)
            .stderr(Stdio::null()).arg("--host-cold-keygen-internal").spawn()?);
        let mut input = child.0.stdin.take().context("cold keygen fixture input")?;
        let mut output = child.0.stdout.take().context("cold keygen fixture output")?;
        send(&mut input, &grant)?;
        send(&mut input, &request)?;
        let response: Value = receive(&mut output)?;
        let exit = wait(&mut child.0)?;
        anyhow::ensure!(response["id"] == request_id && response["pid"] == child.0.id(),
            "cold keygen did not match its exact inherited request");
        if capability {
            assert!(response["operation_id"].is_null() && response["idempotency_key"].is_null());
        } else {
            assert!(response["operation_id"] == operation && response["idempotency_key"] == operation);
        }
        anyhow::ensure!(exit.code().is_some_and(|code| response["exit"] == code),
            "cold keygen process status mismatch");
        Ok(response)
    };

    assert!(!fixture.0.join("runtime").exists());
    let negotiated = call("control.auth.cold_keygen_capability")?;
    assert!(negotiated["ok"] == true);
    assert_eq!(negotiated["envelope"]["command"], "keygen-capability");
    assert_eq!(negotiated["envelope"]["summary"], json!({"protocol":"cold_keygen.v1"}));
    assert!(!fixture.0.join("runtime").exists() && !fixture.0.join("session.json").exists());

    let lifetime = LibraryLock::acquire(&fixture.0.join("runtime"), Duration::ZERO)?;
    let blocked = call(HOST_AUTH_KEYGEN_METHOD)?;
    assert_eq!(blocked["code"], "auth_ownership_unavailable");
    assert!(!fixture.0.join("session.json").exists());
    drop(lifetime);

    let committed = call(HOST_AUTH_KEYGEN_METHOD)?;
    anyhow::ensure!(committed["ok"] == true, "cold keygen did not commit: {}", committed["code"]);
    let session = fixture.0.join("session.json");
    let original = fs::read(&session)?;
    let receipt = &committed["envelope"]["summary"];
    assert_eq!(receipt["outcome"], "committed");
    assert_eq!(receipt["session_sha256"], hash(&original));
    assert!(receipt["super_password"].is_null());
    let recovered = call(HOST_AUTH_RESULT_METHOD)?;
    assert!(recovered["ok"] == true);
    assert_eq!(recovered["envelope"]["summary"], *receipt);
    assert_eq!(fs::read(session)?, original);
    Ok(())
}
