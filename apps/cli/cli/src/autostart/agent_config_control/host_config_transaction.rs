fn control_host_agent_config(intent: Option<respire_protocol::config_control::AgentConfigIntent>,
    query: Option<(&respire_commands::runtime_error::OperationContext, &std::path::Path)>,
    request: Option<(&str, bool)>) -> anyhow::Result<respire_commands::output::ResultEnvelope> {
    use respire_client_host::lock::LibraryLock;
    let _takeover = crate::runtime_policy::takeover_lock()?;
    let (root, profile) = respire_client_host::config::control_roots()?;
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&profile)?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    let identity = metadata_identity(&profile, super::hash)?;
    let account = identity.account_id.clone().unwrap_or_else(|| super::hash(profile.display().to_string().as_bytes()));
    let saved = super::lifecycle::pending_agent_config(&root, query.is_some() || request.is_some())?;
    let online = OnlineAgentConfig::read(&root)?;
    anyhow::ensure!(saved.is_none() || online.record.is_none(), "multiple agent configuration operations are pending");
    let mut query_only = query.is_some() || request.is_some_and(|(_, query)| query);
    let requested = query.map(|(context, _)| context.operation_id.as_str())
        .or(request.map(|(id, _)| id));
    if online.record.as_ref().is_some_and(|record| requested.is_none_or(|id| id == record.review.operation_id)) {
        return online.resume(intent.as_ref(), requested, query, &profile, &identity);
    }
    if let Some(operation_id) = requested {
        if let Some((context, source)) = query {
            anyhow::ensure!(source.canonicalize()? == profile && context.operation_id == context.idempotency_key,
                "core configuration query identity mismatch");
        }
        if let Some((record, _, _)) = saved.as_ref().filter(|(record, _, _)|
            record.review.operation_id == operation_id) {
            query_only = true;
            anyhow::ensure!(intent.as_ref().is_none_or(|intent| *intent == record.review.intent),
                "core configuration request ID is bound to another intent");
            anyhow::ensure!(matches!(record.review.intent.key.trim(), "read_heat" | "recall_fusion")
                && record.review.profile == profile && record.review.identity == identity,
                "core configuration query differs from original operation");
            if let Some((context, _)) = query {
                let control: CoreConfigTransaction = serde_json::from_value(record.receipt.as_ref()
                    .context("original core scope unavailable")?.result["core_control"].clone())?;
                anyhow::ensure!(control.scope.as_ref() == Some(&context.scope),
                    "core configuration query differs from original scope");
            }
            if record.receipt.as_ref().is_none_or(|receipt| receipt.outcome == AgentConfigOutcome::Unknown) {
                return Err(CoreConfigUnknown { details: core_transaction_details(record)? }.into());
            }
        } else if query_only || core_result_path(&root, operation_id)?.try_exists()? {
            let output: ResultEnvelope = serde_json::from_slice(&std::fs::read(core_result_path(&root, operation_id)?)?)?;
            anyhow::ensure!(intent.as_ref().is_none_or(|intent| output.summary["key"] == intent.key
                && output.summary["value"] == intent.value()),
                "core configuration request ID is bound to another intent");
            anyhow::ensure!(output.details["operation_id"] == operation_id
                && output.details["source_profile"] == serde_json::to_value(&profile)?
                && output.details["identity"] == serde_json::to_value(&identity)?
                && query.map(|(context, _)| serde_json::to_value(&context.scope)).transpose()?
                    .is_none_or(|scope| output.details["scope"] == scope),
                "core configuration terminal receipt identity mismatch");
            return Ok(output);
        } else {
            anyhow::ensure!(saved.is_none() && online.record.is_none(), "another agent configuration operation is pending");
        }
    }
    let registration = super::read_registration(&root)?;
    let managed = registration.as_ref().filter(|config| config.enabled && !config.manually_stopped);
    let owner = crate::rpc::probe_runtime()?;
    anyhow::ensure!(owner.is_some() || !crate::rpc::endpoint_path().try_exists()?, "agent config owner is not verifiable");
    if let Some(config) = managed {
        super::lifecycle::owned(config)?;
        super::lifecycle::ensure_control_endpoint(config)?;
        anyhow::ensure!(owner.is_some() || !super::super::super::manager::inspect(config)?.running,
            "agent config cannot identify the registered owner");
    }
    let runtime = if owner.is_some() || saved.as_ref().is_some_and(|(record, _, _)| record.was_serving) {
        Some(super::model_control::installed_model_runtime()?)
    } else { None };
    if let (Some(owner), Some((binary, manifest))) = (owner.as_ref(), runtime.as_ref()) {
        super::model_control::verify_model_owner(owner, &root, &profile, managed, binary, manifest, Some(&account))?;
    }
    host_config_preflight(&root, &profile)?;
    if saved.is_none() {
        super::lifecycle::check_direct_access()?;
        let intent = intent.context("core configuration query cannot create an operation")?;
        validate_host_agent_config(&intent, &profile)?;
        let review = crate::client_config::review_agent_config_for_operation(intent, root, profile,
            request.map(|(id, _)| id))?;
        return online.start(review, owner.as_ref());
    }
    let (mut record, mut maintenance) = if let Some((record, token, coordinator)) = saved {
        anyhow::ensure!(intent.as_ref().is_none_or(|intent| record.review.intent == *intent) && record.review.identity == identity
            && record.review.installation_root == root && record.review.profile == profile,
            "repeat the original agent-config intent and identity; a new operation is not allowed");
        let maintenance = super::lifecycle::resume_agent_config_maintenance(&root, &profile, &token, &coordinator)?;
        (record, maintenance)
    } else { anyhow::bail!("legacy agent configuration operation disappeared"); };
    let operation = record.review.operation_id.clone();
    let core_setting = matches!(record.review.intent.key.trim(), "read_heat" | "recall_fusion");
    let mut finished = false;
    let result = (|| -> Result<ResultEnvelope> {
        maintenance.auth_launch_binding(&profile)?.context("agent config lost its Paused grant")?;
        if core_setting {
            let mut output = control_core_setting(&mut record, &mut AgentConfigJournal::Legacy(&mut maintenance),
                owner.as_ref(), query_only)?;
            if let Some(previous) = owner.as_ref() {
                let current = crate::rpc::probe_runtime()?.context("core config owner exited before completion")?;
                let (binary, manifest) = runtime.as_ref().context("core config Runtime selection missing")?;
                let (selected, selected_manifest) = super::model_control::installed_model_runtime()?;
                anyhow::ensure!(selected == *binary && selected_manifest.version == manifest.version
                    && selected_manifest.sha256.eq_ignore_ascii_case(&manifest.sha256)
                    && current.pid == previous.pid && current.process_start_id == previous.process_start_id
                    && crate::net_rpc::request_scope(&current)? == crate::net_rpc::request_scope(previous)?,
                    "core config owner or Runtime selection changed before completion");
                super::model_control::verify_model_owner(&current, &root, &profile, managed, binary, manifest, Some(&account))?;
            }
            output.details = core_transaction_details(&record)?;
            let path = core_result_path(&root, &operation)?;
            let previous = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            crate::autostart::private_write(&path, &serde_json::to_vec(&output)?,
                previous.as_deref().map(crate::autostart::hash).as_deref())?;
            if query_only { maintenance.finish_core_config_query()?; } else { maintenance.finish()?; }
            finished = true;
            return Ok(output);
        }
        anyhow::ensure!(!record.sent || record.receipt.is_some(),
            "agent_config_outcome_unknown operation_id={operation}; original O has no receipt; do not replay");
        if record.was_serving { maintenance.check_account_runtime()?; }
        if let Some(owner) = owner.as_ref() { maintenance.drain_runtime(owner)?; }
        anyhow::ensure!(!crate::net_rpc::port_is_open(), "agent config requires a drained Runtime");
        {
            let _root_lock = LibraryLock::acquire(&root, Duration::from_secs(15))?;
            let _profile_lock = if root == profile { None } else { Some(LibraryLock::acquire(&profile, Duration::from_secs(15))?) };
            host_config_preflight(&root, &profile)?;
            if !record.sent {
                anyhow::ensure!(record.receipt.is_none(), "agent config unsent operation has a receipt");
                validate_host_agent_config(&record.review.intent, &profile)?;
                let intent = &record.review.intent;
                let result = ResultEnvelope::new("agent-config", Status::Ok,
                    serde_json::json!({"action":"updated", "key":intent.key, "value":intent.value()}),
                    vec![respire_commands::output::Item::new(&intent.key, Status::Ok, &intent.raw_value)]);
                let receipt = AgentConfigReceipt { review: record.review.clone(), outcome: AgentConfigOutcome::Committed,
                    result: serde_json::to_value(result)? };
                let mut preview = record.clone();
                preview.sent = true;
                preview.receipt = Some(receipt.clone());
                maintenance.check_agent_config_record(&preview)?;
                record.sent = true;
                maintenance.save_agent_config(&record)?;
                crate::client_config::publish_agent_config(&receipt.review, || {
                    record.receipt = Some(receipt.clone());
                    maintenance.save_agent_config(&record)
                })?;
            }
            crate::client_config::verify_agent_config_receipt(record.receipt.as_ref().context("agent config receipt missing")?)?;
        }
        let receipt = record.receipt.as_ref().context("agent config receipt missing")?;
        anyhow::ensure!(receipt.review == record.review, "agent config receipt changed original intent");
        if let Some((binary, manifest)) = runtime.as_ref() {
            let (current_binary, current_manifest) = super::model_control::installed_model_runtime()?;
            anyhow::ensure!(*binary == current_binary && manifest.version == current_manifest.version
                && manifest.sha256.eq_ignore_ascii_case(&current_manifest.sha256), "agent config Runtime selection changed");
            if record.was_serving {
                maintenance.start_account_runtime()?;
                let restored = crate::rpc::probe_runtime()?.context("agent config original owner did not resume")?;
                super::model_control::verify_model_owner(&restored, &root, &profile, managed, binary, manifest, Some(&account))?;
                anyhow::ensure!(!record.store_ready || restored.store_ready || respire_client_host::settings::off_mode()?,
                    "agent config original owner lacks store readiness");
            }
        }
        crate::client_config::verify_agent_config_receipt(receipt)?;
        let result: ResultEnvelope = serde_json::from_value(receipt.result.clone())?;
        anyhow::ensure!(result.command == "agent-config" && matches!(result.status, Status::Ok)
            == (receipt.outcome == AgentConfigOutcome::Committed), "invalid agent config setter receipt");
        if receipt.outcome == AgentConfigOutcome::Committed {
            let intent = &record.review.intent;
            anyhow::ensure!(result.summary == serde_json::json!({"action":"updated", "key":intent.key, "value":intent.value()})
                && result.items.len() == 1 && result.items[0].name == intent.key
                && result.items[0].value == intent.raw_value && matches!(result.items[0].status, Status::Ok)
                && result.details.is_null() && result.errors.is_empty() && result.actions.is_empty() && result.related.is_empty(),
                "agent config result differs from original setter output");
        }
        maintenance.finish()?;
        finished = true;
        Ok(result)
    })();
    if !finished {
        if core_setting && record.sent {
            let details = core_transaction_details(&record)?;
            let cleanup = (|| {
                maintenance.refresh_agent_config()?;
                maintenance.mark(super::lifecycle::Phase::Paused)
            })();
            return CoreConfigUnknown::after_cleanup(details, result, cleanup);
        }
        let actual = maintenance.refresh_agent_config()?;
        maintenance.mark(super::lifecycle::Phase::Paused)?;
        if actual.sent && actual.receipt.is_none() {
            return result.map_err(|error| error.context(format!(
                "agent_config_outcome_unknown operation_id={operation}; retain original O; do not replay")));
        }
    }
    if result.is_ok() && finished && record.was_serving && !core_setting { maintenance.finish_handoff()?; }
    result.map_err(|error| error.context(format!("agent config operation_id={operation}; repeat the same command to query original O")))
}
