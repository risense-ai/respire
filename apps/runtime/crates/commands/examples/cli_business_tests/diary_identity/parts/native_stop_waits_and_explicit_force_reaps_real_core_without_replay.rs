#[test]
fn native_stop_waits_and_explicit_force_reaps_real_core_without_replay() -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    enum OwnedStopMode { Registered, Client2Owned }
    let owned_mode = match std::env::var_os("RSRS_TEST_NATIVE_OWNED_STOP") {
        None => None,
        Some(mode) if mode == "registered" => Some(OwnedStopMode::Registered),
        Some(mode) if mode == "client2-owned" => Some(OwnedStopMode::Client2Owned),
        Some(_) => anyhow::bail!("unsupported native owned-stop fixture mode"),
    };
    match owned_mode {
        Some(OwnedStopMode::Registered) => return stop_native_registered_held_write(),
        Some(OwnedStopMode::Client2Owned) => return stop_native_client2_owned_held_write(),
        None => {},
    }
    stop_native_commits_accepted_encrypted_write_after_drain()?;
    for forced in [false, true] {
        let checkpoint = |stage: &str| eprintln!("NATIVE_STOP_CHECKPOINT mode={} stage={stage}",
            if forced { "forced" } else { "normal" });
        checkpoint("start");
        let mut fixture = Fixture::new()?;
        checkpoint("fixture_ready");
        let mut memory = entry("native-stop-source", "synthetic-diary", "Native classification source", "Synthetic native stop evidence");
        memory.parent_id.clear();
        memory.importance = "important".into();
        fixture.seed(&memory, false)?;
        checkpoint("memory_seeded");
        let before = serde_json::to_value(fixture.store()?.all(true)?)?;
        let health = fixture.start_standalone_for_stop()?;
        checkpoint("runtime_ready");
        assert!(health["managed_id"].is_null(), "standalone case must not bypass a lifecycle coordinator");
        let mut provider = HeldClassifyProvider::new()?;
        let rpc_url = format!("http://127.0.0.1:{}/api/rpc", fixture.port);
        let stop_url = format!("http://127.0.0.1:{}/api/runtime/stop", fixture.port);
        let scope = json!({"account_id":health["account_id"],"instance_id":health["instance_id"],"generation":health["generation"]});
        let business_operation = format!("native-stop-classify-{forced}");
        let request = json!({"v":1,"id":"native-stop-original","method":"cli.exec",
            "operation_id":business_operation,"idempotency_key":business_operation,"scope":scope,
            "args":["--json","classify","--backend","jev","--api-base",provider.endpoint,"--limit","1","--samples","1"]});
        let (url, original) = (rpc_url.clone(), request.clone());
        let business = std::thread::spawn(move || stop_native_post(&url, &original));
        provider.received.recv_timeout(Duration::from_secs(30))
            .context("actual SDK classification did not reach the loopback provider")?;
        checkpoint("provider_received");
        let io_started = std::time::Instant::now();
        let active = fixture.standalone_stop_health()?;
        assert!(active["control"]["admitted"].as_array().context("admission list missing")?.iter()
            .any(|item| item["operation_id"] == business_operation));
        let worker = &active["inference"];
        assert!(worker["worker_pending_calls"].as_u64().is_some_and(|count| count > 0));
        assert!(worker["worker_call_deadline_secs"].as_u64().is_some_and(|seconds| seconds >= 30),
            "a short Core fault-test deadline must not substitute for explicit stop force");
        assert!(worker["worker_host_callbacks_active"].as_u64().is_some_and(|count| count > 0));
        assert_eq!(worker["worker_retired"], false);
        assert_eq!(worker["worker_reaped"], false);
        let worker_pid = u32::try_from(worker["worker_pid"].as_u64().context("actual Core worker PID missing")?)?;
        let worker_identity = stop_native_worker_identity(worker_pid)?.context("actual Core worker was not live")?;
        assert_eq!(Path::new(worker_identity["image"].as_str().context("worker OS image missing")?).canonicalize()?, fixture.runtime);
        if let Some(parent) = worker_identity.get("parent") { assert_eq!(*parent, health["pid"]); }
        fixture.assert_standalone_stop_lock_held()?;
        checkpoint("active_verified");
        std::thread::sleep(Duration::from_millis(4200));
        assert_eq!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
        assert!(!business.is_finished(), "real business completed before stop");
        let business_before_stop_ms = io_started.elapsed().as_millis();
        assert!(business_before_stop_ms > 4000);
        let identity = json!({"installation_id":health["installation_id"],"instance_id":health["instance_id"],
            "account_id":health["account_id"],"generation":health["generation"]});
        let (status, accepted) = stop_native_post(&stop_url, &identity)?;
        assert_eq!(status, 200);
        assert_eq!(accepted["state"], "accepted");
        assert!(accepted["deadline_ms"].is_null() && accepted["timeout_state"].is_null());
        let operation = accepted["operation_id"].clone();
        anyhow::ensure!(operation.as_str().is_some_and(|id| !id.is_empty()), "stop operation missing");
        checkpoint("stop_accepted");
        let accepted_at = std::time::Instant::now();
        let mut force_requests = Vec::new();
        let held_health;
        if forced {
            let mut force = identity.clone();
            force["force"] = json!(true);
            force["operation_id"] = operation.clone();
            for key in ["installation_id", "instance_id", "account_id", "generation", "operation_id"] {
                let mut stale = force.clone();
                stale[key] = if key == "generation" { json!(0) } else { json!("stale-stop-target") };
                assert_eq!(stop_native_post(&stop_url, &stale)?.0, 409);
                assert!(fixture.standalone_stop_health()?["inference"]["worker_retired"] == false);
                assert!(stop_native_post(&stop_url, &identity)?.1["deadline_ms"].is_null());
            }
            let mut missing = force.clone();
            missing.as_object_mut().context("force body missing")?.remove("operation_id");
            assert_eq!(stop_native_post(&stop_url, &missing)?.0, 409);
            assert!(stop_native_post(&stop_url, &identity)?.1["deadline_ms"].is_null());
            checkpoint("force_identities_rejected");
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut previous_ms = 4000;
            let mut request_ids = std::collections::HashSet::new();
            loop {
                let (status, response) = stop_native_post(&stop_url, &force)?;
                assert_eq!(status, 200);
                assert_eq!(response["operation_id"], operation);
                assert_eq!(response["state"], "accepted");
                assert_eq!(response["timeout_state"], "forced_timeout");
                assert_ne!(response["request_id"], accepted["request_id"]);
                let remaining = response["deadline_ms"].as_u64().context("force deadline missing")?;
                assert!(remaining <= previous_ms, "repeated force extended the grace period");
                previous_ms = remaining;
                assert!(request_ids.insert(response["request_id"].as_str().context("force request ID missing")?.to_owned()));
                force_requests.push(response);
                let observed = fixture.standalone_stop_health()?;
                let retired = &observed["inference"]["retired_worker"];
                if retired["worker_reaped"] == true {
                    assert_eq!(retired["worker_pid"], worker_pid);
                    assert_eq!(retired["worker_retired"], true);
                    assert!(observed["inference"]["host_transport_callbacks_active"].as_u64().is_some_and(|count| count > 0));
                    assert_ne!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
                    held_health = observed;
                    checkpoint("worker_reaped");
                    break;
                }
                anyhow::ensure!(std::time::Instant::now() < deadline, "repeated force prevented actual worker retirement");
                std::thread::sleep(Duration::from_millis(250));
            }
        } else {
            std::thread::sleep(Duration::from_millis(4200));
            let pending = fixture.standalone_stop_health()?;
            assert_eq!(pending["control"]["state"], "draining");
            assert_eq!(pending["inference"]["worker_pid"], worker_pid);
            assert_eq!(pending["inference"]["worker_retired"], false);
            assert_eq!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
            assert!(accepted_at.elapsed() > Duration::from_secs(4));
            held_health = pending;
            checkpoint("normal_waited");
        }
        let business_held_after_stop_ms = accepted_at.elapsed().as_millis();
        assert!(!business.is_finished(), "host callback was silently cancelled");
        fixture.assert_standalone_stop_lock_held()?;
        let mut replay = request.clone();
        replay["id"] = json!("native-stop-same-key-retry");
        let (_, replayed) = stop_native_post(&rpc_url, &replay)?;
        assert_eq!(replayed["code"], "request_in_progress");
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        checkpoint("same_key_checked");
        provider.release.store(true, Ordering::Release);
        let (_, result) = business.join().map_err(|_| anyhow::anyhow!("native business client panicked"))??;
        assert_eq!(result["id"], request["id"]);
        assert_eq!(result["operation_id"], business_operation);
        if forced { assert_eq!(result["ok"], false); }
        checkpoint("business_completed");
        let receipt = fixture.finish_standalone_stop(&health, &operation, forced)?;
        checkpoint("runtime_stopped");
        if forced {
            let unfinished = receipt["unfinished"].as_array().context("forced unfinished list missing")?;
            assert_eq!(unfinished.iter().filter(|item| item["operation_id"] == business_operation).count(), 1);
            assert!(unfinished.iter().any(|item| item["operation_id"] == business_operation && item["result"] == "unknown"));
            assert!(unfinished.iter().all(|item| item["result"] == "unknown" || item["result"] == "aborted"));
        } else { assert_eq!(receipt["unfinished"], json!([])); }
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        assert_eq!(serde_json::to_value(fixture.store()?.all(true)?)?, before, "classification changed stored memory");
        checkpoint("store_unchanged");
        let restarted = fixture.start_standalone_for_stop()?;
        checkpoint("restarted");
        assert_ne!(restarted["instance_id"], health["instance_id"]);
        replay["id"] = json!("native-stop-original-scope-after-restart");
        let (_, stale) = stop_native_post(&rpc_url, &replay)?;
        assert_eq!(stale["code"], "scope_mismatch");
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        checkpoint("stale_rejected");
        let restart_identity = json!({"installation_id":restarted["installation_id"],"instance_id":restarted["instance_id"],
            "account_id":restarted["account_id"],"generation":restarted["generation"]});
        let (_, stopped) = stop_native_post(&stop_url, &restart_identity)?;
        let restarted_receipt = fixture.finish_standalone_stop(&restarted, &stopped["operation_id"], false)?;
        checkpoint("restart_stopped");
        provider.finish()?;
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        checkpoint("complete");
        eprintln!("NATIVE_STOP_RAW {}", json!({"schema":1,"evidence_class":"native-raw","gate":"not-evaluated",
            "startup_mode":"standalone","forced":forced,"passed":true,"runtime_identity":health,
            "worker_identity":worker_identity,"active_health":active,"held_health":held_health,
            "business_before_stop_ms":business_before_stop_ms,"business_held_after_stop_ms":business_held_after_stop_ms,
            "accepted":accepted,"force_requests":force_requests,"business_result":result,
            "final_receipt":receipt,"provider_http_status":401,"provider_requests":provider.calls.load(Ordering::Acquire),
            "same_key_result":replayed,"stale_scope_result":stale,"restarted_receipt":restarted_receipt}));
    }
    Ok(())
}
// Explicit isolated native acceptance only: this installs a real user service.
// It is intentionally absent from ordinary cargo-test runs and G1 certificates.
fn owned_stop_checkpoint(startup_mode: &str, case: &str, stage: &str) {
    eprintln!("OWNED_STOP_CHECKPOINT {}", json!({"startup_mode":startup_mode,"case":case,"stage":stage}));
}

