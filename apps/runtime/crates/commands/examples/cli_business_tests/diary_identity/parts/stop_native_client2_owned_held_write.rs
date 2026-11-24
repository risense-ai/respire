fn stop_native_client2_owned_held_write() -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    use sha2::{Digest, Sha256};
    anyhow::ensure!(std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"),
        "Client2-owned native acceptance requires an ephemeral GitHub-hosted runner");
    let probe = PathBuf::from(std::env::var_os("RSRS_CLIENT2_PROBE_BIN")
        .context("supply the source-bound existing original Go Client2 probe executable")?).canonicalize()?;
    anyhow::ensure!(probe.is_file(), "original Client2 probe is not a file");
    // The producer must independently bind this binary to the authenticated Client2
    // source and the existing CLI probe fixtures; a caller-provided digest alone
    // is not a source certificate. Keep this raw report gate=not-evaluated.
    let probe_digest = format!("{:x}", Sha256::digest(std::fs::read(&probe)?));
    for mode in ["normal", "caller-timeout", "explicit-force"] {
        let checkpoint = |stage| owned_stop_checkpoint("client2-owned", mode, stage);
        checkpoint("start");
        let owned = prepare_client2_stop_fixture(mode)?;
        checkpoint("fixture_ready");
        let write = StopWriteEvidence::seed(owned.fixture()?)?;
        checkpoint("memory_seeded");
        let root = owned.fixture()?.home.path().join("data");
        let selected: Value = serde_json::from_slice(&std::fs::read(root.join("bin/runtime.json"))?)?;
        let version = selected["version"].as_str().context("installed Runtime version missing")?;
        let version_dir = owned.fixture()?.runtime.parent().context("Runtime bundle missing")?.to_path_buf();
        let target = root.join("client2/runtime").join(version);
        let mut pending = vec![(version_dir, target.clone())];
        while let Some((source, destination)) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&source)?;
            anyhow::ensure!(!metadata.file_type().is_symlink(), "Client2 fixture bundle contains a link");
            #[cfg(windows)] {
                use std::os::windows::fs::MetadataExt;
                anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "Client2 fixture contains a reparse point");
            }
            if metadata.is_dir() {
                std::fs::create_dir_all(&destination)?;
                for entry in std::fs::read_dir(&source)? { let entry = entry?; pending.push((entry.path(), destination.join(entry.file_name()))); }
            } else {
                anyhow::ensure!(metadata.is_file(), "Client2 fixture bundle contains a non-file");
                std::fs::copy(source, destination)?;
            }
        }
        // Original InstalledRuntime is exactly version+sha256. This is the
        // original launcher fixture installation shape, not an OS registration.
        std::fs::write(root.join("client2/runtime.json"), serde_json::to_vec(&json!({"version":version,"sha256":selected["sha256"]}))?)?;
        checkpoint("bundle_staged");
        let mut client2 = Client2OwnedStopFixture { owned: Some(owned), probe: probe.clone(), observed_runtime: None, observed_actors: Vec::new(), identity_observation_complete: false, start_attempted: false, completed: false };
        client2.start_attempted = true;
        checkpoint("start_requested");
        let started = client2.command("start")?.output()?;
        anyhow::ensure!(started.status.success(), "original Go Manager.Start failed");
        checkpoint("start_complete");
        let startup: Value = serde_json::from_slice(&started.stdout)?;
        for key in ["health_error", "diagnostic_error", "business_error", "catalog_error", "progress_error", "variants_error"] {
            assert_eq!(startup[key], "", "original Go startup/RPC failed: {key}");
        }
        let backend = client2.health()?;
        checkpoint("health_bound");
        let selected_cli = startup["selected_cli"].clone();
        let (backend_pid, backend_os) = client2.observe_actor(&backend, &selected_cli)?;
        for key in ["pid", "instance_id", "process_start_id", "image_path", "image_sha256", "installation_id", "account_id", "generation"] {
            anyhow::ensure!(startup["health"][key] == backend[key], "original Go startup Runtime identity mismatch");
        }
        client2.observed_runtime = Some(backend.clone());
        client2.observed_actors.push((backend_pid, backend_os.clone()));
        checkpoint("actors_bound");
        let mut provider = write.provider()?;
        let request = json!({"v":1,"id":uuid::Uuid::new_v4().to_string(),"method":"cli.exec",
            "operation_id":uuid::Uuid::new_v4().to_string(),"idempotency_key":uuid::Uuid::new_v4().to_string(),
            "scope":{"account_id":backend["account_id"],"instance_id":backend["instance_id"],"generation":backend["generation"]},
            "args":write.args(&provider.endpoint)});
        let rpc_url = format!("http://127.0.0.1:{}/api/rpc", client2.owned()?.fixture()?.port);
        let (url, original) = (rpc_url.clone(), request.clone());
        client2.identity_observation_complete = false;
        checkpoint("provider_requested");
        let business = std::thread::spawn(move || stop_native_post(&url, &original));
        provider.received.recv_timeout(Duration::from_secs(30)).context("actual Client2-owned SDK callback missing")?;
        checkpoint("provider_received");
        let active = client2.health()?;
        let worker_pid = u32::try_from(active["inference"]["worker_pid"].as_u64().context("Core worker missing")?)?;
        let worker_os = stop_native_worker_identity(worker_pid)?.context("actual Core worker missing")?;
        client2.observed_actors.push((worker_pid, worker_os.clone()));
        assert_ne!(worker_pid, backend_pid, "Core worker must be separate from its Runtime owner");
        let worker_generation = active["inference"]["worker_generation"].as_u64().context("Core worker generation missing")?;
        assert_eq!(Path::new(worker_os["image"].as_str().context("Core OS image missing")?).canonicalize()?, client2.runtime_image()?);
        client2.verify_parent(worker_pid, &worker_os, backend_pid)?;
        assert!(active["control"]["admitted"].as_array().context("admission missing")?.iter()
            .any(|item| item["operation_id"] == request["operation_id"]));
        assert!(active["inference"]["worker_host_callbacks_active"].as_u64().is_some_and(|n| n > 0));
        assert!(active["inference"]["worker_call_deadline_secs"].as_u64().is_some_and(|n| n >= 30));
        client2.identity_observation_complete = true;
        checkpoint("core_bound");
        let fixture = client2.owned()?.fixture()?;
        assert_owned_stop_lock_held(&root, backend_pid, &backend_os)?;
        std::thread::sleep(Duration::from_millis(4200)); assert!(!business.is_finished());
        checkpoint("lock_held_initial");
        let stop_output_path = root.join("original-go-stop.stdout");
        checkpoint("stop_requested");
        let mut stop = client2.command("stop")?.stdout(std::fs::File::create(&stop_output_path)?)
            .stderr(std::fs::File::create(root.join("original-go-stop.stderr"))?).spawn()?;
        checkpoint("stop_started");
        let started_stop = std::time::Instant::now();
        let deadline = started_stop + Duration::from_secs(15);
        let pending = loop {
            let health = client2.health()?;
            if health["control"]["state"] == "draining" { break health; }
            anyhow::ensure!(stop.try_wait()?.is_none() && std::time::Instant::now() < deadline, "original Go stop did not request drain");
            std::thread::sleep(Duration::from_millis(50));
        };
        let operation = pending["control"]["operation_id"].clone();
        anyhow::ensure!(operation.as_str().is_some_and(|id| !id.is_empty()), "original stop operation missing");
        checkpoint("drain_observed");
        let mut caller_timeout_ms = None;
        if mode == "normal" {
            std::thread::sleep(Duration::from_millis(4200)); assert!(stop.try_wait()?.is_none());
        } else {
            let deadline = started_stop + Duration::from_secs(40);
            loop {
                if let Some(status) = stop.try_wait()? {
                    assert!(!status.success(), "old caller unexpectedly completed while business was held"); break;
                }
                anyhow::ensure!(std::time::Instant::now() < deadline, "original Go caller did not return within its original timeout");
                std::thread::sleep(Duration::from_millis(100));
            }
            let stop_result: Value = serde_json::from_slice(&std::fs::read(&stop_output_path)?)?;
            assert!(stop_result["action_error"].as_str().context("Client2 caller timeout error missing")?
                .contains(STOP_UNCONFIRMED));
            assert!(started_stop.elapsed() >= Duration::from_secs(35));
            caller_timeout_ms = Some(started_stop.elapsed().as_millis());
        }
        checkpoint("caller_wait_complete");
        assert!(!business.is_finished());
        assert_eq!(stop_native_worker_identity(backend_pid)?.as_ref(), Some(&backend_os));
        assert_eq!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_os));
        let after_caller = client2.health()?;
        assert_eq!(after_caller["control"]["state"], "draining");
        assert_eq!(after_caller["inference"]["worker_retired"], false);
        assert_eq!(after_caller["inference"]["worker_pid"], worker_pid);
        assert_eq!(after_caller["inference"]["worker_generation"], worker_generation);
        let mut replay = request.clone(); replay["id"] = json!(uuid::Uuid::new_v4().to_string());
        let (_, same_key) = stop_native_post(&rpc_url, &replay)?;
        assert_eq!(same_key["code"], "request_in_progress"); assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        assert_owned_stop_lock_held(&root, backend_pid, &backend_os)?;
        checkpoint("lock_held_draining");
        let mut force_receipt = None;
        if mode == "explicit-force" {
            // The old Stop(bool) is ownedOnly, never force. This is a separate,
            // explicit modern request tied to the original stop operation/actor.
        checkpoint("force_requested");
            let force = json!({"installation_id":backend["installation_id"],"instance_id":backend["instance_id"],
                "account_id":backend["account_id"],"generation":backend["generation"],"operation_id":operation,"force":true});
            let (status, receipt) = stop_native_post(&format!("http://127.0.0.1:{}/api/runtime/stop", fixture.port), &force)?;
            assert_eq!(status, 200); assert_eq!(receipt["operation_id"], operation);
            assert_eq!(receipt["timeout_state"], "forced_timeout");
            assert!(receipt["deadline_ms"].as_u64().is_some_and(|ms| ms <= 4000));
            force_receipt = Some(receipt);
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                let health = client2.health()?;
                if health["inference"]["retired_worker"]["worker_reaped"] == true { break; }
                anyhow::ensure!(std::time::Instant::now() < deadline, "explicit force did not retire the original Core worker");
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_ne!(stop_native_worker_identity(worker_pid)?.as_ref(), Some(&worker_os));
            assert!(!business.is_finished());
            assert_owned_stop_lock_held(&root, backend_pid, &backend_os)?;
            checkpoint("core_reaped");
        }
        checkpoint("provider_release_requested");
        provider.release.store(true, Ordering::Release);
        let (_, result) = business.join().map_err(|_| anyhow::anyhow!("Client2-owned modern business thread panicked"))??;
        assert_eq!(result["id"], request["id"]); assert_eq!(result["operation_id"], request["operation_id"]);
        if mode == "explicit-force" { assert_eq!(result["ok"], false); }
        else { write.assert_result(&result, &request); }
        checkpoint("business_completed");
        if mode == "normal" {
            let deadline = std::time::Instant::now() + Duration::from_secs(40);
            loop {
                if let Some(status) = stop.try_wait()? { anyhow::ensure!(status.success(), "original normal Go Stop failed"); break; }
                anyhow::ensure!(std::time::Instant::now() < deadline, "original Go Stop did not complete after business");
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(&stop_output_path)?)?["stopped"], true);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if stop_native_worker_identity(backend_pid)?.as_ref() != Some(&backend_os)
                && stop_native_worker_identity(worker_pid)?.as_ref() != Some(&worker_os) { break; }
            anyhow::ensure!(std::time::Instant::now() < deadline, "Client2-owned host/backend/Core did not finish exit");
            std::thread::sleep(Duration::from_millis(100));
        }
        checkpoint("actors_exited");
        let original_receipt_path = root.join("runtime")
            .join(format!("stop-{}.json", backend["instance_id"].as_str().context("backend instance missing")?));
        let original_receipt_bytes = std::fs::read(&original_receipt_path)?;
        let final_receipt: Value = serde_json::from_slice(&original_receipt_bytes)?;
        assert_eq!(final_receipt["schema_version"], 1); assert_eq!(final_receipt["protocol_version"], 1);
        assert_eq!(final_receipt["operation_id"], operation);
        for key in ["installation_id","instance_id","pid","process_start_id","image_path","image_sha256","runtime_version","account_id","generation"] {
            assert_eq!(final_receipt[key], backend[key], "Client2-owned final receipt identity changed: {key}");
        }
        assert_eq!(final_receipt["state"], if mode == "explicit-force" { "forced_timeout" } else { "drained" });
        if mode == "explicit-force" {
            let unfinished = final_receipt["unfinished"].as_array().context("forced unfinished records missing")?;
            assert_eq!(unfinished.iter().filter(|item| item["operation_id"] == request["operation_id"] && item["result"] == "unknown").count(), 1);
        } else { assert_eq!(final_receipt["unfinished"], json!([])); }
        checkpoint("receipt_verified");
        anyhow::ensure!(!stop_native_listener_open(fixture.port, Duration::from_secs(1))?,
            "Client2-owned stopped port still has a local listener");
        let lock = respire::lock::LibraryLock::acquire(&root, Duration::from_secs(1))?; drop(lock);
        if mode == "explicit-force" { write.assert_uncommitted(fixture)?; }
        else { write.assert_committed(fixture)?; }
        checkpoint("write_verified");
        // Explicit recovery starts a new actor; querying the old fingerprint
        // cannot re-execute it or convert an old unknown result to success.
        client2.identity_observation_complete = false;
        client2.observed_runtime = None;
        checkpoint("recovery_requested");
        let restarted_output = client2.command("start")?.output()?;
        anyhow::ensure!(restarted_output.status.success(), "explicit original Go recovery start failed");
        checkpoint("recovery_started");
        let recovered_startup: Value = serde_json::from_slice(&restarted_output.stdout)?;
        let recovered = client2.health()?;
        assert_eq!(recovered["store_ready"], true);
        assert_ne!(recovered["instance_id"], backend["instance_id"]);
        anyhow::ensure!(recovered_startup["selected_cli"] == selected_cli,
            "Client2-owned recovery selected a different CLI");
        let (recovered_pid, recovered_os) = client2.observe_actor(&recovered, &recovered_startup["selected_cli"])?;
        for key in ["pid", "instance_id", "process_start_id", "image_path", "image_sha256", "installation_id", "account_id", "generation"] {
            anyhow::ensure!(recovered_startup["health"][key] == recovered[key], "original Go recovery Runtime identity mismatch");
        }
        client2.observed_runtime = Some(recovered.clone());
        client2.observed_actors.push((recovered_pid, recovered_os.clone()));
        assert_ne!(recovered_os, backend_os);
        let recovered_worker = match recovered["inference"].get("worker_pid") {
            None if recovered["inference"]["phase"] == "idle"
                && recovered["inference"]["host_recovery_required"] == false => None,
            Some(Value::Null) => None,
            Some(value) => {
                let pid = u32::try_from(value.as_u64().context("recovered Core PID is invalid")?)?;
                let identity = stop_native_worker_identity(pid)?.context("recovered Core worker not live")?;
                assert_ne!(pid, recovered_pid, "recovered Core worker must be separate from its Runtime owner");
                anyhow::ensure!(recovered["inference"]["worker_generation"].as_u64().is_some(),
                    "recovered Core worker generation missing");
                anyhow::ensure!(Path::new(identity["image"].as_str().context("recovered Core OS image missing")?).canonicalize()?
                    == client2.runtime_image()?, "recovered Core OS image mismatch");
                client2.verify_parent(pid, &identity, recovered_pid)?;
                client2.observed_actors.push((pid, identity.clone()));
                Some((pid, identity))
            }
            None => anyhow::bail!("recovered Core observation missing"),
        };
        client2.identity_observation_complete = true;
        checkpoint("recovery_bound");
        replay["id"] = json!(uuid::Uuid::new_v4().to_string());
        let (_, stale) = stop_native_post(&rpc_url, &replay)?;
        assert_eq!(stale["code"], "scope_mismatch"); assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        checkpoint("recovery_stop_requested");
        let recovered_stop = client2.command("stop")?.output()?;
        anyhow::ensure!(recovered_stop.status.success(), "explicit recovered actor did not stop normally");
        let recovered_receipt: Value = serde_json::from_slice(&std::fs::read(root.join("runtime")
            .join(format!("stop-{}.json", recovered["instance_id"].as_str().context("recovered instance missing")?)))?)?;
        assert_eq!(recovered_receipt["state"], "drained"); assert_eq!(recovered_receipt["unfinished"], json!([]));
        for key in ["installation_id","instance_id","pid","process_start_id","image_path","image_sha256","runtime_version","account_id","generation"] {
            assert_eq!(recovered_receipt[key], recovered[key], "recovered final receipt identity changed: {key}");
        }
        assert_ne!(stop_native_worker_identity(recovered_pid)?.as_ref(), Some(&recovered_os));
        if let Some((pid, identity)) = recovered_worker.as_ref() {
            assert_ne!(stop_native_worker_identity(*pid)?.as_ref(), Some(identity));
        }
        assert_eq!(std::fs::read(&original_receipt_path)?, original_receipt_bytes,
            "explicit recovery changed the original drained/unknown receipt");
        checkpoint("recovery_stopped");
        provider.finish()?; assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        // The listener is now closed, so owned cleanup only removes the exact
        // synthetic persistent credential; it does not send another stop.
        checkpoint("cleanup_requested");
        client2.cleanup()?;
        checkpoint("cleanup_complete");
        checkpoint("complete");
        eprintln!("NATIVE_CLIENT2_STOP_RAW {}", json!({"schema":1,"evidence_class":"native-raw","gate":"not-evaluated",
            "startup_mode":"client2-owned","actor_lane":"client2-cli-runtime-core","case":mode,"business_sender":"modern-harness","stop_sender":"original-go-client2",
            "probe_sha256":probe_digest,"runtime_identity":backend,"cli_control":selected_cli,
            "backend_os_identity":backend_os,"worker_os_identity":worker_os,"worker_generation":worker_generation,
            "recovered_backend_os_identity":recovered_os,
            "caller_timeout_ms":caller_timeout_ms,"runtime_compatibility_timeout_claimed":false,"after_caller_health":after_caller,
            "business_result":result,"same_key_result":same_key,"explicit_force_response":force_receipt,
            "final_receipt":final_receipt,"recovered_identity":recovered,"recovered_receipt":recovered_receipt,"stale_scope_result":stale,
            "provider_http_status":200,"encrypted_write_committed":mode != "explicit-force",
            "forced_operation_remains_unknown":mode == "explicit-force",
            "provider_requests":1,"credential_cleanup_verified":true,"passed":true}));
    }
    Ok(())
}

include!("../i18n/stop_messages.rs");
