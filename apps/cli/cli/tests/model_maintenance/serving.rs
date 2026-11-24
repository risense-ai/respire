#[test]
fn anonymous_owner_cpu_reset_drains_and_restores_verified_runtime() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let original = runtime.start()?;
    let (output, result) = runtime.fixture.run("reset-cpu")?;
    anyhow::ensure!(output.status.success(), "owned CPU reset failed: {result}");
    assert_eq!(result["command"], "model");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["errors"], json!([]));
    // Manual reset intentionally stops, rather than automatically resurrecting,
    // an unregistered owner. Restore explicitly using the public lifecycle API.
    assert_eq!(
        result["summary"],
        json!({"engine":"cpu", "force_cpu":true,
        "runtime_restarted":false, "stopped_pid":original.pid})
    );
    let receipt: ModelMaintenanceReceipt = serde_json::from_value(result["details"].clone())?;
    assert_eq!(receipt.plan.kind, ModelMaintenanceKind::ResetCpu);
    assert_eq!(receipt.phase, ModelMaintenancePhase::Apply);
    assert!(receipt.config_applied && receipt.verified && receipt.warning.is_none());
    let bytes = std::fs::read(runtime.fixture.root.join("inference.json"))?;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes)?,
        json!({"engine":"cpu","model":"q4","force_cpu":true})
    );
    assert_eq!(
        receipt.plan.cpu_candidate_sha256.as_deref(),
        Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
    );
    assert!(!runtime.fixture.marker().exists());
    assert!(!runtime.fixture.model.exists());
    runtime.verify_drained(&original)?;
    assert!(!net_rpc::port_is_open_on(runtime.fixture.port));
    runtime.cleanup_required = false;

    let restored = runtime.start()?;
    assert_ne!(restored.instance_id, original.instance_id);
    assert_ne!(
        (restored.pid, &restored.process_start_id),
        (original.pid, &original.process_start_id)
    );
    assert_eq!(restored.account_id, original.account_id);
    assert_eq!(restored.installation_id, original.installation_id);
    assert_eq!(restored.image_sha256, original.image_sha256);
    assert!(!original.store_ready || restored.store_ready);
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("inference.json"))?,
        bytes
    );
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("bin/runtime.json"))?,
        runtime.fixture.selection
    );
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("client.json"))?,
        runtime.fixture.client
    );
    assert_eq!(
        std::fs::read(runtime.fixture.profile.join("keep.txt"))?,
        b"synthetic profile marker"
    );
    assert!(!runtime.fixture.profile.join("session.json").exists());
    assert!(!runtime
        .fixture
        .root
        .join("runtime/autostart/registration.json")
        .exists());
    // Uninstall does automatically restore a previously serving manual owner.
    // The owned files are deliberately not loadable models; no backend is claimed.
    std::fs::create_dir_all(&runtime.fixture.model)?;
    std::fs::write(
        runtime.fixture.model.join("owned-marker"),
        b"synthetic removable bytes",
    )?;
    let sibling = runtime.fixture.root.join("models/keep.txt");
    std::fs::write(&sibling, b"unrelated model marker")?;
    let (output, result) = runtime.fixture.run("uninstall-m3")?;
    anyhow::ensure!(
        output.status.success(),
        "owned model uninstall failed: {result}"
    );
    assert_eq!(result["status"], "ok");
    assert_eq!(result["errors"], json!([]));
    assert_eq!(
        result["summary"],
        json!({"dir":runtime.fixture.model,"removed":true,"model":"bge-m3"})
    );
    let receipt: ModelMaintenanceReceipt = serde_json::from_value(result["details"].clone())?;
    assert_eq!(receipt.plan.kind, ModelMaintenanceKind::UninstallM3);
    assert_eq!(receipt.phase, ModelMaintenancePhase::Verify);
    assert!(receipt.verified && !receipt.config_applied && receipt.warning.is_none());
    assert!(!runtime.fixture.model.exists());
    assert_eq!(std::fs::read(&sibling)?, b"unrelated model marker");
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("inference.json"))?,
        bytes
    );
    assert!(!runtime.fixture.marker().exists());
    runtime.verify_drained(&restored)?;
    let automatic = runtime.health()?;
    assert_ne!(automatic.instance_id, restored.instance_id);
    assert_ne!(
        (automatic.pid, &automatic.process_start_id),
        (restored.pid, &restored.process_start_id)
    );
    assert_eq!(automatic.account_id, restored.account_id);
    assert_eq!(automatic.installation_id, restored.installation_id);
    assert_eq!(automatic.image_sha256, restored.image_sha256);
    assert!(!restored.store_ready || automatic.store_ready);
    runtime.last_owner = Some(automatic);
    runtime.stop()?;
    assert!(!runtime.fixture.marker().exists());
    assert!(!runtime.fixture.profile.join("session.json").exists());
    assert!(!runtime
        .fixture
        .root
        .join("runtime/autostart/registration.json")
        .exists());
    Ok(())
}

