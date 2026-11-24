use super::*;

#[test]
fn register_request_requires_host_material_and_preserves_the_private_payload() -> Result<()> {
    let root = std::env::temp_dir();
    let id = uuid::Uuid::new_v4().to_string();
    let binding = ControlBinding { installation_root: root.clone(), profile: root,
        launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None };
    let operation = |material| AccountControl::Register { addr: "https://fixture.invalid".into(),
        user: "synthetic-register".into(), password: "synthetic-password".into(),
        super_password: None, material };
    assert!(account_request(binding.clone(), HostAuthIdentity::Absent, &id, operation(None)).is_err());
    let request = account_request(binding, HostAuthIdentity::Absent, &id,
        operation(Some(respire_protocol::HostAuthKeygenMaterial {
            kdf_salt: "synthetic-salt".into(), wrapped_urk: "synthetic-wrap".into(),
            urk_nonce: "synthetic-nonce".into(), super_password: "synthetic-super".into(),
            keyring_account: "login-synthetic".into(), keyring_backend: "native".into(),
        })))?;
    assert_eq!(request.method, HOST_AUTH_REGISTER_METHOD);
    assert_eq!(request.operation_id.as_deref(), Some(id.as_str()));
    let params: HostAuthRegisterParams = serde_json::from_str(&request.args[0])?;
    let material = params.material.context("private registration material was lost")?;
    assert_eq!(material.super_password, "synthetic-super");
    assert_eq!(material.wrapped_urk, "synthetic-wrap");
    assert_eq!(params.password, "synthetic-password");
    assert!(params.super_password.is_none());
    let outer = "registration outcome unknown; query the original operation, do not resend";
    let url = "https://private-host.invalid/private-path?user=private-user&token=private-token";
    let sentinel = "private-body-and-credential";
    let mut cases = Vec::new();
    for (status, suffix) in [(401, "unauthorized"), (403, "forbidden"), (429, "rate_limited"),
        (503, "server_error"), (400, "other")] {
        cases.push((format!("authentication salt request failed: {url}: status code {status}"),
            format!("auth_salt_http_{suffix}")));
        cases.push((format!("register request failed: {url}: status code {status}"),
            format!("register_http_{suffix}")));
    }
    for (cause, class) in [
        (format!("authentication salt request failed: {url}: Dns Failed: {sentinel}"), "auth_salt_transport"),
        (format!("register request failed: {url}: Connection Failed: {sentinel}"), "register_transport"),
        ("server did not return an authentication salt".into(), "auth_salt_missing"),
        (format!("salt hex decode failed: {sentinel}"), "auth_salt_invalid_hex"),
        (format!("Failed to read JSON: {sentinel}"), "auth_salt_reply_parse_failed"),
        (format!("failed to parse response: Failed to read JSON: {sentinel}"), "register_reply_parse_failed"),
        (format!("register failed: {sentinel}"), "register_token_missing"),
        (format!("{sentinel}: authentication salt request failed: {url}: status code 401"), "register_remote_unknown"),
        (format!("authentication salt request failed: {url}: status code 401: {sentinel}"), "register_remote_unknown"),
        (format!("register failed: authentication salt request failed: {url}: status code 401"), "register_token_missing"),
        (format!("{sentinel}: Failed to read JSON: private-user"), "register_remote_unknown"),
    ] {
        cases.push((cause, class.to_owned()));
    }
    let profile = std::env::temp_dir();
    for (cause, class) in cases {
        // Match Runtime24's format!("{failure:#}") over its authorization error context.
        let private = format!("{:#}", anyhow::anyhow!(cause).context(outer));
        let mut envelope = ResultEnvelope::new("auth-account", Status::Fail, serde_json::json!({}),
            vec![respire_commands::output::Item::new("control_ready", Status::Ok, "true"),
                respire_commands::output::Item::new("store_ready", Status::Skip, "false")]);
        envelope.errors.push(private.clone());
        let response: RpcResponse<ResultEnvelope> = serde_json::from_value(serde_json::json!({
            "v": request.v, "id": request.id, "operation_id": request.operation_id,
            "idempotency_key": request.idempotency_key, "bin": "rsrs-runtime", "pid": 5252,
            "ok": false, "exit": 2, "code": "account_outcome_unknown", "error": private,
            "envelope": envelope,
        }))?;
        let error = validate_account_response(response.clone(), &request, &profile, 5252, &profile, true)
            .err().context("registration rejection was accepted")?.to_string();
        assert_eq!(error, format!("Runtime rejected account control (account_outcome_unknown): account outcome is unknown; query the original operation without replaying; register_kind={class}"));
        for private in [url, "private-host", "private-user", "private-token", sentinel] {
            assert!(!error.contains(private));
        }
        let mut wrong_owner = response.clone();
        wrong_owner.pid = Some(5253);
        assert_eq!(validate_account_response(wrong_owner, &request, &profile, 5252, &profile, true)
            .err().context("foreign registration rejection was accepted")?.to_string(),
            "account control response identity mismatch");
        let mut result_request = request.clone();
        result_request.method = HOST_AUTH_ACCOUNT_RESULT_METHOD.into();
        assert!(!validate_account_response(response.clone(), &result_request, &profile, 5252, &profile, true)
            .err().context("result rejection was accepted")?.to_string().contains("register_kind="));
        assert!(!validate_account_response(response, &request, &profile, 5252, &profile, false)
            .err().context("non-registration rejection was accepted")?.to_string().contains("register_kind="));
    }
    assert_eq!(register_rejection_class(outer), "register_remote_unknown");
    assert_eq!(register_rejection_class(&format!("outer: {outer}: Failed to read JSON: {sentinel}")),
        "register_unclassified");
    Ok(())
}

