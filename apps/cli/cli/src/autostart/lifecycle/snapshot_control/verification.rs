impl Maintenance {
    /// Startup consumes only a drain-backed Paused snapshot capability. The
    /// selected Runtime is still the existing registered/detached launch path.
    pub(crate) fn snapshot_start_runtime(
        &mut self,
        takeover: &LibraryLock,
        operation: &SnapshotOperation,
    ) -> Result<()> {
        self.snapshot_launch_binding(takeover, operation)?;
        self.check_account_runtime()?;
        self.mark(Phase::Starting)?;
        crate::rpc::ensure_daemon_locked()
    }

    fn snapshot_target_proof(
        &self,
        operation: &SnapshotOperation,
    ) -> Result<SnapshotVerifiedRuntime> {
        self.snapshot_live_proof(operation, true)
    }

    fn snapshot_live_proof(
        &self,
        operation: &SnapshotOperation,
        require_snapshot: bool,
    ) -> Result<SnapshotVerifiedRuntime> {
        self.snapshot_live_binding(&operation.original().binding,
            require_snapshot.then_some(operation.original().snapshot_id.as_str()))
    }

    fn snapshot_live_binding(&self, binding: &ControlBinding, snapshot_id: Option<&str>) -> Result<SnapshotVerifiedRuntime> {
        let (root, profile) = respire_client_host::config::control_roots()?;
        anyhow::ensure!(
            root.canonicalize()? == binding.installation_root
                && profile.canonicalize()? == binding.profile,
            "snapshot target selection changed"
        );
        let health = crate::net_rpc::health()?;
        let scope = crate::net_rpc::request_scope(&health)?;
        let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
        let birth = manager::process_birth(health.pid)?.context("snapshot target exited")?;
        let process = crate::net_rpc::runtime_process_identity(health.pid)?;
        let expected_managed = self.config.as_ref().map(|config| config.id.as_str());
        anyhow::ensure!(
            scope.generation > 0
                && health.pid != std::process::id()
                && health.process_start_id == format!("{}{birth}", snapshot_birth_prefix())
                && (process.started_at == birth || process.started_at == health.process_start_id)
                && Some(scope.account_id.as_str())
                    == binding.expected_account_id.as_deref()
                && health.managed_id.as_deref() == expected_managed
                && Path::new(&health.installation_root).canonicalize()?
                    == binding.installation_root
                && health.installation_id
                    == hash(
                        binding
                            .installation_root
                            .display()
                            .to_string()
                            .as_bytes()
                    )
                && Path::new(&health.data_dir).canonicalize()? == binding.profile
                && Path::new(&health.image_path).canonicalize()? == binary
                && Path::new(&health.exe).canonicalize()? == binary
                && Path::new(&process.executable).canonicalize()? == binary
                && health.image_sha256.eq_ignore_ascii_case(&digest)
                && !health.runtime_version.is_empty()
                && health.store_ready
                && (snapshot_id.is_none()
                    || health.snapshot_restore_id.as_deref()
                        == snapshot_id),
            "snapshot target process, scope or actual restored store is not verified"
        );
        anyhow::ensure!(
            manager::process_birth(health.pid)?.as_deref() == Some(birth.as_str())
                && crate::net_rpc::runtime_process_identity(health.pid)? == process,
            "snapshot target changed during verification"
        );
        Ok(SnapshotVerifiedRuntime {
            scope,
            pid: health.pid,
            process_start_id: health.process_start_id,
            image_path: binary,
            image_sha256: digest,
            runtime_version: health.runtime_version,
        })
    }

    /// Real target proof and Verified phase are one marker CAS. No generic
    /// mark(Verified) call can substitute for this snapshot transition.
    pub(crate) fn snapshot_target_verified(
        &mut self,
        takeover: &LibraryLock,
        operation: &SnapshotOperation,
    ) -> Result<()> {
        let original = operation.original();
        let root = self.installation_root()?.to_path_buf();
        anyhow::ensure!(
            root == original.binding.installation_root
                && takeover.protects(&root.join("runtime/takeover"))?,
            "snapshot verification requires the original installation takeover"
        );
        let path = marker_path(&root);
        let bytes = marker_bytes(&root)?.context("snapshot target marker missing")?;
        let mut actual = marker(&root)?.context("snapshot target marker missing")?;
        let held = self
            .marker
            .as_ref()
            .context("snapshot target ownership missing")?;
        anyhow::ensure!(
            matches!(actual.phase, Phase::Starting | Phase::Verified)
                && actual.phase == held.phase
                && actual.id == held.id
                && actual.token == held.token
                && actual.owner_pid == std::process::id()
                && actual.owner_birth == held.owner_birth
                && actual.expected_version == env!("CARGO_PKG_VERSION")
                && actual.expected_profile.as_ref() == Some(&original.binding.profile)
                && current_boot_token().as_deref() == Some(held.token.as_str())
                && controller_alive(&actual)?
                && hash(&bytes)
                    == self
                        .marker_digest
                        .as_deref()
                        .context("snapshot target digest missing")?
                && serde_json::to_vec(&actual)?
                    == serde_json::to_vec(&serde_json::from_slice::<Marker>(&bytes)?)?,
            "snapshot target does not belong to this exact Starting/Verified controller"
        );
        let bound = actual
            .snapshot
            .as_ref()
            .context("snapshot target has no drain-backed operation")?;
        anyhow::ensure!(
            bound.operation_id == original.operation_id
                && bound.request_fingerprint == original.request_fingerprint
                && bound.sync_generation == original.sync_generation
                && bound.snapshot_id == original.snapshot_id
                && Some(bound.account_id.as_str())
                    == original.binding.expected_account_id.as_deref(),
            "snapshot target operation changed"
        );
        let proof = self.snapshot_target_proof(operation)?;
        anyhow::ensure!(
            self.snapshot_target_proof(operation)? == proof,
            "snapshot target changed before Verified publication"
        );
        actual.phase = Phase::Verified;
        actual
            .snapshot
            .as_mut()
            .context("snapshot target operation missing")?
            .verified_runtime = Some(proof);
        let next = serde_json::to_vec(&actual)?;
        anyhow::ensure!(
            next.len() <= MAINTENANCE_MARKER_MAX_BYTES,
            "snapshot proof exceeds marker limit"
        );
        private_write(&path, &next, Some(&hash(&bytes)))?;
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&next));
        Ok(())
    }

    /// Consumes only an already Verified marker published by the lifecycle
    /// owner. This method cannot promote Paused/Starting or invent target proof.
    pub(crate) fn snapshot_finalize_locked(
        &self,
        takeover: &LibraryLock,
        operation: &SnapshotOperation,
        action: MaintenanceAction,
        deadline: Instant,
    ) -> Result<MaintenanceControlResult> {
        crate::runtime_policy::require_host("snapshot Verified metadata finalization")?;
        anyhow::ensure!(
            matches!(
                action,
                MaintenanceAction::Commit | MaintenanceAction::Release | MaintenanceAction::Query
            ),
            "Verified snapshot helper cannot replace the store"
        );
        let original = operation.original();
        let root = self.installation_root()?;
        let actual = marker(root)?.context("Verified snapshot marker missing")?;
        let held = self
            .marker
            .as_ref()
            .context("Verified snapshot ownership missing")?;
        let bytes = marker_bytes(root)?.context("Verified snapshot marker missing")?;
        anyhow::ensure!(
            root == original.binding.installation_root.as_path()
                && takeover.protects(&root.join("runtime/takeover"))?
                && actual.phase == Phase::Verified
                && held.phase == Phase::Verified
                && actual.id == held.id
                && actual.token == held.token
                && actual.owner_pid == std::process::id()
                && actual.owner_birth == held.owner_birth
                && current_boot_token().as_deref() == Some(held.token.as_str())
                && controller_alive(&actual)?
                && actual
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.verified_runtime.is_some())
                && serde_json::to_vec(&actual)?
                    == serde_json::to_vec(&serde_json::from_slice::<Marker>(&bytes)?)?
                && hash(&bytes)
                    == self
                        .marker_digest
                        .as_deref()
                        .context("Verified snapshot digest missing")?,
            "snapshot finalization requires this controller's exact Verified marker"
        );
        let binding = maintenance_binding(&original.binding.profile, None, false)?
            .context("Verified snapshot launch binding missing")?;
        let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
        let (grant, _) = super::super::supervise::prepare_snapshot_grant(
            &binary,
            root,
            &original.binding.profile,
            self.config
                .as_ref()
                .filter(|config| config.enabled && !config.manually_stopped)
                .map(|config| config.id.as_str()),
            crate::net_rpc::rpc_port(),
            original
                .binding
                .expected_account_id
                .as_deref()
                .context("snapshot account missing")?,
            Some(binding),
        )?;
        let grant: HostControlLaunchGrant = serde_json::from_value(grant)?;
        anyhow::ensure!(
            grant.image_sha256.eq_ignore_ascii_case(&digest),
            "snapshot finalizer image changed after manifest verification"
        );
        operation.finalize_helper(takeover, &binary, &grant, action, deadline)
    }

    /// Replacing an old drain proof requires another actual drain, including
    /// final receipt and supervisor exit. Never infer this from absent health.
    fn snapshot_pause_after_drain(
        &mut self,
        takeover: &LibraryLock,
        binding: &ControlBinding,
        expected: &SnapshotMarkerBinding,
        health: &crate::net_rpc::Health,
    ) -> Result<()> {
        let root = self.installation_root()?.to_path_buf();
        let bytes = marker_bytes(&root)?.context("snapshot recovery marker missing")?;
        let mut actual: Marker = serde_json::from_slice(&bytes)?;
        let held = self
            .marker
            .as_ref()
            .context("snapshot recovery ownership missing")?;
        anyhow::ensure!(
            takeover.protects(&root.join("runtime/takeover"))?
                && hash(&bytes)
                    == self
                        .marker_digest
                        .as_deref()
                        .context("snapshot recovery digest missing")?
                && serde_json::to_vec(&actual)? == serde_json::to_vec(held)?
                && actual.owner_pid == std::process::id()
                && controller_alive(&actual)?
                && current_boot_token().as_deref() == Some(actual.token.as_str()),
            "snapshot recovery controller changed"
        );
        crate::net_rpc::request_scope(health)?;
        let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
        let birth =
            manager::process_birth(health.pid)?.context("snapshot recovery owner exited")?;
        let process = crate::net_rpc::runtime_process_identity(health.pid)?;
        anyhow::ensure!(
            Path::new(&health.installation_root).canonicalize()? == root
                && Path::new(&health.data_dir).canonicalize()? == binding.profile
                && Some(health.account_id.as_str())
                    == binding.expected_account_id.as_deref()
                && health.process_start_id == format!("{}{birth}", snapshot_birth_prefix())
                && (process.started_at == birth || process.started_at == health.process_start_id)
                && Path::new(&process.executable).canonicalize()? == binary
                && Path::new(&health.image_path).canonicalize()? == binary
                && health.image_sha256.eq_ignore_ascii_case(&digest)
                && health.managed_id.as_deref()
                    == self.config.as_ref().map(|config| config.id.as_str()),
            "snapshot recovery owner differs from original operation"
        );
        self.drain_runtime(health)?;
        let drain: RuntimeStopReceipt =
            serde_json::from_value(crate::net_rpc::final_stop_receipt(health, None)?)?;
        let snapshot = actual
            .snapshot
            .as_mut()
            .context("snapshot recovery operation missing")?;
        anyhow::ensure!(
            same_snapshot_intent(snapshot, expected),
            "snapshot recovery operation changed"
        );
        snapshot.drain = Some(drain);
        snapshot.offline = None;
        snapshot.drained_birth = birth;
        snapshot.verified_runtime = None;
        actual.phase = Phase::Paused;
        let next = serde_json::to_vec(&actual)?;
        anyhow::ensure!(
            next.len() <= MAINTENANCE_MARKER_MAX_BYTES,
            "snapshot recovery marker too large"
        );
        private_write(&marker_path(&root), &next, Some(&hash(&bytes)))?;
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&next));
        Ok(())
    }
}
