impl Maintenance {
    pub fn start_account_runtime(&mut self) -> Result<()> {
        self.check_account_runtime()?;
        anyhow::ensure!(self.drained, "replacement startup requires a drained owner");
        let (_, selected) = selected_roots()?;
        self.write_phase(Phase::Starting)?;
        let launch = LaunchContext {
            binding: self.binding(&selected, LaunchPurpose::Serving)?,
            identity: crate::identity(&selected)?,
            claimed: Cell::new(false),
        };
        self.host.start_account_runtime(&launch)?;
        self.drained = false;
        Ok(())
    }

    pub fn start_account_target(
        &mut self,
        source: &Path,
        operation_id: &str,
        receipt: &HostAuthAccountResult,
        register: bool,
    ) -> Result<()> {
        crate::account_control::validate_account_receipt(receipt, operation_id, source, register)?;
        self.start_account_runtime()?;
        self.account_target_verified(source, operation_id, receipt, register)
    }

    pub(crate) fn start_key_target(
        &mut self,
        source: &Path,
        operation_id: &str,
        receipt: &respire_protocol::key_control::HostKeyResult,
    ) -> Result<()> {
        let target = crate::key::validate_applied_target(receipt, operation_id, source)?;
        self.start_account_runtime()?;
        self.verify_ready_runtime(&target, &receipt.identity)?;
        crate::key::validate_applied_target(receipt, operation_id, source)?;
        self.check_owner()?;
        anyhow::ensure!(
            matches!(self.marker.phase, Phase::Starting | Phase::Verified)
                && self.marker.expected_profile.as_deref() == Some(target.as_path()),
            "key target launch does not belong to this maintenance controller"
        );
        self.write_phase(Phase::Verified)
    }

