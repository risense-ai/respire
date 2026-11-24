fn exchange(
    takeover: &LibraryLock,
    maintenance: &mut Maintenance,
    host: &dyn HostLifecycle,
    original: &Original,
    method: &str,
    input: Option<&HostKeyInput>,
    deadline: Instant,
) -> Result<HostKeyResult> {
    exchange_inner(
        takeover,
        maintenance,
        host,
        original,
        method,
        input,
        None,
        deadline,
    )
}

fn exchange_complete(
    takeover: &LibraryLock,
    maintenance: &mut Maintenance,
    host: &dyn HostLifecycle,
    original: &Original,
    receipt: &HostKeyResult,
    deadline: Instant,
) -> Result<HostKeyResult> {
    exchange_inner(
        takeover,
        maintenance,
        host,
        original,
        HOST_KEY_COMPLETE_METHOD,
        None,
        Some(receipt),
        deadline,
    )
}

fn exchange_inner(
    takeover: &LibraryLock,
    maintenance: &mut Maintenance,
    host: &dyn HostLifecycle,
    original: &Original,
    method: &str,
    input: Option<&HostKeyInput>,
    completion: Option<&HostKeyResult>,
    deadline: Instant,
) -> Result<HostKeyResult> {
    let input = Some(if original.rolled_back { HostKeyInput::default() }
        else { material::load(original, input, method != HOST_KEY_ROLLBACK_METHOD)? });
    host.require_account_write()?;
    let image = host.verified_runtime()?;
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    anyhow::ensure!(
        takeover.protects(&root.join("runtime/takeover"))?,
        "key control requires this installation's takeover ownership"
    );
    let maintenance_binding = if let Some(receipt) = completion {
        validate_receipt(receipt, original)?;
        maintenance.key_complete_launch_binding(
            &original.profile,
            &original.operation_id,
            receipt,
        )?
    } else {
        maintenance.auth_launch_binding(&profile)?
    }
    .context("key control requires owned maintenance")?;
    let identity = crate::identity(&profile)?;
    let expected_account_id = match &identity {
        HostAuthIdentity::Bound { account_id, .. } => Some(account_id.clone()),
        _ => None,
    };
    let grant = crate::launch::prepare_grant(&image, &maintenance_binding, &identity)?;
    let binding = ControlBinding {
        installation_root: root,
        profile: profile.clone(),
        launch_id: grant.instance_id.clone(),
        expected_account_id,
    };
    anyhow::ensure!(
        binding.installation_root == original.root
            && (binding.profile == original.profile
                || matches!(original.operation, HostKeyOperation::Fivekeys { .. })),
        "key control launch selection changed"
    );
    let params = HostKeyParams {
        schema_version: HOST_KEY_SCHEMA_V,
        binding,
        identity,
        source_identity: original.source_identity.clone(),
        operation: original.operation.clone(),
        input,
    };
    let argument = serde_json::to_string(&params)
        .map_err(|_| anyhow::anyhow!("cannot encode private key request"))?;
    let request = RpcRequest {
        v: crate::PROTOCOL_V,
        id: uuid::Uuid::new_v4().to_string(),
        method: method.to_owned(),
        args: vec![argument],
        scope: None,
        operation_id: Some(original.operation_id.clone()),
        idempotency_key: Some(original.key.clone()),
        parent_operation_id: None,
        queue_wait_ms: None,
        lifecycle: None,
    };
    // Exactly one exchange. Uncertainty keeps the sent journal and original ID.
    let envelope = crate::exchange::exchange_control(
        &image.image_path,
        &grant,
        &request,
        deadline,
        |response, pid| validate_response(response, &request, &profile, pid, original),
    )
    .map_err(|_| anyhow::anyhow!("private key exchange failed; query the original operation"))?;
    let mut receipt = serde_json::from_value(envelope.summary)
        .map_err(|_| anyhow::anyhow!("invalid private key receipt"))?;
    material::checkpoint_challenge(original, &mut receipt)?;
    Ok(receipt)
}

