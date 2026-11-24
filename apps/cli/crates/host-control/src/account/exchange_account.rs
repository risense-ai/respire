fn exchange_account(
    takeover: &LibraryLock,
    maintenance: &mut Maintenance,
    host: &dyn HostLifecycle,
    operation_id: &str,
    source: &Path,
    register: bool,
    operation: AccountControl,
    completion: Option<&HostAuthAccountResult>,
) -> Result<ResultEnvelope> {
    anyhow::ensure!(matches!(&operation, AccountControl::Complete) == completion.is_some(),
        "account completion requires its original receipt and dedicated grant");
    let image = host.verified_runtime()?;
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    anyhow::ensure!(takeover.protects(&root.join("runtime/takeover"))?,
        "auth control requires this installation's takeover ownership");
    let maintenance_binding = if let Some(receipt) = completion {
        maintenance.account_complete_launch_binding(source, operation_id, receipt, register)?
    } else { maintenance.auth_launch_binding(&profile)? }
        .context("account control requires persistent maintenance")?;
    let expected = crate::identity(&profile)?;
    let account_id = match &expected {
        HostAuthIdentity::Bound { account_id, .. } => Some(account_id.clone()), _ => None,
    };
    let grant = crate::launch::prepare_grant(&image, &maintenance_binding, &expected)?;
    let mut request = account_control::account_request(ControlBinding {
        installation_root: root.clone(), profile: profile.clone(), launch_id: grant.instance_id.clone(),
        expected_account_id: account_id,
    }, expected, operation_id, operation)?;
    let mut custody = if register { None } else {
        Some(crate::login_custody::LoginCustody::open(takeover, &root, source, operation_id)?)
    };
    if let Some(custody) = custody.as_mut() { custody.prepare(&mut request)?; }
    if !maintenance.account_operation_sent()? { maintenance.mark_account_operation_sent()?; }
    let unknown = |error: anyhow::Error| error.context(AccountOutcomeUnknown {
        operation_id: operation_id.to_owned(), source_profile: source.to_owned(), register,
    });
    let mut handoff = None;
    let envelope = crate::exchange::exchange_control(&image.image_path, &grant, &request,
        Instant::now() + Duration::from_secs(120), |mut response, pid| {
            handoff = crate::login_custody::unpack(&mut response, register)?;
            account_control::validate_account_response(response, &request, &profile, pid, source, register)
        }).map_err(&unknown)?;
    let Some(handoff) = handoff else { return Ok(envelope); };
    let receipt: HostAuthAccountResult = serde_json::from_value(envelope.summary.clone())?;
    let resumed = custody.as_mut().context("login handoff returned to registration")?
        .consume(handoff, &request, &receipt).map_err(&unknown)?;
    let Some(mut request) = resumed else { return Ok(envelope); };
    // This is the acknowledged candidate from the same authorized operation; never resend authorization.
    let expected = crate::identity(&profile)?;
    let grant = crate::launch::prepare_grant(&image, &maintenance_binding, &expected)?;
    let mut params: respire_protocol::host_control::HostAuthLoginParams = serde_json::from_str(
        request.args.first().context("login acknowledgement parameters missing")?)?;
    params.binding.launch_id = grant.instance_id.clone();
    params.identity = expected;
    request.args = vec![serde_json::to_string(&params)?];
    crate::exchange::exchange_control(&image.image_path, &grant, &request,
        Instant::now() + Duration::from_secs(120), |mut response, pid| {
            let handoff = crate::login_custody::unpack(&mut response, false)?;
            let envelope = account_control::validate_account_response(response, &request, &profile, pid, source, false)?;
            anyhow::ensure!(handoff.is_none(), "acknowledged login returned another custody handoff; original operation retained");
            Ok(envelope)
        }).map_err(unknown)
}

fn require_unsent_admission(
    operation: &AccountControl,
    current: &HostAuthIdentity,
    original: &HostAuthIdentity,
) -> Result<()> {
    anyhow::ensure!(
        matches!(
            operation,
            AccountControl::Register { .. } | AccountControl::LoginStep { .. }
        ) && current == original,
        "unadmitted original operation needs matching explicit authentication input"
    );
    Ok(())
}