    pub(crate) fn key_complete_launch_binding(
        &self,
        source: &Path,
        operation_id: &str,
        receipt: &respire_protocol::key_control::HostKeyResult,
    ) -> Result<Option<OwnedMaintenanceBinding>> {
        let target = crate::key::validate_applied_target(receipt, operation_id, source)?;
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Verified
                && self.marker.expected_profile.as_deref() == Some(target.as_path()),
            "key completion requires this controller's exact Verified marker"
        );
        Ok(Some(self.binding(&target, LaunchPurpose::AccountComplete)?))
    }

    pub fn account_target_verified(
        &mut self,
        source: &Path,
        operation_id: &str,
        receipt: &HostAuthAccountResult,
        register: bool,
    ) -> Result<()> {
        crate::account_control::validate_account_receipt(receipt, operation_id, source, register)?;
        ensure_applied(receipt)?;
        let target = receipt_target(receipt)?;
        let identity = crate::identity(&target)?;
        self.verify_ready_runtime(&target, &identity)?;
        self.check_owner()?;
        anyhow::ensure!(
            matches!(self.marker.phase, Phase::Starting | Phase::Verified)
                && self.marker.expected_profile.as_deref() == Some(target.as_path()),
            "account target launch does not belong to this maintenance controller"
        );
        self.write_phase(Phase::Verified)
    }

    pub fn account_complete_launch_binding(
        &self,
        source: &Path,
        operation_id: &str,
        receipt: &HostAuthAccountResult,
        register: bool,
    ) -> Result<Option<OwnedMaintenanceBinding>> {
        crate::account_control::validate_account_receipt(receipt, operation_id, source, register)?;
        ensure_applied(receipt)?;
        let target = receipt_target(receipt)?;
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Verified,
            "account completion requires this controller's exact Verified marker"
        );
        Ok(Some(self.binding(&target, LaunchPurpose::AccountComplete)?))
    }

    /// Completion cannot promote a merely Paused or Starting marker. Failure at
    /// any earlier step leaves durable maintenance in place for explicit recovery.
    pub fn finish(&mut self) -> Result<()> {
        if self.completed {
            return Ok(());
        }
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Verified,
            "maintenance completion requires independently Verified state"
        );
        if matches!(self.registration, RegistrationMode::Managed { .. }) {
            self.host.set_maintenance(&self.root, false)?;
        }
        self.check_owner()?;
        let path = marker_path(&self.root);
        fs::remove_file(&path)?;
        let durable = respire_spawn::durable::sync_directory(
            path.parent().context("maintenance parent missing")?,
        );
        // Account commit is not undone by a namespace-sync failure. Never
        // manufacture durable completion or attempt a fresh authentication.
        self.completed = true;
        self.clear_active();
        durable.context("account verified; maintenance namespace cleanup is unconfirmed")
    }

    /// A rollback can restore a non-serving source. That is verified by exact
    /// source identity and owner absence, not by inventing runtime readiness.
    pub fn finish_rollback(
        &mut self,
        source: &Path,
        original_identity: &HostAuthIdentity,
        was_serving: bool,
    ) -> Result<()> {
        self.check_owner()?;
        verify_selection(&self.root, source)?;
        anyhow::ensure!(
            &crate::identity(source)? == original_identity,
            "account rollback did not restore the original source identity"
        );
        anyhow::ensure!(
            was_serving
                == self.account_origin.as_ref().map_or_else(
                    || self.original_health.is_some(),
                    |origin| origin.was_serving
                ),
            "rollback serving state differs from the actually drained owner"
        );
        if was_serving {
            self.start_account_runtime()?;
            self.verify_runtime(
                source,
                original_identity,
                self.account_origin.as_ref().map_or_else(
                    || {
                        self.original_health
                            .as_ref()
                            .is_some_and(|health| health.store_ready)
                    },
                    |origin| origin.store_ready,
                ),
            )?;
        } else {
            anyhow::ensure!(
                self.marker.phase == Phase::Paused && self.drained,
                "non-serving rollback requires the original drained Paused owner"
            );
            ensure_no_owner(self.host.as_ref())?;
        }
        self.write_phase(Phase::Verified)?;
        self.finish()
    }

    /// Recheck a committed target before removing the marker after its completion
    /// helper returns. A typed receipt alone is not a readiness proof.
    pub fn verify_account_target(
        &self,
        source: &Path,
        operation_id: &str,
        receipt: &HostAuthAccountResult,
        register: bool,
    ) -> Result<()> {
        crate::account_control::validate_account_receipt(receipt, operation_id, source, register)?;
        ensure_applied(receipt)?;
        let target = receipt_target(receipt)?;
        self.verify_ready_runtime(&target, &crate::identity(&target)?)
    }

    fn verify_ready_runtime(&self, profile: &Path, identity: &HostAuthIdentity) -> Result<()> {
        self.verify_runtime(profile, identity, true)
    }

    fn verify_runtime(
        &self,
        profile: &Path,
        identity: &HostAuthIdentity,
        require_ready: bool,
    ) -> Result<()> {
        self.check_owner()?;
        verify_selection(&self.root, profile)?;
        let runtime = self.host.verified_runtime()?;
        runtime.revalidate()?;
        let health = self
            .host
            .probe_runtime()?
            .context("account target Runtime is unavailable")?;
        verify_health_roots(&health, &self.root, profile)?;
        respire_commands::net_rpc::request_scope(&health)?;
        let process = respire_commands::net_rpc::runtime_process_identity(health.pid)?;
        let account_id = match identity {
            HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
            _ => hash(profile.display().to_string().as_bytes()),
        };
        anyhow::ensure!(
            (!require_ready || health.store_ready)
                && health.account_id == account_id
                && health.runtime_version == runtime.version()
                && Path::new(&health.image_path).canonicalize()? == runtime.path()
                && Path::new(&health.exe).canonicalize()? == runtime.path()
                && Path::new(&process.executable).canonicalize()? == runtime.path()
                && process_start_matches(&process.started_at, &health.process_start_id)
                && health.image_sha256.eq_ignore_ascii_case(runtime.sha256())
                && health.managed_id.as_deref() == self.registration.managed_id(),
            "target Runtime does not match the selected account, image or managed owner"
        );
        anyhow::ensure!(
            respire_commands::net_rpc::runtime_process_identity(health.pid)? == process
                && &crate::identity(profile)? == identity,
            "target Runtime or account identity changed during verification"
        );
        self.check_owner()
    }

    fn check_owner(&self) -> Result<()> {
        anyhow::ensure!(
            !self.completed,
            "maintenance transaction is already completed"
        );
        let (root, _) = selected_roots()?;
        anyhow::ensure!(root == self.root, "maintenance installation changed");
        ensure_registration(self.host.as_ref(), &self.root, &self.registration)?;
        let bytes = marker_bytes(&self.root)?.context("maintenance marker disappeared")?;
        let actual = parse_marker(&bytes)?;
        ensure_exact_owner(&actual, &self.marker, &bytes, &self.marker_digest)
    }

    fn write_phase(&mut self, phase: Phase) -> Result<()> {
        // expected_profile may be newly pinned by auth_launch_binding, but the
        // current persisted bytes must still exactly match the last held digest.
        let bytes = marker_bytes(&self.root)?.context("maintenance marker disappeared")?;
        let actual = parse_marker(&bytes)?;
        let mut held = self.marker.clone();
        held.expected_profile = actual.expected_profile.clone();
        ensure_exact_owner(&actual, &held, &bytes, &self.marker_digest)?;
        ensure_registration(self.host.as_ref(), &self.root, &self.registration)?;
        let mut next = self.marker.clone();
        next.phase = phase;
        if matches!(phase, Phase::Starting | Phase::Verified) {
            let (root, selected) = selected_roots()?;
            anyhow::ensure!(root == self.root, "maintenance installation changed");
            next.expected_profile = Some(selected);
        }
        let next_bytes = serde_json::to_vec(&next)?;
        private_write(
            &marker_path(&self.root),
            &next_bytes,
            Some(&self.marker_digest),
        )?;
        self.marker = next;
        self.marker_digest = hash(&next_bytes);
        Ok(())
    }

    fn binding(&self, profile: &Path, purpose: LaunchPurpose) -> Result<OwnedMaintenanceBinding> {
        self.check_owner()?;
        verify_selection(&self.root, profile)?;
        ensure_purpose(purpose, self.marker.phase)?;
        let result = OwnedMaintenanceBinding {
            root: self.root.clone(),
            profile: profile.to_path_buf(),
            marker: self.marker.clone(),
            maintenance: HostControlMaintenanceBinding {
                marker_sha256: self.marker_digest.clone(),
                coordinator_id: self.marker.id.clone(),
                controller_pid: self.marker.owner_pid,
                controller_birth: self.marker.owner_birth.clone(),
                issuer_cli_version: self.marker.expected_version.clone(),
            },
            registration: self.registration.clone(),
            registration_sha256: registration_digest(&self.root, &self.registration)?,
            purpose,
            host: Arc::clone(&self.host),
        };
        result.revalidate()?;
        Ok(result)
    }

    fn clear_active(&self) {
        ACTIVE.with(|slot| {
            let mut active = slot.borrow_mut();
            if active.as_deref() == Some(self.marker.token.as_str()) {
                *active = None;
            }
        });
    }
}
