fn read_original(path: &Path) -> Result<Option<(Original, String)>> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(!metadata.file_type().is_symlink(), "identity recovery path is a symlink");
                #[cfg(windows)] {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "identity recovery path is a reparse point");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(metadata.is_file() && metadata.len() <= 16 * 1024, "identity recovery context is not a bounded file");
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 16 * 1024, "identity recovery context exceeds its bound");
    let original = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid identity recovery context"))?;
    Ok(Some((original, super::hash(&bytes))))
}

fn credential_alias(original: &Original) -> Result<String> {
    Ok(format!("login-{}", uuid::Uuid::parse_str(&original.operation_id)?.simple()))
}

fn material(original: &Original, invite_code: Option<&str>) -> Result<Option<HostSpaceMaterial>> {
    if matches!(original.operation, HostSpaceOperation::Kick { .. }) { return Ok(None); }
    let backend = original.credential_backend.as_deref().context("space host credential checkpoint missing; retain the original operation")?;
    let alias = credential_alias(original)?;
    let (secret, legacy_secret) = if original.sent && original.legacy_factor {
        let session = json!({"vault_version":3,"keyring_account":alias,"keyring_backend":backend,"legacy_secret_backend":backend});
        let (secret, legacy) = respire_client_host::host_credentials::read_export_factors(&session)?;
        (secret.context("space host credential missing; retain the original operation")?, legacy)
    } else if original.sent {
        (respire_client_host::host_credentials::read_auth_super(&alias, backend)?
            .context("space host credential missing; retain the original operation")?, None)
    } else {
        let (secret, legacy) = match original.operation {
            HostSpaceOperation::Invite { .. } => {
                let session: serde_json::Value = serde_json::from_slice(&std::fs::read(original.profile.join("session.json"))?)?;
                let (secret, legacy) = respire_client_host::host_credentials::read_export_factors(&session)?;
                (secret.context("space source super password unavailable")?, legacy)
            }
            HostSpaceOperation::Join { .. } => {
                let value = respire::space::parse_invite(invite_code.context("join invite input missing")?)?;
                (value["super"].as_str().filter(|value| !value.is_empty()).context("join super password missing")?.to_owned(),
                    value["secret_key"].as_str().filter(|value| !value.is_empty()).map(str::to_owned))
            }
            HostSpaceOperation::Kick { .. } => unreachable!("kick has no credential material"),
        };
        anyhow::ensure!(legacy.is_some() == original.legacy_factor, "space legacy factor differs from its checkpoint");
        if let Some(factor) = legacy.as_deref() {
            let session = json!({"vault_version":3,"super":secret,"secret_key":factor,
                "keyring_account":alias,"keyring_backend":backend});
            let prepared = respire_client_host::host_credentials::prepare_legacy_session(&session,&alias)?;
            anyhow::ensure!(prepared["keyring_backend"].as_str() == Some(backend)
                && prepared["legacy_secret_backend"].as_str() == Some(backend), "space legacy credential backend changed");
        } else { respire_client_host::host_credentials::write_auth_super(&alias, backend, &secret)?; }
        (secret, legacy)
    };
    Ok(Some(HostSpaceMaterial { super_password: secret, keyring_account: alias, keyring_backend: backend.to_owned(), legacy_secret }))
}

fn checkpoint_invite(original: &Original, code: Option<&str>) -> Result<Option<String>> {
    if !matches!(original.operation, HostSpaceOperation::Invite { .. }) { return Ok(None); }
    let backend = original.credential_backend.as_deref().context("space host credential backend missing")?;
    let alias = credential_alias(original)?;
    if let Some(code) = code {
        respire_client_host::host_credentials::write_auth_password(&alias, backend, code)?;
    }
    respire_client_host::host_credentials::read_auth_password(&alias, backend)
}

