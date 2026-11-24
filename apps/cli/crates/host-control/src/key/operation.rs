fn control_key(host: Arc<dyn HostLifecycle>, action: Action<'_>) -> Result<ResultEnvelope> {
    control_key_inner(host, action, None, None)
}

fn control_key_inner(host: Arc<dyn HostLifecycle>, action: Action<'_>,
    source: Option<&Path>, supplemental: Option<HostKeyInput>,
) -> Result<ResultEnvelope> {
    host.require_account_write()?;
    anyhow::ensure!(
        crate::maintenance::current_boot_token().is_none(),
        "key control must own maintenance"
    );
    let (root, profile) = respire_client_host::config::control_roots()?;
    let takeover = crate::maintenance::takeover_lock()?;
    let image = host.verified_runtime()?;
    image.revalidate()?;
    if source.is_none() {
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&profile)?;
    }
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    let path = root.join("runtime/autostart/key-control.json");
    let active = read_original(&path)?;
    let (saved, archived) = if active.is_some() {
        (active, false)
    } else {
        match &action {
            Action::Query(id) | Action::Rollback(id) => {
                anyhow::ensure!(
                    canonical(id),
                    "key query requires a canonical original operation ID"
                );
                (read_original(&archive_path(&root, id))?, true)
            }
            _ => (None, false),
        }
    };
    let expected = crate::identity(&profile)?;
    let owner = host.probe_runtime()?;
    anyhow::ensure!(
        owner.is_some() || !host.endpoint_path().try_exists()?,
        "key control has a recorded owner without verifiable health"
    );
    if let Some(owner) = owner.as_ref() {
        verify_owner(host.as_ref(), owner, &root, &profile, &image, &expected)?;
    }
    let (mut original, mut digest, mut maintenance, reconnect) = if let Some((original, digest)) =
        saved
    {
        anyhow::ensure!(
            original.schema_version == HOST_KEY_SCHEMA_V
                && canonical(&original.operation_id)
                && original.key == original.operation_id
                && canonical(&original.token),
            "invalid key recovery binding"
        );
        anyhow::ensure!(
            original.root == root
                && (original.profile == profile
                    || matches!(original.operation, HostKeyOperation::Fivekeys { .. })),
            "key recovery belongs to another selection"
        );
        match &action {
            Action::Reset {
                operation, resume, ..
            } => {
                anyhow::ensure!(
                    serde_json::to_value(operation)? == serde_json::to_value(&original.operation)?,
                    "key continuation changed the original operation"
                );
                if let Some(receipt) = resume {
                    validate_receipt(receipt, &original)?;
                }
            }
            Action::Query(id) | Action::Rollback(id) => anyhow::ensure!(
                *id == original.operation_id,
                "key recovery requires the original operation ID"
            ),
        }
        if let Some(source) = source {
            let source = source.canonicalize()?;
            anyhow::ensure!(source == original.profile,
                "key continuation source differs from original operation");
            if supplemental.is_none() {
                anyhow::ensure!(profile == source, "key query requires the original selected source");
            }
        }
        let maintenance = Maintenance::resume_key_locked(
            &takeover,
            Arc::clone(&host),
            &root,
            &profile,
            &original.profile,
            &original.token,
            &original.coordinator,
        )?;
        (original, digest, maintenance, true)
    } else {
        let operation = match &action {
            Action::Reset {
                operation,
                resume: None,
                ..
            } => operation.clone(),
            _ => anyhow::bail!(
                "original key recovery metadata is missing; no new operation was sent"
            ),
        };
        let mut maintenance =
            Maintenance::begin_locked(&takeover, Arc::clone(&host), RecoveryMode::Fresh)?;
        maintenance.drain_observed_runtime(owner.as_ref())?;
        maintenance
            .auth_launch_binding(&profile)?
            .context("key control requires an actual drained Paused binding")?;
        let (token, coordinator) = maintenance.key_recovery_binding()?;
        let id = uuid::Uuid::new_v4().to_string();
        let original = Original {
            schema_version: HOST_KEY_SCHEMA_V,
            root: root.clone(),
            profile: profile.clone(),
            operation_id: id.clone(),
            key: id,
            source_identity: expected,
            operation,
            token,
            coordinator,
            was_serving: owner.is_some(),
            store_ready: owner.as_ref().is_some_and(|health| health.store_ready),
            sent: false,
            candidate: None,
            rolled_back: false,
        };
        let bytes = serde_json::to_vec(&original)?;
        crate::maintenance::write_recovery(&path, &bytes, None)?;
        let digest = crate::launch::hash(&bytes);
        (original, digest, maintenance, false)
    };
    let mut finished = false;
    let result: Result<ResultEnvelope> = (|| {
        maintenance.check_account_runtime()?;
        maintenance.drain_observed_runtime(owner.as_ref())?;
        maintenance
            .auth_launch_binding(&profile)?
            .context("key control requires an actual drained Paused binding")?;
        if !original.sent && matches!(action, Action::Rollback(_)) {
            anyhow::ensure!(profile == original.profile && crate::identity(&profile)? == original.source_identity,
                "unsent key source changed before host compensation");
            material::rollback(&original)?;
            if original.was_serving {
                maintenance.start_account_runtime()?;
                let restored = host.probe_runtime()?.context("unsent key owner did not resume")?;
                verify_owner(host.as_ref(), &restored, &root, &profile, &image, &original.source_identity)?;
                anyhow::ensure!(!original.store_ready || restored.store_ready, "unsent key owner lost store readiness");
            }
            maintenance.finish_key(&profile, &original.source_identity, original.was_serving, original.store_ready)?;
            finished = true;
            host.finish_key_handoff(&profile, &original.source_identity, original.was_serving)?;
            anyhow::ensure!(read_original(&path)?.as_ref().map(|(_, hash)| hash.as_str()) == Some(digest.as_str()),
                "unsent key recovery changed before cleanup");
            std::fs::remove_file(&path)?;
            respire_spawn::durable::sync_directory(path.parent().context("key recovery parent missing")?)?;
            return Ok(ResultEnvelope::new("key-control", Status::Ok,
                json!({"operation_id":original.operation_id,"state":"not_sent","credential_cleanup":"completed"}), Vec::new()));
        }
        let deadline = Instant::now() + Duration::from_secs(185);
        if let Action::Reset { input, .. } = &action {
            let (candidate, private) = material::prepare(&original, input)?;
            let new_candidate = candidate.is_some();
            if let Some(candidate) = candidate {
                original.candidate = Some(candidate);
                let bytes = serde_json::to_vec(&original)?;
                crate::maintenance::write_recovery(&path, &bytes, Some(&digest))?;
                digest = crate::launch::hash(&bytes);
            }
            if !original.sent || new_candidate { material::persist(&original, &private)?; }
        }
        let mut receipt = if original.sent {
            exchange(
                &takeover,
                &mut maintenance,
                host.as_ref(),
                &original,
                if matches!(action, Action::Rollback(_)) { HOST_KEY_ROLLBACK_METHOD } else { HOST_KEY_RESULT_METHOD },
                None,
                deadline,
            )?
        } else {
            require_unsent_admission(&action, &original, &crate::identity(&profile)?)?;
            // Persist uncertainty before sending any operation; a failed write is never retried.
            original.sent = true;
            let bytes = serde_json::to_vec(&original)?;
            crate::maintenance::write_recovery(&path, &bytes, Some(&digest))?;
            digest = crate::launch::hash(&bytes);
            let input = match &action {
                Action::Reset { input, .. } => Some(input),
                _ => None,
            };
            exchange(
                &takeover,
                &mut maintenance,
                host.as_ref(),
                &original,
                execute_method(&original.operation),
                input,
                deadline,
            )?
        };
        if source.is_some()
            && !host_completion_required(&receipt)
            && (supplemental.is_none() || receipt.outcome != HostKeyOutcome::AwaitingInput)
        {
            maintenance.pause_key()?;
            return Ok(ResultEnvelope::new("key-control", Status::Pending,
                serde_json::to_value(receipt)?, Vec::new()));
        }
        if let Some(input) = supplemental.as_ref().filter(|_| receipt.outcome == HostKeyOutcome::AwaitingInput) {
            let (candidate, private) = material::prepare(&original, input)?;
            let new_candidate = candidate.is_some();
            if let Some(candidate) = candidate {
                original.candidate = Some(candidate);
                let bytes = serde_json::to_vec(&original)?;
                crate::maintenance::write_recovery(&path, &bytes, Some(&digest))?;
                digest = crate::launch::hash(&bytes);
            }
            if new_candidate { material::persist(&original, &private)?; }
            receipt = exchange(&takeover, &mut maintenance, host.as_ref(), &original,
                execute_method(&original.operation), Some(input), deadline)?;
        }
        if supplemental.is_none() && reconnect && receipt.outcome == HostKeyOutcome::AwaitingInput {
            match &action {
                Action::Reset {
                    input,
                    resume: Some(previous),
                    ..
                } if previous.outcome == HostKeyOutcome::AwaitingInput => {
                    receipt = exchange(
                        &takeover,
                        &mut maintenance,
                        host.as_ref(),
                        &original,
                        execute_method(&original.operation),
                        Some(input),
                        deadline,
                    )?;
                }
                Action::Rollback(_) => {
                    receipt = exchange(
                        &takeover,
                        &mut maintenance,
                        host.as_ref(),
                        &original,
                        HOST_KEY_ROLLBACK_METHOD,
                        None,
                        deadline,
                    )?;
                }
                _ => {}
            }
        } else if reconnect
            && receipt.outcome == HostKeyOutcome::Pending
            && matches!(action, Action::Rollback(_))
        {
            receipt = exchange(
                &takeover,
                &mut maintenance,
                host.as_ref(),
                &original,
                HOST_KEY_ROLLBACK_METHOD,
                None,
                deadline,
            )?;
        }
        if matches!(original.operation, HostKeyOperation::Fivekeys { .. })
            && (receipt.verification_required || receipt.outcome == HostKeyOutcome::Committed)
        {
            maintenance.start_key_target(&original.profile, &original.operation_id, &receipt)?;
            if receipt.verification_required {
                receipt = exchange_complete(
                    &takeover,
                    &mut maintenance,
                    host.as_ref(),
                    &original,
                    &receipt,
                    deadline,
                )?;
                anyhow::ensure!(
                    receipt.outcome == HostKeyOutcome::Committed && !receipt.verification_required,
                    "fivekeys completion is not committed; retain the original operation"
                );
            }
        }
        match receipt.outcome {
            HostKeyOutcome::Unknown => {
                anyhow::bail!("key_outcome_unknown: query the original operation")
            }
            HostKeyOutcome::Pending | HostKeyOutcome::AwaitingInput => {
                maintenance.pause_key()?;
                let mut output = ResultEnvelope::new(
                    "key-control",
                    Status::Pending,
                    serde_json::to_value(receipt)?,
                    Vec::new(),
                );
                output.actions.push("supply only the requested inputs using this same operation; query after uncertainty".into());
                return Ok(output);
            }
            HostKeyOutcome::Committed | HostKeyOutcome::RolledBack => {}
        }
        if receipt.outcome == HostKeyOutcome::Committed { material::complete(&original)?; }
        if receipt.outcome == HostKeyOutcome::RolledBack {
            if !original.rolled_back {
                original.rolled_back = true;
                let bytes = serde_json::to_vec(&original)?;
                crate::maintenance::write_recovery(&path, &bytes, Some(&digest))?;
                digest = crate::launch::hash(&bytes);
            }
            material::rollback(&original)?;
        }
        unchanged(host.as_ref(), &original, &receipt, &image)?;
        let restore_owner = if archived {
            owner.is_some()
        } else {
            original.was_serving
        };
        let restore_store = if archived {
            owner.as_ref().is_some_and(|health| health.store_ready)
        } else {
            original.store_ready
        };
        let fivekeys = matches!(original.operation, HostKeyOperation::Fivekeys { .. });
        if restore_owner && !(fivekeys && receipt.outcome == HostKeyOutcome::Committed) {
            maintenance.start_account_runtime()?;
            let restored = host
                .probe_runtime()?
                .context("original key owner did not resume")?;
            let (_, selected) = respire_client_host::config::control_roots()?;
            let selected = selected.canonicalize()?;
            verify_owner(
                host.as_ref(),
                &restored,
                &root,
                &selected,
                &image,
                &receipt.identity,
            )?;
            anyhow::ensure!(
                !restore_store || restored.store_ready,
                "restored key owner did not confirm its original store readiness"
            );
        }
        unchanged(host.as_ref(), &original, &receipt, &image)?;
        let (_, selected) = respire_client_host::config::control_roots()?;
        let selected = selected.canonicalize()?;
        let serving = restore_owner || (fivekeys && receipt.outcome == HostKeyOutcome::Committed);
        maintenance.finish_key(&selected, &receipt.identity, serving, restore_store)?;
        finished = true;
        host.finish_key_handoff(&selected, &receipt.identity, serving)?;
        if !archived {
            // Retain only the original nonsecret binding so an explicit O query can recover lost output.
            let archive = archive_path(&root, &original.operation_id);
            let bytes = serde_json::to_vec(&original)?;
            match read_original(&archive)? {
                Some((_, hash)) => anyhow::ensure!(
                    hash == crate::launch::hash(&bytes),
                    "key result binding archive changed"
                ),
                None => crate::maintenance::write_recovery(&archive, &bytes, None)?,
            }
            anyhow::ensure!(
                read_original(&path)?
                    .as_ref()
                    .map(|(_, hash)| hash.as_str())
                    == Some(digest.as_str()),
                "key recovery metadata changed before cleanup"
            );
            std::fs::remove_file(&path)?;
            respire_spawn::durable::sync_directory(
                path.parent().context("key recovery parent missing")?,
            )
            .map_err(|_| {
                anyhow::anyhow!(
                    "key operation completed; recovery namespace cleanup needs verification"
                )
            })?;
        }
        if receipt.outcome == HostKeyOutcome::Committed {
            return Ok(ResultEnvelope::new(
                if fivekeys { "fivekeys" } else { "super-reset" },
                Status::Ok,
                committed_summary(&receipt)?,
                vec![Item::new(if fivekeys { "session" } else { "password" }, Status::Ok,
                    if fivekeys { "ready" } else { "issued" })],
            ));
        }
        Ok(ResultEnvelope::new(
            "key-control",
            Status::Ok,
            serde_json::to_value(receipt)?,
            Vec::new(),
        ))
    })();
    result.map_err(|error| {
        let error = if finished {
            error.context(
                "key operation verified and maintenance finished; cleanup needs verification",
            )
        } else {
            let pause = maintenance.pause_key();
            error.context(format!(
                "key operation retained; Paused publication: {pause:?}"
            ))
        };
        error.context(original.recovery())
    })
}
