fn snapshot_deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn legacy_marker(params: &LegacyParams) -> Result<SnapshotMarkerBinding> {
    let account_id = params.binding.expected_account_id.clone().context("legacy snapshot account missing")?;
    Ok(SnapshotMarkerBinding {
        // These fields never identify an original restore for legacy recovery.
        operation_id: String::new(), request_fingerprint: String::new(), sync_generation: 0,
        snapshot_id: String::new(), account_id: account_id.clone(), drained_birth: String::new(),
        drain: None, offline: None, verified_runtime: None,
        legacy_recovery: Some(LegacySnapshotRecoveryBinding {
            recovery_operation_id: params.recovery_operation_id.clone(),
            request_fingerprint: params.request_fingerprint.clone(),
            journal_sha256: params.journal_sha256.clone(), account_id,
        }),
    })
}

impl Maintenance {
    pub(super) fn legacy_pause_for_query(&mut self, takeover: &LibraryLock,
        params: &LegacyParams, health: Option<&crate::net_rpc::Health>) -> Result<()> {
        let expected = legacy_marker(params)?;
        anyhow::ensure!(params.request_fingerprint == legacy_fingerprint(params)?
            && self.marker.as_ref().and_then(|marker| marker.snapshot.as_ref())
                .is_some_and(|bound| same_snapshot_intent(bound, &expected)),
            "legacy recovery query intent changed");
        if let Some(health) = health {
            self.snapshot_pause_after_drain(takeover, &params.binding, &expected, health)?;
        } else {
            self.snapshot_bind_offline_intent(takeover, &params.binding, expected)?;
        }
        self.legacy_launch_binding(takeover, params)?;
        Ok(())
    }

    fn legacy_launch_binding(&self, takeover: &LibraryLock, params: &LegacyParams) -> Result<Value> {
        let (_, bytes, actual) = self.snapshot_owned_marker(takeover, &params.binding)?;
        let bound = actual.snapshot.as_ref().context("legacy snapshot binding missing")?;
        anyhow::ensure!(same_snapshot_intent(bound, &legacy_marker(params)?)
            && actual.expected_profile.as_ref() == Some(&params.binding.profile)
            && bound.verified_runtime.is_none()
            && params.request_fingerprint == legacy_fingerprint(params)?
            && match (&bound.drain, &bound.offline) {
                (Some(drain), None) => manager::process_birth(drain.pid)?.as_deref()
                    != Some(bound.drained_birth.as_str()),
                (None, Some(proof)) => { proof.verify(&params.binding.installation_root)?; true }
                _ => false,
            }
            && match self.config.as_ref() {
                Some(config) => !manager::inspect(config)?.running,
                None => true,
            }, "legacy recovery intent or stopped owner changed");
        Ok(json!({"marker_sha256":hash(&bytes), "coordinator_id":actual.id,
            "controller_pid":actual.owner_pid, "controller_birth":actual.owner_birth,
            "issuer_cli_version":actual.expected_version}))
    }

    fn legacy_dispatch(&self, takeover: &LibraryLock, original: &LegacyParams,
        action: LegacyAction) -> Result<LegacyReceipt> {
        let binding = self.legacy_launch_binding(takeover, original)?;
        let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
        let managed_id = self.config.as_ref().filter(|config| config.enabled && !config.manually_stopped)
            .map(|config| config.id.as_str());
        let (grant, _) = super::super::supervise::prepare_snapshot_grant(&binary,
            &original.binding.installation_root, &original.binding.profile, managed_id,
            crate::net_rpc::rpc_port(), original.binding.expected_account_id.as_deref()
                .context("legacy snapshot account missing")?, Some(binding))?;
        let grant: HostControlLaunchGrant = serde_json::from_value(grant)?;
        anyhow::ensure!(grant.image_sha256.eq_ignore_ascii_case(&digest), "legacy snapshot helper image changed");
        let mut params = original.clone();
        params.action = action;
        params.binding.launch_id = grant.instance_id.clone();
        dispatch_legacy_snapshot(takeover, &grant, &params)
    }
}

