fn stop_native_worker_identity(pid: u32) -> anyhow::Result<Option<Value>> {
    #[cfg(windows)]
    {
        return Ok(respire_spawn::process_identity(pid)?.map(|identity|
            json!({"birth":identity.birth.to_string(),"image":identity.executable})));
    }
    #[cfg(target_os = "linux")]
    {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(3) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let fields: Vec<_> = stat[stat.rfind(')').context("worker stat name missing")? + 1..]
            .split_whitespace().collect();
        if fields.first() == Some(&"Z") { return Ok(None); }
        let image = match std::fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(image) => image,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        return Ok(Some(json!({"birth":fields.get(19).context("worker birth missing")?,
            "parent":fields.get(1).context("worker parent missing")?.parse::<u32>()?,"image":image})));
    }
    #[cfg(target_os = "macos")]
    {
        let read = |field: &str| -> anyhow::Result<Option<String>> {
            let output = Command::new("ps").args(["-p", &pid.to_string(), "-o", field]).output()?;
            if !output.status.success() { return Ok(None); }
            let value = String::from_utf8(output.stdout)?.trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        };
        let Some(birth) = read("lstart=")? else { return Ok(None) };
        let Some(image) = read("comm=")? else { return Ok(None) };
        let Some(parent) = read("ppid=")? else { return Ok(None) };
        return Ok(Some(json!({"birth":birth,"parent":parent.parse::<u32>()?,"image":image})));
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    anyhow::bail!("native worker identity is unsupported on this target")
}

impl Fixture {
    fn start_standalone_for_stop(&mut self) -> anyhow::Result<Value> {
        anyhow::ensure!(self.child.is_none(), "standalone fixture already running");
        let template = self.command(&[]);
        let mut command = Command::new(&self.runtime);
        for (name, value) in template.get_envs() {
            match value { Some(value) => { command.env(name, value); }, None => { command.env_remove(name); } }
        }
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            command.env_remove(name);
        }
        let root = self.home.path().join("data");
        self.child = Some(command.args(["serve", "--host", "127.0.0.1", "--port", &self.port.to_string(),
            "--data-dir", root.to_str().context("fixture root is not UTF-8")?])
            .current_dir(self.home.path())
            .env("TYPESAFE_API_KEY", "synthetic-native-stop-key")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .stdout(std::fs::File::create(self.home.path().join("stop-runtime.stdout"))?)
            .stderr(std::fs::File::create(self.home.path().join("stop-runtime.stderr"))?).spawn()?);
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            anyhow::ensure!(self.child.as_mut().context("runtime child missing")?.try_wait()?.is_none(),
                "standalone Runtime exited before readiness");
            if let Ok(health) = self.standalone_stop_health() {
                if health["startup_complete"] == true {
                    assert_eq!(health["store_ready"], false, "standalone stop fixture unlocked without its host");
                    assert_eq!(health["credential_state"], "required");
                    let delivered = self.command(&["--json", "runtime", "unlock"]).output()?;
                    // Retain only the host's scalar diagnostic marker; raw stderr stays private.
                    for line in String::from_utf8_lossy(&delivered.stderr).lines()
                        .filter(|line| line.starts_with("HOST_PROVIDER_CREDENTIAL_FAILURE ")) {
                        eprintln!("{line}");
                    }
                    anyhow::ensure!(delivered.status.success(), "standalone stop fixture host key delivery failed");
                    let receipt: Value = serde_json::from_slice(&delivered.stdout)?;
                    let provider_status: respire_protocol::credentials::ProviderCredentialStatus =
                        serde_json::from_value(receipt["summary"]["provider_status"].clone())?;
                    eprintln!("NATIVE_STOP_PROVIDER_STATUS {}", serde_json::to_string(&provider_status)?);
                    anyhow::ensure!(matches!(provider_status, respire_protocol::credentials::ProviderCredentialStatus::Ready),
                        "standalone stop fixture provider credentials are not ready");
                    let unlocked = self.standalone_stop_health()?;
                    assert_eq!(unlocked["instance_id"], health["instance_id"]);
                    assert_eq!(unlocked["generation"], health["generation"]);
                    assert_eq!(unlocked["store_ready"], true, "standalone stop fixture remains locked");
                    return Ok(unlocked);
                }
            }
            anyhow::ensure!(std::time::Instant::now() < deadline, "standalone Runtime readiness timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn standalone_stop_health(&self) -> anyhow::Result<Value> {
        let health: Value = ureq::AgentBuilder::new().try_proxy_from_env(false).build()
            .get(&format!("http://127.0.0.1:{}/api/health", self.port))
            .timeout(Duration::from_secs(3)).call()?.into_json()?;
        let pid = self.child.as_ref().context("runtime child missing")?.id();
        anyhow::ensure!(pid != std::process::id() && health["pid"] == pid, "unexpected Runtime process");
        for field in ["image_path", "exe"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("Runtime image missing")?)
                .canonicalize()? == self.runtime, "standalone Runtime image mismatch");
        }
        let root = self.home.path().join("data").canonicalize()?;
        for field in ["installation_root", "data_dir"] {
            anyhow::ensure!(Path::new(health[field].as_str().context("Runtime root missing")?)
                .canonicalize()? == root, "standalone Runtime root mismatch");
        }
        Ok(health)
    }

    fn assert_standalone_stop_lock_held(&self) -> anyhow::Result<()> {
        let owner = self.child.as_ref().context("Runtime owner missing")?.id();
        anyhow::ensure!(owner != std::process::id(), "lock check must run outside the Runtime process");
        match respire::lock::LibraryLock::acquire(&self.home.path().join("data"), Duration::from_millis(100)) {
            Ok(_) => anyhow::bail!("Runtime released its library lock before business cleanup"),
            Err(error) => anyhow::ensure!(error.to_string().starts_with("memory library still locked after "),
                "library lock check failed without proving contention: {error:#}"),
        }
        Ok(())
    }

    fn finish_standalone_stop(&mut self, health: &Value, operation: &Value, forced: bool) -> anyhow::Result<Value> {
        let started = std::time::Instant::now();
        let checkpoint = |stage: &str| eprintln!("NATIVE_STOP_FINISH_CHECKPOINT stage={stage} elapsed_ms={}", started.elapsed().as_millis());
        checkpoint("wait_requested");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = self.child.as_mut().context("runtime child missing")?.try_wait()? { break status; }
            anyhow::ensure!(std::time::Instant::now() < deadline, "Runtime did not finish native stop");
            std::thread::sleep(Duration::from_millis(50));
        };
        checkpoint("exit_observed");
        self.child.take();
        assert_eq!(status.code(), Some(if forced { 1 } else { 0 }));
        checkpoint("exit_verified");
        let receipt_bytes = std::fs::read(self.home.path().join("data/runtime")
            .join(format!("stop-{}.json", health["instance_id"].as_str().context("instance missing")?)))?;
        checkpoint("receipt_read");
        let receipt: Value = serde_json::from_slice(&receipt_bytes)?;
        checkpoint("receipt_parsed");
        assert_eq!(receipt["schema_version"], 1);
        assert_eq!(receipt["protocol_version"], 1);
        assert_eq!(receipt["operation_id"], *operation);
        for key in ["installation_id", "instance_id", "pid", "process_start_id", "image_path", "image_sha256", "runtime_version", "account_id", "generation"] {
            assert_eq!(receipt[key], health[key], "final stop identity changed: {key}");
        }
        assert_eq!(receipt["state"], if forced { "forced_timeout" } else { "drained" });
        checkpoint("receipt_verified");
        let listener_started = std::time::Instant::now();
        match stop_native_listener_open(self.port, Duration::from_secs(2)) {
            Ok(false) => {},
            other => {
                match &other {
                    Ok(_) => {
                        #[cfg(windows)]
                        eprintln!("NATIVE_STOP_LISTENER_FAILURE outcome=error kind=AddrInUse os_error=None probe_elapsed_ms={}", listener_started.elapsed().as_millis());
                        #[cfg(not(windows))]
                        eprintln!("NATIVE_STOP_LISTENER_FAILURE outcome=connected probe_elapsed_ms={}", listener_started.elapsed().as_millis());
                    },
                    Err(error) => eprintln!("NATIVE_STOP_LISTENER_FAILURE outcome=error kind={:?} os_error={:?} probe_elapsed_ms={}",
                        error.kind(), error.raw_os_error(), listener_started.elapsed().as_millis()),
                }
                anyhow::bail!("stopped listener is open or its state is unproved: {other:?}");
            }
        }
        checkpoint("listener_verified");
        // This harness process is distinct from the Runtime owner; use the actual file-lock API.
        let ownership = respire::lock::LibraryLock::acquire(&self.home.path().join("data"), Duration::from_secs(1))?;
        drop(ownership);
        checkpoint("lock_verified");
        Ok(receipt)
    }
}