#[test]
fn anonymous_owner_secret_reads_restore_without_unlocking_credentials() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let mut owner = runtime.start()?;
    for args in [vec!["secret"], vec!["secret", "--reveal"]] {
        let (output, result) = runtime.fixture.command(&args)?;
        anyhow::ensure!(
            output.status.success(),
            "anonymous secret read failed: {result}"
        );
        assert_eq!(result["command"], "secret");
        assert_eq!(result["status"], "skip");
        assert_eq!(result["summary"], json!({"session":false}));
        assert_eq!(result["errors"], json!([]));
        assert!(result["details"].is_null());
        owner = runtime.restored_after(&owner)?;
        runtime.assert_anonymous_selection_unchanged()?;
    }
    runtime.stop()?;
    Ok(())
}

#[test]
fn anonymous_owner_metadata_disable_commits_and_restores_the_original_scope() -> Result<()> {
    use respire_protocol::metadata_control::{
        HostMetadataIdentity, HostMetadataIntent, HostMetadataOperation,
    };
    let mut runtime = OwnedRuntime::new()?;
    let owner = runtime.start()?;
    assert!(runtime.fixture.profile.join("rsrs.db").is_file());
    // Public startup creates the legitimate empty database. No session or fake
    // metadata receipt is installed, and no enable/backup success is claimed.
    let (status, before) = runtime.fixture.command(&["metadata", "status"])?;
    anyhow::ensure!(
        status.status.success(),
        "initial metadata status failed: {before}"
    );
    assert_eq!(before["summary"]["enabled"], false);
    let (output, result) = runtime.fixture.command(&["metadata", "disable"])?;
    anyhow::ensure!(
        output.status.success(),
        "anonymous metadata disable failed: {result}"
    );
    assert_eq!(result["command"], "metadata");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["errors"], json!([]));
    assert_eq!(
        result["summary"],
        json!({"action":"disabled","enabled":false})
    );
    assert_eq!(
        result["details"],
        json!({"enabled":false,"mode":"isolated",
        "metadata_retained":false,"sync_isolation_retained":true})
    );
    let record_path = runtime
        .fixture
        .root
        .join("runtime/autostart/metadata")
        .join(format!(
            "{:x}.json",
            Sha256::digest(runtime.fixture.profile.display().to_string().as_bytes())
        ));
    let record_bytes = std::fs::read(&record_path)?;
    let record: Value = serde_json::from_slice(&record_bytes)?;
    assert_eq!(record["sent"], true);
    assert_eq!(record["finished"], true);
    assert_eq!(record["was_serving"], true);
    let original: HostMetadataOperation = serde_json::from_value(record["operation"].clone())?;
    assert_eq!(
        uuid::Uuid::parse_str(&original.operation_id)?.get_version_num(),
        4
    );
    assert_eq!(original.key, original.operation_id);
    assert_eq!(original.installation_root, runtime.fixture.root);
    assert_eq!(original.profile, runtime.fixture.profile);
    assert!(original.intent == HostMetadataIntent::Disable);
    assert!(
        original.identity
            == HostMetadataIdentity {
                account_id: None,
                session_sha256: None
            }
    );
    assert_eq!(
        original.fingerprint,
        format!("{:x}", Sha256::digest(original.fingerprint_bytes()?))
    );
    runtime.restored_after(&owner)?;
    let (status, after) = runtime.fixture.command(&["metadata", "status"])?;
    anyhow::ensure!(
        status.status.success(),
        "committed metadata status failed: {after}"
    );
    assert_eq!(after["summary"]["enabled"], false);
    assert_eq!(std::fs::read(&record_path)?, record_bytes);
    runtime.assert_anonymous_selection_unchanged()?;
    runtime.stop()?;
    Ok(())
}