fn validate_receipt(receipt: &HostKeyResult, original: &Original) -> Result<()> {
    anyhow::ensure!(
        receipt.operation_id == original.operation_id
            && receipt.fingerprint == original.fingerprint()?
            && serde_json::to_value(&receipt.operation)?
                == serde_json::to_value(&original.operation)?
            && receipt.source_profile == original.profile,
        "key receipt changed its original intent or source"
    );
    if let HostKeyOperation::Fivekeys { .. } = &original.operation {
        anyhow::ensure!(
            receipt.super_password.is_none()
                && receipt.required_inputs.iter().all(|field| matches!(
                    field.as_str(),
                    "password" | "super_password" | "account_secret" | "totp"
                )),
            "invalid fivekeys secret or requested input"
        );
        if receipt.verification_required || receipt.outcome == HostKeyOutcome::Committed {
            validate_applied_target(receipt, &original.operation_id, &original.profile)?;
        } else {
            anyhow::ensure!(
                receipt.account_id.is_none()
                    && receipt.session_sha256.is_none()
                    && receipt.identity == original.source_identity,
                "unpublished fivekeys changed its source identity"
            );
            anyhow::ensure!(
                (receipt.outcome == HostKeyOutcome::AwaitingInput)
                    == !receipt.required_inputs.is_empty(),
                "fivekeys input status mismatch"
            );
        }
        return Ok(());
    }
    anyhow::ensure!(
        !receipt.verification_required
            && receipt
                .target_profile
                .as_ref()
                .is_none_or(|path| path == &original.profile),
        "super-reset changed its profile"
    );
    let source_account = match &original.source_identity {
        HostAuthIdentity::Bound { account_id, .. } => account_id,
        _ => anyhow::bail!("super-reset requires an original bound account"),
    };
    let (account, digest) = match &receipt.identity {
        HostAuthIdentity::Bound {
            account_id,
            session_sha256,
        } => (account_id, session_sha256),
        _ => anyhow::bail!("key receipt lost its bound account"),
    };
    anyhow::ensure!(
        account == source_account,
        "key receipt changed its original account"
    );
    anyhow::ensure!(
        receipt
            .required_inputs
            .iter()
            .all(|key| matches!(key.as_str(), "password" | "super_password")),
        "unsupported key continuation inputs"
    );
    if receipt.outcome == HostKeyOutcome::Committed {
        anyhow::ensure!(
            receipt.target_profile.as_ref() == Some(&original.profile)
                && receipt.account_id.as_ref() == Some(account)
                && receipt.session_sha256.as_ref() == Some(digest)
                && receipt.required_inputs.is_empty()
                && receipt
                    .super_password
                    .as_ref()
                    .is_some_and(|code| !code.is_empty()),
            "invalid committed key receipt"
        );
    } else {
        anyhow::ensure!(
            receipt.super_password.is_none()
                && receipt.session_sha256.is_none()
                && receipt.account_id.is_none()
                && receipt.identity == original.source_identity,
            "noncommitted key receipt exposed a code or changed its source"
        );
        if receipt.outcome == HostKeyOutcome::AwaitingInput {
            anyhow::ensure!(
                !receipt.required_inputs.is_empty(),
                "missing key continuation inputs"
            );
        } else if receipt.outcome != HostKeyOutcome::RolledBack {
            anyhow::ensure!(
                receipt.required_inputs.is_empty(),
                "unexpected key input request"
            );
        }
    }
    Ok(())
}

fn validate_response(
    response: RpcResponse<ResultEnvelope>,
    request: &RpcRequest,
    profile: &Path,
    pid: u32,
    original: &Original,
) -> Result<ResultEnvelope> {
    anyhow::ensure!(
        response.v == request.v
            && response.id == request.id
            && response.pid == Some(pid)
            && response.bin == "rsrs-runtime"
            && response.scope.is_none()
            && response.web_url.is_none()
            && response.operation_id.as_deref() == Some(original.operation_id.as_str())
            && response.idempotency_key.as_deref() == Some(original.key.as_str()),
        "key response binding mismatch"
    );
    if let Some(path) = response.data_dir.as_deref() {
        anyhow::ensure!(
            Path::new(path).canonicalize()? == profile,
            "key response profile mismatch"
        );
    }
    let envelope = response.envelope.context("private key receipt missing")?;
    anyhow::ensure!(
        envelope.command == "key-control"
            && envelope.details.is_null()
            && envelope.actions.is_empty()
            && envelope.items.len() == 2
            && envelope.related.is_empty(),
        "invalid key response envelope"
    );
    for (name, value) in [("control_ready", "true"), ("store_ready", "false")] {
        let mut items = envelope.items.iter().filter(|item| item.name == name);
        anyhow::ensure!(
            items.next().is_some_and(|item| item.value == value
                && if name == "control_ready" {
                    matches!(item.status, Status::Ok)
                } else {
                    matches!(item.status, Status::Skip)
                })
                && items.next().is_none(),
            "key helper readiness mismatch"
        );
    }
    let receipt: HostKeyResult = serde_json::from_value(envelope.summary.clone())
        .map_err(|_| anyhow::anyhow!("invalid private key receipt shape"))?;
    validate_receipt(&receipt, original)?;
    let (_, selected) = respire_client_host::config::control_roots()?;
    anyhow::ensure!(
        crate::identity(&selected.canonicalize()?)? == receipt.identity,
        "key receipt does not match the selected session"
    );
    match receipt.outcome {
        HostKeyOutcome::Committed | HostKeyOutcome::RolledBack => anyhow::ensure!(
            response.ok
                && response.exit == 0
                && response.code.is_none()
                && response.error.is_none()
                && matches!(envelope.status, Status::Ok)
                && envelope.errors.is_empty(),
            "invalid terminal key response"
        ),
        HostKeyOutcome::Pending | HostKeyOutcome::AwaitingInput => anyhow::ensure!(
            response.ok
                && response.exit == 0
                && response.code.is_none()
                && response.error.is_none()
                && matches!(envelope.status, Status::Pending)
                && envelope.errors.is_empty(),
            "invalid pending key response"
        ),
        HostKeyOutcome::Unknown => anyhow::ensure!(
            !response.ok
                && response.exit == 2
                && response.code.as_deref() == Some("key_outcome_unknown")
                && matches!(envelope.status, Status::Fail),
            "invalid unknown key response"
        ),
    }
    Ok(envelope)
}
