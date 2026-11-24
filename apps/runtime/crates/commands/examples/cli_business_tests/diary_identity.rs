//! Actual client-only commands against a fresh synthetic runtime and local library.
//! No real account, keyring, model download, or remote memory service is used.
//! Supply RSRS_RUNTIME_BIN or a neighboring standalone Runtime bundle.
//! RSRS_CORE_TEST_MODEL_DIR must contain the separately verified real CPU model.
//! These synthetic business regressions alone are not standalone G1 evidence.
use anyhow::Context;
use respire_app as respire;
use super::support;
use respire::memory::{crypto, search::HashingEmbedder};
use respire::transport::local::LocalStore;
use respire::{MemoryEngine, MemoryEntry, MemoryTransport, SessionKeys, StoredMemory};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::Duration;

struct Fixture {
    cli: PathBuf,
    business_cli: Option<PathBuf>,
    home: tempfile::TempDir,
    port: u16,
    secret: String,
    keys: SessionKeys,
    child: Option<Child>,
    runtime: PathBuf,
    models: PathBuf,
    runtime_owner: RefCell<Option<Value>>,
    extra_env: Vec<(&'static str, String)>,
}

impl Fixture {
    /// Create an isolated local vault containing only deterministic synthetic keys.
    fn new() -> anyhow::Result<Self> {
        let cli = support::cli_bin()?;
        let runtime = support::runtime_bin()?;
        anyhow::ensure!(
            runtime.is_file(),
            "standalone Runtime fixture missing at {}; supply RSRS_RUNTIME_BIN with its dependency bundle",
            runtime.display()
        );
        let runtime = runtime.canonicalize()?;
        anyhow::ensure!(
            runtime != cli,
            "CLI shim cannot substitute for the standalone Runtime fixture"
        );
        let business_cli = match (
            std::env::var_os("RSRS_TEST_STABLE_DIARY_SUITE"),
            std::env::var_os("RSRS_TEST_STABLE_CLI_BIN"),
        ) {
            (None, None) => None,
            (Some(suite), Some(binary)) if suite == "stable-diary-remote" => {
                let binary = PathBuf::from(binary).canonicalize()?;
                anyhow::ensure!(binary.is_file() && binary != cli && binary != runtime,
                    "stable business CLI must be separate from the manager and Runtime");
                Some(binary)
            }
            _ => anyhow::bail!("stable business CLI requires the explicit stable-diary-remote suite"),
        };
        let models = PathBuf::from(
            std::env::var_os("RSRS_CORE_TEST_MODEL_DIR")
                .context("supply RSRS_CORE_TEST_MODEL_DIR with the verified real CPU model fixture")?,
        )
        .canonicalize()?;
        for file in [
            "tokenizer.json",
            "onnx/model_quantized.onnx",
            "onnx/model_quantized.cpu-v1.ort",
        ] {
            anyhow::ensure!(models.join(file).is_file(), "model fixture missing {file}");
        }
        let home = tempfile::tempdir()?;
        #[cfg(target_os = "macos")]
        if let Some(keychain) = std::env::var_os("RSRS_TEST_NATIVE_KEYCHAIN") {
            let keychain = PathBuf::from(keychain);
            anyhow::ensure!(keychain.is_absolute() && keychain.is_file(),
                "synthetic native keychain is missing");
            // Security preferences belong to HOME; keep this fixture's data isolated.
            std::fs::create_dir_all(home.path().join("Library/Preferences"))?;
            for operation in ["list-keychains", "default-keychain"] {
                let configured = Command::new("/usr/bin/security")
                    .args([operation, "-d", "user", "-s"]).arg(&keychain)
                    .env("HOME", home.path()).env("USERPROFILE", home.path()).output()?;
                anyhow::ensure!(configured.status.success(),
                    "synthetic fixture keychain configuration failed");
                let selected = Command::new("/usr/bin/security")
                    .args([operation, "-d", "user"])
                    .env("HOME", home.path()).env("USERPROFILE", home.path()).output()?;
                anyhow::ensure!(selected.status.success()
                    && String::from_utf8(selected.stdout)?.trim() == format!("\"{}\"", keychain.display()),
                    "synthetic fixture keychain readback failed");
            }
        }
        std::fs::create_dir(home.path().join("data"))?;
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        let secret = format!("A3-{}", "07".repeat(18));
        let salt = "08".repeat(16);
        let urk = [9; 32];
        let kek = crypto::derive_rsrs_kek(&secret, &salt)?;
        let (nonce, wrapped) = crypto::wrap_key(&urk, &kek)?;
        std::fs::write(
            home.path().join("data/session.json"),
            serde_json::to_vec(&json!({
                "user":"synthetic-diary", "pass":"synthetic-unused", "vault_version":4,
                "kdf_salt":salt, "wrapped_urk":format!("{}{wrapped}", crypto::RSRS_PREFIX),
                "urk_nonce":nonce
            }))?,
        )?;
        std::fs::write(
            home.path().join("data/agent.json"),
            br#"{"diary_mode":"concise"}"#,
        )?;
        Ok(Self {
            cli,
            business_cli,
            home,
            port,
            secret,
            keys: SessionKeys::from_urk(urk)?,
            child: None,
            runtime,
            models,
            runtime_owner: RefCell::new(None),
            extra_env: Vec::new(),
        })
    }