#[test]
fn anonymous_owner_agent_setting_is_persisted_without_changing_other_fields() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let agent = runtime.fixture.profile.join("agent.json");
    std::fs::write(
        &agent,
        br#"{"keep":"synthetic unrelated setting","diary_mode":"concise"}"#,
    )?;
    let owner = runtime.start()?;
    let (output, result) =
        runtime
            .fixture
            .command(&["agent-config", "--set", "diary_mode=verbose"])?;
    anyhow::ensure!(
        output.status.success(),
        "anonymous agent setter failed: {result}"
    );
    assert_eq!(result["command"], "agent-config");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["summary"]["action"], "updated");
    assert_eq!(result["summary"]["key"], "diary_mode");
    assert_eq!(result["summary"]["value"], "verbose");
    assert_eq!(result["errors"], json!([]));
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&agent)?)?,
        json!({"keep":"synthetic unrelated setting","diary_mode":"verbose"})
    );
    let restored = runtime.restored_after(&owner)?;
    runtime.assert_anonymous_selection_unchanged()?;
    let committed = std::fs::read(&agent)?;
    // Host input rejection neither publishes bytes nor restarts the verified owner.
    let (output, rejected) = runtime.fixture.command(
        &["agent-config", "--set", "read_heat=not-a-bool"])?;
    assert_eq!(output.status.code(), Some(1), "{rejected}");
    assert_eq!(rejected["status"], "fail");
    assert!(rejected["errors"].as_array().context("rejection errors missing")?.iter()
        .any(|error| error.as_str().is_some_and(|error| error.contains("read_heat must be true or false"))));
    assert_eq!(std::fs::read(&agent)?, committed);
    let unchanged = runtime.health()?;
    assert_eq!(unchanged.instance_id, restored.instance_id);
    assert_eq!(unchanged.pid, restored.pid);
    assert!(!runtime.fixture.marker().exists());
    runtime.assert_anonymous_selection_unchanged()?;
    runtime.stop()?;
    Ok(())
}

#[test]
fn anonymous_owner_host_config_reports_workers_without_restarting_the_owner() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let owner = runtime.start()?;
    let workers = owner.rpc["effective_parallelism"]
        .as_u64()
        .context("serving worker count missing")?;
    assert!(workers > 0);
    for args in [vec!["config"], vec!["config", "--rpc-parallelism", "1"]] {
        let (output, result) = runtime.fixture.command(&args)?;
        anyhow::ensure!(output.status.success(), "host config failed: {result}");
        assert_eq!(result["command"], "config");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["summary"]["rpc_workers"], workers);
        let current = runtime.health()?;
        assert_eq!(current.rpc["effective_parallelism"], workers);
        assert_eq!(result["errors"], json!([]));
        assert_eq!(current.pid, owner.pid);
        assert_eq!(current.process_start_id, owner.process_start_id);
        assert_eq!(current.instance_id, owner.instance_id);
        assert!(!runtime.fixture.marker().exists());
        if args.len() > 1 {
            assert_eq!(result["summary"]["rpc_parallelism"], 1);
            let items = result["items"]
                .as_array()
                .context("config output items missing")?;
            let item = items
                .iter()
                .find(|item| item["name"] == "rpc workers")
                .context("worker output missing")?;
            assert_eq!(item["value"], format!("{workers} (after runtime restart)"));
        }
    }
    let mut expected: Value = serde_json::from_slice(&runtime.fixture.client)?;
    expected["rpc_parallelism"] = json!(1);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(runtime.fixture.root.join("client.json"))?)?,
        expected
    );
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("bin/runtime.json"))?,
        runtime.fixture.selection
    );
    assert!(!runtime.fixture.profile.join("session.json").exists());
    assert!(!runtime
        .fixture
        .root
        .join("runtime/autostart/registration.json")
        .exists());
    runtime.stop()?;
    Ok(())
}

