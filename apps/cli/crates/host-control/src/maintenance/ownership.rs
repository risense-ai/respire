impl Maintenance {
    /// Caller retains the same takeover lock through drain, private exchange,
    /// target verification and completion. An AwaitingInput continuation may keep
    /// this owner alive, but must reacquire takeover before using it again.
    pub(crate) fn begin_locked(
        takeover: &LibraryLock,
        host: Arc<dyn HostLifecycle>,
        recovery: RecoveryMode,
    ) -> Result<Self> {
        Self::begin_with_token(takeover, host, recovery, None)
    }

    fn begin_with_token(
        takeover: &LibraryLock,
        host: Arc<dyn HostLifecycle>,
        recovery: RecoveryMode,
        retained_token: Option<&str>,
    ) -> Result<Self> {
        host.require_account_write()?;
        anyhow::ensure!(
            current_boot_token().is_none(),
            "host maintenance already has an active controller"
        );
        let (root, _) = selected_roots()?;
        reject_offline_keygen(&root)?;
        anyhow::ensure!(
            takeover.protects(&root.join("runtime/takeover"))?,
            "maintenance requires this installation's takeover ownership"
        );
        let registration = host.inspect_registration(&root)?;
        validate_registration_mode(&root, &registration)?;
        let previous = marker_bytes(&root)?;
        let prior = previous.as_deref().map(parse_marker).transpose()?;
        anyhow::ensure!(prior.is_none() || recovery == RecoveryMode::QueryOriginal,
            "interrupted host maintenance; query the original operation before starting another write");
        let id = registration.coordinator_id(&root);
        if let Some(prior) = prior.as_ref() {
            anyhow::ensure!(
                prior.id == id && prior.snapshot.is_none() && prior.agent_config.is_none(),
                "maintenance belongs to another coordinator or operation family"
            );
            anyhow::ensure!(
                !controller_alive(prior)? || prior.owner_pid == std::process::id(),
                "maintenance still belongs to a live controller"
            );
        }
        let value = Marker {
            schema: 1,
            id,
            token: account_owner_token(recovery, prior.as_ref(), retained_token)?,
            phase: Phase::Paused,
            expected_profile: None,
            expected_version: host.issuer_version().to_owned(),
            owner_pid: std::process::id(),
            owner_birth: process_birth(std::process::id())?
                .context("controller process identity unavailable")?,
            snapshot: None,
            agent_config: None,
        };
        anyhow::ensure!(
            !value.expected_version.is_empty(),
            "maintenance issuer version is missing"
        );
        let bytes = serde_json::to_vec(&value)?;
        private_write(
            &marker_path(&root),
            &bytes,
            previous.as_ref().map(|bytes| hash(bytes)).as_deref(),
        )?;
        if matches!(registration, RegistrationMode::Managed { .. }) {
            host.set_maintenance(&root, true)?;
        }
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
        Ok(Self {
            root,
            host,
            registration,
            marker: value,
            marker_digest: hash(&bytes),
            drained: false,
            original_health: None,
            account_origin: None,
            account_record: None,
            completed: false,
        })
    }

    /// Only the shared key coordinator calls this after validating the exact
    /// persisted journal (or its exact archive). Request input cannot construct
    /// the original maintenance token or coordinator.
    pub(crate) fn resume_key_locked(
        takeover: &LibraryLock,
        host: Arc<dyn HostLifecycle>,
        root: &Path,
        profile: &Path,
        source: &Path,
        token: &str,
        coordinator: &str,
    ) -> Result<Self> {
        host.require_account_write()?;
        reject_offline_keygen(root)?;
        anyhow::ensure!(
            current_boot_token().is_none(),
            "host maintenance already has a controller"
        );
        verify_selection(root, profile)?;
        anyhow::ensure!(
            takeover.protects(&root.join("runtime/takeover"))?,
            "key recovery requires this installation's takeover ownership"
        );
        let id = uuid::Uuid::parse_str(token).context("invalid key maintenance token")?;
        anyhow::ensure!(
            id.get_version_num() == 4
                && id.to_string() == token
                && respire_client_host::config::normalize_control_path(source)?.canonicalize()?
                    == source,
            "key recovery requires the canonical original owner and source"
        );
        let registration = host.inspect_registration(root)?;
        validate_registration_mode(root, &registration)?;
        anyhow::ensure!(
            registration.coordinator_id(root) == coordinator,
            "key recovery registration or coordinator changed"
        );
        let previous = marker_bytes(root)?;
        if let Some(bytes) = previous.as_ref() {
            let value = parse_marker(bytes)?;
            anyhow::ensure!(
                value.id == coordinator
                    && value.token == token
                    && value.snapshot.is_none()
                    && value.agent_config.is_none()
                    && matches!(value.expected_profile.as_deref(), Some(expected) if expected == profile || expected == source)
                    && value.expected_version == host.issuer_version()
                    && (!controller_alive(&value)? || value.owner_pid == std::process::id()),
                "key recovery does not own the original maintenance marker"
            );
        }
        let value = Marker {
            schema: 1,
            id: coordinator.to_owned(),
            token: token.to_owned(),
            phase: Phase::Paused,
            expected_profile: Some(profile.to_path_buf()),
            expected_version: host.issuer_version().to_owned(),
            owner_pid: std::process::id(),
            owner_birth: process_birth(std::process::id())?
                .context("key recovery controller birth unavailable")?,
            snapshot: None,
            agent_config: None,
        };
        let bytes = serde_json::to_vec(&value)?;
        private_write(
            &marker_path(root),
            &bytes,
            previous.as_ref().map(|bytes| hash(bytes)).as_deref(),
        )?;
        if matches!(registration, RegistrationMode::Managed { .. }) {
            host.set_maintenance(root, true)?;
        }
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(token.to_owned()));
        Ok(Self {
            root: root.to_path_buf(),
            host,
            registration,
            marker: value,
            marker_digest: hash(&bytes),
            drained: false,
            original_health: None,
            account_origin: None,
            account_record: None,
            completed: false,
        })
    }

    pub(crate) fn key_recovery_binding(&self) -> Result<(String, String)> {
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Paused && self.drained,
            "key recovery metadata requires the drained Paused owner"
        );
        Ok((self.marker.token.clone(), self.marker.id.clone()))
    }

    pub(crate) fn pause_key(&mut self) -> Result<()> {
        self.check_owner()?;
        self.write_phase(Phase::Paused)
    }

    /// Restored serving/readiness expectations come only from the shared key
    /// coordinator's revalidated journal, not an authorization flag from a client.
    pub(crate) fn finish_key(
        &mut self,
        profile: &Path,
        identity: &HostAuthIdentity,
        restore_owner: bool,
        restore_store: bool,
    ) -> Result<()> {
        self.check_owner()?;
        verify_selection(&self.root, profile)?;
        anyhow::ensure!(
            &crate::identity(profile)? == identity,
            "key terminal identity differs from the selected profile"
        );
        anyhow::ensure!(
            !restore_store || restore_owner,
            "key recovery store expectation has no original owner"
        );
        let require_ready = restore_store
            || self
                .original_health
                .as_ref()
                .is_some_and(|health| health.store_ready);
        if self.marker.phase == Phase::Verified {
            self.verify_runtime(profile, identity, true)?;
        } else if restore_owner {
            anyhow::ensure!(
                self.marker.phase == Phase::Starting,
                "key owner must be explicitly restored before completion"
            );
            self.verify_runtime(profile, identity, require_ready)?;
        } else {
            anyhow::ensure!(
                self.marker.phase == Phase::Paused && self.drained,
                "non-serving key completion requires the drained Paused owner"
            );
            ensure_no_owner(self.host.as_ref())?;
        }
        self.check_owner()?;
        anyhow::ensure!(
            &crate::identity(profile)? == identity,
            "key identity changed during owner restoration"
        );
        self.write_phase(Phase::Verified)?;
        self.finish()
    }

    pub fn installation_root(&self) -> &Path {
        &self.root
    }
    pub fn phase(&self) -> Phase {
        self.marker.phase
    }
    pub fn coordinator_id(&self) -> &str {
        &self.marker.id
    }

    pub fn check_account_runtime(&self) -> Result<()> {
        self.check_owner()?;
        self.host.require_account_write()?;
        self.host.verified_runtime()?.revalidate()
    }

    /// Consume the exact owner observed by the trusted coordinator before it
    /// published Paused. Some serving modes deliberately revoke health when the
    /// marker appears; probing again before the identity-bound stop deadlocks
    /// maintenance. The native owner and image are rechecked before any effect.
    pub(crate) fn drain_observed_runtime(&mut self, observed: Option<&Health>) -> Result<()> {
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Paused,
            "drain requires Paused maintenance"
        );
        if self.drained {
            // Key setup may call this again after writing its original journal.
            // Never send a second stop for an already consumed observation.
            ensure_no_owner(self.host.as_ref())?;
            return Ok(());
        }
        match observed {
            Some(health) => self.drain_runtime(health),
            None => {
                ensure_no_owner(self.host.as_ref())?;
                self.drained = true;
                Ok(())
            }
        }
    }

    pub(crate) fn drain_runtime(&mut self, health: &Health) -> Result<()> {
        self.check_owner()?;
        anyhow::ensure!(
            self.marker.phase == Phase::Paused && !self.drained,
            "drain requires an unconsumed Paused owner observation"
        );
        let (_, selected) = selected_roots()?;
        verify_health_roots(health, &self.root, &selected)?;
        respire_commands::net_rpc::request_scope(health)?;
        let process = respire_commands::net_rpc::runtime_process_identity(health.pid)?;
        let image = Path::new(&process.executable).canonicalize()?;
        let image_sha256 = crate::launch::hash_file(&image)?;
        verify_observed_process(health, &process, &image_sha256)?;
        anyhow::ensure!(
            health.managed_id.as_deref() == self.registration.managed_id(),
            "observed runtime belongs to a different managed owner"
        );
        let birth =
            process_birth(health.pid)?.context("observed runtime process already exited")?;
        anyhow::ensure!(
            process_start_matches(&birth, &health.process_start_id)
                && respire_commands::net_rpc::runtime_process_identity(health.pid)? == process,
            "observed runtime process changed before typed drain"
        );
        // No fresh health read here: the checked observation supplies the exact
        // installation, instance, account and generation for request_stop_for.
        self.host.drain_runtime(health)?;
        anyhow::ensure!(
            process_birth(health.pid)?.as_deref() != Some(birth.as_str()),
            "runtime drain returned before the exact owner exited"
        );
        ensure_no_owner(self.host.as_ref())?;
        self.check_owner()?;
        if self.original_health.is_none() {
            self.original_health = Some(health.clone());
        }
        self.drained = true;
        Ok(())
    }

    pub fn auth_launch_binding(
        &mut self,
        profile: &Path,
    ) -> Result<Option<OwnedMaintenanceBinding>> {
        self.check_owner()?;
        anyhow::ensure!(
            self.drained && self.marker.phase == Phase::Paused,
            "account authorization requires the drained Paused owner"
        );
        verify_selection(&self.root, profile)?;
        ensure_no_owner(self.host.as_ref())?;
        self.marker.expected_profile = Some(profile.to_path_buf());
        self.write_phase(Phase::Paused)?;
        Ok(Some(self.binding(profile, LaunchPurpose::Account)?))
    }
}