    /// Build a command confined to this profile, port, and disabled remote services.
    fn command(&self, args: &[&str]) -> Command {
        self.command_for(&self.cli, args)
    }

    fn command_for(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|name| {
                name.starts_with("RSRS_")
                    || name.starts_with("ONEMEMORY_")
                    || name.starts_with("RESPIRE_")
            }) {
                command.env_remove(name);
            }
        }
        command
            .args(args)
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_DATA_HOME", self.home.path().join("share"))
            .env("RSRS_DATA_DIR", self.home.path().join("data"))
            .env("RSRS_RPC_PORT", self.port.to_string())
            .env("RSRS_USER", "synthetic-diary")
            .env("RSRS_SUPER", &self.secret)
            .env("RSRS_NO_AUTOSYNC", "1")
            .env("RSRS_UPDATE_CHECK", "0")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_RUNTIME_BIN", &self.runtime)
            .env("RSRS_M3_DIR", &self.models)
            .env("RSRS_RPC_PARALLELISM", "2")
            .env("RSRS_LANG", "en")
            .env("TZ", "UTC")
            // Refuse background model downloads at a loopback-only destination.
            .env("RSRS_MIRROR", "http://127.0.0.1:9")
            .env("RSRS_CPU_ORT_MIRROR", "http://127.0.0.1:9");
        command.envs(self.extra_env.iter().map(|(name, value)| (*name, value)));
        command
    }

    fn business_command(&self, args: &[&str]) -> anyhow::Result<Command> {
        let Some(binary) = self.business_cli.as_deref() else {
            return Ok(self.command(args));
        };
        anyhow::ensure!(args.contains(&"--client-only")
            && !args.iter().any(|arg| *arg == "--direct" || arg.starts_with("--direct=")),
            "stable business calls must be client-only and cannot use direct execution");
        anyhow::ensure!(self.owns_runtime()?, "candidate Runtime owner changed before stable business call");
        let mut command = self.command_for(binary, args);
        command.env("RSRS_CLIENT_ONLY", "1").env("RSRS_NO_AUTOSTART", "1");
        Ok(command)
    }

    fn verify_business_owner(&self) -> anyhow::Result<()> {
        if self.business_cli.is_some() {
            anyhow::ensure!(self.owns_runtime()?, "candidate Runtime owner changed after stable business call");
        }
        Ok(())
    }

    fn business_output(&self, args: &[&str]) -> anyhow::Result<Output> {
        let output = self.business_command(args)?.output();
        self.verify_business_owner()?;
        Ok(output?)
    }

    /// Start an owned runtime and verify its process and profile before use.
    fn start(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.child.is_none(),
            "fixture already has a runtime launcher"
        );
        *self.runtime_owner.borrow_mut() = None;
        let mut command = if self.business_cli.is_some() {
            // This is the existing legacy host startup boundary. A normal
            // authorized `serve` listener intentionally requires modern scope.
            self.command_for(&self.runtime, &[
                "--runtime-internal", "--runtime-background-internal",
                "--port", &self.port.to_string(),
            ])
        } else {
            self.command(&["runtime", "run"])
        };
        self.child = Some(
            command
                .stdout(std::fs::File::create(
                    self.home.path().join("runtime.stdout"),
                )?)
                .stderr(std::fs::File::create(
                    self.home.path().join("runtime.stderr"),
                )?)
                .spawn()?,
        );
        for _ in 0..100 {
            if let Some(status) = self
                .child
                .as_mut()
                .context("synthetic runtime child missing")?
                .try_wait()?
            {
                anyhow::bail!("synthetic runtime exited: {status}");
            }
            if self.owns_runtime()? {
                if self.business_cli.is_some() {
                    self.verify_legacy_projection()?;
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("synthetic runtime did not become ready")
    }

    /// Prove this listener projects the historical unscoped wire request.
    /// The probe is issued by the harness; subsequent business calls use the old binary.
    fn verify_legacy_projection(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.owns_runtime()?, "legacy probe requires the owned candidate");
        let id = uuid::Uuid::new_v4().to_string();
        let response: Value = ureq::post(&format!("http://127.0.0.1:{}/api/rpc", self.port))
            .timeout(Duration::from_secs(30))
            .send_json(json!({"v": 1, "id": id, "method": "cli.exec",
                "args": ["--json", "status"], "lifecycle": null}))?
            .into_json()?;
        let object = response.as_object().context("legacy probe response is not an object")?;
        anyhow::ensure!(response["v"] == 1 && response["id"] == id
            && response["ok"] == true && response["exit"] == 0
            && ["scope", "operation_id", "idempotency_key"].iter()
                .all(|key| !object.contains_key(*key)),
            "legacy probe did not receive the original unscoped projection");
        anyhow::ensure!(self.owns_runtime()?, "candidate actor changed after legacy probe");
        eprintln!("stable-diary-legacy-probe {}", json!({
            "probe_without_scope": true, "legacy_projection_verified": true,
            "same_actor_before_after": true, "candidate_process_verified": true,
        }));
        Ok(())
    }

    /// Reject port collisions instead of submitting commands to another runtime.
    fn owns_runtime(&self) -> anyhow::Result<bool> {
        let Some(child) = self.child.as_ref() else {
            return Ok(false);
        };
        let Ok(response) = ureq::get(&format!("http://127.0.0.1:{}/api/health", self.port))
            .timeout(Duration::from_millis(100))
            .call()
        else {
            return Ok(false);
        };
        let health: Value = response.into_json()?;
        if health["store_ready"] != true {
            return Ok(false);
        }
        let profile = self.home.path().join("data").canonicalize()?;
        let path_matches = |field: &str, expected: &Path| -> anyhow::Result<bool> {
            let actual = health[field]
                .as_str()
                .context("runtime health path missing")?;
            Ok(Path::new(actual).canonicalize()? == expected)
        };
        anyhow::ensure!(
            health["pid"]
                .as_u64()
                .is_some_and(|pid| pid > 0 && if self.business_cli.is_some() {
                    pid == u64::from(child.id())
                } else {
                    pid != u64::from(child.id())
                })
                && path_matches("exe", &self.runtime)?
                && path_matches("image_path", &self.runtime)?
                && path_matches("data_dir", &profile)?
                && path_matches("installation_root", &profile)?
                && health["generation"]
                    .as_u64()
                    .is_some_and(|generation| generation > 0)
                && health["account_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
                && health["process_start_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()),
            "fixture port belongs to a different process or profile; no command submitted"
        );
        let instance = health["instance_id"]
            .as_str()
            .context("runtime instance missing")?;
        let nonce = uuid::Uuid::parse_str(instance)?;
        anyhow::ensure!(
            nonce.get_version_num() == 4 && nonce.to_string() == instance,
            "runtime launch instance is invalid"
        );
        let prefix = if cfg!(windows) {
            "windows-filetime-"
        } else if cfg!(target_os = "linux") {
            "linux-starttime-"
        } else {
            "darwin-lstart-"
        };
        if self.business_cli.is_some() {
            let process = respire_commands::net_rpc::runtime_process_identity(child.id())?;
            anyhow::ensure!(Path::new(&process.executable).canonicalize()? == self.runtime
                && health["process_start_id"] == format!("{prefix}{}", process.started_at),
                "legacy runtime does not match the directly owned process and birth");
        } else {
            let claim: Value = serde_json::from_slice(&std::fs::read(
                profile.join("runtime/autostart/launch").join(format!("launch-{instance}.claimed")),
            )?)?;
            let birth = claim["process_start_id"].as_str().context("launch claim birth missing")?;
            anyhow::ensure!(claim["pid"] == health["pid"]
                && health["process_start_id"] == format!("{prefix}{birth}"),
                "runtime does not match the consumed launch grant");
        }
        let identity = json!({
            "pid": health["pid"], "instance_id": health["instance_id"],
            "process_start_id": health["process_start_id"], "image_sha256": health["image_sha256"],
            "installation_id": health["installation_id"], "account_id": health["account_id"],
            "generation": health["generation"],
        });
        let mut owner = self.runtime_owner.borrow_mut();
        if let Some(expected) = owner.as_ref() {
            anyhow::ensure!(
                *expected == identity,
                "fixture runtime owner changed; no command submitted"
            );
        } else {
            *owner = Some(identity);
        }
        Ok(true)
    }

    /// Execute a public client-only command after rechecking runtime ownership.
    fn run(&self, args: &[&str]) -> anyhow::Result<Value> {
        anyhow::ensure!(self.owns_runtime()?, "synthetic runtime is not ready");
        let mut all = vec!["--client-only", "--json"];
        all.extend_from_slice(args);
        Self::result(self.business_output(&all)?)
    }

    /// Require a successful command and decode its public JSON envelope.
    fn result(output: Output) -> anyhow::Result<Value> {
        let value: Value = serde_json::from_slice(&output.stdout)?;
        anyhow::ensure!(
            output.status.success(),
            "synthetic command failed: {value}; {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(value)
    }

    /// Append one trivial event using the public remember command.
    fn remember(&self, content: &str) -> anyhow::Result<Value> {
        self.run(&["remember", content, "--importance", "trivial"])
    }

    /// Read the full saved entry using its public command contract.
    fn show(&self, id: &str) -> anyhow::Result<Value> {
        Ok(self.run(&["show", id])?["details"]["entry"].clone())
    }

    /// Seed before startup; assertions against a running owner stay read-only.
    /// Reopening with migrations/WAL setup races the runtime's own startup writes
    /// and can also change the data whose immutability these tests are checking.
    fn store(&self) -> anyhow::Result<LocalStore> {
        let path = self.home.path().join("data/rsrs.db");
        if self.child.is_some() {
            LocalStore::open_read_only(&path)
        } else {
            LocalStore::open(&path)
        }
    }

    /// Seed a synthetic entry whose payload author and wire owner agree.
    fn seed(&self, entry: &MemoryEntry, deleted: bool) -> anyhow::Result<StoredMemory> {
        self.seed_with_owner(entry, deleted, &entry.user)
    }

    /// Seed separate logical authors and routing owners without model downloads.
    fn seed_with_owner(
        &self,
        entry: &MemoryEntry,
        deleted: bool,
        wire_user: &str,
    ) -> anyhow::Result<StoredMemory> {
        let _defer = respire::core_sdk::defer_indexing();
        let mut stored =
            MemoryEngine::seal(&self.keys, &HashingEmbedder::default(), entry, wire_user)?;
        stored.deleted = deleted;
        anyhow::ensure!(self.store()?.put(&stored)?, "fixture seed was not stored");
        Ok(stored)
    }

    /// Verify rejected identity cannot mutate records/outbox or disclose fixture secrets.
    fn assert_author_rejection(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.owns_runtime()?, "synthetic runtime is not ready");
        let before = serde_json::to_value(self.store()?.all(true)?)?;
        let boundary = self.store()?.outgoing_boundary()?;
        let outgoing = serde_json::to_value(self.store()?.outgoing()?)?;
        let output = self
            .business_output(&[
                "--client-only",
                "--json",
                "remember",
                "rejected-private-event",
                "--importance",
                "trivial",
            ])?;
        assert!(!output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(value["errors"]
            .as_array()
            .context("missing error array")?
            .iter()
            .any(|error| error.as_str().is_some_and(
                |message| message.contains("daily track author does not match this profile")
            )));
        assert!(
            before == serde_json::to_value(self.store()?.all(true)?)?,
            "rejected append mutated rows"
        );
        assert_eq!(
            boundary,
            self.store()?.outgoing_boundary()?,
            "rejected append queued a sync operation"
        );
        assert!(
            outgoing == serde_json::to_value(self.store()?.outgoing()?)?,
            "rejected append changed pending sync bytes"
        );
        let emitted = format!(
            "{}{}{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            std::fs::read_to_string(self.home.path().join("runtime.stdout"))?,
            std::fs::read_to_string(self.home.path().join("runtime.stderr"))?
        );
        for private in [
            &self.secret,
            "foreign-private-author",
            "foreign-private-event",
            "rejected-private-event",
        ] {
            assert!(
                !emitted.contains(private),
                "rejection exposed private fixture data"
            );
        }
        for (_, private) in &self.extra_env {
            assert!(
                !emitted.contains(private),
                "rejection exposed synthetic fallback key material"
            );
        }
        Ok(())
    }

    /// Stop only the verified runtime or the exact child process we spawned.
    fn stop(&mut self) {
        let owned = self.owns_runtime().unwrap_or(false);
        if let Some(mut child) = self.child.take() {
            if owned {
                let _ = self.command(&["runtime", "stop"]).output();
            }
            for _ in 0..100 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Fixture {
    /// Reap the fixture runtime even when a test exits early.
    fn drop(&mut self) {
        self.stop();
    }
}

/// Match the child runtime's calendar-day boundary on the current platform.
fn day() -> String {
    // Child TZ=UTC applies on Unix; Windows uses the OS local timezone.
    #[cfg(windows)]
    let now = chrono::Local::now();
    #[cfg(not(windows))]
    let now = chrono::Utc::now();
    now.format("%Y-%m-%d").to_string()
}
/// Reproduce the public daily identity namespace for synthetic authors and dates.
fn track_id(user: &str, day: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("activity-track/{user}/{day}").as_bytes(),
    )
    .to_string()
}
/// Build synthetic metadata whose preservation can be checked after appends.
fn entry(id: &str, user: &str, title: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        metadata: Default::default(),
        unknown_fields: Default::default(),
        id: id.into(),
        user: user.into(),
        title: title.into(),
        content: content.into(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
        kind: respire::Kind::Context,
        tags: vec!["preserve-tag".into()],
        computer: "fixture-computer".into(),
        project: "fixture-project".into(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        emotion: 0.75,
        parent_id: "fixture-parent".into(),
        importance: "normal".into(),
        device: "fixture-device".into(),
        modified_by: "fixture-device".into(),
    }
}


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/explicit_titles_preserve_entries_and_leave_the_daily_track_unchanged.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/concurrent_same_day_client_commands_preserve_each_successful_addition_once.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/stop_native_worker_identity.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/native_stop_waits_and_explicit_force_reaps_real_core_without_replay.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/stop_native_registered_held_write.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/diary_identity/parts/stop_native_client2_owned_held_write.rs"));