#[test]
fn anonymous_owner_profile_switch_validates_and_serves_the_selected_target() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let source = runtime.fixture.profile.clone();
    let original = runtime.start()?;
    let target = runtime.fixture.root.join("profiles/config-target");
    std::fs::create_dir(&target)?;
    let target = target.canonicalize()?;
    std::fs::write(target.join("keep.txt"), b"synthetic target marker")?;
    let (output, result) = runtime.fixture.command(&[
        "config",
        "--data-dir",
        target.to_str().context("target is not UTF-8")?,
        "--autosync",
        "true",
    ])?;
    anyhow::ensure!(output.status.success(), "profile switch failed: {result}");
    // Future health/normal-stop checks must use the committed target, not the old profile.
    runtime.fixture.profile = target.clone();
    assert_eq!(result["command"], "config");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["errors"], json!([]));
    runtime.verify_drained(&original)?;
    let current = runtime.health()?;
    runtime.last_owner = Some(current.clone());
    assert_ne!(current.instance_id, original.instance_id);
    assert_ne!(
        (current.pid, &current.process_start_id),
        (original.pid, &original.process_start_id)
    );
    assert_ne!(current.account_id, original.account_id);
    assert_eq!(current.installation_id, original.installation_id);
    assert_eq!(current.image_sha256, original.image_sha256);
    assert!(current.store_ready);
    assert_eq!(
        result["summary"]["rpc_workers"],
        current.rpc["effective_parallelism"]
    );
    let mut expected: Value = serde_json::from_slice(&runtime.fixture.client)?;
    expected["data_dir"] = json!(target);
    expected["autosync"] = json!(true);
    let committed = std::fs::read(runtime.fixture.root.join("client.json"))?;
    assert_eq!(serde_json::from_slice::<Value>(&committed)?, expected);
    assert_eq!(
        std::fs::read(source.join("keep.txt"))?,
        b"synthetic profile marker"
    );
    assert_eq!(
        std::fs::read(target.join("keep.txt"))?,
        b"synthetic target marker"
    );
    assert!(source.join("rsrs.db").is_file());
    assert!(target.join("rsrs.db").is_file());
    assert!(!source.join("session.json").exists());
    assert!(!target.join("session.json").exists());
    assert!(!runtime.fixture.marker().exists());
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("bin/runtime.json"))?,
        runtime.fixture.selection
    );
    assert!(!runtime
        .fixture
        .root
        .join("runtime/autostart/registration.json")
        .exists());
    runtime.stop()?;
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("client.json"))?,
        committed
    );
    Ok(())
}

