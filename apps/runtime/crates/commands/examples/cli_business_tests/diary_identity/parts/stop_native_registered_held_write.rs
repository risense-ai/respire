fn stop_native_registered_held_write() -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    use sha2::{Digest, Sha256};
    anyhow::ensure!(std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"),
        "real OS registration requires an ephemeral GitHub-hosted runner");
    let probe = PathBuf::from(std::env::var_os("RSRS_CLIENT2_PROBE_BIN")
        .context("registered caller-timeout requires the source-bound original Go probe")?).canonicalize()?;
    anyhow::ensure!(probe.is_file(), "original Go probe is not a file");
    let probe_digest = format!("{:x}", Sha256::digest(std::fs::read(&probe)?));
    for mode in ["normal", "caller-timeout", "explicit-force"] {
    let case_started = std::time::Instant::now();
    let checkpoint = |stage| {
        owned_stop_checkpoint("registered", mode, stage);
        if matches!(stage, "start" | "caller_wait_complete" | "receipt_verified" | "quiet_period_verified"
            | "recovery_requested" | "recovery_started" | "recovery_stopped" | "complete") {
            eprintln!("OWNED_STOP_TIMING case={mode} stage={stage} elapsed_ms={}", case_started.elapsed().as_millis());
        }
    };
    checkpoint("start");
    let fixture = Fixture::new()?;
    checkpoint("fixture_created");
    anyhow::ensure!(fixture.business_cli.is_none(), "registered control requires the bound modern CLI");
    let mut owned = RegisteredStopFixture { fixture: Some(fixture),
        alias: format!("login-{}", uuid::Uuid::new_v4().simple()), backends: Vec::new(), registration_attempted: false };
    let alias = owned.alias.clone();
    let secret = owned.fixture()?.secret.clone();
    checkpoint("credential_requested");
    let backend = respire::keystore::import_credential_checkpoint(&alias, "super", &secret, |backend| {
        anyhow::ensure!(matches!(backend, "native" | "secret_service"), "registered fixture requires persistent OS credentials");
        owned.backends.push(backend.to_owned());
        Ok(())
    })?;
    anyhow::ensure!(respire::keystore::read_imported(&alias, "super", backend)? == secret,
        "registered credential checkpoint readback mismatch");
    checkpoint("credential_ready");
    let fixture = owned.fixture.as_mut().context("fixture missing")?;
    let root = fixture.home.path().join("data");
    let mut session: Value = serde_json::from_slice(&std::fs::read(root.join("session.json"))?)?;
    session["keyring_account"] = json!(alias);
    session["keyring_backend"] = json!(backend);
    std::fs::write(root.join("session.json"), serde_json::to_vec(&session)?)?;
    // Use the existing public classification file contract, with only a synthetic key.
    anyhow::ensure!(respire::keystore::load_classify_key("typesafe").is_none(), "fixture classification slot is not empty");
    std::fs::write(root.join("classify.json"), br#"{"api_key":"synthetic-native-stop-key"}"#)?;
    checkpoint("runtime_install_requested");
    support::install_runtime_fixture(&root)?;
    let selected: Value = serde_json::from_slice(&std::fs::read(root.join("bin/runtime.json"))?)?;
    fixture.runtime = root.join("bin/runtime").join(selected["version"].as_str().context("installed version missing")?)
        .join(if cfg!(windows) { "rsrs-runtime.exe" } else { "rsrs-runtime" }).canonicalize()?;
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::copy(&fixture.cli, root.join("bin").join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" }))?;
    checkpoint("runtime_installed");
    let write = StopWriteEvidence::seed(fixture)?;
    checkpoint("memory_seeded");
    owned.registration_attempted = true;
    checkpoint("start_requested");
    let enabled = owned.run(&["--json", "runtime", "autostart", "enable", "--now"])?;
    anyhow::ensure!(enabled["enabled"] == true && enabled["registered"] == true, "real OS registration did not complete");
    checkpoint("start_complete");
    let health = owned.health()?;
    anyhow::ensure!(health["store_ready"] == true, "registered service could not unlock its actual persistent credential");
    checkpoint("health_bound");
    let fixture = owned.fixture()?;
    let runtime_pid = u32::try_from(health["pid"].as_u64().context("runtime PID missing")?)?;
    let runtime_identity = stop_native_worker_identity(runtime_pid)?.context("registered backend is not live")?;
    let claim: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/autostart/launch")
        .join(format!("launch-{}.claimed", health["instance_id"].as_str().context("instance missing")?)))?)?;
    let grant_json = claim["grant_json"].as_str().context("consumed grant JSON missing")?;
    assert_eq!(claim["schema_version"], 2);
    assert_eq!(claim["grant_sha256"], format!("{:x}", Sha256::digest(grant_json.as_bytes())));
    let grant: Value = serde_json::from_str(grant_json)?;
    let supervisor_pid = u32::try_from(grant["supervisor_pid"].as_u64().context("supervisor PID missing")?)?;
    anyhow::ensure!(supervisor_pid != runtime_pid && supervisor_pid != std::process::id(), "supervisor identity collapsed into backend/harness");
    let supervisor_identity = stop_native_worker_identity(supervisor_pid)?.context("registered supervisor is not live")?;
    let registration: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/autostart/registration.json"))?)?;
    anyhow::ensure!(Path::new(supervisor_identity["image"].as_str().context("supervisor image missing")?).canonicalize()?
        == Path::new(registration["executable"].as_str().context("registration executable missing")?).canonicalize()?
        && supervisor_identity["birth"] == grant["supervisor_birth"] && claim["pid"] == health["pid"]
        && grant["managed_id"] == health["managed_id"], "registered launcher/backend claim differs from live OS identity");
    let prefix = if cfg!(windows) { "windows-filetime-" } else if cfg!(target_os = "linux") { "linux-starttime-" } else { "darwin-lstart-" };
    assert_eq!(health["process_start_id"], format!("{prefix}{}", claim["process_start_id"].as_str().context("claimed birth missing")?));
    assert_eq!(runtime_identity["birth"], claim["process_start_id"]);
    anyhow::ensure!(Path::new(runtime_identity["image"].as_str().context("backend OS image missing")?).canonicalize()? == fixture.runtime,
        "registered backend OS image mismatch");
    if let Some(parent) = runtime_identity.get("parent") { assert_eq!(*parent, json!(supervisor_pid)); }
    assert_eq!(health["image_sha256"], format!("{:x}", Sha256::digest(std::fs::read(&fixture.runtime)?)));
    checkpoint("actors_bound");
    let mut provider = write.provider()?;
    let rpc_url = format!("http://127.0.0.1:{}/api/rpc", fixture.port);
    let request = json!({"v":1,"id":"registered-stop-original","method":"cli.exec",
        "operation_id":"registered-stop-classify","idempotency_key":"registered-stop-classify",
        "scope":{"account_id":health["account_id"],"instance_id":health["instance_id"],"generation":health["generation"]},
        "args":write.args(&provider.endpoint)});
    let (url, original) = (rpc_url.clone(), request.clone());
    checkpoint("provider_requested");
    let business = std::thread::spawn(move || stop_native_post(&url, &original));
    provider.received.recv_timeout(Duration::from_secs(30)).context("registered business did not reach the actual provider")?;
    checkpoint("provider_received");
    let active = owned.health()?;
    assert!(active["control"]["admitted"].as_array().context("admissions missing")?.iter()
        .any(|item| item["operation_id"] == request["operation_id"]));
    let worker_pid = u32::try_from(active["inference"]["worker_pid"].as_u64().context("Core PID missing")?)?;
    let worker_identity = stop_native_worker_identity(worker_pid)?.context("actual Core worker missing")?;
    anyhow::ensure!(Path::new(worker_identity["image"].as_str().context("Core OS image missing")?).canonicalize()? == fixture.runtime,
        "registered Core OS image mismatch");
    if let Some(parent) = worker_identity.get("parent") { assert_eq!(*parent, json!(runtime_pid)); }
    assert!(active["inference"]["worker_host_callbacks_active"].as_u64().is_some_and(|n| n > 0));
    assert!(active["inference"]["worker_call_deadline_secs"].as_u64().is_some_and(|n| n >= 30));
    checkpoint("core_bound");
    assert_owned_stop_lock_held(&root, runtime_pid, &runtime_identity)?;
    std::thread::sleep(Duration::from_millis(4200));
    assert!(!business.is_finished());
    checkpoint("lock_held_initial");
    let stop_output_path = root.join("registered-stop.stdout");
    let started_stop = std::time::Instant::now();
    checkpoint("stop_requested");
    let mut stop_command = if mode == "normal" { owned.command(&["--json", "runtime", "stop"])? }
        else { owned.probe_command(&probe, "stop")? };
    let mut stop = stop_command
        .stdout(std::fs::File::create(&stop_output_path)?)
        .stderr(std::fs::File::create(root.join("registered-stop.stderr"))?).spawn()?;
    checkpoint("stop_started");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let pending = loop {
        let observed = owned.health()?;
        if observed["control"]["state"] == "draining" { break observed; }
        anyhow::ensure!(stop.try_wait()?.is_none() && std::time::Instant::now() < deadline, "registered stop failed before drain");
        std::thread::sleep(Duration::from_millis(50));
    };
    let operation = pending["control"]["operation_id"].clone();
    anyhow::ensure!(operation.as_str().is_some_and(|id| !id.is_empty()), "registered stop operation missing");
    checkpoint("drain_observed");
    let mut caller_timeout_ms = None;
    if mode == "normal" {
        std::thread::sleep(Duration::from_millis(4200));
        assert!(stop.try_wait()?.is_none());
    } else {
        let deadline = started_stop + Duration::from_secs(45);
        loop {
            if let Some(status) = stop.try_wait()? {
                assert!(!status.success(), "old managed caller completed before the held business");
                break;
            }
            anyhow::ensure!(std::time::Instant::now() < deadline, "original managed caller did not hit its 35s deadline");
            std::thread::sleep(Duration::from_millis(100));
        }
        let output: Value = serde_json::from_slice(&std::fs::read(&stop_output_path)?)?;
        assert!(output["action_error"].as_str().context("old managed caller error missing")?
            .contains(STOP_FROM_CLI));
        assert!(started_stop.elapsed() >= Duration::from_secs(35));
        caller_timeout_ms = Some(started_stop.elapsed().as_millis());
    }
    checkpoint("caller_wait_complete");
    assert!(!business.is_finished());
    assert_eq!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
    assert_eq!(stop_native_worker_identity(supervisor_pid)?.as_ref(), Some(&supervisor_identity));
    let held = owned.health()?;
    assert_eq!(held["control"]["state"], "draining");
    assert_eq!(held["inference"]["worker_retired"], false);
    let registration: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/autostart/registration.json"))?)?;
    assert_eq!(registration["manually_stopped"], true);
    assert_eq!(registration["maintenance"], true);
    let mut replay = request.clone(); replay["id"] = json!("registered-stop-retry");
    let (_, same_key) = stop_native_post(&rpc_url, &replay)?;
    assert_eq!(same_key["code"], "request_in_progress");
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    assert_owned_stop_lock_held(&root, runtime_pid, &runtime_identity)?;
    checkpoint("lock_held_draining");
    let mut force_response = None;
    if mode == "explicit-force" {
    checkpoint("force_requested");
        let force = json!({"installation_id":health["installation_id"],"instance_id":health["instance_id"],
            "account_id":health["account_id"],"generation":health["generation"],"operation_id":operation,"force":true});
        let (status, response) = stop_native_post(&format!("http://127.0.0.1:{}/api/runtime/stop", fixture.port), &force)?;
        assert_eq!(status, 200); assert_eq!(response["operation_id"], operation);
        assert_eq!(response["timeout_state"], "forced_timeout");
        assert!(response["deadline_ms"].as_u64().is_some_and(|ms| ms <= 4000));
        force_response = Some(response);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let observed = owned.health()?;
            if observed["inference"]["retired_worker"]["worker_reaped"] == true { break; }
            anyhow::ensure!(std::time::Instant::now() < deadline, "registered force did not retire the original Core");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_ne!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
        assert!(!business.is_finished());
        assert_owned_stop_lock_held(&root, runtime_pid, &runtime_identity)?;
        checkpoint("core_reaped");
    }
    checkpoint("provider_release_requested");
    provider.release.store(true, Ordering::Release);
    let (_, result) = business.join().map_err(|_| anyhow::anyhow!("registered business thread panicked"))??;
    assert_eq!(result["id"], request["id"]);
    assert_eq!(result["operation_id"], request["operation_id"]);
    if mode == "explicit-force" { assert_eq!(result["ok"], false); }
    else { write.assert_result(&result, &request); }
    checkpoint("business_completed");
    if mode == "normal" {
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = stop.try_wait()? { anyhow::ensure!(status.success(), "registered normal stop failed"); break; }
            anyhow::ensure!(std::time::Instant::now() < deadline, "registered stop did not finish");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if stop_native_worker_identity(runtime_pid)?.as_ref() != Some(&runtime_identity)
            && stop_native_worker_identity(supervisor_pid)?.as_ref() != Some(&supervisor_identity)
            && stop_native_worker_identity(worker_pid)?.as_ref() != Some(&worker_identity) { break; }
        anyhow::ensure!(std::time::Instant::now() < deadline, "registered backend/supervisor/Core did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }
    checkpoint("actors_exited");
    let receipt_path = root.join("runtime").join(format!("stop-{}.json", health["instance_id"].as_str().context("instance missing")?));
    let receipt_bytes = std::fs::read(&receipt_path)?;
    let receipt: Value = serde_json::from_slice(&receipt_bytes)?;
    assert_eq!(receipt["schema_version"], 1); assert_eq!(receipt["protocol_version"], 1);
    assert_eq!(receipt["operation_id"], operation);
    for key in ["installation_id", "instance_id", "pid", "process_start_id", "image_path", "image_sha256", "runtime_version", "account_id", "generation"] {
        assert_eq!(receipt[key], health[key], "registered final identity changed: {key}");
    }
    assert_eq!(receipt["state"], if mode == "explicit-force" { "forced_timeout" } else { "drained" });
    if mode == "explicit-force" {
        let unfinished = receipt["unfinished"].as_array().context("registered forced unfinished list missing")?;
        assert_eq!(unfinished.iter().filter(|item| item["operation_id"] == request["operation_id"]
            && item["result"] == "unknown").count(), 1);
    } else { assert_eq!(receipt["unfinished"], json!([])); }
    assert_ne!(stop_native_worker_identity(runtime_pid)?.as_ref(), Some(&runtime_identity));
    assert_ne!(stop_native_worker_identity(supervisor_pid)?.as_ref(), Some(&supervisor_identity));
    assert_ne!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
    checkpoint("receipt_verified");
    // Exceed both the 10s systemd/launchd delay and 60s Task Scheduler retry interval.
    let quiet = std::time::Instant::now() + Duration::from_secs(65);
    while std::time::Instant::now() < quiet {
        anyhow::ensure!(!stop_native_listener_open(fixture.port, Duration::from_secs(1))?,
            "registered owner resurrected: stopped port has a local listener");
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(owned.run(&["--json", "runtime", "autostart", "status"])?["runtime"]["running"], false);
    checkpoint("quiet_period_verified");
    let lock = respire::lock::LibraryLock::acquire(&root, Duration::from_secs(1))?; drop(lock);
    if mode == "explicit-force" { write.assert_uncommitted(fixture)?; }
    else { write.assert_committed(fixture)?; }
    checkpoint("write_verified");
    let recovery = if mode != "normal" {
    checkpoint("recovery_requested");
        if let Err(error) = owned.run(&["--json", "runtime", "restart"]) {
            registered_restart_diagnostics(&root);
            return Err(error);
        }
    checkpoint("recovery_started");
        let recovered = owned.health()?;
        assert_eq!(recovered["store_ready"], true);
        assert_ne!(recovered["instance_id"], health["instance_id"]);
        let recovered_pid = u32::try_from(recovered["pid"].as_u64().context("recovered registered PID missing")?)?;
        let recovered_os = stop_native_worker_identity(recovered_pid)?.context("recovered registered actor missing")?;
        let recovered_claim: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/autostart/launch")
            .join(format!("launch-{}.claimed", recovered["instance_id"].as_str().context("recovered instance missing")?)))?)?;
        let recovered_grant: Value = serde_json::from_str(recovered_claim["grant_json"].as_str().context("recovered grant missing")?)?;
        let recovered_supervisor = u32::try_from(recovered_grant["supervisor_pid"].as_u64().context("recovered supervisor missing")?)?;
        let recovered_supervisor_os = stop_native_worker_identity(recovered_supervisor)?.context("recovered supervisor not live")?;
        let recovered_worker = match recovered["inference"].get("worker_pid") {
            None if recovered["inference"]["phase"] == "idle"
                && recovered["inference"]["host_recovery_required"] == false => None,
            Some(Value::Null) => None,
            Some(value) => {
                let pid = u32::try_from(value.as_u64().context("recovered registered Core PID is invalid")?)?;
                let identity = stop_native_worker_identity(pid)?.context("recovered registered Core not live")?;
                anyhow::ensure!(Path::new(identity["image"].as_str().context("recovered registered Core image missing")?).canonicalize()?
                    == fixture.runtime, "recovered registered Core OS image mismatch");
                if let Some(parent) = identity.get("parent") { assert_eq!(*parent, json!(recovered_pid)); }
                Some((pid, identity))
            }
            None => anyhow::bail!("recovered registered Core observation missing"),
        };
    checkpoint("recovery_bound");
        replay["id"] = json!("registered-old-scope-after-explicit-recovery");
        let (_, stale) = stop_native_post(&rpc_url, &replay)?;
        assert_eq!(stale["code"], "scope_mismatch");
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    checkpoint("recovery_stop_requested");
        owned.run(&["--json", "runtime", "stop"])?;
        let final_recovered: Value = serde_json::from_slice(&std::fs::read(root.join("runtime")
            .join(format!("stop-{}.json", recovered["instance_id"].as_str().context("recovered instance missing")?)))?)?;
        assert_eq!(final_recovered["state"], "drained"); assert_eq!(final_recovered["unfinished"], json!([]));
        for key in ["installation_id","instance_id","pid","process_start_id","image_path","image_sha256","runtime_version","account_id","generation"] {
            assert_eq!(final_recovered[key], recovered[key], "recovered registered receipt mismatch: {key}");
        }
        assert_ne!(stop_native_worker_identity(recovered_pid)?.as_ref(), Some(&recovered_os));
        assert_ne!(stop_native_worker_identity(recovered_supervisor)?.as_ref(), Some(&recovered_supervisor_os));
        if let Some((pid, identity)) = recovered_worker.as_ref() {
            assert_ne!(stop_native_worker_identity(*pid)?.as_ref(), Some(identity));
        }
        assert_eq!(std::fs::read(&receipt_path)?, receipt_bytes, "explicit recovery rewrote the old receipt");
    checkpoint("recovery_stopped");
        Some(json!({"identity":recovered,"receipt":final_recovered,"stale_scope_result":stale}))
    } else { None };
    provider.finish()?; assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    checkpoint("cleanup_requested");
    owned.cleanup()?;
    checkpoint("cleanup_complete");
    checkpoint("complete");
    eprintln!("NATIVE_REGISTERED_STOP_RAW {}", json!({"schema":1,"evidence_class":"native-raw","gate":"not-evaluated",
        "startup_mode":"cli-supervised","case":mode,"forced":mode == "explicit-force","passed":true,"runtime_identity":health,
        "probe_sha256":probe_digest,"business_sender":"modern-harness",
        "stop_sender":if mode == "normal" { "modern-cli" } else { "original-go-client2-managed" },
        "caller_timeout_ms":caller_timeout_ms,"runtime_compatibility_timeout_claimed":false,
        "explicit_force_response":force_response,"recovery":recovery,
        "runtime_os_identity":runtime_identity,"supervisor_os_identity":supervisor_identity,"worker_os_identity":worker_identity,
        "active_health":active,"held_health":held,"business_result":result,"same_key_result":same_key,"final_receipt":receipt,
        "provider_http_status":200,"encrypted_write_committed":mode != "explicit-force",
        "forced_operation_remains_unknown":mode == "explicit-force","provider_requests":1,"quiet_after_stop_ms":65000,"credential_cleanup_verified":true}));
    }
    Ok(())
}