fn recover_legacy_snapshot(takeover: &LibraryLock) -> Result<ResultEnvelope> {
    anyhow::ensure!(current_boot_token().is_none(), "legacy recovery cannot nest maintenance");
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    anyhow::ensure!(takeover.protects(&root.join("runtime/takeover"))?
        && marker_bytes(&root)?.is_none(), "legacy recovery requires an unclaimed installation");
    let descriptor = super::super::supervise::discover_legacy_snapshot()?;
    let descriptor = descriptor.context("no legacy snapshot recovery journal; no recovery attempted")?;
    validate_legacy_descriptor(&descriptor)?;
    let account = super::super::recovery::account_identity_source(&profile)?;
    anyhow::ensure!(hash(account.as_bytes()) == descriptor.account_id,
        "legacy snapshot journal account differs from selected profile");
    let mut params = LegacyParams { schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
        binding: ControlBinding { installation_root: root, profile, launch_id: uuid::Uuid::new_v4().to_string(),
            expected_account_id: Some(descriptor.account_id) },
        recovery_operation_id: uuid::Uuid::new_v4().to_string(), request_fingerprint: String::new(),
        journal_sha256: descriptor.journal_sha256, action: LegacyAction::Recover };
    params.request_fingerprint = legacy_fingerprint(&params)?;
    let health = crate::rpc::probe_runtime()?;
    let mut maintenance = begin_maintenance_locked()?;
    if let Some(health) = health.as_ref() {
        maintenance.snapshot_drain_intent(takeover, &params.binding, legacy_marker(&params)?, health)?;
    } else {
        maintenance.snapshot_bind_offline_intent(takeover, &params.binding, legacy_marker(&params)?)?;
    }
    // This fresh O is dispatched once. A lost reply permits Query only.
    let receipt = match maintenance.legacy_dispatch(takeover, &params, LegacyAction::Recover) {
        Ok(receipt) => receipt,
        Err(_) => maintenance.legacy_dispatch(takeover, &params, LegacyAction::Query)
            .context("legacy recovery outcome unknown; marker retained; no recovery replayed")?,
    };
    complete_legacy_snapshot(&mut maintenance, takeover, &params, receipt)
}

fn query_legacy_snapshot(takeover: &LibraryLock) -> Result<ResultEnvelope> {
    let mut maintenance = adopt_snapshot_marker(takeover)?;
    let marker = maintenance.marker.as_ref().context("legacy recovery marker missing")?;
    let bound = marker.snapshot.as_ref().and_then(|snapshot| snapshot.legacy_recovery.as_ref())
        .context("legacy recovery request identity missing")?;
    let params = LegacyParams { schema_version: MAINTENANCE_CONTROL_SCHEMA_V,
        binding: ControlBinding { installation_root: maintenance.installation_root()?.to_path_buf(),
            profile: marker.expected_profile.clone().context("legacy recovery profile missing")?,
            launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: Some(bound.account_id.clone()) },
        recovery_operation_id: bound.recovery_operation_id.clone(), request_fingerprint: bound.request_fingerprint.clone(),
        journal_sha256: bound.journal_sha256.clone(), action: LegacyAction::Query };
    let health = crate::rpc::probe_runtime()?;
    maintenance.legacy_pause_for_query(takeover, &params, health.as_ref())?;
    let receipt = maintenance.legacy_dispatch(takeover, &params, LegacyAction::Query)?;
    complete_legacy_snapshot(&mut maintenance, takeover, &params, receipt)
}

fn complete_legacy_snapshot(maintenance: &mut Maintenance, takeover: &LibraryLock,
    params: &LegacyParams, mut receipt: LegacyReceipt) -> Result<ResultEnvelope> {
    anyhow::ensure!(matches!(receipt.state, MaintenanceState::Released | MaintenanceState::RolledBack
        | MaintenanceState::Verified),
        "snapshot_outcome_unknown recovery_operation_id={}; marker retained; no recovery replayed",
        params.recovery_operation_id);
    let mut runtime_verified = false;
    if !receipt.cleanup_pending {
        anyhow::ensure!(matches!(receipt.state, MaintenanceState::Released | MaintenanceState::RolledBack),
            "legacy recovery cleanup is not complete; marker retained");
        maintenance.legacy_launch_binding(takeover, params)?;
        maintenance.check_account_runtime()?;
        maintenance.mark(Phase::Starting)?;
        crate::rpc::ensure_daemon_locked()?;
        let proof = maintenance.snapshot_live_binding(&params.binding, None)?;
        anyhow::ensure!(maintenance.snapshot_live_binding(&params.binding, None)? == proof,
            "legacy recovered Runtime changed before completion");
        runtime_verified = true;
        if maintenance.finish().and_then(|()| maintenance.finish_handoff()).is_err() {
            receipt.cleanup_pending = true;
            receipt.cleanup_warning = Some("snapshot recovery completed; host maintenance cleanup pending".to_owned());
        }
    }
    let mut output = ResultEnvelope::new("snapshot", if receipt.cleanup_pending { Status::Warn } else { Status::Ok },
        json!({"action":"recovered", "sync_quarantined":true, "cloud_rollback":false,
            "runtime_verified":runtime_verified, "service_released":!receipt.cleanup_pending,
            "operation_id":receipt.recovery_operation_id}), Vec::new());
    output.details = json!({"operation":{"recovery_operation_id":receipt.recovery_operation_id,
        "request_fingerprint":receipt.request_fingerprint}, "journal_sha256":receipt.journal_sha256,
        "state":receipt.state, "cleanup_pending":receipt.cleanup_pending});
    if receipt.cleanup_pending {
        output.errors.push(receipt.cleanup_warning.unwrap_or_else(||
            "snapshot recovery committed; cleanup pending; no rollback attempted".to_owned()));
    }
    Ok(output)
}
