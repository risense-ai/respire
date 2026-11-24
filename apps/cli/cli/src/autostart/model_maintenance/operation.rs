pub(crate) fn control_install_engines() -> Result<ResultEnvelope> {
    coordinate(ModelMaintenanceKind::InstallEngines)
}
pub(crate) fn control_uninstall_m3() -> Result<ResultEnvelope> {
    coordinate(ModelMaintenanceKind::UninstallM3)
}
pub(crate) fn control_reset_cpu() -> Result<ResultEnvelope> {
    coordinate(ModelMaintenanceKind::ResetCpu)
}

fn roots() -> Result<(PathBuf, PathBuf)> {
    let (root, profile) = respire_client_host::config::control_roots()?;
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&profile)?;
    Ok((root.canonicalize()?, profile.canonicalize()?))
}

fn account(profile: &Path) -> Result<Option<String>> {
    Ok(
        respire_client_host::identity::host_session_user_of_dir(profile, hash)?
            .map(|user| hash(user.as_bytes())),
    )
}

fn verify_owner(
    health: &crate::net_rpc::Health,
    root: &Path,
    profile: &Path,
    binary: &Path,
    manifest: &crate::runtime_fetch::ManagedManifest,
    managed: Option<&super::super::super::Registration>,
    expected_account: Option<&str>,
) -> Result<()> {
    crate::net_rpc::request_scope(health)?;
    if let Some(config) = managed {
        super::lifecycle::verify_owned_runtime(config, health)?;
    }
    let birth =
        crate::autostart::process_birth(health.pid)?.context("model maintenance owner exited")?;
    let process = crate::net_rpc::runtime_process_identity(health.pid)?;
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    let expected_scope = super::model_control::owner_scope_id(profile, expected_account)?;
    // Unlike normal preparation, reset must accept a correctly identified owner
    // whose inference is stalled. Identity and installation checks are unchanged.
    anyhow::ensure!(
        health.managed_id.as_deref() == managed.map(|config| config.id.as_str())
            && health.account_id == expected_scope
            && Path::new(&health.installation_root).canonicalize()? == root
            && Path::new(&health.data_dir).canonicalize()? == profile
            && health.installation_id == hash(root.display().to_string().as_bytes())
            && Path::new(&health.exe).canonicalize()? == binary
            && Path::new(&health.image_path).canonicalize()? == binary
            && Path::new(&process.executable).canonicalize()? == binary
            && health.runtime_version == manifest.version
            && health.image_sha256.eq_ignore_ascii_case(&manifest.sha256)
            && health.process_start_id == format!("{prefix}{birth}")
            && (process.started_at == birth || process.started_at == health.process_start_id),
        "model maintenance owner identity/profile/image mismatch"
    );
    anyhow::ensure!(
        crate::net_rpc::runtime_process_identity(health.pid)? == process,
        "model maintenance owner changed during verification"
    );
    Ok(())
}

