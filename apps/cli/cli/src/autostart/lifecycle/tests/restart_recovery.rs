#[cfg(target_os = "linux")]
#[test]
fn explicit_restart_recovers_every_phase_and_clears_manual_stop() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Vec<(std::ffi::OsString, std::ffi::OsString)>);
    impl Restore {
        fn captures(key: &std::ffi::OsStr) -> bool {
            key.to_str().is_some_and(|key| {
                matches!(key, "HOME" | "USERPROFILE")
                    || ["RSRS_", "ONEMEMORY_", "RESPIRE_"]
                        .iter()
                        .any(|prefix| key.starts_with(prefix))
            })
        }
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
            for (key, _) in std::env::vars_os().filter(|(key, _)| Self::captures(key)) {
                std::env::remove_var(key);
            }
            for (key, value) in &self.0 {
                std::env::set_var(key, value);
            }
        }
    }
    let _restore = Restore(std::env::vars_os().filter(|(key, _)| Restore::captures(key)).collect());
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_BIN_DIR", root.join("bin"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);
    let mut config = super::super::tests::fixture(Platform::Linux);
    config.installation = root.clone();
    config.user_home = root.join("home");
    config.executable = root.join("bin/rsrs");
    config.id = identity(&root, &config.owner);
    config.environment.insert(
        "RSRS_RPC_PORT".into(),
        crate::net_rpc::rpc_port().to_string(),
    );
    config
        .environment
        .insert("RSRS_DATA_DIR".into(), root.display().to_string());
    config.environment.insert(
        "RSRS_BIN_DIR".into(),
        root.join("bin").display().to_string(),
    );
    fs::create_dir_all(root.join("bin"))?;
    fs::copy(std::env::current_exe()?, &config.executable)?;
    let runtime = crate::runtime_fetch::runtime_fetch_tests::install_lifecycle_fixture()?;
    std::env::remove_var("RSRS_RUNTIME_BIN");
    let birth =
        manager::process_birth(std::process::id())?.context("test process birth missing")?;
    let health = json!({
        "server": "respire", "bin": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(), "v": crate::rpc::PROTOCOL_V,
        "exe": runtime, "image_path": runtime, "data_dir": root,
        "installation_root": root, "installation_id": hash(root.display().to_string().as_bytes()),
        "managed_id": config.id, "process_start_id": format!("linux-starttime-{birth}")
    });
    verify_owned_runtime(&config, &serde_json::from_value(health.clone())?)?;
    for (field, foreign) in [
        ("exe", json!(config.executable)),
        ("image_path", json!(config.executable)),
        ("data_dir", json!(config.user_home)),
        ("installation_root", json!(config.user_home)),
        ("installation_id", json!("foreign-installation")),
        ("managed_id", json!("foreign-registration")),
        ("process_start_id", json!("linux-starttime-reused")),
        ("pid", json!(0)),
    ] {
        let mut changed = health.clone();
        changed[field] = foreign;
        anyhow::ensure!(
            verify_owned_runtime(&config, &serde_json::from_value(changed)?).is_err()
        );
    }
    config.enabled = true;
    let (_scope, state) = manager::test_adapter::install(&config);
    manager::install(&mut config)?;
    save(&mut config)?;
    let version = crate::runtime_fetch::pinned()?.required.clone();
    let installed = root.join("bin/runtime").join(&version);
    fs::create_dir_all(&installed)?;
    let inventory = runtime_inventory()?;
    anyhow::ensure!(inventory["channel"] == crate::runtime_fetch::default_runtime_channel_name()
        && inventory["installed"][0]["version"] == version
        && inventory["installed"][0]["current"] == true);
    anyhow::ensure!(install_runtime("runtime.install", Some(&runtime), None, None, None).is_err());
    anyhow::ensure!(install_runtime("runtime.install", None, Some("invalid-version"), None, None).is_err());
    anyhow::ensure!(install_runtime("runtime.install", Some(&runtime), Some(&version),
        Some("https://example.invalid"), None).is_err());

    // Recovery must revalidate the registered owner again at the cleanup boundary.
    {
        let _takeover = crate::runtime_policy::takeover_lock()?;
        let session = root.join("session.json");
        fs::write(&session, br#"{"user":"recovery-fixture-account"}"#)?;
        let instance = uuid::Uuid::new_v4().to_string();
        let registration = fs::read(control_path(&root))?;
        let grant = serde_json::to_string(&json!({
            "schema_version":1, "protocol_version":crate::rpc::PROTOCOL_V,
            "nonce":instance, "instance_id":instance, "installation_root":root,
            "installation_id":hash(root.display().to_string().as_bytes()), "profile":root,
            "account_id":hash(b"recovery-fixture-account"), "sync_generation":0,
            "image_path":runtime, "image_sha256":hash(b"synthetic runtime; never executed"),
            "supervisor_pid":u32::MAX-1, "supervisor_birth":"456", "managed_id":config.id,
            "registration_sha256":hash(&registration), "maintenance":null,
            "port":crate::net_rpc::rpc_port(), "issued_at_ms":1, "expires_at_ms":2,
        }))?;
        let receipt = serde_json::to_vec(&json!({
            "schema_version":2, "protocol_version":crate::rpc::PROTOCOL_V, "state":"consumed",
            "grant_json":grant, "grant_sha256":hash(grant.as_bytes()),
            "registration_json":std::str::from_utf8(&registration)?,
            "pid":u32::MAX, "process_start_id":"123",
        }))?;
        let launch = root.join("runtime/autostart/launch");
        fs::create_dir_all(&launch)?;
        let claim = launch.join(format!("launch-{instance}.claimed"));
        fs::write(&claim, &receipt)?;
        let endpoint = crate::rpc::endpoint_path();
        let record = serde_json::to_vec(&json!({
            "schema_version":2, "server":"respire", "v":crate::rpc::PROTOCOL_V,
            "bin":version, "runtime_version":version, "url":crate::net_rpc::rpc_base_url(),
            "pid":u32::MAX, "process_start_id":"linux-starttime-123", "instance_id":instance,
            "installation_root":root, "data_dir":root,
            "installation_id":hash(root.display().to_string().as_bytes()),
            "account_id":hash(b"recovery-fixture-account"), "generation":7,
            "exe":runtime, "image_path":runtime, "image_sha256":hash(b"synthetic runtime; never executed"),
            "managed_id":config.id, "launch_receipt_sha256":hash(&receipt),
        }))?;
        fs::write(&endpoint, &record)?;
        state.borrow_mut().running = true;
        anyhow::ensure!(super::super::StaleOwner::prepare_locked().is_err());
        state.borrow_mut().running = false;
        let listener = std::net::TcpListener::bind(("127.0.0.1", crate::net_rpc::rpc_port()))?;
        anyhow::ensure!(super::super::StaleOwner::prepare_locked().is_err());
        drop(listener);
        let stale = super::super::StaleOwner::prepare_locked()?;
        let mut changed = config.clone();
        changed.manually_stopped = true;
        save(&mut changed)?;
        anyhow::ensure!(stale.remove_endpoint_locked().is_err());
        anyhow::ensure!(fs::read(&endpoint)? == record && fs::read(&claim)? == receipt);
        let rejected = super::super::StaleOwner::prepare_locked()
            .err().context("manual stop unexpectedly passed ordinary recovery")?;
        anyhow::ensure!(format!("{rejected:#}").contains("registration changed since the consumed launch"));
        anyhow::ensure!(super::super::StaleOwner::prepare_restart_locked().is_err());

        let health: Value = serde_json::from_slice(&record)?;
        let forced = json!({
            "schema_version":1, "protocol_version":crate::rpc::PROTOCOL_V,
            "operation_id":"stop-id-unknown-to-restart", "state":"forced_timeout",
            "installation_id":health["installation_id"], "instance_id":health["instance_id"],
            "pid":health["pid"], "process_start_id":health["process_start_id"],
            "image_path":health["image_path"], "image_sha256":health["image_sha256"],
            "runtime_version":health["runtime_version"], "account_id":health["account_id"],
            "generation":health["generation"],
            "unfinished":[{"operation_id":"unknown-write", "result":"unknown"}],
        });
        let stop_path = root.join("runtime").join(format!("stop-{instance}.json"));
        for (field, invalid) in [
            ("schema_version", json!(2)), ("protocol_version", json!(0)),
            ("operation_id", json!("")), ("state", json!("drained")),
            ("installation_id", json!("foreign")), ("instance_id", json!("foreign")),
            ("pid", json!(std::process::id())), ("process_start_id", json!("reused")),
            ("image_path", json!("foreign")), ("image_sha256", json!("foreign")),
            ("runtime_version", json!("foreign")), ("account_id", json!("foreign")),
            ("generation", json!(8)),
            ("unfinished", json!([{"operation_id":"unknown-write", "result":"completed"}])),
            ("unfinished", json!([{"operation_id":"", "result":"unknown"}])),
        ] {
            let mut invalid_receipt = forced.clone();
            invalid_receipt[field] = invalid;
            fs::write(&stop_path, serde_json::to_vec(&invalid_receipt)?)?;
            anyhow::ensure!(super::super::StaleOwner::prepare_restart_locked().is_err(),
                "accepted invalid forced stop field {field}");
            anyhow::ensure!(fs::read(&endpoint)? == record && fs::read(&claim)? == receipt);
        }
        let mut aborted = forced.clone();
        aborted["unfinished"] = json!([{"operation_id":"aborted-write", "result":"aborted"}]);
        fs::write(&stop_path, serde_json::to_vec(&aborted)?)?;
        super::super::StaleOwner::prepare_restart_locked()?;
        let forced_bytes = serde_json::to_vec(&forced)?;
        fs::write(&stop_path, &forced_bytes)?;
        state.borrow_mut().running = true;
        let error = super::super::StaleOwner::prepare_restart_locked()
            .err().context("running supervisor was accepted")?;
        anyhow::ensure!(format!("{error:#}").contains("registered supervisor still reports running"));
        state.borrow_mut().running = false;
        let listener = std::net::TcpListener::bind(("127.0.0.1", crate::net_rpc::rpc_port()))?;
        let error = super::super::StaleOwner::prepare_restart_locked()
            .err().context("active listener was accepted")?;
        anyhow::ensure!(format!("{error:#}").contains("loopback listener is alive"));
        drop(listener);
        let unconsumed = claim.with_file_name(format!("launch-{instance}.json"));
        fs::write(&unconsumed, grant.as_bytes())?;
        let error = super::super::StaleOwner::prepare_restart_locked()
            .err().context("unconsumed launch authorization was accepted")?;
        anyhow::ensure!(format!("{error:#}").contains("consumed launch authorization is still present"));
        fs::remove_file(&unconsumed)?;
        struct LiveChild(std::process::Child);
        impl Drop for LiveChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = LiveChild(std::process::Command::new("sleep").arg("300").spawn()?);
        let live_pid = child.0.id();
        let live_birth = manager::process_birth(live_pid)?.context("live child birth missing")?;
        for live_runtime in [true, false] {
            let mut live_receipt: Value = serde_json::from_slice(&receipt)?;
            let mut live_health = health.clone();
            let mut live_stop = forced.clone();
            if live_runtime {
                live_receipt["pid"] = json!(live_pid);
                live_receipt["process_start_id"] = json!(live_birth);
                live_health["pid"] = json!(live_pid);
                live_health["process_start_id"] = json!(format!("linux-starttime-{live_birth}"));
                live_stop["pid"] = live_health["pid"].clone();
                live_stop["process_start_id"] = live_health["process_start_id"].clone();
            } else {
                let mut live_grant: Value = serde_json::from_str(&grant)?;
                live_grant["supervisor_pid"] = json!(live_pid);
                live_grant["supervisor_birth"] = json!(live_birth);
                let live_grant = serde_json::to_string(&live_grant)?;
                live_receipt["grant_sha256"] = json!(hash(live_grant.as_bytes()));
                live_receipt["grant_json"] = json!(live_grant);
            }
            let live_receipt = serde_json::to_vec(&live_receipt)?;
            live_health["launch_receipt_sha256"] = json!(hash(&live_receipt));
            let live_health = serde_json::to_vec(&live_health)?;
            fs::write(&claim, &live_receipt)?;
            fs::write(&endpoint, &live_health)?;
            fs::write(&stop_path, serde_json::to_vec(&live_stop)?)?;
            let error = super::super::StaleOwner::prepare_restart_locked()
                .err().context("live process was accepted")?;
            anyhow::ensure!(format!("{error:#}").contains("old runtime or supervisor is still alive"));
            anyhow::ensure!(fs::read(&endpoint)? == live_health && fs::read(&claim)? == live_receipt);
        }
        fs::write(&claim, &receipt)?;
        fs::write(&endpoint, &record)?;
        fs::write(&stop_path, &forced_bytes)?;
        let stale = super::super::StaleOwner::prepare_restart_locked()?;
        fs::write(&stop_path, b"changed after verification")?;
        anyhow::ensure!(stale.remove_endpoint_locked().is_err());
        anyhow::ensure!(fs::read(&endpoint)? == record && fs::read(&claim)? == receipt);
        fs::write(&stop_path, &forced_bytes)?;
        let stale = super::super::StaleOwner::prepare_restart_locked()?;
        let mut recovering = restart_maintenance_locked()?;
        stale.remove_endpoint_locked()?;
        recovering.finish()?;
        anyhow::ensure!(!endpoint.exists() && fs::read(&claim)? == receipt
            && fs::read(&stop_path)? == forced_bytes && !root.join("rsrs.db").exists());
        fs::write(&endpoint, &record)?;
        fs::write(control_path(&root), &registration)?;
        super::super::StaleOwner::prepare_locked()?.remove_endpoint_locked()?;
        anyhow::ensure!(!endpoint.exists() && fs::read(&claim)? == receipt && !root.join("rsrs.db").exists());
        fs::remove_file(session)?;
    }
    let changes = state.borrow().changes.clone();
    config.enabled = false;
    config.manually_stopped = true;
    save(&mut config)?;
    anyhow::ensure!(!ensure_managed_locked()? && !stop_managed_locked()?);
    run_foreground(Some(&config.id), Some(&root), false)?;
    std::env::set_var("RSRS_RUNTIME_BIN", root.join("missing-runtime"));
    anyhow::ensure!(start_explicit_locked(&crate::runtime_policy::takeover_lock()?).is_err());
    std::env::remove_var("RSRS_RUNTIME_BIN");
    config = read_registration(&root)?.context("disabled registration disappeared")?;
    anyhow::ensure!(!config.enabled && !config.manually_stopped
        && state.borrow().changes == changes);
    config.enabled = true;
    save(&mut config)?;

    // Stop at real boot fences, before signal handlers or runtime_entry can run.
    let mut paused = Maintenance::begin(config.clone(), false)?;
    anyhow::ensure!(run_foreground(Some(&config.id), None, false).is_err());
    anyhow::ensure!(run_foreground(Some("foreign"), Some(&root), false).is_err());
    anyhow::ensure!(run_foreground(Some(&config.id), Some(&root), false).is_err());
    let executable = fs::read(&config.executable)?;
    fs::remove_file(&config.executable)?;
    std::os::unix::fs::symlink(std::env::current_exe()?, &config.executable)?;
    for key in ["RSRS_UNREGISTERED", "ONEMEMORY_UNREGISTERED", "RESPIRE_UNREGISTERED"] {
        std::env::set_var(key, "must-not-reach-the-child");
    }
    run_foreground(Some(&config.id), Some(&root), false)?;
    for key in ["RSRS_UNREGISTERED", "ONEMEMORY_UNREGISTERED", "RESPIRE_UNREGISTERED"] {
        anyhow::ensure!(std::env::var_os(key).is_none());
    }
    anyhow::ensure!(std::env::var_os("HOME").as_deref() == Some(config.user_home.as_os_str())
        && std::env::var_os("USERPROFILE").as_deref() == Some(config.user_home.as_os_str()));
    paused.mark(Phase::Starting)?;
    let now = now_seconds()?;
    for _ in 0..3 {
        anyhow::ensure!(record_attempt(&config, now)?);
    }
    run_foreground(Some(&config.id), Some(&root), false)?;
    anyhow::ensure!(managed_id().is_none() && read_attempts(&config)?.0.starts.len() == 3);
    let events = fs::read_to_string(control_path(&root).with_file_name("events.jsonl"))?;
    anyhow::ensure!(events.contains("\"event\":\"paused\"") && events.contains("\"event\":\"crash_limit\""));
    paused.finish()?;
    config = read_registration(&root)?.context("paused registration disappeared")?;
    fs::remove_file(&config.executable)?;
    fs::write(&config.executable, executable)?;
    reset_attempts(&config)?;
    state.borrow_mut().running = true;
    let error = ensure_managed_locked()
        .err()
        .context("maintenance without owner proof unexpectedly succeeded")?;
    anyhow::ensure!(format!("{error:#}").contains("manager stop is blocked")
        && state.borrow().running && background_paused());
    state.borrow_mut().running = false;
    let mut recovered = restart_maintenance_locked()?;
    recovered.finish()?;
    config = read_registration(&root)?.context("recovered registration disappeared")?;
    let cold_changes = state.borrow().changes.clone();
    raw_stop_owned(&config)?;
    anyhow::ensure!(
        !state.borrow().running && state.borrow().changes == cold_changes,
        "cold start attempted to stop an absent owner"
    );
    // Model the interval after a verified child drain but before supervisor exit.
    state.borrow_mut().running = true;
    anyhow::ensure!(
        raw_stop_owned(&config).is_err() && state.borrow().changes == cold_changes,
        "an active supervisor without drain proof was stopped"
    );
    let mut maintenance = Maintenance::begin(config.clone(), false)?;
    let mut foreign = health.clone();
    foreign["managed_id"] = json!("foreign-registration");
    anyhow::ensure!(
        maintenance.drain_runtime(&serde_json::from_value(foreign)?).is_err()
            && state.borrow().running,
        "profile maintenance stopped a foreign owner"
    );
    let before_stop = state.borrow().changes.clone();
    state.borrow_mut().fail = Some("stop".into());
    anyhow::ensure!(
        stop_drained_supervisor(&config).is_err() && state.borrow().running,
        "failed supervisor stop permitted replacement"
    );
    state.borrow_mut().fail = None;
    stop_drained_supervisor(&config)?;
    let after_stop = state.borrow().changes.clone();
    anyhow::ensure!(!state.borrow().running && after_stop.len() == before_stop.len() + 1);
    raw_stop_owned(&config)?;
    anyhow::ensure!(
        state.borrow().changes == after_stop,
        "a drained and stopped supervisor required a second health proof"
    );
    maintenance.finish()?;
    let cold_changes = state.borrow().changes.clone();
    let endpoint = crate::rpc::endpoint_path();
    fs::create_dir_all(endpoint.parent().context("test endpoint parent missing")?)?;
    anyhow::ensure!(!endpoint.exists());
    fs::write(&endpoint, br#"{"v":1,"pid":0}"#)?;
    anyhow::ensure!(
        raw_stop_owned(&config).is_err()
            && endpoint.exists()
            && state.borrow().changes == cold_changes,
        "unverified stopped owner was reclaimed"
    );
    fs::remove_file(endpoint)?;
    for phase in [Phase::Paused, Phase::Starting, Phase::Verified] {
        let current = read_registration(&root)?.context("test registration missing")?;
        let mut interrupted = Maintenance::begin(current, false)?;
        interrupted.mark(phase)?;
        let old_token = interrupted
            .marker
            .as_ref()
            .context("test marker missing")?
            .token
            .clone();
        drop(interrupted);
        anyhow::ensure!(check_direct_access().is_err() && background_paused());
        let current = read_registration(&root)?.context("test registration missing")?;
        anyhow::ensure!(Maintenance::begin(current, false).is_err());
        let mut recovered = restart_maintenance_locked()?;
        anyhow::ensure!(
            recovered
                .marker
                .as_ref()
                .context("test recovery marker missing")?
                .token
                != old_token
        );
        authorize_detached_boot_locked()?;
        recovered.finish()?;
        anyhow::ensure!(marker(&root)?.is_none() && !background_paused());
    }
    state.borrow_mut().running = true;
    let changes = state.borrow().changes.clone();
    let blocked = stop_managed_locked()
        .err()
        .context("missing health permitted manager stop")?;
    anyhow::ensure!(format!("{blocked:#}").contains("manager stop is blocked"));
    anyhow::ensure!(state.borrow().running && state.borrow().changes == changes);
    anyhow::ensure!(background_paused());
    state.borrow_mut().running = false;
    let mut recovered = restart_maintenance_locked()?;
    recovered.finish()?;
    let mut stopped = read_registration(&root)?.context("test registration missing")?;
    stopped.manually_stopped = true;
    save(&mut stopped)?;
    anyhow::ensure!(
        read_registration(&root)?
            .context("test registration missing")?
            .manually_stopped
    );
    let unchanged = fs::read(control_path(&root))?;
    state.borrow_mut().dropin = true;
    anyhow::ensure!(restart_maintenance_locked().is_err() && start_explicit_locked(&crate::runtime_policy::takeover_lock()?).is_err());
    anyhow::ensure!(fs::read(control_path(&root))? == unchanged && marker(&root)?.is_none());
    state.borrow_mut().dropin = false;
    let original_port = std::env::var("RSRS_RPC_PORT")?;
    let different = if original_port == "65534" {
        "65533"
    } else {
        "65534"
    };
    std::env::set_var("RSRS_RPC_PORT", different);
    anyhow::ensure!(restart_maintenance_locked().is_err() && start_explicit_locked(&crate::runtime_policy::takeover_lock()?).is_err());
    anyhow::ensure!(fs::read(control_path(&root))? == unchanged && marker(&root)?.is_none());
    std::env::set_var("RSRS_RPC_PORT", original_port);
    let mut restarted = restart_maintenance_locked()?;
    anyhow::ensure!(
        !read_registration(&root)?
            .context("test registration missing")?
            .manually_stopped
    );
    restarted.finish()?;
    // Failure after marking verified must retain a fence, not silently let
    // new background or direct work run against a partially committed plan.
    let current = read_registration(&root)?.context("test registration missing")?;
    let mut interrupted = Maintenance::begin(current, false)?;
    interrupted.mark(Phase::Starting)?;
    let path = control_path(&root);
    let mut bytes = fs::read(&path)?;
    bytes.push(b' ');
    fs::write(path, bytes)?;
    anyhow::ensure!(interrupted.finish().is_err());
    drop(interrupted);
    anyhow::ensure!(background_paused() && check_direct_access().is_err());
    let mut recovered = restart_maintenance_locked()?;
    recovered.finish()?;
    anyhow::ensure!(!background_paused());
    // Recreate a running owner without health before checking the stop fence.
    state.borrow_mut().running = true;
    state.borrow_mut().fail = Some("enable".into());
    let failed = enable(false)
        .err()
        .context("backend registration failure was not returned")?;
    anyhow::ensure!(
        format!("{failed:#}").contains("manager stop is blocked"),
        "missing health did not block before backend registration"
    );
    anyhow::ensure!(
        marker(&root)?.is_some()
            && !read_registration(&root)?
                .context("failed registration missing")?
                .enabled
    );
    state.borrow_mut().fail = None;
    let changes = state.borrow().changes.clone();
    let blocked = disable(true, true)
        .err()
        .context("missing health permitted removal")?;
    anyhow::ensure!(format!("{blocked:#}").contains("manager stop is blocked"));
    anyhow::ensure!(
        marker(&root)?.is_some()
            && manager::inspect(
                &read_registration(&root)?.context("preserved registration missing")?
            )?
            .registered
            && state.borrow().changes == changes
    );
    Ok(())
}