// Same safe keyring/installed-bundle preparation as the registered case; this
// mode starts through the original Go Manager, never the service supervisor.
fn prepare_client2_stop_fixture(case: &str) -> anyhow::Result<RegisteredStopFixture> {
    let checkpoint = |stage| owned_stop_checkpoint("client2-owned", case, stage);
    let fixture = Fixture::new()?;
    checkpoint("fixture_created");
    anyhow::ensure!(fixture.business_cli.is_none(), "Client2-owned control requires the bound modern CLI");
    let mut owned = RegisteredStopFixture { fixture: Some(fixture),
        alias: format!("login-{}", uuid::Uuid::new_v4().simple()), backends: Vec::new(), registration_attempted: false };
    let alias = owned.alias.clone();
    let secret = owned.fixture()?.secret.clone();
    checkpoint("credential_requested");
    let backend = respire::keystore::import_credential_checkpoint(&alias, "super", &secret, |backend| {
        anyhow::ensure!(matches!(backend, "native" | "secret_service"), "registered fixture requires persistent OS credentials");
        owned.backends.push(backend.to_owned());
        Ok(())
    })?;
    anyhow::ensure!(respire::keystore::read_imported(&alias, "super", backend)? == secret,
        "registered credential checkpoint readback mismatch");
    checkpoint("credential_ready");
    let fixture = owned.fixture.as_mut().context("fixture missing")?;
    let root = fixture.home.path().join("data");
    let mut session: Value = serde_json::from_slice(&std::fs::read(root.join("session.json"))?)?;
    session["keyring_account"] = json!(alias);
    session["keyring_backend"] = json!(backend);
    std::fs::write(root.join("session.json"), serde_json::to_vec(&session)?)?;
    // Use the existing public classification file contract, with only a synthetic key.
    anyhow::ensure!(respire::keystore::load_classify_key("typesafe").is_none(), "fixture classification slot is not empty");
    std::fs::write(root.join("classify.json"), br#"{"api_key":"synthetic-native-stop-key"}"#)?;
    checkpoint("runtime_install_requested");
    support::install_runtime_fixture(&root)?;
    let selected: Value = serde_json::from_slice(&std::fs::read(root.join("bin/runtime.json"))?)?;
    fixture.runtime = root.join("bin/runtime").join(selected["version"].as_str().context("installed version missing")?)
        .join(if cfg!(windows) { "rsrs-runtime.exe" } else { "rsrs-runtime" }).canonicalize()?;
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::copy(&fixture.cli, root.join("bin").join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" }))?;
    checkpoint("runtime_installed");
    Ok(owned)
}


struct Client2OwnedStopFixture {
    owned: Option<RegisteredStopFixture>,
    probe: PathBuf,
    observed_runtime: Option<Value>,
    observed_actors: Vec<(u32, Value)>,
    identity_observation_complete: bool,
    start_attempted: bool,
    completed: bool,
}

impl Client2OwnedStopFixture {
    fn owned(&self) -> anyhow::Result<&RegisteredStopFixture> {
        self.owned.as_ref().context("Client2-owned fixture missing")
    }

    fn command(&self, action: &str) -> anyhow::Result<Command> {
        let owned = self.owned()?;
        let template = owned.command(&[])?;
        let mut command = Command::new(&self.probe);
        command.arg(action).current_dir(owned.fixture()?.home.path());
        for (name, value) in template.get_envs() {
            match value { Some(value) => { command.env(name, value); }, None => { command.env_remove(name); } }
        }
        Ok(command)
    }

    fn health(&self) -> anyhow::Result<Value> {
        let fixture = self.owned()?.fixture()?;
        eprintln!("CLIENT2_STOP_HEALTH_CHECKPOINT view=runtime stage=requested");
        let health: Value = ureq::AgentBuilder::new().try_proxy_from_env(false).build()
            .get(&format!("http://127.0.0.1:{}/api/health", fixture.port))
            .set("X-Respire-Health", "runtime-v1").timeout(Duration::from_secs(3)).call()?.into_json()?;
        let root = fixture.home.path().join("data").canonicalize()?;
        anyhow::ensure!(health["server"] == "respire" && health["v"] == 1 && health["managed_id"].is_null()
            && health.get("compatibility_host").is_none(),
            "Client2-owned mode must use the independent unregistered Runtime");
        for field in ["data_dir", "installation_root"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("Client2-owned root missing")?).canonicalize()? == root,
                "Client2-owned scope root mismatch");
        }
        let expected = self.runtime_image()?;
        for field in ["exe", "image_path"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("Client2-owned image missing")?).canonicalize()? == expected,
                "Client2-owned Runtime image mismatch");
        }
        if let Some(observed) = &self.observed_runtime {
            for key in ["pid", "instance_id", "process_start_id", "image_path", "image_sha256",
                "runtime_version", "installation_id", "account_id", "generation"] {
                anyhow::ensure!(health[key] == observed[key], "Client2-owned Runtime identity changed");
            }
        }
        eprintln!("CLIENT2_STOP_HEALTH_CHECKPOINT view=runtime stage=verified");
        Ok(health)
    }

    fn runtime_image(&self) -> anyhow::Result<PathBuf> {
        // Manager.Start delegates to the CLI, which selects its verified managed installation.
        Ok(self.owned()?.fixture()?.runtime.clone())
    }

    fn verify_parent(&self, pid: u32, identity: &Value, expected: u32) -> anyhow::Result<()> {
        #[cfg(windows)]
        let parent = {
            use std::os::windows::process::CommandExt;
            let output = Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command",
                &format!("$ErrorActionPreference='Stop'; (Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').ParentProcessId")])
                .creation_flags(0x08000000).output()?;
            anyhow::ensure!(output.status.success(), "Client2-owned process parent inspection failed");
            String::from_utf8(output.stdout)?.trim().parse::<u32>()?
        };
        #[cfg(not(windows))]
        let parent = u32::try_from(identity["parent"].as_u64().context("Client2-owned process parent missing")?)?;
        anyhow::ensure!(parent == expected && stop_native_worker_identity(pid)?.as_ref() == Some(identity),
            "Client2-owned process parent or identity changed");
        Ok(())
    }

    fn observe_actor(&self, runtime: &Value, cli: &Value) -> anyhow::Result<(u32, Value)> {
        use sha2::{Digest, Sha256};
        let fixture = self.owned()?.fixture()?;
        let root = fixture.home.path().join("data").canonicalize()?;
        let runtime_pid = u32::try_from(runtime["pid"].as_u64().context("Client2-owned Runtime PID missing")?)?;
        anyhow::ensure!(runtime_pid > 0 && runtime_pid != std::process::id()
            && runtime["instance_id"].as_str().is_some_and(|value| !value.is_empty())
            && runtime["account_id"].as_str().is_some_and(|value| !value.is_empty())
            && runtime["generation"].as_u64().is_some_and(|value| value > 0)
            && runtime["installation_id"] == format!("{:x}", Sha256::digest(root.display().to_string().as_bytes())),
            "Client2-owned Runtime identity mismatch");
        let runtime_os = stop_native_worker_identity(runtime_pid)?.context("actual Client2-owned Runtime missing")?;
        let runtime_image = self.runtime_image()?;
        anyhow::ensure!(Path::new(runtime_os["image"].as_str().context("Client2-owned OS image missing")?).canonicalize()? == runtime_image,
            "Client2-owned OS image mismatch");
        let prefix = if cfg!(windows) { "windows-filetime-" } else if cfg!(target_os = "linux") { "linux-starttime-" } else { "darwin-lstart-" };
        anyhow::ensure!(runtime["process_start_id"] == format!("{prefix}{}", runtime_os["birth"].as_str().context("Client2-owned OS birth missing")?),
            "Client2-owned process birth mismatch");
        // The original Manager selects and executes the CLI, then verifies the
        // independent Runtime. It does not keep a CLI compatibility host alive.
        let cli_image = Path::new(cli["path"].as_str().context("Client2-owned selected CLI path missing")?).canonicalize()?;
        let cli_hash = format!("{:x}", Sha256::digest(std::fs::read(&cli_image)?));
        anyhow::ensure!(cli_image.starts_with(&root) && cli_image != runtime_image
            && cli_hash == format!("{:x}", Sha256::digest(std::fs::read(&fixture.cli)?))
            && cli["sha256"] == cli_hash
            && cli["version"].as_str().is_some_and(|value| !value.is_empty()),
            "Client2-owned CLI control image mismatch");
        let selected: Value = serde_json::from_slice(&std::fs::read(root.join("client2/runtime.json"))?)?;
        let installed: Value = serde_json::from_slice(&std::fs::read(root.join("bin/runtime.json"))?)?;
        let runtime_hash = format!("{:x}", Sha256::digest(std::fs::read(&runtime_image)?));
        anyhow::ensure!(selected["version"] == installed["version"] && runtime["runtime_version"] == installed["version"]
            && runtime["image_sha256"] == runtime_hash && selected["sha256"] == runtime_hash
            && installed["sha256"] == runtime_hash, "Client2-owned Runtime release mismatch");
        anyhow::ensure!(stop_native_worker_identity(runtime_pid)?.as_ref() == Some(&runtime_os),
            "Client2-owned actor changed during verification");
        Ok((runtime_pid, runtime_os))
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        if self.completed { return Ok(()); }
        let fixture = self.owned()?.fixture()?;
        anyhow::ensure!(!self.start_attempted || self.observed_runtime.is_some(),
            "Client2 startup outcome is unverified; preserve root and credential for explicit recovery");
        anyhow::ensure!(!self.start_attempted || self.identity_observation_complete,
            "Client2 actor observation incomplete; preserve root and credential for explicit recovery");
        if let Some(expected) = self.observed_runtime.as_ref() {
            match stop_native_listener_open(fixture.port, Duration::from_secs(1)) {
                Ok(true) => {
                    let observed = self.health()?;
                    for key in ["pid", "exe", "installation_root", "data_dir"] {
                        anyhow::ensure!(observed[key] == expected[key], "Client2-owned cleanup target changed");
                    }
                    let output = self.command("stop")?.output()?;
                    anyhow::ensure!(output.status.success(), "original Go owned cleanup failed");
                },
                Ok(false) => {},
                Err(error) => return Err(error.into()),
            }
        }
        for (pid, identity) in &self.observed_actors {
            anyhow::ensure!(stop_native_worker_identity(*pid)?.as_ref() != Some(identity),
                "observed Client2 host/backend/Core is still live; preserve root and credential");
        }
        self.owned.as_mut().context("owned fixture missing")?.cleanup()?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for Client2OwnedStopFixture {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            // Do not delete a possibly live root or its recovery credential.
            if let Some(owned) = self.owned.take() { std::mem::forget(owned); }
            eprintln!("CLIENT2_OWNED_STOP_CLEANUP_FAILED explicit owned recovery required");
        }
    }
}

