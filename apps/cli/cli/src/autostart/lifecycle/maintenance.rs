pub(crate) struct Maintenance {
    installation: Option<PathBuf>,
    config: Option<Registration>,
    marker: Option<Marker>,
    marker_digest: Option<String>,
    completed: bool,
}

impl Maintenance {
    fn inactive() -> Self {
        Self {
            installation: None,
            config: None,
            marker: None,
            marker_digest: None,
            completed: true,
        }
    }

    fn begin(mut config: Registration, recover: bool) -> Result<Self> {
        super::supervise::offline_keygen::reject_generic_recovery(&config.installation)?;
        owned(&config)?;
        ensure_control_endpoint(&config)?;
        let registration = manager::inspect(&config)?;
        anyhow::ensure!(
            !registration.registered || config.artifact_hashes.contains_key("service"),
            "unknown existing startup registration; no lifecycle action taken"
        );
        let previous = marker(&config.installation)?;
        anyhow::ensure!(
            previous.is_none() || recover,
            "interrupted runtime maintenance; use runtime restart on the host"
        );
        let previous_digest = if previous.is_some() {
            Some(hash(&fs::read(marker_path(&config.installation))?))
        } else {
            None
        };
        let value = Marker {
            schema: 1,
            id: config.id.clone(),
            token: uuid::Uuid::new_v4().to_string(),
            phase: Phase::Paused,
            expected_profile: None,
            expected_version: env!("CARGO_PKG_VERSION").into(),
            owner_pid: std::process::id(),
            owner_birth: manager::process_birth(std::process::id())?
                .context("controller process identity unavailable")?,
            snapshot: None,
            agent_config: None,
        };
        if let Some(previous) = previous {
            anyhow::ensure!(
                previous.id == config.id,
                "maintenance belongs to another runtime registration"
            );
        }
        let bytes = serde_json::to_vec(&value)?;
        private_write(
            &marker_path(&config.installation),
            &bytes,
            previous_digest.as_deref(),
        )?;
        config.maintenance = true;
        save(&mut config)?;
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
        Ok(Self {
            installation: Some(config.installation.clone()),
            config: Some(config),
            marker: Some(value),
            marker_digest: Some(hash(&bytes)),
            completed: false,
        })
    }

    fn installation_root(&self) -> Result<&Path> {
        self.installation.as_deref().context("maintenance installation is unavailable")
    }

    fn begin_local(root: PathBuf, config: Option<Registration>, recover: bool) -> Result<Self> {
        super::supervise::offline_keygen::reject_generic_recovery(&root)?;
        let id = if let Some(config) = config.as_ref() {
            owned(config)?;
            anyhow::ensure!(config.installation == root, "maintenance installation changed");
            config.id.clone()
        } else {
            format!("host-{}", hash(root.display().to_string().as_bytes()))
        };
        let previous = marker(&root)?;
        anyhow::ensure!(previous.is_none() || recover,
            "interrupted host maintenance; query the original operation before starting another write");
        if let Some(previous) = previous.as_ref() {
            anyhow::ensure!(previous.id == id, "maintenance belongs to another installation coordinator");
        }
        let previous_digest = previous.as_ref().map(|_| fs::read(marker_path(&root)))
            .transpose()?.map(|bytes| hash(&bytes));
        let value = Marker {
            schema: 1, id, token: uuid::Uuid::new_v4().to_string(), phase: Phase::Paused,
            expected_profile: None, expected_version: env!("CARGO_PKG_VERSION").into(),
            owner_pid: std::process::id(), owner_birth: manager::process_birth(std::process::id())?
                .context("controller process identity unavailable")?,
            snapshot: None,
            agent_config: None,
        };
        let bytes = serde_json::to_vec(&value)?;
        private_write(&marker_path(&root), &bytes, previous_digest.as_deref())?;
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
        // Local maintenance never owns or changes the optional OS service registration.
        Ok(Self { installation: Some(root), config: None, marker: Some(value),
            marker_digest: Some(hash(&bytes)), completed: false })
    }

    pub(super) fn mark(&mut self, phase: Phase) -> Result<()> {
        let Some(root) = self.installation.as_ref() else {
            return Ok(());
        };
        let actual = fs::read(marker_path(root))?;
        let current: Marker = serde_json::from_slice(&actual)?;
        let value = self
            .marker
            .as_mut()
            .context("maintenance marker unavailable")?;
        anyhow::ensure!(
            current.id == value.id && current.token == value.token
                && current.agent_config == value.agent_config,
            "maintenance owner changed before phase transition"
        );
        value.phase = phase;
        if matches!(phase, Phase::Starting | Phase::Verified) {
            value.expected_profile = Some(respire_client_host::config::control_roots()?.1.canonicalize()?);
        }
        let bytes = serde_json::to_vec(value)?;
        anyhow::ensure!(bytes.len() <= MAINTENANCE_MARKER_MAX_BYTES, "runtime maintenance marker too large");
        private_write(
            &marker_path(root),
            &bytes,
            Some(&hash(&actual)),
        )?;
        self.marker_digest = Some(hash(&bytes));
        Ok(())
    }