fn registered_restart_diagnostics(root: &Path) {
    let path = root.join("runtime/autostart/events.jsonl");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            let stage = if error.kind() == std::io::ErrorKind::NotFound { "event_file_missing" } else { "event_file_unreadable" };
            eprintln!("REGISTERED_RESTART_EVENT stage={stage}");
            return;
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        eprintln!("REGISTERED_RESTART_EVENT stage=event_file_invalid");
        return;
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("REGISTERED_RESTART_EVENT stage=event_file_unreadable");
            return;
        }
    };
    let mut stages = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(_) => {
                eprintln!("REGISTERED_RESTART_EVENT stage=event_file_invalid");
                return;
            }
        };
        if let Some(stage) = value.get("event").and_then(Value::as_str) {
            if matches!(stage, "restart_probe" | "restart_managed" | "restart_identity" | "restart_stop" | "restart_start" | "restart_ready") {
                stages.push(stage.to_owned());
            }
        }
    }
    if stages.is_empty() {
        eprintln!("REGISTERED_RESTART_EVENT stage=no_restart_event");
    }
    for stage in &stages {
        eprintln!("REGISTERED_RESTART_EVENT stage={stage}");
    }
}

fn assert_owned_stop_lock_held(root: &Path, backend_pid: u32, backend_identity: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(backend_pid != std::process::id(), "lock check must run outside the backend process");
    anyhow::ensure!(stop_native_worker_identity(backend_pid)?.as_ref() == Some(backend_identity),
        "backend identity changed before library lock observation");
    match respire::lock::LibraryLock::acquire(root, Duration::from_millis(100)) {
        Ok(_) => anyhow::bail!("owned Runtime released its library lock before business cleanup"),
        Err(error) => anyhow::ensure!(error.to_string().starts_with("memory library still locked after "),
            "library lock check failed without proving contention: {error:#}"),
    }
    anyhow::ensure!(stop_native_worker_identity(backend_pid)?.as_ref() == Some(backend_identity),
        "backend identity changed during library lock observation");
    Ok(())
}