#[test]
fn rolled_back_receipt_rejects_nested_issued_material() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().canonicalize()?;
    let mut receipt = HostAuthAccountResult {
        operation_id: uuid::Uuid::new_v4().to_string(),
        outcome: HostAuthAccountOutcome::RolledBack,
        source_profile: source.clone(),
        target_profile: None,
        account_id: None,
        session_sha256: None,
        super_password: None,
        login: Some(respire_protocol::host_control::HostAuthLoginStatus::default()),
    };
    validate_account_receipt(&receipt, &receipt.operation_id, &source, false)?;
    if let Some(login) = receipt.login.as_mut() {
        login.super_issued = Some("synthetic-only".into());
    }
    assert!(validate_account_receipt(&receipt, &receipt.operation_id, &source, false).is_err());

    // Exercise the shared validator used by the account coordinator, not its legacy CLI copy.
    let request = account_request(ControlBinding {
        installation_root: source.clone(), profile: source.clone(),
        launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
    }, HostAuthIdentity::Absent, &receipt.operation_id, AccountControl::Login {
        addr: "https://fixture.invalid".into(), user: "fixture".into(),
        authorization: HostAuthAuthorization::Password { password: "fixture-only".into(), totp: None },
        super_password: "fixture-only".into(),
    })?;
    let private = "synthetic-private-account-material";
    let mut awaiting = ResultEnvelope::new("auth-account", Status::Pending, serde_json::json!({
        "receipt": {"operation_id":receipt.operation_id,"outcome":"awaiting_input",
            "source_profile":source,"login":{"user":"fixture","addr":"https://fixture.invalid",
                "required_inputs":["totp"]}},
        "handoff":{"kind":"challenge","payload":private,
            "sha256":"a".repeat(64)}
    }), Vec::new());
    awaiting.items = vec![
        respire_commands::output::Item::new("control_ready", Status::Ok, "true"),
        respire_commands::output::Item::new("store_ready", Status::Skip, "false"),
    ];
    let mut response = RpcResponse {
        v: request.v, id: request.id.clone(), operation_id: request.operation_id.clone(),
        idempotency_key: request.idempotency_key.clone(), ok: true, exit: 0,
        bin: "rsrs-runtime".into(), pid: Some(5252), scope: None, web_url: None,
        data_dir: None, code: None, error: None, envelope: Some(awaiting),
    };
    let handoff = crate::login_custody::unpack(&mut response, false)?;
    assert!(matches!(handoff, Some(respire_protocol::host_control::HostAuthLoginHandoff::Challenge { payload, .. }) if payload == private));
    let public = validate_account_response(response, &request, &source, 5252, &source, false)?;
    assert!(!serde_json::to_string(&public)?.contains(private));
    assert!(public.summary.get("handoff").is_none());
    for head in ["local library uses different key material; original data was preserved",
        "destination belongs to another account"] {
        for (diagnostic, classified) in [
            (head.to_owned(), true), (format!("{head}: {private}"), true),
            (format!("{head}-suffix: {private}"), false),
            (format!("outer: {head}: {private}"), false),
            (format!("{head}:{private}"), false),
            (format!("local library uses different key material or compatibility could not be verified; original data was preserved: {private}"), false),
        ] {
            let mut envelope = ResultEnvelope::new("auth-account", Status::Fail,
                serde_json::json!({}), Vec::new());
            envelope.errors.push(diagnostic.clone());
            envelope.items = vec![
                respire_commands::output::Item::new("control_ready", Status::Ok, "true"),
                respire_commands::output::Item::new("store_ready", Status::Skip, "false"),
            ];
            let response = RpcResponse {
                v: request.v, id: request.id.clone(), operation_id: request.operation_id.clone(),
                idempotency_key: request.idempotency_key.clone(), ok: false, exit: 2,
                bin: "rsrs-runtime".into(), pid: Some(5252), scope: None, web_url: None,
                data_dir: None, code: Some("account_outcome_unknown".into()),
                error: Some(diagnostic), envelope: Some(envelope),
            };
            let error = validate_account_response(response.clone(), &request, &source, 5252, &source, false)
                .err().context("login rejection was accepted")?.to_string();
            assert_eq!(error.ends_with("; login_kind=account_material_rejected"), classified);
            assert!(!error.contains(private));
            assert!(!error.contains(head));
            for (mut other, register) in [(request.clone(), true), (request.clone(), false)] {
                if !register { other.method = HOST_AUTH_ACCOUNT_RESULT_METHOD.into(); }
                let error = validate_account_response(response.clone(), &other, &source, 5252, &source, register)
                    .err().context("non-login rejection was accepted")?.to_string();
                assert!(!error.contains("login_kind="));
                assert!(!error.contains(private));
            }
            for code in ["unauthorized", "scope_mismatch", "auth_ownership_unavailable", "control_unavailable"] {
                let mut unexecuted = response.clone();
                unexecuted.code = Some(code.into());
                let envelope = unexecuted.envelope.as_mut().context("rejection envelope missing")?;
                envelope.items[0].value = "false".into();
                envelope.items[0].status = Status::Fail;
                let error = validate_account_response(unexecuted, &request, &source, 5252, &source, false)
                    .err().context("unexecuted rejection was accepted")?.to_string();
                assert!(!error.contains("login_kind="));
                assert!(!error.contains(private));
            }
            let mut malformed = response.clone();
            malformed.pid = Some(5253);
            assert!(validate_account_response(malformed, &request, &source, 5252, &source, false)
                .err().context("foreign PID was accepted")?.to_string().contains("response identity mismatch"));
            let mut malformed = response;
            malformed.envelope.as_mut().context("rejection envelope missing")?.details = serde_json::json!({"private":private});
            let error = validate_account_response(malformed, &request, &source, 5252, &source, false)
                .err().context("private rejection details were accepted")?.to_string();
            assert_eq!(error, "invalid account control rejection envelope");
        }
    }
    Ok(())
}