    /// Bind the stopped owner's actual Paused marker without authorizing serving.
    pub(crate) fn auth_launch_binding(&mut self, profile: &Path) -> Result<Option<Value>> {
        let (root, selected) = respire_client_host::config::control_roots()?;
        let root = root.canonicalize()?;
        anyhow::ensure!(selected.canonicalize()? == profile,
            "auth maintenance profile differs from the selected profile");
        if self.installation.is_some() {
            anyhow::ensure!(self.installation_root()? == root, "auth maintenance installation changed");
            if let Some(config) = self.config.as_ref() {
                owned(config)?;
                anyhow::ensure!(config.installation == root, "auth maintenance installation changed");
            }
            let actual = marker(&root)?.context("auth maintenance marker is missing")?;
            let held = self.marker.as_ref().context("auth maintenance ownership is missing")?;
            anyhow::ensure!(actual.phase == Phase::Paused && held.phase == Phase::Paused
                && actual.id == held.id && actual.token == held.token
                && actual.owner_pid == std::process::id()
                && current_boot_token().as_deref() == Some(held.token.as_str())
                && hash(&fs::read(marker_path(&root))?) == self.marker_digest.as_deref()
                    .context("auth maintenance digest is missing")?,
                "auth maintenance owner or phase changed");
            self.marker.as_mut().context("auth maintenance ownership is missing")?
                .expected_profile = Some(profile.to_path_buf());
            self.mark(Phase::Paused)?;
        }
        maintenance_binding(profile, None, true)
    }

    pub(crate) fn check_agent_config_record(&self, record: &respire_protocol::config_control::AgentConfigMaintenance) -> Result<()> {
        let mut preview = self.marker.clone().context("agent config owner missing")?;
        preview.agent_config = Some(serde_json::to_value(record)?);
        anyhow::ensure!(serde_json::to_vec(&preview)?.len() <= MAINTENANCE_MARKER_MAX_BYTES,
            "agent config exceeds maintenance record bound");
        Ok(())
    }

    pub(crate) fn save_agent_config(&mut self, record: &respire_protocol::config_control::AgentConfigMaintenance) -> Result<()> {
        let root = self.installation_root()?.to_path_buf();
        let actual = marker_bytes(&root)?.context("agent config maintenance missing")?;
        anyhow::ensure!(Some(hash(&actual)).as_deref() == self.marker_digest.as_deref(),
            "agent config maintenance changed");
        let mut updated = self.marker.clone().context("agent config owner missing")?;
        anyhow::ensure!(updated.phase == Phase::Paused && updated.snapshot.is_none(), "agent config requires Paused maintenance");
        updated.agent_config = Some(serde_json::to_value(record)?);
        let bytes = serde_json::to_vec(&updated)?;
        anyhow::ensure!(bytes.len() <= MAINTENANCE_MARKER_MAX_BYTES, "agent config exceeds maintenance record bound");
        private_write(&marker_path(&root), &bytes, Some(&hash(&actual)))?;
        self.marker = Some(updated);
        self.marker_digest = Some(hash(&bytes));
        Ok(())
    }

