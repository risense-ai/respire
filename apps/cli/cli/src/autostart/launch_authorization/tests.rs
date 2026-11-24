use super::*;

#[test]
fn managed_supervisor_accepts_only_the_registered_image() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let stable = dir.path().join("stable");
    let npm = dir.path().join("npm");
    fs::write(&stable, b"installed CLI")?;
    fs::copy(&stable, &npm)?;
    verify_registered_supervisor(&stable, &stable)?;
    verify_registered_supervisor(&npm, &stable)?;
    fs::write(&npm, b"other CLI")?;
    let error = verify_registered_supervisor(&npm, &stable)
        .err()
        .context("accepted a different CLI image")?;
    assert!(error.to_string().contains("differs from the registered CLI"));
    Ok(())
}

#[test]
fn launch_roots_validate_before_creating_fresh_profiles() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Vec<(&'static str, Option<OsString>)>);
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
    let keys = ["RSRS_DATA_DIR", "RSRS_M3_DIR"];
    let _restore = Restore(
        keys.into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("fresh installation");
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_M3_DIR", temporary.path().join("models"));
    assert!(!root.exists());
    let read_roots = respire_client_host::config::control_roots()?;
    #[cfg(target_os = "macos")]
    let expected_root = temporary.path().canonicalize()?.join("fresh installation");
    #[cfg(not(target_os = "macos"))]
    let expected_root = std::path::absolute(&root)?;
    assert_eq!(read_roots, (expected_root.clone(), expected_root.clone()));
    assert!(!root.exists());
    let (installation, profile) = prepare_launch_roots()?;
    assert_eq!(installation, root.canonicalize()?);
    assert_eq!(profile, installation);
    assert!(!root.join("runtime").exists());

    let account = root.join("accounts/fresh profile");
    let config = root.join("client.json");
    let outside = temporary.path().join("outside installation");
    fs::write(&config, serde_json::to_vec(&json!({"data_dir": outside}))?)?;
    assert_eq!(respire_client_host::config::control_roots()?.1, expected_root);
    assert!(!outside.exists());
    fs::write(&config, serde_json::to_vec(&json!({"data_dir": account}))?)?;
    let (_, profile) = prepare_launch_roots()?;
    assert_eq!(profile, account.canonicalize()?);

    assert_eq!(
        account_identity_source(&profile)?,
        profile.display().to_string()
    );
    let session = profile.join("session.json");
    fs::write(&session, br#"{"user":"Fixture-Account"}"#)?;
    assert_eq!(account_identity_source(&profile)?, "Fixture-Account");
    for invalid in ["{", "{}", r#"{"user":42}"#, r#"{"user":""}"#] {
        fs::write(&session, invalid)?;
        assert!(account_identity_source(&profile).is_err());
        assert_eq!(fs::read_to_string(&session)?, invalid);
    }
    use respire_client_host::identity::{auth_session_identity, AuthSessionIdentity};
    fs::remove_file(&session)?;
    assert!(matches!(auth_session_identity(&profile, hash)?, AuthSessionIdentity::Absent));
    let operation_id = uuid::Uuid::new_v4().to_string();
    let candidate = serde_json::to_vec_pretty(&json!({
        "vault_version": 4, "kdf_salt": "fixture-salt", "wrapped_urk": "fixture-wrap",
        "urk_nonce": "fixture-nonce", "keyring_account": "fixture-alias",
        "keyring_backend": "native", "auth_operation_id": operation_id,
    }))?;
    fs::write(&session, &candidate)?;
    assert!(matches!(auth_session_identity(&profile, hash)?,
        AuthSessionIdentity::Unbound { operation_id: id, session_sha256 }
            if id == operation_id && session_sha256 == hash(&candidate)));
    assert_eq!(account_identity_source(&profile)?, profile.display().to_string());
    for invalid in ["{}", "{", r#"{"user":null}"#, r#"{"user":""}"#] {
        fs::write(&session, invalid)?;
        assert!(auth_session_identity(&profile, hash).is_err());
    }
    fs::write(&session, br#"{"user":"Fixture-Account"}"#)?;
    assert!(matches!(auth_session_identity(&profile, hash)?,
        AuthSessionIdentity::Bound { account_id, session_sha256 }
            if account_id == hash(b"Fixture-Account")
                && session_sha256 == hash(br#"{"user":"Fixture-Account"}"#)));
    fs::write(&session, &candidate)?;
    let auth_request: RpcRequest = serde_json::from_value(json!({
        "v": respire_commands::rpc_client::PROTOCOL_V,
        "id": uuid::Uuid::new_v4().to_string(),
        "method": respire_protocol::HOST_AUTH_KEYGEN_METHOD,
        "operation_id": operation_id, "idempotency_key": operation_id,
    }))?;
    let auth_pid = 4243;
    let auth_response: RpcResponse<ResultEnvelope> = serde_json::from_value(json!({
        "v": auth_request.v, "id": auth_request.id,
        "operation_id": operation_id, "idempotency_key": operation_id,
        "bin": "rsrs-runtime", "pid": auth_pid, "ok": true, "exit": 0,
        "envelope": ResultEnvelope::new("keygen", Status::Ok,
            json!({"operation_id": operation_id, "outcome": "committed",
                "session_sha256": hash(&candidate), "path": session,
                "super_password": "synthetic-private-code"}),
            vec![respire_commands::output::Item::new("control_ready", Status::Ok, "true"),
                respire_commands::output::Item::new("store_ready", Status::Skip, "false")]),
    }))?;
    assert!(control::validate_auth_response(auth_response.clone(), &auth_request, &profile, auth_pid).is_ok());
    for missing in [serde_json::Value::Null, json!("")] {
        let mut invalid = auth_response.clone();
        invalid.envelope.as_mut().context("auth fixture envelope missing")?
            .summary["super_password"] = missing;
        assert!(control::validate_auth_response(invalid, &auth_request, &profile, auth_pid).is_err());
    }
    let mut missing = auth_response.clone();
    missing.envelope.as_mut().context("auth fixture envelope missing")?
        .summary.as_object_mut().context("auth fixture summary missing")?
        .remove("super_password");
    assert!(control::validate_auth_response(missing, &auth_request, &profile, auth_pid).is_err());
    let mut foreign = auth_response.clone();
    foreign.operation_id = Some(uuid::Uuid::new_v4().to_string());
    assert!(control::validate_auth_response(foreign, &auth_request, &profile, auth_pid).is_err());
    let mut rolled_back = auth_response.clone();
    rolled_back.ok = false;
    rolled_back.exit = 2;
    rolled_back.code = Some("auth_not_committed".to_owned());
    let envelope = rolled_back.envelope.as_mut().context("auth fixture envelope missing")?;
    envelope.status = Status::Fail;
    envelope.summary["outcome"] = json!("rolled_back");
    envelope.summary["session_sha256"] = serde_json::Value::Null;
    envelope.summary["super_password"] = serde_json::Value::Null;
    assert!(control::validate_auth_response(rolled_back, &auth_request, &profile, auth_pid).is_ok());
    let mut unknown = auth_response;
    unknown.ok = false;
    unknown.exit = 2;
    unknown.code = Some("control_outcome_unknown".to_owned());
    for (code, private_error, diagnostic) in [
        ("control_outcome_unknown", "fixture-private", "auth_parent_revoked_or_channel_interrupted"),
        ("auth_outcome_unknown", "fixture-private: Secret Service initialization failed", "auth_credential_backend_unavailable"),
        ("auth_outcome_unknown", "fixture-private: kernel keyring initialization failed", "auth_credential_backend_unavailable"),
        ("auth_outcome_unknown", "fixture-private: offline auth requires a persistent credential backend; the volatile kernel keyring cannot recover this transaction", "auth_credential_backend_not_persistent"),
        ("auth_outcome_unknown", "fixture-private: migration keyring readback mismatch", "auth_credential_write_readback_failed"),
        ("auth_outcome_unknown", "fixture-private: migration keyring write/readback failed; no snapshot was published", "auth_credential_write_readback_failed"),
        ("auth_outcome_unknown", "fixture-private: unknown credential failure", "unclassified"),
        ("auth_outcome_unknown", "fixture-private: Secret Service initialization failed with extra text", "unclassified"),
    ] {
        let mut failed = unknown.clone();
        failed.code = Some(code.to_owned());
        failed.error = Some(private_error.to_owned());
        failed.envelope.as_mut().context("auth fixture envelope missing")?.errors.push(private_error.to_owned());
        let failure = control::validate_auth_response(failed, &auth_request, &profile, auth_pid)
            .err().context("unknown auth result must remain rejected")?;
        assert_eq!(format!("{failure:#}"), format!(
            "auth control completion is unknown; query the original operation; auth_diagnostic={diagnostic}"));
    }
    let mut foreign_unknown = unknown.clone();
    foreign_unknown.operation_id = Some(uuid::Uuid::new_v4().to_string());
    assert_eq!(control::validate_auth_response(foreign_unknown, &auth_request, &profile, auth_pid)
        .err().context("foreign unknown auth result accepted")?.to_string(), "auth control response identity mismatch");
    unknown.data_dir = Some(installation.to_string_lossy().into_owned());
    assert_eq!(control::validate_auth_response(unknown, &auth_request, &profile, auth_pid)
        .err().context("foreign unknown auth profile accepted")?.to_string(), "auth control response profile mismatch");
    fs::remove_file(&session)?;
    fs::create_dir(&session)?;
    assert!(account_identity_source(&profile).is_err());
    fs::remove_dir(&session)?;

    let request: RpcRequest = serde_json::from_value(json!({
        "v": respire_commands::rpc_client::PROTOCOL_V,
        "id": uuid::Uuid::new_v4().to_string(),
        "method": HostControlOperation::MemoryRevision.method(),
    }))?;
    let pid = 4242;
    let revision = "0123456789abcdef0123456789abcdef";
    let response: RpcResponse<ResultEnvelope> = serde_json::from_value(json!({
        "v": request.v,
        "id": request.id,
        "bin": "rsrs-runtime",
        "pid": pid,
        "ok": true,
        "exit": 0,
        "envelope": ResultEnvelope::new(
            "memory-revision",
            Status::Ok,
            json!({"profile": profile, "revision": revision}),
            vec![respire_commands::output::Item::new("control_ready", Status::Ok, "true")],
        ),
    }))?;
    assert_eq!(
        validate_control_response(response.clone(), &request, &profile, pid)?.summary
            ["revision"],
        revision
    );

    let mut wrong_id = response.clone();
    wrong_id.id = uuid::Uuid::new_v4().to_string();
    assert!(validate_control_response(wrong_id, &request, &profile, pid).is_err());
    let mut wrong_pid = response.clone();
    wrong_pid.pid = Some(pid + 1);
    assert!(validate_control_response(wrong_pid, &request, &profile, pid).is_err());
    let mut wrong_profile = response.clone();
    wrong_profile
        .envelope
        .as_mut()
        .context("fixture envelope missing")?
        .summary["profile"] = json!(installation);
    assert!(validate_control_response(wrong_profile, &request, &profile, pid).is_err());
    let mut duplicate_ready = response.clone();
    duplicate_ready
        .envelope
        .as_mut()
        .context("fixture envelope missing")?
        .items
        .push(respire_commands::output::Item::new(
            "control_ready",
            Status::Ok,
            "true",
        ));
    assert!(validate_control_response(duplicate_ready, &request, &profile, pid).is_err());
    let mut scoped = response.clone();
    scoped.scope = Some(respire_protocol::RuntimeRequestScope {
        account_id: "foreign-account".to_owned(),
        instance_id: "foreign-instance".to_owned(),
        generation: 0,
    });
    assert!(validate_control_response(scoped, &request, &profile, pid).is_err());

    let diagnostic = "both rsrs.db and onememory.db exist; choose one database";
    let mut unavailable = response;
    unavailable.ok = false;
    unavailable.exit = 2;
    unavailable.code = Some("revision_unavailable".to_owned());
    unavailable.error = Some(diagnostic.to_owned());
    unavailable.envelope = Some(ResultEnvelope::new(
        "memory-revision",
        Status::Fail,
        json!({}),
        vec![respire_commands::output::Item::new(
            "control_ready",
            Status::Ok,
            "true",
        )],
    ));
    let error = validate_control_response(unavailable.clone(), &request, &profile, pid)
        .err()
        .context("DB failure must return its diagnostic")?
        .to_string();
    assert!(error.contains(diagnostic));
    let mut foreign_failure = unavailable.clone();
    foreign_failure.pid = Some(pid + 1);
    let error = validate_control_response(foreign_failure, &request, &profile, pid)
        .err()
        .context("foreign Runtime must be rejected")?
        .to_string();
    assert!(!error.contains(diagnostic));
    let mut unready = unavailable.clone();
    unready
        .envelope
        .as_mut()
        .context("fixture envelope missing")?
        .items
        .clear();
    let error = validate_control_response(unready, &request, &profile, pid)
        .err()
        .context("unconfirmed control must remain redacted")?
        .to_string();
    assert!(!error.contains(diagnostic));
    unavailable.code = Some("unauthorized".to_owned());
    unavailable.error = Some("private grant/session fixture marker".to_owned());
    let error = validate_control_response(unavailable, &request, &profile, pid)
        .err()
        .context("authorization failure must remain redacted")?
        .to_string();
    assert!(!error.contains("private grant/session fixture marker"));

    assert!(!control_exit_confirmed(pid, Ok(false), false, None)?);
    assert!(control_exit_confirmed(pid, Ok(true), false, None)?);
    let kill_error = std::io::Error::other("fixture kill failed");
    assert!(control_exit_confirmed(
        pid,
        Ok(true),
        true,
        Some(&kill_error)
    )?);
    let timeout = control_exit_confirmed(pid, Ok(false), true, Some(&kill_error))
        .err()
        .context("cleanup deadline must fail closed")?
        .to_string();
    assert!(timeout.contains("cleanup_failed owned_pid=4242"));
    assert!(timeout.contains("exit confirmation deadline exceeded"));
    assert!(timeout.contains("fixture kill failed"));
    let observation_error = control_exit_confirmed(
        pid,
        Err(std::io::Error::other("fixture observation failed")),
        false,
        None,
    )
    .err()
    .context("cleanup observation error must fail closed")?
    .to_string();
    assert!(observation_error.contains("cleanup_failed owned_pid=4242"));
    assert!(observation_error.contains("fixture observation failed"));

    for invalid in ["{", r#"{"data_dir":42}"#] {
        fs::write(&config, invalid)?;
        assert!(prepare_launch_roots().is_err());
        assert_eq!(fs::read_to_string(&config)?, invalid);
    }
    let missing_account = root.join("must remain absent");
    fs::write(
        &config,
        serde_json::to_vec(&json!({"data_dir": missing_account.join("../escape")}))?,
    )?;
    assert!(prepare_launch_roots().is_err());
    assert!(!missing_account.exists() && !root.join("escape").exists());
    let missing = temporary.path().join("must remain absent");
    let unsafe_root = missing.join("..").join("escape");
    std::env::set_var("RSRS_DATA_DIR", &unsafe_root);
    assert!(prepare_launch_roots().is_err());
    assert!(!missing.exists() && !temporary.path().join("escape").exists());
    for invalid in ["", " "] {
        std::env::set_var("RSRS_DATA_DIR", invalid);
        assert!(prepare_launch_roots().is_err());
    }
    Ok(())
}

#[test]
fn explicit_launch_port_is_exact_and_unambiguous() -> Result<()> {
    use respire_protocol::host_plugin::*;
    let directory = tempfile::tempdir()?;
    let config = directory.path().join("plugins.json");
    let id = uuid::Uuid::new_v4().to_string();
    let request = HostPluginEventRequest { plugin_event: HOST_PLUGIN_SCHEMA_V, id: id.clone(),
        event: HostPluginEvent::PostSync, data: HostPluginSyncData { pulled: 2, pushed: 3 } };
    let bytes = serde_json::to_vec(&request)?;
    let ack = execute_plugin_event(&bytes, &config)?;
    assert_eq!(ack.id, id);
    assert_eq!(ack.plugin_event_ack, HOST_PLUGIN_SCHEMA_V);
    assert!(matches!(ack.result, HostPluginEventResult::Completed { warnings } if warnings.is_empty()));
    for policy in ["skip", "warn", "block", ""] {
        fs::write(&config, serde_json::to_vec(&json!({"hooks":{"post-sync":[
            {"cmd":"exit 1", "on_error":policy}
        ]}}))?)?;
        let ack = execute_plugin_event(&bytes, &config)?;
        assert!(matches!(ack.result, HostPluginEventResult::Failed { error } if error.contains("failed")));
    }
    fs::write(&config, b"{")?;
    assert!(matches!(execute_plugin_event(&bytes, &config)?.result, HostPluginEventResult::Failed { .. }));
    fs::remove_file(&config)?;
    let mut frame = u32::try_from(bytes.len())?.to_be_bytes().to_vec();
    frame.extend_from_slice(&bytes);
    let mut reply = Vec::new();
    serve_plugin_events(std::io::Cursor::new(&frame), &mut reply, &config)?;
    assert_eq!(u32::from_be_bytes(reply[..4].try_into()?) as usize, reply.len() - 4);
    let ack: HostPluginEventAck = serde_json::from_slice(&reply[4..])?;
    assert_eq!(ack.id, id);
    assert!(matches!(ack.result, HostPluginEventResult::Completed { warnings } if warnings.is_empty()));
    assert!(serve_plugin_events(std::io::Cursor::new(&frame[..frame.len()-1]), Vec::new(), &config).is_err());
    assert!(serve_plugin_events(std::io::Cursor::new(&frame[..2]), Vec::new(), &config).is_err());
    assert!(serve_plugin_events(std::io::Cursor::new(b"runtime=starting host=127.0.0.1 port=default\n"),
        Vec::new(), &config).is_err(), "human startup output must not enter the framed plugin channel");
    let arbitrary = serde_json::to_vec(&json!({"plugin_event":1, "id":id,
        "event":"command", "data":{"pulled":0,"pushed":0}, "cmd":"exit 0"}))?;
    assert!(execute_plugin_event(&arbitrary, &config).is_err());
    assert_eq!(
        launch_port(&["serve".into(), "--port".into(), "19169".into()])?,
        19169
    );
    for arguments in [
        vec!["--port".into()],
        vec!["--port".into(), "0".into()],
        vec![
            "--port".into(),
            "19169".into(),
            "--port".into(),
            "19170".into(),
        ],
    ] {
        assert!(launch_port(&arguments).is_err());
    }
    Ok(())
}