fn coordinate(kind: ModelMaintenanceKind) -> Result<ResultEnvelope> {
    crate::runtime_policy::require_host("model maintenance")?;
    anyhow::ensure!(
        super::lifecycle::current_boot_token().is_none(),
        "model maintenance cannot nest"
    );
    let takeover = crate::runtime_policy::takeover_lock()?;
    if crate::runtime_fetch::read_manifest()?.is_none() {
        crate::runtime_fetch::download_and_install(None, None)?;
    }
    let (binary, manifest) = super::model_control::installed_model_runtime()?;
    let (root, profile) = roots()?;
    let expected_account = account(&profile)?;
    let registration = super::read_registration(&root)?;
    let managed = registration
        .as_ref()
        .filter(|config| config.enabled && !config.manually_stopped);
    let owner = crate::rpc::probe_runtime()?;
    // Reuse the complete dead-owner claim verifier. A legacy/live unobservable
    // locator cannot authorize forced termination of a possibly foreign owner.
    let stale = if owner.is_none() && crate::rpc::endpoint_path().try_exists()? {
        Some(
            super::super::super::recovery::StaleOwner::prepare_locked().context(
                "model maintenance cannot prove owner exit; no force-stop or config mutation",
            )?,
        )
    } else {
        None
    };
    if let Some(config) = managed {
        super::lifecycle::owned(config)?;
        super::lifecycle::ensure_control_endpoint(config)?;
        anyhow::ensure!(
            owner.is_some() || !super::super::super::manager::inspect(config)?.running,
            "model maintenance has a running supervisor without an observable owner"
        );
    }
    if let Some(owner) = owner.as_ref() {
        verify_owner(
            owner,
            &root,
            &profile,
            &binary,
            &manifest,
            managed,
            expected_account.as_deref(),
        )?;
        anyhow::ensure!(
            kind == ModelMaintenanceKind::ResetCpu || !owner.inference_stalled(),
            "inference is stalled; use model reset-cpu first"
        );
    }
    let unchanged = || -> Result<()> {
        let (current, release) = super::model_control::installed_model_runtime()?;
        anyhow::ensure!(
            current == binary
                && release.version == manifest.version
                && release.sha256.eq_ignore_ascii_case(&manifest.sha256)
                && roots()? == (root.clone(), profile.clone())
                && account(&profile)? == expected_account,
            "model maintenance installation/profile/account changed"
        );
        Ok(())
    };
    let mut maintenance = super::lifecycle::begin_maintenance_locked()?;
    let result = (|| -> Result<ResultEnvelope> {
        maintenance.check_account_runtime()?;
        maintenance
            .auth_launch_binding(&profile)?
            .context("model maintenance requires durable Paused authority")?;
        let cpu_plan = if kind == ModelMaintenanceKind::ResetCpu {
            Some(local_plan(kind, &root, &profile)?)
        } else {
            None
        };
        if let Some(stale) = stale {
            stale.remove_endpoint_locked()?;
        }
        if let Some(owner) = owner.as_ref() {
            let drained = maintenance.drain_runtime(owner);
            if let Err(error) = drained {
                if kind != ModelMaintenanceKind::ResetCpu || managed.is_some() {
                    return Err(error);
                }
                // Manual CPU recovery retains the explicit force-stop behavior,
                // but only for the full owner we observed and verified above.
                force_stop_observed(
                    owner,
                    &root,
                    &profile,
                    &binary,
                    &manifest,
                    expected_account.as_deref(),
                )?;
            }
        }
        unchanged()?;
        let plan = match cpu_plan {
            Some(plan) => plan,
            None => local_plan(kind, &root, &profile)?,
        };
        #[cfg(windows)]
        let plan = {
            let mut plan = plan;
            if kind == ModelMaintenanceKind::InstallEngines {
                plan.preparation = Some(super::model_control::model_install_plan_locked(
                    &takeover,
                    &mut maintenance,
                    &respire_protocol::ModelInstallIntent {
                        for_engine: Some("npu".into()),
                        ..Default::default()
                    },
                )?);
            }
            plan
        };
        let mut providers: Vec<String> = Vec::new();
        let mut removed = false;
        let mut reset_reconciled = false;
        let receipt = match kind {
            ModelMaintenanceKind::ResetCpu => {
                let mut publication_attempted = false;
                match apply_cpu_reset(
                    &takeover,
                    &mut maintenance,
                    &plan,
                    &unchanged,
                    &mut publication_attempted,
                ) {
                    Ok(receipt) => {
                        anyhow::ensure!(
                            receipt.config_applied && receipt.verified,
                            "CPU reset was not confirmed"
                        );
                        receipt
                    }
                    Err(error) if publication_attempted => {
                        // Read the postcondition once. Never repeat the uncertain CAS.
                        let inspected = local_plan(kind, &root, &profile)
                            .context("CPU reset outcome unknown; no Apply was replayed")?;
                        let candidate = plan
                            .cpu_candidate_sha256
                            .as_deref()
                            .context("CPU reset candidate missing")?;
                        let bytes = fs::read(root.join("inference.json"))?;
                        anyhow::ensure!(
                            hash(&bytes) == candidate
                                && inspected.cpu_candidate_sha256.as_deref() == Some(candidate),
                            "CPU reset outcome unknown: {error:#}; no Apply was replayed"
                        );
                        reset_reconciled = true;
                        ModelMaintenanceReceipt {
                            plan: inspected,
                            phase: ModelMaintenancePhase::Apply,
                            verified: true,
                            config_applied: true,
                            npu_ready: false,
                            warning: None,
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            ModelMaintenanceKind::InstallEngines | ModelMaintenanceKind::UninstallM3 => {
                {
                    let _installation =
                        LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)?;
                    let _library = LibraryLock::acquire(&profile, Duration::ZERO)?;
                    check_path(&root.join("inference.json.lock"))?;
                    let lock = fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(root.join("inference.json.lock"))?;
                    lock.lock()?;
                    unchanged()?;
                    anyhow::ensure!(
                        local_plan(kind, &root, &profile)?.review == plan.review,
                        "model maintenance selection/revision conflict; no mutation performed"
                    );
                    if kind == ModelMaintenanceKind::InstallEngines {
                        providers = install_catalog(&root, plan.catalog.as_ref())?;
                    } else {
                        removed = remove_m3(
                            &root,
                            &profile,
                            plan.uninstall_directory
                                .as_deref()
                                .context("host uninstall directory missing")?,
                        )?;
                    }
                }
                let receipt =
                    verify_maintenance(&takeover, &mut maintenance, &plan, &root, &profile)?;
                receipt
            }
        };
        unchanged()?;
        if kind == ModelMaintenanceKind::ResetCpu {
            anyhow::ensure!(
                Some(hash(&fs::read(root.join("inference.json"))?)) == plan.cpu_candidate_sha256,
                "CPU reset configuration changed before owner restoration"
            );
        }
        let restart =
            owner.is_some() && (kind != ModelMaintenanceKind::ResetCpu || managed.is_some());
        if restart {
            maintenance.start_account_runtime()?;
            let restored =
                crate::rpc::probe_runtime()?.context("model maintenance owner did not resume")?;
            verify_owner(
                &restored,
                &root,
                &profile,
                &binary,
                &manifest,
                managed,
                expected_account.as_deref(),
            )?;
            anyhow::ensure!(
                !owner.as_ref().is_some_and(|owner| owner.store_ready) || restored.store_ready,
                "restored model owner lost store readiness"
            );
        }
        unchanged()?;
        if kind == ModelMaintenanceKind::ResetCpu {
            anyhow::ensure!(
                Some(hash(&fs::read(root.join("inference.json"))?)) == plan.cpu_candidate_sha256,
                "CPU reset configuration changed during owner restoration"
            );
        }
        let mut output = match kind {
            ModelMaintenanceKind::InstallEngines => ResultEnvelope::new(
                "model",
                if receipt.warning.is_some() {
                    Status::Warn
                } else {
                    Status::Ok
                },
                json!({"providers":providers,"npu_ready":false}),
                Vec::new(),
            ),
            ModelMaintenanceKind::UninstallM3 => {
                let status = if removed { Status::Ok } else { Status::Skip };
                let directory = plan
                    .uninstall_directory
                    .as_ref()
                    .context("uninstall directory missing")?;
                ResultEnvelope::new(
                    "model",
                    status,
                    json!({"dir":directory,"removed":removed,"model":"bge-m3"}),
                    vec![Item::new(
                        "model",
                        status,
                        if removed {
                            directory.display().to_string()
                        } else {
                            "not installed".into()
                        },
                    )],
                )
            }
            ModelMaintenanceKind::ResetCpu => ResultEnvelope::new(
                "model",
                Status::Ok,
                json!({"engine":"cpu","force_cpu":true,"runtime_restarted":restart,
                    "stopped_pid":owner.as_ref().map(|owner| owner.pid)}),
                Vec::new(),
            ),
        };
        output.details = serde_json::to_value(&receipt)?;
        if reset_reconciled {
            output.details["reset_postcondition_confirmed_by_read"] = json!(true);
        }
        if let Some(warning) = receipt.warning {
            output.errors.push(warning);
        }
        // Files/config are already accepted. Marker cleanup must not undo them.
        if let Err(error) = maintenance
            .finish()
            .and_then(|()| maintenance.finish_handoff())
        {
            output.status = Status::Warn;
            output.errors.push(format!(
                "model maintenance completed; marker cleanup pending: {error:#}"
            ));
        }
        Ok(output)
    })();
    if result.is_err() {
        maintenance
            .mark(super::lifecycle::Phase::Paused)
            .context("model maintenance failed and Paused fencing could not be restored")?;
    }
    result
}

fn force_stop_observed(
    health: &crate::net_rpc::Health,
    root: &Path,
    profile: &Path,
    binary: &Path,
    manifest: &crate::runtime_fetch::ManagedManifest,
    expected_account: Option<&str>,
) -> Result<()> {
    let endpoint = crate::rpc::endpoint_path();
    let original = fs::read(&endpoint)?;
    let record: serde_json::Value = serde_json::from_slice(&original)?;
    anyhow::ensure!(
        record["pid"].as_u64() == Some(u64::from(health.pid)),
        "CPU reset endpoint changed"
    );
    if crate::autostart::process_birth(health.pid)?.is_some() {
        verify_owner(
            health,
            root,
            profile,
            binary,
            manifest,
            None,
            expected_account,
        )?;
        let identity = crate::net_rpc::runtime_process_identity(health.pid)?;
        crate::net_rpc::kill_runtime(health.pid, &identity)?;
        crate::net_rpc::wait_until_exited(health.pid)?;
    }
    let _installation = LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)?;
    let _library = LibraryLock::acquire(profile, Duration::ZERO)?;
    anyhow::ensure!(
        crate::autostart::process_birth(health.pid)?.is_none() && fs::read(&endpoint)? == original,
        "CPU reset old owner/endpoint changed; cleanup refused"
    );
    fs::remove_file(&endpoint)?;
    respire_spawn::durable::sync_directory(endpoint.parent().context("endpoint parent missing")?)?;
    Ok(())
}