    /// Runtime may add only the original operation's receipt, never change its owner or intent.
    pub(crate) fn refresh_agent_config(&mut self) -> Result<respire_protocol::config_control::AgentConfigMaintenance> {
        let root = self.installation_root()?;
        let bytes = marker_bytes(root)?.context("agent config maintenance missing")?;
        let actual: Marker = serde_json::from_slice(&bytes)?;
        let held = self.marker.as_ref().context("agent config owner missing")?;
        let previous: respire_protocol::config_control::AgentConfigMaintenance = serde_json::from_value(
            held.agent_config.clone().context("agent config review missing")?)?;
        let current: respire_protocol::config_control::AgentConfigMaintenance = serde_json::from_value(
            actual.agent_config.clone().context("agent config review disappeared")?)?;
        let mut expected = previous.clone();
        expected.receipt = current.receipt.clone();
        anyhow::ensure!(current == expected && current.receipt.as_ref().is_none_or(|receipt| receipt.review == previous.review)
            && previous.receipt.as_ref().is_none_or(|receipt| current.receipt.as_ref() == Some(receipt)),
            "agent config receipt changed its original operation");
        let mut expected_marker = held.clone();
        expected_marker.agent_config = actual.agent_config.clone();
        anyhow::ensure!(serde_json::to_value(&expected_marker)? == serde_json::to_value(&actual)?,
            "agent config maintenance owner changed");
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&bytes));
        Ok(current)
    }

    pub(crate) fn drain_runtime(&self, health: &crate::net_rpc::Health) -> Result<()> {
        let Some(config) = self.config.as_ref() else {
            return crate::rpc::drain_occupant(health, Duration::from_secs(60));
        };
        owned(config)?;
        ensure_control_endpoint(config)?;
        manager::inspect(config)?;
        verify_owned_runtime(config, health)?;
        crate::rpc::drain_occupant(health, Duration::from_secs(60))?;
        // Finish stopping the supervisor before a profile change makes old health unavailable.
        stop_drained_supervisor(config)
    }

    /// Only an independently checked live target may advance an account transaction.
    pub(crate) fn start_account_target(
        &mut self, source: &Path, operation_id: &str,
        receipt: &respire_protocol::host_control::HostAuthAccountResult, register: bool,
    ) -> Result<()> {
        super::validate_auth_account_receipt(receipt, operation_id, source, register)?;
        self.start_account_runtime()?;
        self.account_target_verified(source, operation_id, receipt, register)
    }

    /// Both host lifecycle modes require the installed independent Runtime.
    pub(crate) fn check_account_runtime(&self) -> Result<()> {
        self.installation_root()?;
        let (binary, _) = super::supervise::installed_auth_runtime()?;
        if let Some(config) = self.config.as_ref() {
            owned(config)?;
            anyhow::ensure!(registered_runtime_image(config, None)? == binary
                && !config.environment.contains_key("GH_TOKEN")
                && !config.environment.contains_key("GITHUB_TOKEN"),
                "account startup requires the installed managed Runtime and nonsecret registration environment");
        } else if let Some(raw) = respire_client_host::env::var_os("RSRS_RUNTIME_BIN") {
            anyhow::ensure!(PathBuf::from(raw).canonicalize()? == binary,
                "account startup cannot select an override instead of the installed managed Runtime");
        }
        Ok(())
    }

    pub(crate) fn start_account_runtime(&mut self) -> Result<()> {
        self.start_account_runtime_with_expectation(
            respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
    }

    pub(crate) fn start_account_runtime_with_expectation(&mut self,
        expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation) -> Result<()> {
        self.check_account_runtime()?;
        self.mark(Phase::Starting)?;
        crate::rpc::ensure_daemon_locked_with_expectation(expectation)
    }

    pub(crate) fn resume_managed_runtime(&mut self) -> Result<()> {
        // begin_maintenance_locked keeps stopped or disabled registrations in local maintenance.
        if self.config.is_some() {
            self.start_account_runtime()?;
        }
        Ok(())
    }

    /// Only an independently checked live target may advance an account transaction.
    pub(crate) fn account_target_verified(
        &mut self, source: &Path, operation_id: &str,
        receipt: &respire_protocol::host_control::HostAuthAccountResult, register: bool,
    ) -> Result<()> {
        super::supervise::verify_account_target(receipt, operation_id, source, register)?;
        let root = self.installation_root()?.to_path_buf();
        if let Some(config) = self.config.as_ref() { owned(config)?; }
        let target = receipt.target_profile.as_ref().context("account target is missing")?;
        let target = respire_client_host::config::normalize_control_path(target)?.canonicalize()?;
        let path = marker_path(&root);
        let bytes = fs::read(&path)?;
        let mut actual = marker(&root)?.context("account maintenance marker is missing")?;
        let held = self.marker.as_ref().context("account maintenance ownership is missing")?;
        anyhow::ensure!(actual.id == held.id && actual.token == held.token
            && actual.owner_pid == std::process::id() && actual.owner_birth == held.owner_birth
            && current_boot_token().as_deref() == Some(held.token.as_str())
            && matches!(actual.phase, Phase::Starting | Phase::Verified)
            && actual.expected_profile.as_ref() == Some(&target)
            && actual.expected_version == env!("CARGO_PKG_VERSION") && controller_alive(&actual)?
            && serde_json::to_vec(&actual)? == serde_json::to_vec(&serde_json::from_slice::<Marker>(&bytes)?)?,
            "account target launch does not belong to this maintenance controller");
        actual.phase = Phase::Verified;
        let next = serde_json::to_vec(&actual)?;
        private_write(&path, &next, Some(&hash(&bytes)))?;
        self.marker = Some(actual);
        self.marker_digest = Some(hash(&next));
        Ok(())
    }

    /// Completion is a separate grant role. It never converts Paused/Starting into Verified.
    pub(crate) fn account_complete_launch_binding(
        &self, source: &Path, operation_id: &str,
        receipt: &respire_protocol::host_control::HostAuthAccountResult, register: bool,
    ) -> Result<Option<Value>> {
        super::validate_auth_account_receipt(receipt, operation_id, source, register)?;
        anyhow::ensure!(matches!(receipt.outcome,
            respire_protocol::host_control::HostAuthAccountOutcome::AwaitingVerification
                | respire_protocol::host_control::HostAuthAccountOutcome::Committed),
            "account completion requires an applied candidate receipt");
        let (root, selected) = respire_client_host::config::control_roots()?;
        let root = root.canonicalize()?;
        let target = receipt.target_profile.as_ref().context("account target is missing")?;
        let target = respire_client_host::config::normalize_control_path(target)?.canonicalize()?;
        anyhow::ensure!(self.installation_root()? == root, "account completion installation changed");
        if let Some(config) = self.config.as_ref() { owned(config)?; }
        let actual = marker(&root)?.context("account completion marker is missing")?;
        let held = self.marker.as_ref().context("account completion ownership is missing")?;
        anyhow::ensure!(selected.canonicalize()? == target,
            "account completion installation or selection changed");
        verified_account_marker(&actual, held, &target, &fs::read(marker_path(&root))?,
            self.marker_digest.as_deref().context("account completion marker digest is missing")?)?;
        maintenance_binding(&target, None, false)
    }

    /// Narrow offline-keygen completion: verify the live actor before advancing.
    pub(crate) fn offline_keygen_target_verified(&mut self, profile: &Path, operation: &str, session: &str) -> Result<()> {
        let root = self.installation_root()?.to_path_buf();
        super::supervise::offline_keygen::verify_target(&root, profile, operation, session)?;
        self.offline_keygen_starting_verified(profile)
    }

    pub(crate) fn offline_keygen_rollback_verified(&mut self, profile: &Path,
        identity: &respire_protocol::HostAuthIdentity, store_ready: bool) -> Result<()> {
        let root = self.installation_root()?.to_path_buf();
        super::supervise::offline_keygen::verify_original_target(&root, profile, identity, store_ready)?;
        self.offline_keygen_starting_verified(profile)
    }

    /// Capture the actual Starting marker before it advances or is removed.
    pub(crate) fn offline_keygen_start_binding(&self, profile: &Path, token: &str, coordinator: &str)
        -> Result<respire_protocol::HostControlMaintenanceBinding> {
        let root = self.installation_root()?;
        let actual = marker(root)?.context("offline keygen Starting marker missing")?;
        let held = self.marker.as_ref().context("offline keygen Starting owner missing")?;
        anyhow::ensure!(actual.phase == Phase::Starting && held.phase == Phase::Starting
            && actual.id == coordinator && actual.id == held.id && actual.token == token && actual.token == held.token
            && actual.owner_pid == std::process::id() && actual.owner_birth == held.owner_birth
            && controller_alive(&actual)? && actual.expected_profile.as_deref() == Some(profile)
            && current_boot_token().as_deref() == Some(token)
            && hash(&fs::read(marker_path(root))?) == self.marker_digest.as_deref().context("offline keygen Starting digest missing")?,
            "offline keygen Starting controller changed");
        serde_json::from_value(maintenance_binding(profile, None, false)?
            .context("offline keygen Starting binding missing")?).map_err(Into::into)
    }

    fn offline_keygen_starting_verified(&mut self, profile: &Path) -> Result<()> {
        let root = self.installation_root()?.to_path_buf();
        let actual = marker(&root)?.context("offline keygen marker missing")?;
        let held = self.marker.as_ref().context("offline keygen ownership missing")?;
        anyhow::ensure!(actual.phase == Phase::Starting && held.phase == Phase::Starting
            && actual.id == held.id && actual.token == held.token && actual.owner_birth == held.owner_birth
            && actual.owner_pid == std::process::id() && controller_alive(&actual)?
            && actual.expected_profile.as_deref() == Some(profile)
            && current_boot_token().as_deref() == Some(held.token.as_str())
            && hash(&fs::read(marker_path(&root))?) == self.marker_digest.as_deref().context("offline keygen marker digest missing")?,
            "offline keygen starting marker changed");
        self.mark(Phase::Verified)
    }

    /// Local coordinator check only; this never issues an account-completion grant.
    pub(crate) fn verify_offline_keygen_completion(&self, profile: &Path) -> Result<()> {
        let (root, selected) = respire_client_host::config::control_roots()?;
        let root = root.canonicalize()?;
        anyhow::ensure!(self.installation_root()? == root && selected.canonicalize()? == profile,
            "offline keygen selection changed before completion");
        let actual = marker(&root)?.context("offline keygen completion marker missing")?;
        let held = self.marker.as_ref().context("offline keygen completion ownership missing")?;
        verified_account_marker(&actual, held, profile, &fs::read(marker_path(&root))?,
            self.marker_digest.as_deref().context("offline keygen completion digest missing")?)
    }

    /// Restore only the verified serving owner after durable maintenance completion.
    pub(crate) fn finish_handoff(&self) -> Result<()> {
        self.finish_handoff_with_expectation(respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)
    }

    pub(crate) fn finish_handoff_with_expectation(&self,
        expectation: respire_host_control::lifecycle_io::AccountRuntimeExpectation) -> Result<()> {
        #[cfg(not(feature = "client2-host-prototype"))]
        let _ = expectation;
        #[cfg(feature = "client2-host-prototype")]
        {
            anyhow::ensure!(self.completed, "compatibility handoff requires completed maintenance");
            let Some(root) = self.installation.as_deref() else { return Ok(()); };
            if read_registration(root)?.is_some() { return Ok(()); }
            anyhow::ensure!(current_boot_token().is_none() && !marker_path(root).try_exists()?,
                "completed operation cannot hand off while another maintenance owns the installation");
            anyhow::ensure!(respire_client_host::config::control_roots()?.0.canonicalize()? == root,
                "completed maintenance installation changed before compatibility handoff");
            if let Some(health) = crate::rpc::probe_runtime()? {
                let profile = respire_client_host::config::control_roots()?.1.canonicalize()?;
                let identity = super::supervise::handoff_identity(&profile)?;
                let account = match identity {
                    respire_protocol::host_control::HostAuthIdentity::Bound { account_id, .. } => account_id,
                    _ => hash(profile.display().to_string().as_bytes()),
                };
                let (binary, manifest) = super::supervise::installed_model_runtime()?;
                super::supervise::verify_model_owner(&health, root, &profile, None,
                    &binary, &manifest, Some(&account))?;
                expectation.confirm_credentials(&health).context(
                    "operation committed and maintenance finished; Runtime credential confirmation failed; do not repeat the operation")?;
            } else {
                anyhow::ensure!(!crate::rpc::endpoint_path().try_exists()?,
                    "completed maintenance has an unverified recorded owner; no Runtime was started");
            }
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        self.finish_with_core_config(true)
    }

    pub(crate) fn finish_core_config_query(&mut self) -> Result<()> {
        self.finish_with_core_config(false)
    }

    fn finish_with_core_config(&mut self, apply_startup: bool) -> Result<()> {
        if self.completed {
            return Ok(());
        }
        // Offline maintenance must not inspect an unrelated listener on the default port.
        if crate::rpc::endpoint_path().try_exists()? {
            let owner = crate::rpc::probe_runtime()?.context("owned Runtime exited before core configuration")?;
            self.mark(Phase::Paused)?;
            if apply_startup { super::supervise::apply_startup_core_config(&owner, Some(self))?; }
        }
        self.mark(Phase::Verified)?;
        if let Some(config) = self.config.as_mut() {
            config.maintenance = false;
            save(config)?;
        }
        let path = marker_path(self.installation_root()?);
        anyhow::ensure!(
            hash(&fs::read(&path)?)
                == self
                    .marker_digest
                    .as_deref()
                    .context("maintenance digest missing")?,
            "maintenance changed before completion"
        );
        fs::remove_file(&path)?;
        // Already verified, so a namespace-cleanup warning must never trigger a
        // later profile rollback after requests have been accepted.
        if respire_spawn::durable::sync_directory(
            path.parent().context("maintenance parent missing")?,
        )
        .is_err()
        {
            eprintln!("Runtime verified; maintenance namespace cleanup needs a later check.");
        }
        self.completed = true;
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
        Ok(())
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        if self.installation.is_some() {
            ACTIVE.with(|slot| {
                let mut active = slot.borrow_mut();
                if active.as_deref() == self.marker.as_ref().map(|value| value.token.as_str()) {
                    *active = None;
                }
            });
        }
    }
}
