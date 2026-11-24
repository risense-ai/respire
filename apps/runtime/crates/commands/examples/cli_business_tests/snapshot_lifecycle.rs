//! Public snapshot commands with an isolated synthetic vault, not user data.
//! Uses the CI-provided standalone Runtime; not release artifact acceptance.
use anyhow::{Context, Result};
use respire_app as respire;
use respire::memory::{crypto, search::HashingEmbedder};
use respire::transport::local::LocalStore;
use respire::{MemoryEngine, MemoryEntry, MemoryTransport, SessionKeys};
use serde_json::{json, Value};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use super::support;

pub(super) struct Fixture {
    cli: std::path::PathBuf,
    runtime: Option<std::process::Child>,
    runtime_bin: std::path::PathBuf,
    root: tempfile::TempDir,
    port: u16,
    secret: String,
}
impl Fixture {
    pub(super) fn new() -> Result<Self> {
        let cli = support::cli_bin()?;
        let runtime_bin = std::path::PathBuf::from(
            std::env::var_os("RSRS_RUNTIME_BIN")
                .context("supply RSRS_RUNTIME_BIN with the official standalone Runtime fixture")?,
        ).canonicalize()?;
        anyhow::ensure!(runtime_bin.is_file(), "standalone Runtime fixture missing");
        anyhow::ensure!(
            runtime_bin != cli,
            "CLI shim cannot substitute for the standalone Runtime fixture"
        );
        let root = tempfile::tempdir()?;
        support::install_runtime_fixture(root.path())?;
        let runtime_bin = root.path().join("bin/runtime")
            .join(runtime_bin.parent().and_then(std::path::Path::file_name)
                .context("Runtime fixture version directory missing")?)
            .join(runtime_bin.file_name().context("Runtime fixture binary name missing")?)
            .canonicalize()?;
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        let secret = format!("A3-{}", "07".repeat(18));
        let salt = "08".repeat(16);
        let kek = crypto::derive_rsrs_kek(&secret, &salt)?;
        let (nonce, wrapped) = crypto::wrap_key(&[9; 32], &kek)?;
        std::fs::write(
            root.path().join("session.json"),
            serde_json::to_vec(
                &json!({"user":"snapshot-fixture","vault_version":4,"kdf_salt":salt,"wrapped_urk":format!("{}{wrapped}",crypto::RSRS_PREFIX),"urk_nonce":nonce}),
            )?,
        )?;
        let mut fixture = Self {
            cli,
            runtime: None,
            runtime_bin,
            root,
            port,
            secret,
        };
        let entry = MemoryEntry {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            id: "12345678-1234-4234-8234-123456789abc".into(),
            user: "snapshot-fixture".into(),
            title: "synthetic snapshot".into(),
            content: "synthetic body".into(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            kind: respire::Kind::Context,
            tags: Vec::new(),
            computer: "fixture".into(),
            project: "fixture".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            emotion: 0.0,
            parent_id: String::new(),
            importance: "normal".into(),
            device: "fixture".into(),
            modified_by: "fixture".into(),
        };
        let _defer = respire::core_sdk::defer_indexing();
        let stored = MemoryEngine::seal(
            &SessionKeys::from_urk([9; 32])?,
            &HashingEmbedder::default(),
            &entry,
            "snapshot-fixture",
        )?;
        assert!(LocalStore::open(&fixture.root.path().join("rsrs.db"))?.put(&stored)?);
        fixture.start_runtime()?;
        Ok(fixture)
    }
    pub(super) fn protected_state(&self) -> Result<Value> {
        let health: Value = ureq::get(&format!("http://127.0.0.1:{}/api/health", self.port))
            .set("X-Respire-Health","runtime-v1").timeout(Duration::from_secs(3)).call()?.into_json()?;
        let pid=u32::try_from(health["pid"].as_u64().context("candidate guard PID missing")?)?;
        let process=respire_commands::net_rpc::runtime_process_identity(pid)?;
        anyhow::ensure!(self.runtime.as_ref().is_some_and(|child|child.id()==pid)
            && std::path::Path::new(&process.executable).canonicalize()?==self.runtime_bin
            && std::path::Path::new(health["exe"].as_str().context("candidate guard image missing")?).canonicalize()?==self.runtime_bin
            && std::path::Path::new(health["data_dir"].as_str().context("candidate guard profile missing")?).canonicalize()?==self.root.path().canonicalize()?,
            "candidate coexistence guard identity changed");
        Ok(json!({"pid":health["pid"],"instance_id":health["instance_id"],
            "account_id":health["account_id"],"generation":health["generation"],
            "process_start_id":health["process_start_id"],"image_sha256":health["image_sha256"],
            "rows":LocalStore::open(&self.root.path().join("rsrs.db"))?.all(true)?,
            "client":match std::fs::read(self.root.path().join("client.json")) {
                Ok(bytes)=>Some(bytes),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>None,
                Err(error)=>return Err(error.into())
            }}))
    }
    pub(super) fn copy_test_rows(&self, target: &std::path::Path) -> Result<()> {
        let ownership=respire::lock::LibraryLock::acquire(target,Duration::from_secs(2))?;
        let destination=LocalStore::open(&target.join("rsrs.db"))?;
        for row in LocalStore::open(&self.root.path().join("rsrs.db"))?.all(true)? {
            destination.put(&row)?;
        }
        drop(destination);
        drop(ownership);
        Ok(())
    }
    fn start_runtime(&mut self) -> Result<()> {
        let runtime_stderr = self.root.path().join("runtime-stderr.log");
        let ready_stdout = self.root.path().join("ready-stdout.log");
        let ready_stderr = self.root.path().join("ready-stderr.log");
        let runtime = self.runtime_command()?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&runtime_stderr)?))
            .spawn()?;
        self.runtime = Some(runtime);
        let began = Instant::now();
        loop {
            if let Some(status) = self.runtime.as_mut().context("fixture runtime missing")?.try_wait()? {
                anyhow::bail!("runtime exited during fixture startup: {status}; stderr={}",
                    std::fs::read_to_string(&runtime_stderr)?);
            }
            let mut ready = self.command(&["--client-only", "--json", "status"])
                .stdin(Stdio::null())
                .stdout(Stdio::from(std::fs::File::create(&ready_stdout)?))
                .stderr(Stdio::from(std::fs::File::create(&ready_stderr)?))
                .spawn()?;
            // A single RPC must not outlive the fixture's existing startup budget.
            let status = loop {
                if let Some(status) = ready.try_wait()? {
                    break Some(status);
                }
                if began.elapsed() > Duration::from_secs(15) {
                    let _ = ready.kill();
                    let _ = ready.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            if status.is_some_and(|status| status.success()) {
                return Ok(());
            }
            if began.elapsed() > Duration::from_secs(15) {
                if let Some(mut runtime) = self.runtime.take() {
                    let _ = runtime.kill();
                    let _ = runtime.wait();
                }
                anyhow::bail!("runtime did not become ready; probe={status:?}; stdout={}; stderr={}; runtime stderr={}",
                    std::fs::read_to_string(&ready_stdout)?,
                    std::fs::read_to_string(&ready_stderr)?,
                    std::fs::read_to_string(&runtime_stderr)?);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn stop_runtime(&mut self) -> Result<()> {
        let _ = self.run(&["--json", "--runtime-internal", "--stop"]);
        if let Some(mut runtime) = self.runtime.take() {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match runtime.try_wait()? {
                    Some(status) => {
                        let _ = runtime.wait();
                        if !status.success() {
                            anyhow::bail!("runtime exited unsuccessfully during cleanup: {status}");
                        }
                        return Ok(());
                    }
                    None if Instant::now() >= deadline => {
                        runtime.kill()?;
                        runtime.wait()?;
                        anyhow::bail!("runtime did not stop after its stop acknowledgement");
                    }
                    None => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }
        Ok(())
    }
    fn command(&self, args: &[&str]) -> Command {
        self.command_for(&self.cli, args)
    }
    fn runtime_command(&self) -> Result<Command> {
        let mut command = if self.runtime_bin==self.cli {
            self.command_for(&self.runtime_bin,&["--runtime-internal","--port",&self.port.to_string()])
        } else if support::legacy_cli()?.is_some() {
            self.command_for(&self.runtime_bin,&["--runtime-internal","--runtime-background-internal",
                "--port",&self.port.to_string()])
        } else {
            self.command_for(&self.runtime_bin, &["serve"])
        };
        command.env("RSRS_PASS", &self.secret);
        Ok(command)
    }
    fn command_for(&self, executable: &std::path::Path, args: &[&str]) -> Command {
        let mut command = Command::new(executable);
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|n| {
                n.starts_with("RSRS_") || n.starts_with("ONEMEMORY_") || n.starts_with("RESPIRE_")
            }) {
                command.env_remove(name);
            }
        }
        command
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env("RSRS_DATA_DIR", self.root.path())
            .env("RSRS_BIN_DIR", self.root.path().join("bin"))
            .env("RSRS_RPC_PORT", self.port.to_string())
            .env("RSRS_SUPER", &self.secret)
            .env("RSRS_NO_AUTOSYNC", "1")
            .env("RSRS_UPDATE_CHECK", "0")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_RUNTIME_BIN", &self.runtime_bin)
            .env("RSRS_LANG", "en")
            .env("RSRS_MIRROR", "http://127.0.0.1:9")
            .env("RSRS_CPU_ORT_MIRROR", "http://127.0.0.1:9");
        if self.runtime_bin==self.cli {
            if let Some(models)=std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") {
                command.env("RSRS_M3_DIR",models).env("RSRS_ENGINE","cpu");
            }
        }
        command
    }
    fn run(&self, args: &[&str]) -> Result<(bool, Value)> {
        self.run_feedback(args, None)
    }
    fn run_feedback(&self, args: &[&str], feedback: Option<&str>) -> Result<(bool, Value)> {
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut command = self.command(args);
        if let Some(value) = feedback {
            command.env("RSRS_FEEDBACK", value);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let began = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if began.elapsed() > Duration::from_secs(90) {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("snapshot fixture command timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        stdout.seek(SeekFrom::Start(0))?;
        stderr.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        stdout.read_to_string(&mut text)?;
        let value = serde_json::from_str(&text)
            .context("snapshot command did not emit a single JSON envelope")?;
        Ok((status.success(), value))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // The unique port and profile are ours. Check the profile before lifecycle cleanup.
        if let Ok((true, status)) = self.run(&["--json", "--runtime-internal", "--status"]) {
            if status["summary"]["data_dir"]
                .as_str()
                .is_some_and(|root| std::path::Path::new(root) == self.root.path())
            {
                let _ = self.run(&["--json", "--runtime-internal", "--stop"]);
            }
        }
        let _ = self.stop_runtime();
    }
}

#[test]
fn public_snapshot_restore_reopens_runtime_and_keeps_sync_quarantined() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::public_snapshot_restore_reopens_runtime_and_keeps_sync_quarantined","restore");
    }
    let fixture = Fixture::new()?;
    let (ok, created) = fixture.run(&[
        "--direct",
        "--json",
        "snapshot",
        "create",
        "--note",
        "synthetic regression",
    ])?;
    anyhow::ensure!(ok, "snapshot create failed: {}", created["errors"]);
    let id = created["details"]["id"].as_str().context("snapshot id")?;
    let (ok, listed) = fixture.run(&["--direct", "--json", "snapshot", "list"])?;
    assert!(ok);
    assert_eq!(
        listed["details"]["snapshots"]
            .as_array()
            .context("list")?
            .len(),
        1
    );
    let (_, before) = fixture.run(&["--json", "status"])?;
    let (ok, _) = fixture.run(&[
        "--client-only",
        "--json",
        "snapshot",
        "restore",
        id,
        "--confirm",
    ])?;
    assert!(!ok, "sandbox restored database");
    let (ok, restored) = fixture.run(&["--json", "snapshot", "restore", id, "--confirm"])?;
    anyhow::ensure!(ok, "snapshot restore failed: {}", restored["errors"]);
    assert_eq!(restored["summary"]["sync_quarantined"], true);
    assert_eq!(restored["summary"]["cloud_rollback"], false);
    assert!(restored["details"]["before_restore"]
        .as_str()
        .is_some_and(|v| v != id));
    let (ok, status) = fixture.run(&["--client-only", "--json", "status"])?;
    assert!(ok);
    assert_eq!(
        status["summary"]["local_alive"],
        before["summary"]["local_alive"]
    );
    let (ok, failed) = fixture.run(&["--client-only", "--json", "sync"])?;
    assert!(!ok);
    assert!(failed["errors"].to_string().contains("quarantined"));
    let (ok, _) = fixture.run(&[
        "--client-only",
        "--json",
        "snapshot",
        "resume-sync",
        "--reviewed-revision",
        "stale",
    ])?;
    assert!(!ok);
    let (ok, revision) = fixture.run(&["--client-only", "--json", "memory-revision"])?;
    assert!(ok);
    let token = revision["summary"]["revision"]
        .as_str()
        .context("revision token")?;
    let (ok, resumed) = fixture.run(&[
        "--client-only",
        "--json",
        "snapshot",
        "resume-sync",
        "--reviewed-revision",
        token,
    ])?;
    anyhow::ensure!(ok, "resume failed: {}", resumed["errors"]);
    assert_eq!(resumed["details"]["sync_quarantined"], false);
    Ok(())
}

fn wait_bounded(mut child: std::process::Child) -> Result<std::process::Output> {
    let began = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if began.elapsed() > Duration::from_secs(90) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("owned snapshot subprocess timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn concurrent_host_recover_cannot_interleave_with_restore_commit() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::concurrent_host_recover_cannot_interleave_with_restore_commit","concurrent_recover");
    }
    let fixture = Fixture::new()?;
    let (ok, created) = fixture.run(&["--direct", "--json", "snapshot", "create"])?;
    assert!(ok);
    let id = created["details"]["id"].as_str().context("snapshot id")?;
    // Bind the result to pre-dispatch inputs; the CLI generates the operation ID.
    use sha2::{Digest, Sha256};
    let profile = fixture.root.path().canonicalize()?;
    let account_id = format!("{:x}", Sha256::digest(b"snapshot-fixture"));
    let health: respire_commands::net_rpc::Health =
        ureq::get(&format!("http://127.0.0.1:{}/api/health", fixture.port))
            .timeout(Duration::from_secs(5))
            .call()?
            .into_json()?;
    assert_eq!(health.pid, fixture.runtime.as_ref().context("fixture runtime missing")?.id());
    assert_eq!(std::path::Path::new(&health.installation_root).canonicalize()?, profile);
    assert_eq!(std::path::Path::new(&health.data_dir).canonicalize()?, profile);
    assert_eq!(health.account_id, account_id);
    assert!(health.store_ready && health.generation > 0);
    let sync_generation = usize::try_from(health.generation)?;
    let expected_fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&(
        "snapshot-restore-v1", &profile, &profile, Some(&account_id), sync_generation, id,
    ))?));
    let primary = fixture
        .command(&["--json", "snapshot", "restore", id, "--confirm"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let competing = fixture
        .command(&["--json", "snapshot", "recover", "--confirm"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let primary = wait_bounded(primary)?;
    let competing = wait_bounded(competing)?;
    anyhow::ensure!(
        primary.status.success(),
        "restore failed under lifecycle contention: {}",
        String::from_utf8_lossy(&primary.stdout)
    );
    assert!(
        !competing.status.success(),
        "recover interleaved with the successful restore"
    );
    let (ok, status) = fixture.run(&["--client-only", "--json", "status"])?;
    assert!(ok);
    assert_eq!(status["summary"]["local_alive"], 1);
    assert!(!fixture
        .root
        .path()
        .join(".snapshot-maintenance.json")
        .exists());
    respire_app::snapshot::ensure_no_incomplete_restore(fixture.root.path())?;
    let restored: Value = serde_json::from_slice(&primary.stdout)?;
    let operation_id = restored["details"]["operation"]["operation_id"]
        .as_str()
        .context("successful restore operation ID missing")?;
    let parsed_operation = uuid::Uuid::parse_str(operation_id)?;
    assert_eq!(parsed_operation.get_version_num(), 4);
    assert_eq!(parsed_operation.to_string(), operation_id);
    assert_eq!(
        restored["details"]["operation"]["request_fingerprint"].as_str(),
        Some(expected_fingerprint.as_str()),
    );
    let operation = respire_app::snapshot::SnapshotOperation {
        operation_id: operation_id.to_owned(),
        request_fingerprint: expected_fingerprint,
    };
    let terminal = respire_app::snapshot::operation_status(
        fixture.root.path(),
        "snapshot-fixture",
        &operation,
    )?
    .context("successful restore terminal receipt missing")?;
    assert_eq!(terminal.receipt.operation.as_ref(), Some(&operation));
    assert_eq!(terminal.receipt.restored, id);
    assert_eq!(terminal.phase, "released");
    assert!(!terminal.cleanup_pending);
    Ok(())
}


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/snapshot_lifecycle/parts/staged_store_open_failure_cannot_pass_boot_readiness_and_is_recoverable.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/snapshot_lifecycle/parts/public_empty_recall_returns_its_query_log_identity.rs"));