struct StopWriteEvidence {
    root_id: String,
    child_id: String,
    parent_id: String,
    child: MemoryEntry,
    original_root: StoredMemory,
    original_child: StoredMemory,
    original_parent: StoredMemory,
    before_boundary: i64,
    before_outgoing: std::collections::HashSet<String>,
}

impl StopWriteEvidence {
    fn seed(fixture: &Fixture) -> anyhow::Result<Self> {
    let root_id = uuid::Uuid::new_v4().simple().to_string();
    let child_id = uuid::Uuid::new_v4().simple().to_string();
    let parent_id = uuid::Uuid::new_v4().simple().to_string();
    assert_ne!(&root_id[..8], &child_id[..8]);
    assert_ne!(&root_id[..8], &parent_id[..8]);
    assert_ne!(&child_id[..8], &parent_id[..8]);
    let mut root = entry(&root_id, "synthetic-diary", "Accepted write root", "Synthetic classification root");
    root.parent_id.clear();
    root.importance = "important".into();
    let mut child = entry(&child_id, "synthetic-diary", "Accepted write child", "Synthetic encrypted child");
    child.parent_id = root_id.clone();
    child.importance = "important".into();
    let mut parent = entry(&parent_id, "synthetic-diary", "Accepted write parent", "Synthetic encrypted parent");
    parent.parent_id = root_id.clone();
    parent.importance = "important".into();
    let original_root = fixture.seed(&root, false)?;
    let original_child = fixture.seed(&child, false)?;
    let original_parent = fixture.seed(&parent, false)?;
    let before_boundary = fixture.store()?.outgoing_boundary()?;
    let before_outgoing: std::collections::HashSet<_> = fixture.store()?.outgoing()?.into_iter()
        .map(|operation| operation.op_id).collect();

        Ok(Self { root_id, child_id, parent_id, child, original_root, original_child,
            original_parent, before_boundary, before_outgoing })
    }