#[test]
fn anonymous_owner_ambiguous_profile_validation_rolls_back_and_retains_source_fence() -> Result<()>
{
    let mut runtime = OwnedRuntime::new()?;
    let original = runtime.start()?;
    // A healthy serving source is the same-installation baseline. The target has
    // two ordinary owned files: database_path rejects their ambiguity before
    // opening either DB, reading credentials or attempting synchronization.
    assert!(original.store_ready);
    assert!(runtime.fixture.profile.join("rsrs.db").is_file());
    let target = runtime.fixture.root.join("profiles/ambiguous-target");
    std::fs::create_dir(&target)?;
    let target = target.canonicalize()?;
    let current = b"synthetic current database marker";
    let legacy = b"synthetic legacy database marker";
    std::fs::write(target.join("rsrs.db"), current)?;
    std::fs::write(target.join("onememory.db"), legacy)?;
    let (output, result) = runtime.fixture.command(&[
        "config",
        "--data-dir",
        target.to_str().context("target is not UTF-8")?,
        "--autosync",
        "true",
    ])?;
    assert_eq!(output.status.code(), Some(1), "{result}");
    assert_eq!(result["status"], "fail");
    let errors = result["errors"]
        .as_array()
        .context("config errors missing")?;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0], "original config restored by CAS; maintenance remains Paused for explicit recovery: Runtime did not confirm private config validation");
    runtime.verify_drained(&original)?;
    assert!(!net_rpc::port_is_open_on(runtime.fixture.port));
    runtime.cleanup_required = false;
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("client.json"))?,
        runtime.fixture.client
    );
    assert_eq!(std::fs::read(target.join("rsrs.db"))?, current);
    assert_eq!(std::fs::read(target.join("onememory.db"))?, legacy);
    let paused = std::fs::read(runtime.fixture.marker())?;
    let marker: Value = serde_json::from_slice(&paused)?;
    assert_eq!(marker["phase"], "Paused");
    assert_eq!(marker["expected_profile"], json!(runtime.fixture.profile));
    assert_eq!(marker["expected_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        marker["id"],
        format!(
            "host-{:x}",
            Sha256::digest(runtime.fixture.root.display().to_string().as_bytes())
        )
    );
    assert!(marker["snapshot"].is_null() && marker["agent_config"].is_null());
    runtime.assert_anonymous_selection_unchanged()?;
    assert!(!target.join("session.json").exists());
    // Read-only access must retain this real failure fence, not replay validation.
    let (status, blocked) = runtime.fixture.command(&["metadata", "status"])?;
    assert_eq!(status.status.code(), Some(1), "{blocked}");
    assert_eq!(blocked["errors"], json!(["runtime maintenance is incomplete; use the host runtime restart or the indicated snapshot/login recovery before accessing the library"]));
    assert_eq!(std::fs::read(runtime.fixture.marker())?, paused);
    assert_eq!(
        std::fs::read(runtime.fixture.root.join("client.json"))?,
        runtime.fixture.client
    );
    Ok(())
}

