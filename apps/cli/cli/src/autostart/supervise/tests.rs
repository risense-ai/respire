use super::*;

#[test]
fn supervisor_never_turns_resolution_failure_into_a_shim() -> Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow::anyhow!("test environment lock poisoned: {error}"))?;
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    let _restore = Restore(["RSRS_RUNTIME_BIN", "RSRS_DATA_DIR", "RSRS_CLIENT_ONLY", "RSRS_NO_AUTOSTART", "RSRS_RPC_PORT"]
        .into_iter().map(|key| (key, std::env::var_os(key))).collect());
    let directory = tempfile::tempdir()?;
    std::env::set_var("RSRS_RUNTIME_BIN", directory.path().join("missing-runtime"));
    anyhow::ensure!(resolve_child_plan().is_err());

    // A file override is a selection fixture only; this test executes no binary.
    let binary = directory.path().join("synthetic-runtime");
    std::fs::write(&binary, b"synthetic runtime selection")?;
    std::env::set_var("RSRS_RUNTIME_BIN", &binary);
    anyhow::ensure!(resolve_child_plan()? == binary);
    std::env::set_var("RSRS_RUNTIME_BIN", std::env::current_exe()?);
    anyhow::ensure!(resolve_child_plan().is_err());

    let root = directory.path().join("installation");
    std::fs::create_dir(&root)?;
    let root = root.canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_CLIENT_ONLY", "0");
    std::env::set_var("RSRS_NO_AUTOSTART", "0");
    let owner = serde_json::from_value(serde_json::json!({
        "server":"respire", "bin":"rsrs-runtime", "pid":0, "v":1
    }))?;
    let marker = root.join("runtime/autostart/maintenance.json");
    std::fs::create_dir_all(marker.parent().context("fixture marker parent missing")?)?;
    std::fs::write(&marker, b"{}")?;
    assert!(!configure_background_owner(&owner)?);
    std::fs::remove_file(marker)?;
    let controller = crate::runtime_policy::takeover_lock()?;
    // The controller cannot release takeover until its supervisor exits.
    assert!(!configure_background_owner(&owner)?);
    assert!(controller.protects(&root.join("runtime/takeover"))?);
    drop(controller);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);
    assert!(!configure_background_owner(&owner)?, "absent owner must leave configuration pending");
    let lock_file = root.join("runtime/takeover/lock.db");
    std::fs::remove_file(&lock_file)?;
    std::fs::create_dir(&lock_file)?;
    assert!(configure_background_owner(&owner).is_err());
    // Busy is the only deferred error; host-policy denial remains fatal.
    std::env::set_var("RSRS_CLIENT_ONLY", "1");
    assert!(configure_background_owner(&owner).is_err());
    Ok(())
}