    fn provider(&self) -> anyhow::Result<HeldClassifyProvider> {
        HeldClassifyProvider::with_response(Some(json!({"choices":[{"message":{"content":
            format!("{} > {}", &self.child_id[..8], &self.parent_id[..8])}}]})))
    }

    fn args(&self, endpoint: &str) -> Vec<String> {
        ["--json", "classify", "--backend", "jev", "--api-base", endpoint,
            "--auto", "--rounds", "1", "--min-kids", "2", "--segments", "1",
            "--limit", "3", "--samples", "1"].into_iter().map(str::to_owned).collect()
    }

    fn assert_result(&self, result: &Value, request: &Value) {
        for key in ["v", "id", "operation_id", "scope", "idempotency_key"] {
            assert_eq!(result[key], request[key], "accepted write identity changed: {key}");
        }
        assert_eq!(result["ok"], true);
        assert_eq!(result["exit"], 0);
        assert_eq!(result["envelope"]["command"], "classify");
        assert_eq!(result["envelope"]["status"], "ok");
        assert_eq!(result["envelope"]["details"]["applied"], 1);
        assert_eq!(result["envelope"]["details"]["ops"], json!([{"id":self.child_id,"parent":self.parent_id}]));
    }

    fn assert_committed(&self, fixture: &Fixture) -> anyhow::Result<()> {
        let root_id = self.root_id.as_str();
        let child_id = self.child_id.as_str();
        let parent_id = self.parent_id.as_str();
        let child = &self.child;
        let original_root = &self.original_root;
        let original_child = &self.original_child;
        let original_parent = &self.original_parent;
        let before_boundary = self.before_boundary;
        let before_outgoing = &self.before_outgoing;
    let after = fixture.store()?.all(true)?;
    assert_eq!(after.len(), 3);
    let written = after.iter().find(|row| row.id == child_id).context("committed child missing")?;
    assert_ne!(written.ciphertext, original_child.ciphertext);
    assert_ne!(written.nonce, original_child.nonce);
    assert_eq!(written.local_parent_id, parent_id);
    let opened = MemoryEngine::open(&fixture.keys, written)?;
    assert_eq!(opened.parent_id, parent_id);
    assert_eq!(MemoryEngine::payload_parent(&fixture.keys, written)?, parent_id);
    assert_eq!(opened.content, child.content);
    assert!(fixture.store()?.outgoing_boundary()? > before_boundary);
    let outgoing = fixture.store()?.outgoing()?;
    let added: Vec<_> = outgoing.iter().filter(|operation| !before_outgoing.contains(&operation.op_id)).collect();
    assert!(added.iter().any(|operation| operation.blob.id == child_id
        && operation.blob.ciphertext == written.ciphertext && operation.blob.nonce == written.nonce),
        "committed encrypted child is missing from the actual outbox");
    for operation in added {
        let expected = if operation.blob.id == child_id { written }
            else if operation.blob.id == root_id { &original_root }
            else if operation.blob.id == parent_id { &original_parent }
            else { anyhow::bail!("unexpected memory in the accepted-write outbox") };
        assert_eq!(operation.blob.ciphertext, expected.ciphertext);
        assert_eq!(operation.blob.nonce, expected.nonce);
        if operation.blob.id == child_id {
            assert_eq!(MemoryEngine::payload_parent(&fixture.keys, &operation.blob)?, parent_id);
        }
    }
    for original in [original_root, original_parent] {
        let unchanged = after.iter().find(|row| row.id == original.id).context("unchanged row missing")?;
        assert_eq!(unchanged.ciphertext, original.ciphertext);
        assert_eq!(unchanged.nonce, original.nonce);
        assert_eq!(unchanged.local_parent_id, original.local_parent_id);
    }

        Ok(())
    }

