pub(crate) fn snapshot_restore_control(snapshot_id: &str) -> Result<ResultEnvelope> {
    crate::runtime_policy::require_host("snapshot restore control")?;
    anyhow::ensure!(
        current_boot_token().is_none(),
        "snapshot restore cannot nest maintenance"
    );
    super::super::supervise::installed_auth_runtime()?;
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    let takeover = crate::runtime_policy::takeover_lock()?;
    let health = crate::rpc::probe_runtime()?;
    let (account_id, generation) = if let Some(health) = health.as_ref() {
        crate::net_rpc::request_scope(health)?;
        (
            health.account_id.clone(),
            usize::try_from(health.generation)?,
        )
    } else {
        // Identity comes from session metadata, never an attempted DB open.
        let account = super::super::recovery::account_identity_source(&profile)?;
        (hash(account.as_bytes()), 0)
    };
    let mut params = MaintenanceControlParams {
        schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
        binding: respire_protocol::ControlBinding {
            installation_root: root,
            profile,
            launch_id: uuid::Uuid::new_v4().to_string(),
            expected_account_id: Some(account_id),
        },
        operation_id: uuid::Uuid::new_v4().to_string(),
        request_fingerprint: String::new(),
        sync_generation: generation,
        snapshot_id: snapshot_id.to_owned(),
        action: MaintenanceAction::Restore,
        material: None,
    };
    params.request_fingerprint = SnapshotOperation::fingerprint(
        &params.binding,
        params.sync_generation,
        &params.snapshot_id,
    )?;
    let operation = SnapshotOperation::from_original(params)?;
    let mut maintenance = begin_maintenance_locked()?;
    if health.is_none() {
        maintenance.snapshot_bind_offline(&takeover, &operation)?;
    }
    let restored = match maintenance.snapshot_offline_locked(
        &takeover,
        &operation,
        MaintenanceAction::Restore,
        health.as_ref(),
        snapshot_deadline(),
    ) {
        Ok(result) => result,
        Err(error) => {
            snapshot_query(&mut maintenance, &takeover, &operation).with_context(|| {
                format!(
                    "snapshot restore exchange failed; query original operation_id={}: {error:#}",
                    operation.original().operation_id
                )
            })?
        }
    };
    anyhow::ensure!(restored.state == MaintenanceState::Installed,
        "snapshot restore is not Installed (operation_id={}); use snapshot recover --confirm; no Restore was replayed", operation.original().operation_id);
    let started = maintenance
        .snapshot_start_runtime(&takeover, &operation)
        .and_then(|_| maintenance.snapshot_target_verified(&takeover, &operation));
    if let Err(error) = started {
        // No Commit has been sent. Query after a genuine target drain before
        // allowing Runtime recovery to choose rollback versus committed cleanup.
        let recovered = recover_owned(&mut maintenance, &takeover, &operation);
        return match recovered {
            Ok((result, verified)) => Err(error.context(if verified && !result.cleanup_pending {
                "snapshot startup/verification failed; original operation recovered".to_owned()
            } else {
                format!("snapshot startup/verification failed; recovery cleanup remains pending operation_id={}", operation.original().operation_id)
            })),
            Err(recovery) => Err(error.context(format!(
                "snapshot startup failed; recovery remains pending operation_id={}: {recovery:#}",
                operation.original().operation_id
            ))),
        };
    }
    // Commit includes verify -> durable commit -> release in Runtime. Any error
    // from this boundary is unknown, never a reason to enter rollback above.
    let mut committed = match maintenance.snapshot_finalize_locked(&takeover, &operation, MaintenanceAction::Commit, snapshot_deadline()) {
        Ok(result) => result,
        Err(error) => maintenance.snapshot_finalize_locked(&takeover, &operation, MaintenanceAction::Query, snapshot_deadline())
            .with_context(|| format!("snapshot commit outcome unknown operation_id={}: {error:#}; no rollback attempted", operation.original().operation_id))?,
    };
    anyhow::ensure!(matches!(committed.state, MaintenanceState::Verified | MaintenanceState::Released),
        "snapshot commit not confirmed operation_id={}; marker kept; use snapshot recover --confirm, do not replay Restore", operation.original().operation_id);
    if committed.state == MaintenanceState::Released && !committed.cleanup_pending {
        finish_snapshot_terminal(&mut maintenance, &mut committed);
    }
    snapshot_output("restored", &committed, true)
}

pub(crate) fn snapshot_recover_control() -> Result<ResultEnvelope> {
    crate::runtime_policy::require_host("snapshot recovery control")?;
    super::super::supervise::installed_auth_runtime()?;
    let takeover = crate::runtime_policy::takeover_lock()?;
    let (root, _) = respire_client_host::config::control_roots()?;
    match marker(&root)? {
        None => return recover_legacy_snapshot(&takeover),
        Some(value) if value.snapshot.as_ref().is_some_and(|snapshot| snapshot.legacy_recovery.is_some()) => {
            return query_legacy_snapshot(&takeover);
        }
        Some(_) => {}
    }
    let (mut maintenance, operation) = adopt_snapshot(&takeover)?;
    let (result, runtime_verified) = recover_owned(&mut maintenance, &takeover, &operation)?;
    snapshot_output("recovered", &result, runtime_verified)
}

fn finish_snapshot_terminal(maintenance: &mut Maintenance, receipt: &mut MaintenanceControlResult) {
    // A terminal journal state is already known. Host marker/registration cleanup
    // failure is a warning, never a reason to compensate accepted data.
    if let Err(error) = maintenance.finish().and_then(|()| maintenance.finish_handoff()) {
        receipt.cleanup_pending = true;
        receipt.cleanup_warning = Some(format!(
            "snapshot transaction completed; host maintenance cleanup pending: {error:#}"
        ));
    }
}

fn snapshot_output(
    action: &str,
    receipt: &MaintenanceControlResult,
    runtime_verified: bool,
) -> Result<ResultEnvelope> {
    let released = !receipt.cleanup_pending
        && matches!(
            receipt.state,
            MaintenanceState::Released | MaintenanceState::RolledBack
        );
    let mut output = ResultEnvelope::new(
        "snapshot",
        if released { Status::Ok } else { Status::Warn },
        json!({"action":action,"sync_quarantined":receipt.sync_quarantined,
            "cloud_rollback":false,"runtime_verified":runtime_verified,"service_released":released,
            "operation_id":receipt.operation_id}),
        Vec::new(),
    );
    output.details = json!({"restored":receipt.snapshot_id,"before_restore":receipt.before_restore,
        "sync_quarantined":receipt.sync_quarantined,
        "operation":{"operation_id":receipt.operation_id,"request_fingerprint":receipt.request_fingerprint},
        "state":receipt.state,"cleanup_pending":receipt.cleanup_pending});
    if receipt.cleanup_pending {
        output.errors.push(receipt.cleanup_warning.clone().unwrap_or_else(||
            "snapshot transaction completed; cleanup pending; use snapshot recover --confirm; do not roll back the committed store".to_owned()));
    }
    Ok(output)
}

fn snapshot_birth_prefix() -> &'static str {
    if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    }
}
