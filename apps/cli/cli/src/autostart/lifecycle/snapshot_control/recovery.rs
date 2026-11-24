fn original_operation(
    root: PathBuf,
    profile: PathBuf,
    snapshot: &SnapshotMarkerBinding,
) -> Result<SnapshotOperation> {
    SnapshotOperation::from_original(MaintenanceControlParams {
        schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
        binding: respire_protocol::ControlBinding {
            installation_root: root,
            profile,
            launch_id: uuid::Uuid::new_v4().to_string(),
            expected_account_id: Some(snapshot.account_id.clone()),
        },
        operation_id: snapshot.operation_id.clone(),
        request_fingerprint: snapshot.request_fingerprint.clone(),
        sync_generation: snapshot.sync_generation,
        snapshot_id: snapshot.snapshot_id.clone(),
        action: MaintenanceAction::Query,
        material: None,
    })
}

/// Adopt only the existing snapshot marker. Generic begin(recover) would erase
/// the original operation and must not be used for snapshot recovery.
fn adopt_snapshot(takeover: &LibraryLock) -> Result<(Maintenance, SnapshotOperation)> {
    let maintenance = adopt_snapshot_marker(takeover)?;
    let held = maintenance.marker.as_ref().context("snapshot marker missing")?;
    let snapshot = held.snapshot.as_ref().context("snapshot operation missing")?;
    anyhow::ensure!(snapshot.legacy_recovery.is_none(), "legacy marker cannot authorize original restore");
    let profile = held.expected_profile.clone().context("snapshot profile missing")?;
    let operation = original_operation(maintenance.installation_root()?.to_path_buf(), profile, snapshot)?;
    Ok((maintenance, operation))
}

fn adopt_snapshot_marker(takeover: &LibraryLock) -> Result<Maintenance> {
    anyhow::ensure!(
        current_boot_token().is_none(),
        "snapshot recovery cannot nest maintenance"
    );
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    anyhow::ensure!(
        takeover.protects(&root.join("runtime/takeover"))?,
        "snapshot recovery takeover mismatch"
    );
    let bytes = marker_bytes(&root)?.context(
        "snapshot recovery unsupported: no original-operation marker; no journal replay attempted",
    )?;
    let mut value = marker(&root)?.context("snapshot recovery marker missing")?;
    anyhow::ensure!(
        serde_json::to_vec(&value)?
            == serde_json::to_vec(&serde_json::from_slice::<Marker>(&bytes)?)?
            && !controller_alive(&value)?
            && value.expected_version == env!("CARGO_PKG_VERSION")
            && value.expected_profile.as_ref() == Some(&profile),
        "snapshot recovery requires the exact abandoned marker and original profile"
    );
    anyhow::ensure!(value.snapshot.is_some(), "snapshot recovery marker has no operation");
    let registration = read_registration(&root)?;
    let expected_id = if let Some(config) = registration.as_ref() {
        owned(config)?;
        anyhow::ensure!(
            config.installation == root,
            "snapshot recovery registration root changed"
        );
        config.id.clone()
    } else {
        format!("host-{}", hash(root.display().to_string().as_bytes()))
    };
    anyhow::ensure!(
        value.id == expected_id,
        "snapshot recovery coordinator changed"
    );
    let config = registration.filter(|config| config.enabled && !config.manually_stopped);
    value.token = uuid::Uuid::new_v4().to_string();
    value.owner_pid = std::process::id();
    value.owner_birth = manager::process_birth(value.owner_pid)?
        .context("snapshot recovery controller birth missing")?;
    let next = serde_json::to_vec(&value)?;
    anyhow::ensure!(
        next.len() <= MAINTENANCE_MARKER_MAX_BYTES,
        "snapshot recovery marker too large"
    );
    private_write(&marker_path(&root), &next, Some(&hash(&bytes)))?;
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
    Ok(Maintenance {
        installation: Some(root), config, marker: Some(value),
        marker_digest: Some(hash(&next)), completed: false,
    })
}

fn snapshot_query(
    maintenance: &mut Maintenance,
    takeover: &LibraryLock,
    operation: &SnapshotOperation,
) -> Result<MaintenanceControlResult> {
    let result = maintenance.snapshot_offline_locked(
        takeover,
        operation,
        MaintenanceAction::Query,
        None,
        snapshot_deadline(),
    )?;
    anyhow::ensure!(
        result.state != MaintenanceState::Unknown,
        "snapshot_outcome_unknown operation_id={}; no Restore or compensation was replayed",
        operation.original().operation_id
    );
    Ok(result)
}

fn recover_owned(
    maintenance: &mut Maintenance,
    takeover: &LibraryLock,
    operation: &SnapshotOperation,
) -> Result<(MaintenanceControlResult, bool)> {
    let health = crate::rpc::probe_runtime()?;
    if let Some(health) = health.as_ref() {
        maintenance.snapshot_pause_after_drain(takeover, &operation.original().binding,
            &normal_marker(operation)?, health)?;
    } else {
        maintenance.snapshot_bind_offline(takeover, operation)?;
    }
    // The Offline authority independently revalidates the stored real drain.
    snapshot_query(maintenance, takeover, operation)?;
    let mut result = maintenance.snapshot_offline_locked(
        takeover,
        operation,
        MaintenanceAction::Recover,
        None,
        snapshot_deadline(),
    )?;
    anyhow::ensure!(
        matches!(
            result.state,
            MaintenanceState::RolledBack | MaintenanceState::Verified | MaintenanceState::Released
        ),
        "snapshot recovery did not return a terminal receipt; original marker retained"
    );
    if result.cleanup_pending {
        return Ok((result, false));
    }
    anyhow::ensure!(
        matches!(
            result.state,
            MaintenanceState::RolledBack | MaintenanceState::Released
        ),
        "snapshot recovery cleanup is not complete"
    );
    maintenance.snapshot_start_runtime(takeover, operation)?;
    let proof = maintenance.snapshot_live_proof(operation, false)?;
    anyhow::ensure!(
        maintenance.snapshot_live_proof(operation, false)? == proof,
        "recovered Runtime changed before maintenance completion"
    );
    finish_snapshot_terminal(maintenance, &mut result);
    Ok((result, true))
}