    fn assert_uncommitted(&self, fixture: &Fixture) -> anyhow::Result<()> {
        let rows = fixture.store()?.all(true)?;
        assert_eq!(rows.len(), 3);
        for original in [&self.original_root, &self.original_child, &self.original_parent] {
            let row = rows.iter().find(|row| row.id == original.id).context("forced fixture row missing")?;
            assert_eq!(row.ciphertext, original.ciphertext);
            assert_eq!(row.nonce, original.nonce);
            assert_eq!(row.local_parent_id, original.local_parent_id);
        }
        for operation in fixture.store()?.outgoing()? {
            let original = [&self.original_root, &self.original_child, &self.original_parent]
                .into_iter().find(|row| row.id == operation.blob.id)
                .context("forced outbox contains an unexpected memory")?;
            assert_eq!(operation.blob.ciphertext, original.ciphertext);
            assert_eq!(operation.blob.nonce, original.nonce);
        }
        Ok(())
    }
}

fn stop_native_commits_accepted_encrypted_write_after_drain() -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=fixture_requested");
    let mut fixture = Fixture::new()?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=fixture_ready");
    let write = StopWriteEvidence::seed(&fixture)?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=seed_ready");
    let health = fixture.start_standalone_for_stop()?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=runtime_ready");
    assert!(health["managed_id"].is_null());
    let mut provider = write.provider()?;
    let rpc_url = format!("http://127.0.0.1:{}/api/rpc", fixture.port);
    let stop_url = format!("http://127.0.0.1:{}/api/runtime/stop", fixture.port);
    let scope = json!({"account_id":health["account_id"],"instance_id":health["instance_id"],"generation":health["generation"]});
    let business_operation = "native-stop-successful-encrypted-write";
    let request = json!({"v":1,"id":"native-stop-held-write","method":"cli.exec",
        "operation_id":business_operation,"idempotency_key":business_operation,"scope":scope,
        "args":write.args(&provider.endpoint)});
    let (url, original) = (rpc_url.clone(), request.clone());
    let business = std::thread::spawn(move || stop_native_post(&url, &original));
    provider.received.recv_timeout(Duration::from_secs(30))
        .context("real Core causal classification did not reach the held provider")?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=provider_received");
    let held_at = std::time::Instant::now();
    let active = fixture.standalone_stop_health()?;
    assert!(active["control"]["admitted"].as_array().context("admission list missing")?.iter()
        .any(|item| item["operation_id"] == business_operation));
    let worker_pid = u32::try_from(active["inference"]["worker_pid"].as_u64().context("Core PID missing")?)?;
    let worker_identity = stop_native_worker_identity(worker_pid)?.context("Core worker not live")?;
    assert_eq!(Path::new(worker_identity["image"].as_str().context("Core image missing")?).canonicalize()?, fixture.runtime);
    if let Some(parent) = worker_identity.get("parent") { assert_eq!(*parent, health["pid"]); }
    assert!(active["inference"]["worker_pending_calls"].as_u64().is_some_and(|count| count > 0));
    assert!(active["inference"]["worker_host_callbacks_active"].as_u64().is_some_and(|count| count > 0));
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=identity_bound");
    std::thread::sleep(Duration::from_millis(4200));
    assert!(!business.is_finished());
    let business_before_stop_ms = held_at.elapsed().as_millis();
    assert!(business_before_stop_ms > 4000);
    fixture.assert_standalone_stop_lock_held()?;
    let identity = json!({"installation_id":health["installation_id"],"instance_id":health["instance_id"],
        "account_id":health["account_id"],"generation":health["generation"]});
    let (status, accepted) = stop_native_post(&stop_url, &identity)?;
    assert_eq!(status, 200);
    assert_eq!(accepted["state"], "accepted");
    assert!(accepted["deadline_ms"].is_null() && accepted["timeout_state"].is_null());
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=stop_accepted");
    let accepted_at = std::time::Instant::now();
    std::thread::sleep(Duration::from_millis(4200));
    let held = fixture.standalone_stop_health()?;
    assert_eq!(held["control"]["state"], "draining");
    assert_eq!(held["inference"]["worker_retired"], false);
    assert_eq!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
    assert!(!business.is_finished());
    fixture.assert_standalone_stop_lock_held()?;
    let business_held_after_stop_ms = accepted_at.elapsed().as_millis();
    assert!(business_held_after_stop_ms > 4000);
    let mut replay = request.clone();
    replay["id"] = json!("native-stop-held-write-same-key");
    let (_, pending) = stop_native_post(&rpc_url, &replay)?;
    assert_eq!(pending["code"], "request_in_progress");
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    provider.release.store(true, Ordering::Release);
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=provider_released");
    let (_, result) = business.join().map_err(|_| anyhow::anyhow!("accepted write client panicked"))??;
    write.assert_result(&result, &request);
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=business_completed");
    let receipt = fixture.finish_standalone_stop(&health, &accepted["operation_id"], false)?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=runtime_stopped");
    assert_eq!(receipt["unfinished"], json!([]));
    assert_ne!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_identity));
    write.assert_committed(&fixture)?;
    eprintln!("NATIVE_ACCEPTED_WRITE_CHECKPOINT stage=write_verified");
    provider.finish()?;
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    eprintln!("NATIVE_ACCEPTED_WRITE_RAW {}", json!({"schema":1,"evidence_class":"native-raw","gate":"not-evaluated",
        "startup_mode":"standalone","runtime_identity":health,"worker_identity":worker_identity,
        "active_health":active,"held_health":held,"business_before_stop_ms":business_before_stop_ms,
        "business_held_after_stop_ms":business_held_after_stop_ms,"accepted":accepted,
        "business_result":result,"final_receipt":receipt,"provider_http_status":200,
        "provider_requests":provider.calls.load(Ordering::Acquire),"same_key_result":pending,
        "encrypted_write_committed":true,"plaintext_parent_matches_encrypted_payload":true}));
    Ok(())
}