struct RegisteredStopFixture {
    fixture: Option<Fixture>,
    alias: String,
    backends: Vec<String>,
    registration_attempted: bool,
}

impl RegisteredStopFixture {
    fn fixture(&self) -> anyhow::Result<&Fixture> {
        self.fixture.as_ref().context("registered fixture is unavailable")
    }

    fn command(&self, args: &[&str]) -> anyhow::Result<Command> {
        let fixture = self.fixture()?;
        let mut command = fixture.command(args);
        // The service must unlock the checkpointed OS credential itself.
        for name in ["RSRS_SUPER", "RSRS_USER", "RSRS_PASS", "TYPESAFE_API_KEY", "DS_API_KEY"] {
            command.env_remove(name);
        }
        command.env("RSRS_BIN_DIR", fixture.home.path().join("data/bin"));
        Ok(command)
    }

    fn probe_command(&self, probe: &Path, action: &str) -> anyhow::Result<Command> {
        let template = self.command(&[])?;
        let mut command = Command::new(probe);
        command.arg(action).current_dir(self.fixture()?.home.path());
        for (name, value) in template.get_envs() {
            match value { Some(value) => { command.env(name, value); }, None => { command.env_remove(name); } }
        }
        Ok(command)
    }

    fn run(&self, args: &[&str]) -> anyhow::Result<Value> {
        let output = self.command(args)?.output()?;
        if !output.status.success() {
            if let Some(code) = output.status.code() {
                eprintln!("REGISTERED_LIFECYCLE_EXIT code={code}");
            }
            // Only the private libtest capture receives command output; public reports use fixed codes.
            eprintln!("\nREGISTERED_LIFECYCLE_PRIVATE_STDOUT\n{}\nREGISTERED_LIFECYCLE_PRIVATE_STDERR\n{}\nREGISTERED_LIFECYCLE_PRIVATE_END",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        }
        anyhow::ensure!(output.status.success(), "registered lifecycle command failed with exit code {:?}", output.status.code());
        Ok(serde_json::from_slice::<Value>(&output.stdout)?["summary"].clone())
    }

    fn health(&self) -> anyhow::Result<Value> {
        let fixture = self.fixture()?;
        let health: Value = ureq::AgentBuilder::new().try_proxy_from_env(false).build()
            .get(&format!("http://127.0.0.1:{}/api/health", fixture.port))
            .set("X-Respire-Health", "runtime-v1")
            .timeout(Duration::from_secs(3)).call()?.into_json()?;
        let root = fixture.home.path().join("data").canonicalize()?;
        let registration: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/autostart/registration.json"))?)?;
        anyhow::ensure!(registration["enabled"] == true && health["managed_id"] == registration["id"]
            && health["managed_id"].as_str().is_some_and(|id| !id.is_empty()), "not a registered supervisor child");
        for field in ["installation_root", "data_dir"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("registered root missing")?).canonicalize()? == root,
                "registered Runtime root mismatch");
        }
        for field in ["image_path", "exe"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("registered image missing")?).canonicalize()? == fixture.runtime,
                "registered Runtime image mismatch");
        }
        Ok(health)
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        if self.registration_attempted {
            let summary = self.run(&["--json", "runtime", "autostart", "remove", "--stop"])?;
            anyhow::ensure!(summary["enabled"] == false && summary["registered"] == false
                && summary["runtime"]["running"] == false, "registered cleanup did not remove and stop the service");
            self.registration_attempted = false;
        }
        for backend in &self.backends {
            respire::keystore::remove_imported(&self.alias, "super", backend)?;
            anyhow::ensure!(respire::keystore::read_imported(&self.alias, "super", backend).is_err(),
                "registered credential remained after cleanup");
        }
        self.backends.clear();
        Ok(())
    }
}

impl Drop for RegisteredStopFixture {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            // Preserve the root/registration if owned cleanup fails; never delete
            // paths still referenced by a real registered service or hide failure.
            if let Some(fixture) = self.fixture.take() { std::mem::forget(fixture); }
            eprintln!("REGISTERED_STOP_CLEANUP_FAILED explicit owned recovery required");
        }
    }
}

