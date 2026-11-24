#[test]
fn every_unfinished_phase_fences_requests_and_background_work() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
            match &self.0 {
                Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
                None => std::env::remove_var("RSRS_DATA_DIR"),
            }
            match &self.1 {
                Some(v) => std::env::set_var("RSRS_RUNTIME_BOOT_TOKEN", v),
                None => std::env::remove_var("RSRS_RUNTIME_BOOT_TOKEN"),
            }
        }
    }
    let _restore = Restore(
        std::env::var_os("RSRS_DATA_DIR"),
        std::env::var_os("RSRS_RUNTIME_BOOT_TOKEN"),
    );
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    anyhow::ensure!(!background_paused());
    for phase in [Phase::Paused, Phase::Starting, Phase::Verified] {
        let mut value = Marker {
            schema: 1,
            id: "synthetic-installation".into(),
            token: uuid::Uuid::new_v4().to_string(),
            phase,
            expected_profile: Some(dir.path().canonicalize()?),
            expected_version: env!("CARGO_PKG_VERSION").into(),
            owner_pid: std::process::id(),
            owner_birth: manager::process_birth(std::process::id())?
                .context("test process birth missing")?,
            snapshot: None,
            agent_config: None,
        };
        let path = marker_path(&respire_client_host::paths::main_data_dir());
        fs::create_dir_all(path.parent().context("test marker parent")?)?;
        fs::write(&path, serde_json::to_vec(&value)?)?;
        anyhow::ensure!(controller_alive(&value)?);
        anyhow::ensure!(background_paused() && check_direct_access().is_err());
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
        check_direct_access()?;
        let profile = dir.path().canonicalize()?;
        let bytes = fs::read(&path)?;
        let authorized = verified_account_marker(&value, &value, &profile, &bytes, &hash(&bytes));
        assert_eq!(authorized.is_ok(), phase == Phase::Verified);
        if phase == Phase::Verified {
            assert!(verified_account_marker(&value, &value, &profile.join("foreign"), &bytes, &hash(&bytes)).is_err());
            assert!(verified_account_marker(&value, &value, &profile, &bytes, &hash(b"changed marker")).is_err());
            let mut foreign = value.clone(); foreign.token = uuid::Uuid::new_v4().to_string();
            assert!(verified_account_marker(&value, &foreign, &profile, &bytes, &hash(&bytes)).is_err());
        }
        if phase == Phase::Paused {
            let binding = maintenance_binding(&profile, None, true)?
                .context("paused auth binding missing")?;
            anyhow::ensure!(binding["marker_sha256"] == hash(&fs::read(&path)?));
            anyhow::ensure!(launch_maintenance_binding(&profile, None).is_err());
            anyhow::ensure!(maintenance_binding(&profile.join("foreign"), None, true).is_err());
        } else {
            anyhow::ensure!(maintenance_binding(&profile, None, true).is_err());
            anyhow::ensure!(launch_maintenance_binding(&profile, None)?.is_some());
        }
        // Owning the controller token never authorizes background threads.
        anyhow::ensure!(background_paused());
        std::env::set_var("RSRS_RUNTIME_BOOT_TOKEN", &value.token);
        if phase == Phase::Paused {
            anyhow::ensure!(verify_managed_boot().is_err());
        } else {
            verify_managed_boot()?;
            std::env::set_var("RSRS_RUNTIME_BOOT_TOKEN", "foreign-controller");
            anyhow::ensure!(verify_managed_boot().is_err());
            std::env::set_var("RSRS_RUNTIME_BOOT_TOKEN", &value.token);
            let mut wrong = value.clone();
            wrong.expected_version.push_str("-foreign");
            fs::write(&path, serde_json::to_vec(&wrong)?)?;
            anyhow::ensure!(verify_managed_boot().is_err());
            wrong = value.clone();
            wrong.expected_profile = Some(dir.path().join("other-profile"));
            fs::write(&path, serde_json::to_vec(&wrong)?)?;
            anyhow::ensure!(verify_managed_boot().is_err());
        }
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
        value.owner_birth.push_str("-reused-pid");
        fs::write(&path, serde_json::to_vec(&value)?)?;
        anyhow::ensure!(!controller_alive(&value)?);
        anyhow::ensure!(verify_managed_boot().is_err());
        fs::remove_file(path)?;
    }
    let path = marker_path(&respire_client_host::paths::main_data_dir());
    fs::write(&path, b"malformed")?;
    anyhow::ensure!(background_paused() && check_direct_access().is_err());
    fs::remove_file(path)?;
    anyhow::ensure!(!background_paused());
    check_direct_access()?;
    let root = dir.path().canonicalize()?;
    let mut local = begin_maintenance_locked()?;
    #[cfg(feature = "client2-host-prototype")]
    {
        let bytes = fs::read(marker_path(&root))?;
        let token = current_boot_token();
        assert!(local.finish_handoff().is_err());
        assert_eq!(fs::read(marker_path(&root))?, bytes);
        assert_eq!(current_boot_token(), token);
        Maintenance::inactive().finish_handoff()?;
    }
    assert_eq!(local.installation_root()?, root);
    assert!(local.config.is_none() && !control_path(&root).exists());
    let first = marker(&root)?.context("local Paused marker missing")?;
    assert_eq!(first.phase, Phase::Paused);
    assert!(!first.id.is_empty());
    assert_eq!(local.auth_launch_binding(&root)?.context("local auth binding missing")?["marker_sha256"],
        hash(&fs::read(marker_path(&root))?));
    local.mark(Phase::Starting)?;
    assert_eq!(marker(&root)?.context("local Starting marker missing")?.expected_profile.as_deref(), Some(root.as_path()));
    local.mark(Phase::Verified)?;
    assert_eq!(marker(&root)?.context("local Verified marker missing")?.phase, Phase::Verified);
    local.finish()?;
    assert!(!marker_path(&root).exists() && !control_path(&root).exists());
    let mut interrupted = begin_maintenance_locked()?;
    interrupted.auth_launch_binding(&root)?;
    let token = current_boot_token().context("local maintenance token missing")?;
    #[cfg(feature = "client2-host-prototype")]
    {
        let bytes = fs::read(marker_path(&root))?;
        assert!(local.finish_handoff().is_err());
        assert_eq!(fs::read(marker_path(&root))?, bytes);
        assert_eq!(current_boot_token().as_deref(), Some(token.as_str()));
    }
    drop(local);
    assert_eq!(current_boot_token().as_deref(), Some(token.as_str()));
    let original = fs::read(marker_path(&root))?;
    drop(interrupted);
    assert!(background_paused() && current_boot_token().is_none());
    assert_eq!(fs::read(marker_path(&root))?, original);
    assert!(begin_maintenance_locked().is_err());
    let mut recovered = with_explicit_recovery(begin_maintenance_locked)?;
    recovered.auth_launch_binding(&root)?;
    let owned = fs::read(marker_path(&root))?;
    let mut foreign: Marker = serde_json::from_slice(&owned)?;
    foreign.token = uuid::Uuid::new_v4().to_string();
    fs::write(marker_path(&root), serde_json::to_vec(&foreign)?)?;
    assert!(recovered.finish().is_err() && marker_path(&root).exists());
    fs::write(marker_path(&root), owned)?;
    recovered.finish()?;
    let takeover = crate::runtime_policy::takeover_lock()?;
    for phase in [Phase::Paused, Phase::Starting, Phase::Verified] {
        use respire_protocol::maintenance_control::{LegacySnapshotRecoveryAction,
            LegacySnapshotRecoveryParams, LegacySnapshotRecoveryBinding, MAINTENANCE_CONTROL_SCHEMA_V};
        let mut maintenance = begin_maintenance_locked()?;
        let mut params = LegacySnapshotRecoveryParams {
            schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
            binding: respire_protocol::ControlBinding { installation_root: root.clone(),
                profile: root.clone(), launch_id: uuid::Uuid::new_v4().to_string(),
                expected_account_id: Some("a".repeat(64)) },
            recovery_operation_id: uuid::Uuid::new_v4().to_string(),
            request_fingerprint: String::new(), journal_sha256: "b".repeat(64),
            action: LegacySnapshotRecoveryAction::Query,
        };
        params.request_fingerprint = super::super::snapshot_control::legacy_fingerprint(&params)?;
        maintenance.marker.as_mut().context("test legacy marker missing")?.snapshot =
            Some(snapshot_control::SnapshotMarkerBinding {
                operation_id: String::new(), request_fingerprint: String::new(),
                sync_generation: 0, snapshot_id: String::new(), account_id: "a".repeat(64),
                drained_birth: String::new(), drain: None, offline: None, verified_runtime: None,
                legacy_recovery: Some(LegacySnapshotRecoveryBinding {
                    recovery_operation_id: params.recovery_operation_id.clone(),
                    request_fingerprint: params.request_fingerprint.clone(),
                    journal_sha256: params.journal_sha256.clone(), account_id: "a".repeat(64),
                }),
            });
        maintenance.mark(phase)?;
        let original = fs::read(marker_path(&root))?;
        let mut foreign = params.clone();
        foreign.recovery_operation_id = uuid::Uuid::new_v4().to_string();
        foreign.request_fingerprint = super::super::snapshot_control::legacy_fingerprint(&foreign)?;
        assert!(maintenance.legacy_pause_for_query(&takeover, &foreign, None).is_err());
        let library = respire_client_host::lock::LibraryLock::acquire(&root, Duration::ZERO)?;
        assert!(maintenance.legacy_pause_for_query(&takeover, &params, None).is_err());
        assert_eq!(fs::read(marker_path(&root))?, original);
        drop(library);
        maintenance.legacy_pause_for_query(&takeover, &params, None)?;
        let paused = marker(&root)?.context("test legacy Paused marker missing")?;
        assert_eq!(paused.phase, Phase::Paused);
        assert_eq!(paused.snapshot.as_ref().and_then(|bound| bound.legacy_recovery.as_ref())
            .context("test legacy intent missing")?.recovery_operation_id, params.recovery_operation_id);
        assert!(paused.snapshot.as_ref().is_some_and(|bound| bound.offline.is_some()
            && bound.drain.is_none() && bound.verified_runtime.is_none()));
        assert!(background_paused());
        maintenance.finish()?;
    }
    drop(takeover);
    for (enabled, stopped) in [(false, false), (true, true)] {
        let mut config = super::super::tests::fixture(Platform::current()?);
        config.owner = manager::owner()?;
        config.installation = root.clone();
        config.id = identity(&root, &config.owner);
        config.executable = root.join("bin").join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" });
        config.environment.insert("RSRS_RPC_PORT".into(), crate::net_rpc::rpc_port().to_string());
        config.enabled = enabled;
        config.manually_stopped = stopped;
        let original = serde_json::to_vec(&config)?;
        private_write(&control_path(&root), &original, None)?;
        let mut manual = begin_maintenance_locked()?;
        assert!(manual.config.is_none());
        manual.auth_launch_binding(&root)?;
        manual.mark(Phase::Starting)?;
        assert!(!ensure_managed_locked()?);
        assert_eq!(fs::read(control_path(&root))?, original);
        manual.finish()?;
        assert_eq!(fs::read(control_path(&root))?, original);
        assert!(!marker_path(&root).exists());
        if stopped { assert!(ensure_managed_locked().is_err()); }
        fs::remove_file(control_path(&root))?;
    }
    struct PortRestore(Option<std::ffi::OsString>);
    impl Drop for PortRestore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("RSRS_RPC_PORT", value),
                None => std::env::remove_var("RSRS_RPC_PORT"),
            }
        }
    }
    let _port = PortRestore(std::env::var_os("RSRS_RPC_PORT"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);
    let result = crate::autostart::control_agent_config("diary_mode=verbose")?;
    assert!(matches!(result.status, respire_commands::output::Status::Ok));
    assert!(!marker_path(&root).exists() && !root.join("bin").exists() && !root.join("rsrs.db").exists());
    let mut terminal_core = None;
    for intent in ["read_heat=false", "read_heat=true", "recall_fusion={\"fusion\":\"attention\"}"] {
        let saved = crate::autostart::control_agent_config(intent)?;
        assert!(matches!(saved.status, respire_commands::output::Status::Ok));
        assert_eq!(saved.summary["core_application"]["state"], "pending");
        assert_eq!(saved.summary["core_application"]["reason"], "runtime_not_running");
        assert!(!marker_path(&root).exists() && !root.join("bin").exists());
        terminal_core = Some(saved);
    }
    let terminal_core = terminal_core.context("terminal core receipt missing")?;
    let request_a = format!("desktop-{}", terminal_core.details["operation_id"].as_str().context("operation A missing")?);
    let saved = fs::read(root.join("agent.json"))?;
    let repeated = crate::autostart::control_core_config_rpc(&request_a, Some("recall_fusion={\"fusion\":\"attention\"}"))?;
    assert_eq!(serde_json::to_value(&repeated)?, serde_json::to_value(&terminal_core)?);
    assert!(crate::autostart::control_core_config_rpc(&request_a, Some("recall_fusion={\"fusion\":\"legacy\"}")).is_err());
    assert_eq!(fs::read(root.join("agent.json"))?, saved);
    assert!(!marker_path(&root).exists());
    for intent in ["read_heat=bad", "recall_fusion={\"unknown\":true}"] {
        assert!(crate::autostart::control_agent_config(intent).is_err());
        assert_eq!(fs::read(root.join("agent.json"))?, saved);
        assert!(!marker_path(&root).exists());
    }
    use respire_protocol::config_control::{AgentConfigIntent, AgentConfigMaintenance};
    let review = crate::client_config::review_agent_config(
        AgentConfigIntent { key: "diary_mode".into(), raw_value: "concise".into() }, root.clone(), root.clone())?;
    let mut interrupted = begin_maintenance_locked()?;
    interrupted.auth_launch_binding(&root)?;
    let original = AgentConfigMaintenance { review: review.clone(), was_serving: false, store_ready: false,
        sent: true, receipt: None };
    interrupted.save_agent_config(&original)?;
    crate::client_config::publish_agent_config(&review, || Ok(()))?;
    let candidate = fs::read(root.join("agent.json"))?;
    drop(interrupted);
    let error = crate::autostart::control_agent_config("diary_mode=concise").err()
        .context("sent agent config was replayed without its receipt")?;
    assert!(format!("{error:#}").contains("agent_config_outcome_unknown"));
    let (pending, _, _) = pending_agent_config(&root, false)?.context("original agent operation disappeared")?;
    assert!(pending == original && fs::read(root.join("agent.json"))? == candidate);
    assert!(marker(&root)?.context("agent maintenance disappeared")?.phase == Phase::Paused);
    let pending_b = fs::read(marker_path(&root))?;
    let queried = crate::autostart::control_core_config_rpc(&request_a, None)?;
    assert_eq!(serde_json::to_value(&queried)?, serde_json::to_value(&terminal_core)?);
    assert_eq!(fs::read(marker_path(&root))?, pending_b);
    assert_eq!(fs::read(root.join("agent.json"))?, candidate);
    let mut non_agent = marker(&root)?.context("B disappeared")?;
    non_agent.agent_config = None;
    fs::write(marker_path(&root), serde_json::to_vec(&non_agent)?)?;
    let non_agent_bytes = fs::read(marker_path(&root))?;
    let queried = crate::autostart::control_core_config_rpc(&request_a, None)?;
    assert_eq!(serde_json::to_value(&queried)?, serde_json::to_value(&terminal_core)?);
    assert_eq!(fs::read(marker_path(&root))?, non_agent_bytes);
    fs::write(marker_path(&root), &pending_b)?;
    fs::remove_file(marker_path(&root))?;
    use respire_protocol::config_control::{AgentConfigReceipt, AgentConfigOutcome};
    let review = crate::client_config::review_agent_config(
        AgentConfigIntent { key:"recall_fusion".into(), raw_value:"{\"fusion\":\"legacy\"}".into() }, root.clone(), root.clone())?;
    let scope = respire_protocol::RuntimeRequestScope {account_id:"synthetic".into(), instance_id:"original".into(), generation:1};
    let unknown = AgentConfigMaintenance {review:review.clone(), was_serving:false, store_ready:false, sent:true,
        receipt:Some(AgentConfigReceipt {review:review.clone(), outcome:AgentConfigOutcome::Unknown,
            result:json!({"core_control":{"scope":scope,"revision":0,"candidate":{},"validated":null,
                "apply_sent":false,"rollback_sent":false,"response":null}})})};
    let mut held = begin_maintenance_locked()?;
    held.save_agent_config(&unknown)?;
    drop(held);
    let request = format!("desktop-{}", review.operation_id);
    let pending_before = fs::read(marker_path(&root))?;
    // A result file can precede completion; matching pending remains authoritative.
    let terminal_path = root.join("runtime/autostart/core-config-results").join(format!("{}.json", review.operation_id));
    private_write(&terminal_path, &serde_json::to_vec(&terminal_core)?, None)?;
    let error = crate::autostart::control_core_config_rpc(&request, None).err().context("publication Unknown was completed")?;
    let details = &error.downcast_ref::<crate::autostart::CoreConfigUnknown>().context("Unknown metadata lost")?.details;
    assert_eq!(details["operation_id"], review.operation_id);
    assert_eq!(details["scope"], serde_json::to_value(&scope)?);
    assert_eq!(details["publication_state"], "unknown");
    let mut context = respire_commands::runtime_error::OperationContext {scope:scope.clone(), operation_id:review.operation_id.clone(), idempotency_key:review.operation_id.clone()};
    context.scope.generation += 1;
    let error = super::super::supervise::control_core_config_result(&context, &root).err().context("foreign scope accepted")?;
    assert!(error.downcast_ref::<crate::autostart::CoreConfigUnknown>().is_none());
    context.scope = scope;
    assert!(super::super::supervise::control_core_config_result(&context, &root).err()
        .context("publication unexpectedly completed")?.downcast_ref::<crate::autostart::CoreConfigUnknown>().is_some());
    assert_eq!(fs::read(marker_path(&root))?, pending_before);
    let mut without_scope = marker(&root)?.context("Unknown marker missing")?;
    without_scope.agent_config.as_mut().context("Unknown record missing")?["receipt"]["result"]["core_control"]["scope"] = Value::Null;
    fs::write(marker_path(&root), serde_json::to_vec(&without_scope)?)?;
    let no_scope_before = fs::read(marker_path(&root))?;
    let error = crate::autostart::control_core_config_rpc(&request, None).err().context("scope-free publication Unknown completed")?;
    let details = &error.downcast_ref::<crate::autostart::CoreConfigUnknown>().context("scope-free Unknown lost identity")?.details;
    assert_eq!(details["operation_id"], review.operation_id);
    assert!(details["scope"].is_null());
    assert_eq!(details["publication_state"], "unknown");
    assert_eq!(fs::read(marker_path(&root))?, no_scope_before);
    assert_eq!(fs::read(root.join("agent.json"))?, candidate);
    fs::remove_file(marker_path(&root))?;
    Ok(())
}