#[test]
fn supervisor_retries_are_bounded_and_reset_only_after_stability() -> Result<()> {
    let _environment = crate::TEST_ENV_LOCK.lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    let mut failures = 0;
    assert!(supervise_retry(&mut failures, Duration::from_secs(1)));
    assert!(supervise_retry(&mut failures, Duration::from_secs(1)));
    assert!(!supervise_retry(&mut failures, Duration::from_secs(1)));
    assert!(supervise_retry(&mut failures, Duration::from_secs(120)));
    assert_eq!(failures, 1);
    use respire_commands::output::{ResultEnvelope, Status};
    use std::cell::Cell;
    let finished = Cell::new(None);
    let operation_id = uuid::Uuid::new_v4().to_string();
    let response = |outcome: &str, secret: serde_json::Value| {
        ResultEnvelope::new("keygen", Status::Ok, serde_json::json!({
            "operation_id": operation_id, "outcome": outcome,
            "path": "fixture/session.json", "session_sha256": null,
            "super_password": secret,
        }), Vec::new())
    };
    let finish = |resume| { finished.set(Some(resume)); Ok(()) };
    let output = auth_control::finish_keygen(
        Ok(response("committed", serde_json::json!("synthetic-recovery-code"))), &operation_id, false, false, finish)?;
    assert_eq!(finished.get(), Some(false));
    finished.set(None);
    assert_eq!(output.summary, serde_json::json!({
        "path": "fixture/session.json", "super": "synthetic-recovery-code"}));
    assert_eq!(output.items[0].name, "keys");
    assert_eq!(output.actions, ["login --interactive --user <user> --pass <password>"]);
    let failed_finish = auth_control::finish_keygen(
        Ok(response("committed", serde_json::json!("synthetic-recovery-code"))), &operation_id, true, false,
        |resume| { assert!(!resume); anyhow::bail!("synthetic finish failure") },
    ).err().context("committed finish failure must remain an error")?;
    let failure = failed_finish.to_string();
    assert!(failure.contains("keygen committed") && failure.contains(&operation_id)
        && failure.contains("force=true") && failure.contains("do not replay keygen"));
    assert!(!failure.contains("synthetic-recovery-code"));
    auth_control::finish_keygen(
        Ok(response("committed", serde_json::json!("synthetic-recovery-code"))), &operation_id, false, true, finish)?;
    assert_eq!(finished.get(), Some(false));
    finished.set(None);
    for missing in [serde_json::Value::Null, serde_json::json!("")] {
        assert!(auth_control::finish_keygen(Ok(response("committed", missing)), &operation_id, false, false, finish).is_err());
        assert_eq!(finished.get(), None);
    }
    let mut malformed = response("committed", serde_json::json!("synthetic-recovery-code"));
    malformed.summary = serde_json::Value::Null;
    assert!(auth_control::finish_keygen(Ok(malformed), &operation_id, false, false, finish).is_err());
    assert_eq!(finished.get(), None);
    let unknown = anyhow::Error::new(AuthOutcomeUnknown { operation_id: operation_id.clone(), force: true });
    let error = auth_control::finish_keygen(Err(unknown), &operation_id, true, false, finish)
        .err().context("unknown result must not finish maintenance")?;
    assert_eq!(error.downcast_ref::<AuthOutcomeUnknown>()
        .context("original operation error was lost")?.operation_id, operation_id);
    assert_eq!(finished.get(), None);
    for force in [false, true] {
        let error = auth_control::finish_keygen(
            Ok(response("unknown", serde_json::Value::Null)), &operation_id, force, false, finish,
        ).err().context("explicit unknown must not finish maintenance")?;
        let original = error.downcast_ref::<AuthOutcomeUnknown>()
            .context("explicit unknown lost its operation context")?;
        assert_eq!(original.operation_id, operation_id);
        assert_eq!(original.force, force);
        assert_eq!(finished.get(), None);
    }
    let refused = auth_control::finish_keygen(
        Err(anyhow::anyhow!("synthetic preflight rejection")), &operation_id, true, false, finish,
    ).err().context("preflight rejection must remain an error")?;
    assert!(refused.downcast_ref::<AuthOutcomeUnknown>().is_none());
    assert_eq!(finished.get(), None);
    auth_control::finish_keygen(Ok(response("rolled_back", serde_json::Value::Null)), &operation_id, false, true, finish)?;
    assert_eq!(finished.get(), None);
    auth_control::finish_keygen(Ok(response("rolled_back", serde_json::Value::Null)), &operation_id, false, false, finish)?;
    assert_eq!(finished.get(), Some(true));
    assert!(auth_control::finish_keygen(Ok(response("rolled_back", serde_json::Value::Null)), &operation_id, false, false,
        |resume| { assert!(resume); anyhow::bail!("synthetic resume failure") }).is_err());
    use respire_protocol::host_control::{HostAuthAccountOutcome, HostAuthAccountResult,
        HostAuthAuthorization, HostAuthLoginParams, HostAuthRegisterParams, HostAuthAccountParams};
    let binding = respire_protocol::ControlBinding {
        installation_root: std::env::current_dir()?.canonicalize()?, profile: std::env::current_dir()?.canonicalize()?,
        launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
    };
    let identity = respire_protocol::HostAuthIdentity::Absent;
    let fixture_password = "test-only-password";
    let fixture_recovery = "test-only-recovery";
    for (operation, method) in [
        (AuthAccountControl::Register { addr: "https://fixture.invalid".into(), user: "fixture".into(),
            password: fixture_password.into(), super_password: None }, "control.auth.register"),
        (AuthAccountControl::Login { addr: "https://fixture.invalid".into(), user: "fixture".into(),
            authorization: HostAuthAuthorization::Password { password: fixture_password.into(), totp: None },
            super_password: fixture_recovery.into() }, "control.auth.login"),
        (AuthAccountControl::Login { addr: "https://fixture.invalid".into(), user: "fixture".into(),
            authorization: HostAuthAuthorization::Browser { token: "synthetic-token".into(), session_id: None },
            super_password: fixture_recovery.into() }, "control.auth.login"),
        (AuthAccountControl::Result, "control.auth.account_result"),
        (AuthAccountControl::Complete, "control.auth.account_complete"),
        (AuthAccountControl::Rollback, "control.auth.account_rollback"),
    ] {
        let request = prepare_auth_account_request(binding.clone(), identity.clone(), &operation_id, operation)?;
        assert_eq!(request.method, method);
        assert_eq!(request.operation_id.as_deref(), Some(operation_id.as_str()));
        assert_eq!(request.idempotency_key.as_deref(), Some(operation_id.as_str()));
        assert_eq!(uuid::Uuid::parse_str(&request.id)?.get_version_num(), 4);
        assert_ne!(request.id, operation_id);
        assert!(request.scope.is_none() && request.lifecycle.is_none()
            && request.queue_wait_ms.is_none() && request.parent_operation_id.is_none());
        assert_eq!(request.args.len(), 1);
        match method {
            "control.auth.register" => {
                let params: HostAuthRegisterParams = serde_json::from_str(&request.args[0])?;
                assert_eq!(params.password, fixture_password);
            }
            "control.auth.login" => {
                let params: HostAuthLoginParams = serde_json::from_str(&request.args[0])?;
                assert_eq!(params.super_password.as_deref(), Some(fixture_recovery));
            }
            _ => { let _: HostAuthAccountParams = serde_json::from_str(&request.args[0])?; }
        }
    }
    assert!(prepare_auth_account_request(binding.clone(), identity, "not-a-uuid", AuthAccountControl::Result).is_err());
    let mut receipt = HostAuthAccountResult {
        operation_id: operation_id.clone(), outcome: HostAuthAccountOutcome::RolledBack,
        source_profile: binding.profile.clone(), target_profile: Some(binding.profile.join("historical-absent-target")),
        account_id: None, session_sha256: None, super_password: None, login: None,
    };
    validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true)?;
    receipt.super_password = Some("synthetic-recovery-code".into());
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true).is_err());
    receipt.super_password = None;
    receipt.account_id = Some("synthetic-account-digest".into());
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true).is_err());
    receipt.outcome = HostAuthAccountOutcome::Unknown;
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true).is_err());
    receipt.account_id = None;
    validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true)?;
    assert_eq!(receipt.outcome, HostAuthAccountOutcome::Unknown);
    assert!(validate_auth_account_receipt(&receipt, &uuid::Uuid::new_v4().to_string(), &binding.profile, true).is_err());
    receipt.outcome = HostAuthAccountOutcome::AwaitingVerification;
    receipt.target_profile = None;
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true).is_err());
    struct RestoreRoot(Option<std::ffi::OsString>);
    impl Drop for RestoreRoot {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
                None => std::env::remove_var("RSRS_DATA_DIR"),
            }
        }
    }
    let _restore = RestoreRoot(std::env::var_os("RSRS_DATA_DIR"));
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let target = root.join("accounts/fixture");
    std::fs::create_dir_all(&target)?;
    let target = target.canonicalize()?;
    let session = br#"{"user":"Fixture-Account"}"#;
    std::fs::write(target.join("session.json"), session)?;
    std::fs::write(root.join("client.json"), serde_json::to_vec(&serde_json::json!({"data_dir": target}))?)?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    receipt.target_profile = Some(target);
    receipt.account_id = Some(super::super::hash(b"Fixture-Account"));
    receipt.session_sha256 = Some(super::super::hash(session));
    receipt.super_password = Some(fixture_recovery.into());
    validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true)?;
    receipt.outcome = HostAuthAccountOutcome::Committed;
    validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, true)?;
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, false).is_err());
    receipt.super_password = None;
    validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, false)?;
    receipt.session_sha256 = Some(super::super::hash(b"wrong-session"));
    assert!(validate_auth_account_receipt(&receipt, &operation_id, &binding.profile, false).is_err());
    receipt.session_sha256 = Some(super::super::hash(session));
    let request = prepare_auth_account_request(binding.clone(), respire_protocol::HostAuthIdentity::Absent,
        &operation_id, AuthAccountControl::Result)?;
    let response: respire_protocol::RpcResponse<ResultEnvelope> = serde_json::from_value(serde_json::json!({
        "v": request.v, "id": request.id, "operation_id": operation_id, "idempotency_key": operation_id,
        "bin": "rsrs-runtime", "pid": 5252, "ok": true, "exit": 0,
        "envelope": ResultEnvelope::new("auth-account", Status::Ok, serde_json::to_value(&receipt)?,
            vec![respire_commands::output::Item::new("control_ready", Status::Ok, "true"),
                respire_commands::output::Item::new("store_ready", Status::Skip, "false")]),
    }))?;
    let validate = |response| launch_authorization::control::validate_account_response(
        response, &request, &binding.profile, 5252, &binding.profile, false);
    assert!(validate(response.clone()).is_ok());
    let diagnostic = "synthetic-private-account-diagnostic";
    let rejection = |code: &str| -> Result<respire_protocol::RpcResponse<ResultEnvelope>> {
        let mut rejected = response.clone();
        rejected.ok = false;
        rejected.exit = 2;
        rejected.code = Some(code.to_owned());
        rejected.error = Some(diagnostic.into());
        rejected.data_dir = Some(binding.profile.to_string_lossy().into_owned());
        let deadline = code == "control_outcome_unknown";
        let executed = code == "account_outcome_unknown";
        let mut envelope = ResultEnvelope::new("auth-account", Status::Fail,
            if deadline { serde_json::json!({"outcome":"unknown"}) } else { serde_json::json!({}) },
            vec![respire_commands::output::Item::new("control_ready",
                if deadline { Status::Skip } else if executed { Status::Ok } else { Status::Fail },
                if deadline { "unknown" } else if executed { "true" } else { "false" }),
                respire_commands::output::Item::new("store_ready", Status::Skip, "false")]);
        if !deadline { envelope.errors.push(diagnostic.into()); }
        rejected.envelope = Some(envelope);
        Ok(rejected)
    };
    for code in ["unauthorized", "scope_mismatch", "auth_ownership_unavailable",
        "control_unavailable", "account_outcome_unknown", "control_outcome_unknown"] {
        let error = validate(rejection(code)?).err().context("rejection was accepted")?.to_string();
        assert!(error.contains(&format!("Runtime rejected account control ({code})")));
        if code == "control_outcome_unknown" {
            assert_eq!(error, "Runtime rejected account control (control_outcome_unknown): parent authorization ended or control channel was interrupted; query the original operation without replaying");
        }
        assert!(!error.contains(diagnostic));
    }
    let mut register_request = prepare_auth_account_request(binding.clone(), respire_protocol::HostAuthIdentity::Absent,
        &operation_id, AuthAccountControl::Register { addr: "https://fixture.invalid".into(),
            user: "fixture".into(), password: "fixture-password".into(), super_password: None })?;
    register_request.id = request.id.clone();
    let validate_register = |response: respire_protocol::RpcResponse<ResultEnvelope>, register| {
        launch_authorization::control::validate_account_response(
            response, &register_request, &binding.profile, 5252, &binding.profile, register)
    };
    for (head, class) in [
        ("registration outcome unknown; query the original operation, do not resend", "register_remote_unknown"),
        ("migration keyring write/readback failed; no snapshot was published", "register_native_credentials"),
        ("auth idempotency conflict", "register_state_conflict"),
        ("authorization did not return a token", "register_material_invalid"),
        ("local key compatibility could not be verified; original data was preserved", "register_local_compatibility"),
    ] {
        for (private, expected) in [
            (head.to_owned(), class),
            (format!("{head}: {diagnostic}"), class),
            (format!("{head}-suffix: {diagnostic}"), "register_unclassified"),
            (format!("outer: {head}: {diagnostic}"), "register_unclassified"),
            (format!("{head}:{diagnostic}"), "register_unclassified"),
        ] {
            let mut rejected = rejection("account_outcome_unknown")?;
            rejected.error = Some(private.clone());
            rejected.envelope.as_mut().context("account rejection fixture envelope missing")?.errors = vec![private];
            let error = validate_register(rejected.clone(), true).err().context("register rejection was accepted")?.to_string();
            assert_eq!(error, format!("Runtime rejected account control (account_outcome_unknown): account outcome is unknown; query the original operation without replaying; register_kind={expected}"));
            assert!(!error.contains(diagnostic));
            assert!(!validate_register(rejected.clone(), false).err().context("register rejection was accepted")?
                .to_string().contains("register_kind="));
            assert!(!validate(rejected).err().context("result rejection was accepted")?
                .to_string().contains("register_kind="));
        }
    }
    let error = validate_register(rejection("account_outcome_unknown")?, true)
        .err().context("unknown register rejection was accepted")?.to_string();
    assert!(error.ends_with("register_kind=register_unclassified"));
    assert!(!error.contains(diagnostic));
    for code in ["unauthorized", "scope_mismatch", "auth_ownership_unavailable", "control_unavailable", "control_outcome_unknown"] {
        let error = validate_register(rejection(code)?, true).err().context("register rejection was accepted")?.to_string();
        assert!(!error.contains("register_kind="));
        assert!(!error.contains(diagnostic));
    }
    let mut valid_rejection = serde_json::to_value(rejection("unauthorized")?)?;
    valid_rejection["scope"] = serde_json::Value::Null;
    for (pointer, value) in [
        ("/v", serde_json::json!(request.v + 1)),
        ("/id", serde_json::json!("foreign-request")),
        ("/pid", serde_json::json!(5253)),
        ("/bin", serde_json::json!("foreign-image")),
        ("/operation_id", serde_json::json!(uuid::Uuid::new_v4().to_string())),
        ("/idempotency_key", serde_json::json!("foreign-key")),
        ("/scope", serde_json::json!({"account_id":"foreign-account","instance_id":"foreign-instance","generation":1})),
        ("/web_url", serde_json::json!("https://fixture.invalid")),
        ("/data_dir", serde_json::json!(root)),
        ("/ok", serde_json::json!(true)),
        ("/exit", serde_json::json!(0)),
        ("/code", serde_json::json!("unrecognized-private-code")),
        ("/error", serde_json::Value::Null),
        ("/envelope/command", serde_json::json!("foreign-command")),
        ("/envelope/status", serde_json::json!("ok")),
        ("/envelope/summary", serde_json::json!({"outcome":"committed"})),
        ("/envelope/details", serde_json::json!({})),
        ("/envelope/actions", serde_json::json!(["untrusted-action"])),
        ("/envelope/errors", serde_json::json!(["different-diagnostic"])),
        ("/envelope/items/0/value", serde_json::json!("true")),
        ("/envelope/items/1/value", serde_json::json!("true")),
    ] {
        let mut malformed = valid_rejection.clone();
        *malformed.pointer_mut(pointer).context("account rejection fixture field missing")? = value.clone();
        let error = validate(serde_json::from_value(malformed)?)
            .err().context("malformed rejection was accepted")?.to_string();
        assert!(!error.contains("Runtime rejected account control"), "accepted malformed field {pointer}");
        assert!(!error.contains(diagnostic));
        let mut register_rejection = rejection("account_outcome_unknown")?;
        let private = format!("auth idempotency conflict: {diagnostic}");
        register_rejection.error = Some(private.clone());
        register_rejection.envelope.as_mut().context("account rejection fixture envelope missing")?.errors = vec![private];
        let mut malformed = serde_json::to_value(register_rejection)?;
        malformed["scope"] = serde_json::Value::Null;
        let value = if pointer == "/envelope/items/0/value" { serde_json::json!("false") } else { value };
        *malformed.pointer_mut(pointer).context("account rejection fixture field missing")? = value;
        let error = validate_register(serde_json::from_value(malformed)?, true)
            .err().context("malformed register rejection was accepted")?.to_string();
        assert!(!error.contains("Runtime rejected account control"), "accepted malformed register field {pointer}");
        assert!(!error.contains(diagnostic));
    }
    let mut awaiting = response.clone();
    awaiting.envelope.as_mut().context("account fixture envelope missing")?
        .summary["outcome"] = serde_json::json!("awaiting_verification");
    assert!(validate(awaiting).is_ok());
    let mut unknown = response.clone();
    unknown.ok = false; unknown.exit = 2; unknown.code = Some("account_outcome_unknown".into());
    let envelope = unknown.envelope.as_mut().context("account fixture envelope missing")?;
    envelope.status = Status::Fail;
    envelope.summary["outcome"] = serde_json::json!("unknown");
    assert!(validate(unknown.clone()).is_ok());
    unknown.ok = true;
    assert!(validate(unknown).is_err());
    let mut wrong = response.clone(); wrong.id = uuid::Uuid::new_v4().to_string();
    assert!(validate(wrong).is_err());
    let mut wrong = response.clone(); wrong.pid = Some(5253);
    assert!(validate(wrong).is_err());
    let mut duplicate = response.clone();
    duplicate.envelope.as_mut().context("account fixture envelope missing")?.items.push(
        respire_commands::output::Item::new("control_ready", Status::Ok, "true"));
    assert!(validate(duplicate).is_err());
    let mut mirrored = response;
    let envelope = mirrored.envelope.as_mut().context("account fixture envelope missing")?;
    envelope.details = envelope.summary.clone();
    assert!(validate(mirrored).is_err());
    Ok(())
}