fn control_write(operation: HostSpaceOperation, invite_code: Option<&str>) -> Result<HostSpaceResult> {
    crate::runtime_policy::require_host("private space write")?;
    respire_client_host::settings::ensure_writable()?;
    anyhow::ensure!(super::lifecycle::current_boot_token().is_none(), "space write must own host maintenance");
    let takeover = crate::runtime_policy::takeover_lock()?;
    let (binary, manifest) = super::model_control::installed_model_runtime()?;
    let (root, profile) = respire_client_host::config::control_roots()?;
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&profile)?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    let path = root.join("runtime/autostart/space-write.json");
    let saved = read_original(&path)?;
    if saved.is_none() {
        respire_host_control::auth_material::prepare_source(&takeover)?;
    }
    if saved.is_none() && matches!(&operation, HostSpaceOperation::Join { .. }) {
        // A retained operation may already own the target; only preflight new joins.
        respire::space::validate_join(invite_code.context("join invite input missing")?)?;
    }
    let expected = super::control::identity(&profile)?;
    let registration = super::read_registration(&root)?;
    let managed = registration.as_ref().filter(|config| config.enabled && !config.manually_stopped);
    let owner = crate::rpc::probe_runtime()?;
    anyhow::ensure!(owner.is_some() || !crate::rpc::endpoint_path().try_exists()?,
        "space write has a recorded owner without a verifiable drain identity");
    if let Some(config) = managed {
        super::lifecycle::owned(config)?;
        super::lifecycle::ensure_control_endpoint(config)?;
        anyhow::ensure!(owner.is_some() || !super::super::super::manager::inspect(config)?.running,
            "space write cannot identify the registered supervisor's owner");
    }
    if let Some(owner) = owner.as_ref() {
        super::model_control::verify_model_owner(owner, &root, &profile, managed, &binary, &manifest,
            Some(&account(&expected, &profile)))?;
    }
    let (mut original, mut digest, mut maintenance) = if let Some((original, digest)) = saved {
        let canonical = |value: &str| uuid::Uuid::parse_str(value)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value);
        anyhow::ensure!(original.schema_version == HOST_SPACE_SCHEMA_V
            && canonical(&original.operation_id) && original.key == original.operation_id && canonical(&original.token),
            "invalid space recovery operation binding");
        if original.root != root || original.profile != profile || original.operation != operation {
            return Err(anyhow::anyhow!("another space operation owns recovery; do not replace its operation or source identity")
                .context(original.recovery()));
        }
        let maintenance = super::lifecycle::resume_identity_write_maintenance(&root, &profile,
            &original.token, &original.coordinator)?;
        (original, Some(digest), maintenance)
    } else {
        let legacy_factor = match &operation {
            HostSpaceOperation::Invite { .. } => {
                let session: serde_json::Value = serde_json::from_slice(&std::fs::read(profile.join("session.json"))?)?;
                session["vault_version"].as_u64() == Some(3)
            }
            HostSpaceOperation::Join { .. } => respire::space::parse_invite(invite_code.context("join invite input missing")?)?
                ["secret_key"].as_str().is_some_and(|value| !value.is_empty()),
            HostSpaceOperation::Kick { .. } => false,
        };
        let mut maintenance = super::lifecycle::begin_maintenance_locked()?;
        let binding = maintenance.auth_launch_binding(&profile)?.context("space write requires persistent Paused maintenance")?;
        let id = uuid::Uuid::new_v4().to_string();
        let credential_backend = if matches!(operation, HostSpaceOperation::Kick { .. }) { None }
            else { Some(if cfg!(target_os = "linux") { "secret_service" } else { "native" }.to_owned()) };
        let original = Original { schema_version: HOST_SPACE_SCHEMA_V, root: root.clone(), profile: profile.clone(),
            operation_id: id.clone(), key: id, source_identity: expected, operation,
            token: super::lifecycle::current_boot_token().context("identity maintenance token missing")?,
            coordinator: binding["coordinator_id"].as_str().context("identity coordinator missing")?.to_owned(),
            was_serving: owner.is_some(), store_ready: owner.as_ref().is_some_and(|owner| owner.store_ready), credential_backend, legacy_factor, sent: false };
        let bytes = serde_json::to_vec(&original)?;
        super::private_write(&path, &bytes, None)?;
        (original, Some(super::hash(&bytes)), maintenance)
    };
    let mut finished = false;
    let result: Result<HostSpaceResult> = (|| {
        maintenance.check_account_runtime()?;
        if let Some(owner) = owner.as_ref() { maintenance.drain_runtime(owner)?; }
        anyhow::ensure!(!crate::net_rpc::port_is_open(), "space write requires the serving Runtime to be drained");
        let query_only = original.sent;
        let host_material = material(&original, invite_code)?;
        let mut checkpoint_code = checkpoint_invite(&original, None)?;
        if !query_only {
            anyhow::ensure!(super::control::identity(&profile)? == original.source_identity,
                "identity source changed before sending the operation");
            original.sent = true;
            let bytes = serde_json::to_vec(&original)?;
            if let Err(error) = super::private_write(&path, &bytes, digest.as_deref()) {
                // Publication can fail after replacement; recover from the actual durable state.
                if let Ok(Some((saved, _))) = read_original(&path) {
                    original.sent = saved.sent;
                }
                return Err(error);
            }
            digest = Some(super::hash(&bytes));
        }
        let mut received = None;
        for attempt in 0..2 {
        let query = query_only && attempt == 0;
        let envelope = super::control::auth_exchange(&takeover, &mut maintenance,
            Instant::now() + Duration::from_secs(180), None,
            |binding, identity| {
                anyhow::ensure!(binding.installation_root == original.root && binding.profile == original.profile,
                    "space write launch selection changed");
                let params = HostSpaceParams { schema_version: HOST_SPACE_SCHEMA_V, binding, identity,
                    source_identity: original.source_identity.clone(), operation: original.operation.clone(),
                    invite_code: if matches!(original.operation, HostSpaceOperation::Invite { .. }) { checkpoint_code.clone() }
                        else { invite_code.map(str::to_owned) },
                    material: host_material.as_ref().map(|value| HostSpaceMaterial { super_password:value.super_password.clone(),
                        keyring_account:value.keyring_account.clone(),keyring_backend:value.keyring_backend.clone(),
                        legacy_secret:value.legacy_secret.clone() }) };
                let method = if query { HOST_SPACE_RESULT_METHOD } else { match &original.operation {
                    HostSpaceOperation::Invite { .. } => HOST_SPACE_INVITE_METHOD,
                    HostSpaceOperation::Join { .. } => HOST_SPACE_JOIN_METHOD, HostSpaceOperation::Kick { .. } => HOST_SPACE_KICK_METHOD,
                }};
                Ok(RpcRequest { v: respire_commands::rpc_client::PROTOCOL_V, id: uuid::Uuid::new_v4().to_string(),
                    method: method.to_owned(), args: vec![serde_json::to_string(&params)?], scope: None,
                    operation_id: Some(original.operation_id.clone()), idempotency_key: Some(original.key.clone()),
                    parent_operation_id: None, queue_wait_ms: None, lifecycle: None })
            }, |response, request, profile, pid| validate_response(response, request, profile, pid, &original), |error| error)?;
        let receipt: HostSpaceResult = serde_json::from_value(envelope.summary)
            .map_err(|_| anyhow::anyhow!("invalid space write receipt"))?;
        // Pending may admit the first POST or require a host code checkpoint.
        // Runtime's durable phase fences continuation; Unknown never replays a POST.
        if receipt.outcome == HostSpaceOutcome::Pending && receipt.invite_code.is_some() {
            checkpoint_code = checkpoint_invite(&original, receipt.invite_code.as_deref())?;
            continue;
        }
        if query && receipt.outcome == HostSpaceOutcome::Pending { continue; }
        received = Some(receipt);
        break;
        }
        let receipt = received.context("space continuation did not return a receipt")?;
        if matches!(original.operation, HostSpaceOperation::Invite { .. }) && receipt.outcome == HostSpaceOutcome::Committed {
            anyhow::ensure!(receipt.invite_code.is_some() && receipt.invite_code == checkpoint_code,
                "committed invite differs from the host checkpoint");
        }
        if !matches!(receipt.outcome,HostSpaceOutcome::Committed | HostSpaceOutcome::Rejected) {
            maintenance.mark(super::lifecycle::Phase::Paused)?;
            return Ok(receipt);
        }
        match receipt.outcome {
            HostSpaceOutcome::Committed | HostSpaceOutcome::Rejected => {}
            HostSpaceOutcome::Pending => anyhow::bail!("space write pending; query the original operation"),
            HostSpaceOutcome::Unknown => anyhow::bail!("space write outcome unknown; query the original operation"),
        }
        unchanged(&original, &receipt.identity, &binary, &manifest)?;
        if original.was_serving {
            maintenance.start_account_runtime()?;
            let restored = crate::rpc::probe_runtime()?.context("identity owner did not resume")?;
            super::model_control::verify_model_owner(&restored, &root, &profile, managed, &binary, &manifest,
                Some(&account(&receipt.identity, &profile)))?;
            anyhow::ensure!(!original.store_ready || restored.store_ready, "identity owner store readiness was not restored");
        }
        unchanged(&original, &receipt.identity, &binary, &manifest)?;
        maintenance.finish()?;
        finished = true;
        anyhow::ensure!(read_original(&path)?.as_ref().map(|(_, hash)| hash.as_str()) == digest.as_deref(),
            "space recovery context changed before cleanup");
        if receipt.outcome == HostSpaceOutcome::Rejected {
            if let Some(value) = &host_material {
                if let Some(legacy) = value.legacy_secret.as_deref() {
                    respire_client_host::host_credentials::space::remove_legacy(&value.keyring_account,&value.keyring_backend,legacy)?;
                }
                respire_client_host::host_credentials::remove_auth_material(&value.keyring_account,
                    &value.keyring_backend, &value.super_password, None)?;
            }
        }
        std::fs::remove_file(&path)?;
        respire_spawn::durable::sync_directory(path.parent().context("identity context parent missing")?)?;
        Ok(receipt)
    })();
    if result.is_ok() && finished { maintenance.finish_handoff()?; }
    result.map_err(|error| {
        let error = if finished {
            error.context("space operation committed and maintenance finished; recovery context cleanup failed")
        } else {
            let pause = maintenance.mark(super::lifecycle::Phase::Paused);
            error.context(format!("space operation retained; Paused publication: {pause:?}"))
        };
        error.context(original.recovery())
    })
}