#[test]
fn off_mode_migration_failure_preserves_original_operation_and_drains_the_owner() -> Result<()> {
    use respire_protocol::migration_control::{HostMigrationOperation, HOST_MIGRATION_SCHEMA_V};
    let mut runtime = OwnedRuntime::new()?;
    let mut config: Value = serde_json::from_slice(&runtime.fixture.client)?;
    config["service_mode"] = json!("off");
    config["autosync"] = json!(false);
    config["cure_auto"] = json!(false);
    runtime.fixture.client = serde_json::to_vec(&config)?;
    std::fs::write(
        runtime.fixture.root.join("client.json"),
        &runtime.fixture.client,
    )?;
    let records = runtime.fixture.root.join("runtime/autostart/migrations");
    for path in [
        runtime.fixture.marker(),
        records.clone(),
        runtime.fixture.profile.join("session.json"),
        runtime.fixture.profile.join("agent.json"),
        runtime.fixture.root.join(".login-recovery.json"),
        runtime.fixture.root.join("accounts"),
    ] {
        assert!(!path.exists(), "fixture is not fresh: {}", path.display());
    }
    // An off-mode owner is a verified live listener, not a ready business store.
    // No read_heat setting is present, so its startup does not request SDK capabilities.
    let owner = runtime.start()?;
    assert!(!owner.store_ready);
    for path in [
        runtime.fixture.profile.join("rsrs.db"),
        runtime.fixture.profile.join("session.json"),
        runtime.fixture.profile.join("agent.json"),
        runtime.fixture.root.join(".login-recovery.json"),
    ] {
        assert!(
            !path.exists(),
            "off startup changed fixture: {}",
            path.display()
        );
    }
    let source = "f".repeat(64);
    let account = "synthetic-missing";
    // Preserve the original operation and fixture if any following assertion or
    // normal owner cleanup fails. Never replay/query an uncertain migration.
    runtime.fixture._temporary.disable_cleanup(true);
    eprintln!(
        "isolated migration fixture (retained on failure): {}",
        runtime.fixture.base.display()
    );
    let (output, result) =
        runtime
            .fixture
            .command(&["migrate", "--source", &source, "--account", account])?;
    assert_eq!(output.status.code(), Some(1), "{result}");
    assert_eq!(result["command"], "cli");
    assert_eq!(result["status"], "fail");
    let paths = std::fs::read_dir(&records)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(paths.len(), 1);
    let original_bytes = std::fs::read(&paths[0])?;
    let original: Value = serde_json::from_slice(&original_bytes)?;
    let operation: HostMigrationOperation = serde_json::from_value(original["operation"].clone())?;
    assert_eq!(operation.schema_version, HOST_MIGRATION_SCHEMA_V);
    let id = uuid::Uuid::parse_str(&operation.operation_id)?;
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(id.to_string(), operation.operation_id);
    assert_eq!(operation.key, operation.operation_id);
    assert_eq!(operation.installation_root, runtime.fixture.root);
    assert_eq!(operation.profile, runtime.fixture.profile);
    assert_eq!(
        operation.identity,
        respire_protocol::HostAuthIdentity::Absent
    );
    assert_eq!(operation.source_id, source);
    assert_eq!(operation.account, account);
    assert_eq!(
        operation.fingerprint,
        format!("{:x}", Sha256::digest(operation.fingerprint_bytes()?))
    );
    assert_eq!(
        paths[0],
        records.join(format!("{}.json", operation.fingerprint))
    );
    assert_eq!(original["was_serving"], true);
    assert_eq!(original["store_ready"], false);
    assert_eq!(original["sent"], true);
    assert_eq!(original["finished"], false);
    assert!(original["terminal_receipt"].is_null());
    // The public adapter deliberately hides the private failure cause. This
    // proves failure fencing, not a terminal rollback or an observed off-guard result.
    assert_eq!(result["errors"], json!([format!(
        "migration_outcome_unknown operation_id={}; repeat the same migrate --source --account to query; no migration was replayed; migration_stage=exchange; migration_exchange=runtime_migration_outcome_unknown",
        operation.operation_id)]));
    let marker: Value = serde_json::from_slice(&std::fs::read(runtime.fixture.marker())?)?;
    assert_eq!(marker["phase"], "Paused");
    assert_eq!(marker["expected_profile"], json!(runtime.fixture.profile));
    let token = original["token"]
        .as_str()
        .context("original token is a string")?;
    let token_id = uuid::Uuid::parse_str(token)?;
    assert_eq!(token_id.get_version_num(), 4);
    assert_eq!(token_id.to_string(), token);
    let coordinator = format!(
        "host-{:x}",
        Sha256::digest(runtime.fixture.root.display().to_string().as_bytes())
    );
    assert_eq!(original["coordinator"].as_str(), Some(coordinator.as_str()));
    assert_eq!(marker["token"].as_str(), Some(token));
    assert_eq!(marker["id"].as_str(), Some(coordinator.as_str()));
    assert!(marker["snapshot"].is_null() && marker["agent_config"].is_null());
    runtime.verify_drained(&owner)?;
    assert!(!net_rpc::port_is_open_on(runtime.fixture.port));
    runtime.cleanup_required = false;
    runtime.assert_anonymous_selection_unchanged()?;
    for path in [
        runtime.fixture.profile.join("rsrs.db"),
        runtime.fixture.profile.join("onememory.db"),
        runtime.fixture.profile.join("agent.json"),
        runtime.fixture.root.join(".login-recovery.json"),
        runtime.fixture.root.join("accounts"),
        runtime.fixture.base.join("home/.rsrs-migration-lock"),
        runtime.fixture.base.join("home/.rsrs-migration-staging"),
    ] {
        assert!(
            !path.exists(),
            "unexpected migration data: {}",
            path.display()
        );
    }
    assert_eq!(std::fs::read(&paths[0])?, original_bytes);
    // All intended evidence and normal drain checks succeeded. Only this owned
    // temporary fixture may now be cleaned; no other paused operation is touched.
    runtime.fixture._temporary.disable_cleanup(false);
    Ok(())
}
