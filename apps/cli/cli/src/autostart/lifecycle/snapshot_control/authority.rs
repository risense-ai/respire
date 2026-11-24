impl Maintenance {
    /// Publish absence only while both existing locks exclude serving/startup.
    /// The helper reacquires installation ownership before the existing DB lock.
    fn snapshot_bind_offline(&mut self, takeover: &LibraryLock, operation: &SnapshotOperation) -> Result<()> {
        self.snapshot_bind_offline_intent(takeover, &operation.original().binding, normal_marker(operation)?)
    }

    fn snapshot_bind_offline_intent(&mut self, takeover: &LibraryLock, binding: &ControlBinding,
        mut next_binding: SnapshotMarkerBinding) -> Result<()> {
        let root = &binding.installation_root;
        anyhow::ensure!(
            takeover.protects(&root.join("runtime/takeover"))?
                && self.installation_root()? == root.as_path(),
            "snapshot takeover changed"
        );
        let _owner = LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)
            .context("snapshot installation ownership unavailable; absent health is not stopped proof")?;
        let _library = LibraryLock::acquire(&binding.profile, Duration::ZERO)
            .context("snapshot library ownership unavailable; no offline action taken")?;
        if let Some(config) = self.config.as_ref() {
            owned(config)?;
            anyhow::ensure!(
                !manager::inspect(config)?.running,
                "snapshot supervisor is still running; explicit drain is required"
            );
        }
        let bytes = marker_bytes(root)?.context("snapshot offline marker missing")?;
        let mut actual: Marker = serde_json::from_slice(&bytes)?;
        let held = self
            .marker
            .as_ref()
            .context("snapshot offline owner missing")?;
        anyhow::ensure!(
            hash(&bytes)
                == self
                    .marker_digest
                    .as_deref()
                    .context("snapshot digest missing")?
                && serde_json::to_vec(&actual)? == serde_json::to_vec(held)?
                && actual.owner_pid == std::process::id()
                && controller_alive(&actual)?
                && current_boot_token().as_deref() == Some(actual.token.as_str()),
            "snapshot offline controller changed"
        );
        if let Some(bound) = actual.snapshot.as_ref() {
            anyhow::ensure!(same_snapshot_intent(bound, &next_binding), "snapshot offline intent changed");
        }
        let prior = actual
            .snapshot
            .as_ref()
            .and_then(|bound| bound.verified_runtime.clone())
            .or_else(|| {
                actual
                    .snapshot
                    .as_ref()
                    .and_then(|bound| bound.offline.as_ref())
                    .and_then(|proof| proof.prior_runtime.clone())
            });
        let proof = SnapshotOfflineProof::capture(root, prior)?;
        actual.phase = Phase::Paused;
        actual.expected_profile = Some(binding.profile.clone());
        next_binding.offline = Some(proof);
        actual.snapshot = Some(next_binding);
        let next = serde_json::to_vec(&actual)?;
        anyhow::ensure!(
            next.len() <= MAINTENANCE_MARKER_MAX_BYTES,
            "snapshot offline marker too large"
        );
        actual
            .snapshot
            .as_ref()
            .and_then(|bound| bound.offline.as_ref())
            .context("snapshot offline evidence missing")?
            .verify(root)?;
        private_write(&marker_path(root), &next, Some(&hash(&bytes)))?;
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&next));
        Ok(())
    }

    fn snapshot_owned_marker(
        &self,
        takeover: &LibraryLock,
        binding: &ControlBinding,
    ) -> Result<(PathBuf, Vec<u8>, Marker)> {
        let root = &binding.installation_root;
        if let Some(config) = self.config.as_ref() {
            owned(config)?;
            anyhow::ensure!(
                config.installation == *root,
                "snapshot registration root changed"
            );
        }
        let (selected_root, selected_profile) = respire_client_host::config::control_roots()?;
        anyhow::ensure!(
            takeover.protects(&root.join("runtime/takeover"))?
                && self.installation_root()? == root.as_path()
                && selected_root.canonicalize()? == *root
                && selected_profile.canonicalize()? == binding.profile,
            "snapshot takeover, installation or selected profile changed"
        );
        let path = marker_path(root);
        let bytes = marker_bytes(root)?.context("snapshot maintenance marker missing")?;
        let actual = marker(root)?.context("snapshot maintenance marker missing")?;
        let held = self
            .marker
            .as_ref()
            .context("snapshot maintenance ownership missing")?;
        anyhow::ensure!(
            actual.phase == Phase::Paused
                && held.phase == Phase::Paused
                && actual.id == held.id
                && actual.token == held.token
                && actual.owner_pid == std::process::id()
                && actual.owner_birth == held.owner_birth
                && controller_alive(&actual)?
                && current_boot_token().as_deref() == Some(held.token.as_str())
                && actual.expected_version == env!("CARGO_PKG_VERSION")
                && serde_json::to_vec(&actual)?
                    == serde_json::to_vec(&serde_json::from_slice::<Marker>(&bytes)?)?
                && hash(&bytes)
                    == self
                        .marker_digest
                        .as_deref()
                        .context("snapshot marker digest missing")?,
            "snapshot requires this controller's exact Paused marker"
        );
        Ok((path, bytes, actual))
    }

    /// No capability is published before drain_runtime validates final receipt,
    /// process exit and supervisor shutdown. Probe/lock availability is not proof.
    pub(crate) fn snapshot_drain_and_bind(&mut self, takeover: &LibraryLock,
        operation: &SnapshotOperation, health: &crate::net_rpc::Health) -> Result<()> {
        self.snapshot_drain_intent(takeover, &operation.original().binding, normal_marker(operation)?, health)
    }

    fn snapshot_drain_intent(&mut self, takeover: &LibraryLock, binding: &ControlBinding,
        mut next_binding: SnapshotMarkerBinding, health: &crate::net_rpc::Health) -> Result<()> {
        let (_, _, actual) = self.snapshot_owned_marker(takeover, binding)?;
        anyhow::ensure!(
            actual.snapshot.is_none(),
            "snapshot marker already bound; query that operation instead of draining again"
        );
        let birth = if let Some(config) = self.config.as_ref() {
            verify_owned_runtime(config, health)?
        } else {
            // Manual mode has no Registration to impersonate. Fence the real
            // installed Runtime and process before requesting its actual drain.
            crate::net_rpc::request_scope(health)?;
            let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
            let process = crate::net_rpc::runtime_process_identity(health.pid)?;
            let birth = manager::process_birth(health.pid)?
                .context("manual snapshot owner exited before drain")?;
            let prefix = snapshot_birth_prefix();
            anyhow::ensure!(
                health.managed_id.is_none()
                    && Path::new(&health.installation_root).canonicalize()?
                        == binding.installation_root
                    && Path::new(&health.data_dir).canonicalize()? == binding.profile
                    && health.installation_id
                        == hash(
                            binding
                                .installation_root
                                .display()
                                .to_string()
                                .as_bytes()
                        )
                    && Path::new(&health.image_path).canonicalize()? == binary
                    && Path::new(&health.exe).canonicalize()? == binary
                    && Path::new(&process.executable).canonicalize()? == binary
                    && health.process_start_id == format!("{prefix}{birth}")
                    && (process.started_at == birth
                        || process.started_at == health.process_start_id)
                    && health.image_sha256.eq_ignore_ascii_case(&digest),
                "manual snapshot owner differs from the installed Runtime"
            );
            birth
        };
        anyhow::ensure!(
            Some(health.account_id.as_str()) == binding.expected_account_id.as_deref(),
            "snapshot drain account differs from original operation"
        );
        self.drain_runtime(health)?;
        // Reuse the existing receipt reader and validation, including Drained
        // and empty unfinished. The existing drain already bound its stop O.
        let drain: RuntimeStopReceipt =
            serde_json::from_value(crate::net_rpc::final_stop_receipt(health, None)?)?;
        anyhow::ensure!(
            manager::process_birth(health.pid)?.as_deref() != Some(birth.as_str()),
            "snapshot drained owner is still alive"
        );
        let (path, bytes, mut actual) = self.snapshot_owned_marker(takeover, binding)?;
        anyhow::ensure!(
            actual.snapshot.is_none(),
            "snapshot marker changed during drain"
        );
        actual.expected_profile = Some(binding.profile.clone());
        next_binding.drained_birth = birth;
        next_binding.drain = Some(drain);
        actual.snapshot = Some(next_binding);
        let next = serde_json::to_vec(&actual)?;
        anyhow::ensure!(
            next.len() <= MAINTENANCE_MARKER_MAX_BYTES,
            "snapshot maintenance marker exceeds bounded limit"
        );
        private_write(&path, &next, Some(&hash(&bytes)))?;
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&next));
        Ok(())
    }

    pub(crate) fn snapshot_launch_binding(
        &self,
        takeover: &LibraryLock,
        operation: &SnapshotOperation,
    ) -> Result<Value> {
        let (_, bytes, actual) = self.snapshot_owned_marker(takeover, &operation.original().binding)?;
        let bound = actual
            .snapshot
            .as_ref()
            .context("snapshot has no authenticated drain capability")?;
        let original = operation.original();
        anyhow::ensure!(
            bound.operation_id == original.operation_id
                && bound.request_fingerprint == original.request_fingerprint
                && bound.sync_generation == original.sync_generation
                && bound.snapshot_id == original.snapshot_id
                && Some(bound.account_id.as_str())
                    == original.binding.expected_account_id.as_deref()
                && actual.expected_profile.as_ref() == Some(&original.binding.profile)
                && match (&bound.drain, &bound.offline) {
                    (Some(drain), None) =>
                        manager::process_birth(drain.pid)?.as_deref()
                            != Some(bound.drained_birth.as_str()),
                    (None, Some(proof)) => {
                        proof.verify(&original.binding.installation_root)?;
                        true
                    }
                    _ => false,
                }
                && match self.config.as_ref() {
                    Some(config) => !manager::inspect(config)?.running,
                    None => true,
                },
            "snapshot operation or drained owner changed"
        );
        // No generation is copied into a fresh launch grant. The marker hash
        // binds the original operation's generation independently.
        Ok(json!({
            "marker_sha256": hash(&bytes),
            "coordinator_id": actual.id,
            "controller_pid": actual.owner_pid,
            "controller_birth": actual.owner_birth,
            "issuer_cli_version": actual.expected_version,
        }))
    }

    /// Actual path: authenticated drain -> marker CAS -> fresh grant -> existing
    /// bounded exchange. Subsequent Query reuses the already published marker.
    pub(crate) fn snapshot_offline_locked(
        &mut self,
        takeover: &LibraryLock,
        operation: &SnapshotOperation,
        action: MaintenanceAction,
        health_to_drain: Option<&crate::net_rpc::Health>,
        deadline: Instant,
    ) -> Result<MaintenanceControlResult> {
        crate::runtime_policy::require_host("snapshot offline maintenance")?;
        anyhow::ensure!(
            matches!(
                action,
                MaintenanceAction::Restore
                    | MaintenanceAction::Rollback
                    | MaintenanceAction::Recover
                    | MaintenanceAction::Query
            ),
            "offline finalization forbidden"
        );
        if let Some(health) = health_to_drain {
            self.snapshot_drain_and_bind(takeover, operation, health)?;
        }
        let binding = self.snapshot_launch_binding(takeover, operation)?;
        let original = operation.original();
        let (binary, digest) = super::super::supervise::installed_auth_runtime()?;
        let managed_id = self
            .config
            .as_ref()
            .filter(|config| config.enabled && !config.manually_stopped)
            .map(|config| config.id.as_str());
        let (grant, _) = super::super::supervise::prepare_snapshot_grant(
            &binary,
            &original.binding.installation_root,
            &original.binding.profile,
            managed_id,
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
            "snapshot helper image changed after manifest verification"
        );
        // Original generation is marker-bound; the fresh helper keeps its own.
        operation.offline(takeover, &binary, &grant, action, deadline)
    }
}
