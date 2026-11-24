use super::*;

fn request(method: &str, params: &[(&str, &str)]) -> Request {
    Request {
        v: 1,
        id: "original-client2-request".into(),
        method: method.into(),
        params: params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    }
}

#[test]
fn original_offline_recovery_keeps_empty_server_and_user() -> Result<()> {
    let command = arguments(&request(
        "auth.recover",
        &[
            ("super", "synthetic-super"),
            ("kdf_salt", "synthetic-salt"),
            ("wrapped_urk", "synthetic-wrap"),
            ("urk_nonce", "synthetic-nonce"),
        ],
    ))?;
    assert_eq!(command[1], "fivekeys");
    assert!(command.iter().any(|value| value == "--pass="));
    assert!(!command
        .iter()
        .any(|value| value.starts_with("--addr=") || value.starts_with("--user=")));
    Ok(())
}

#[test]
fn original_password_totp_form_retains_its_existing_fields() -> Result<()> {
    let command = arguments(&request(
        "auth.login",
        &[
            ("addr", "http://127.0.0.1:9"),
            ("user", "synthetic-user"),
            ("password", "synthetic-password"),
            ("totp", "123456"),
        ],
    ))?;
    let input = initial_input(
        &request(
            "auth.login",
            &[
                ("migrate_local", "true"),
                ("offer_local_merge", "false"),
                (
                    "continuation",
                    r#"{"source_secret":"synthetic-source","merge_local":true}"#,
                ),
            ],
        )
        .params,
    )?;
    assert!(input.migrate_local && !input.migrate_vault && !input.offer_local_merge);
    assert_eq!(input.source_secret.as_deref(), Some("synthetic-source"));
    assert_eq!(input.merge_local, Some(true));
    let browser = arguments(&request("auth.login.browser", &[]))?;
    assert_eq!(browser, vec!["--json", "login", "--oauth"]);
    let browser = arguments(&request(
        "auth.login.browser",
        &[("addr", "http://127.0.0.1:9")],
    ))?;
    assert!(browser
        .iter()
        .any(|value| value == "--addr=http://127.0.0.1:9"));
    for field in [
        "password",
        "user",
        "authorization",
        "token",
        "totp",
        "continuation",
        "material",
        "migrate_local",
    ] {
        assert!(arguments(&request("auth.login.browser", &[(field, "forbidden")])).is_err());
    }
    assert!(command.iter().any(|value| value == "--interactive"));
    assert!(!command.iter().any(|value| value.contains("123456")));
    assert!(arguments(&request(
        "auth.login",
        &[
            ("user", "synthetic-user"),
            ("password", "synthetic-password"),
            ("totp", "invalid")
        ]
    ))
    .is_err());
    Ok(())
}

#[test]
fn existing_transport_rejects_unknown_method_fields_and_missing_inputs() -> Result<()> {
    let login: LoginContinue = serde_json::from_value(json!({
        "operation_id":"a7e71cb4-0016-4a1d-bc18-8f83816078ec", "source_profile":"synthetic-profile",
        "continuation":{"account_secret":"synthetic-secret"}, "totp":"123456"
    }))?;
    assert!(supplemental_input(login.continuation).is_ok());
    assert!(key_input(json!({"totp":"123456"})).is_ok());
    for field in [
        "material",
        "challenge",
        "custody_ack",
        "operation",
        "receipt",
    ] {
        let mut value = json!({});
        value[field] = json!({});
        assert!(key_input(value).is_err());
    }
    for field in [
        "authorization",
        "addr",
        "user",
        "migrate_local",
        "material",
        "challenge",
        "custody_ack",
    ] {
        let mut value = json!({"operation_id":"original", "source_profile":"synthetic-profile", "continuation":{}});
        value[field] = json!("forbidden");
        assert!(serde_json::from_value::<LoginContinue>(value).is_err());
        assert!(supplemental_input(json!({field:"forbidden"})).is_err());
    }
    assert!(serde_json::from_value::<LoginContinue>(
        json!({"source_profile":"synthetic-profile", "continuation":{}})
    )
    .is_err());
    assert!(serde_json::from_value::<KeyContinue>(json!({"operation_id":"original", "source_profile":"synthetic-profile", "input":{}, "receipt":{}})).is_err());
    assert!(initial_input(&request("auth.login", &[("migrate_local", "yes")]).params).is_err());
    assert!(initial_input(
        &request(
            "auth.login",
            &[("continuation", r#"{"migrate_local":true}"#)]
        )
        .params
    )
    .is_err());
    for (method, params) in [
        ("core_config.set", json!({})),
        ("core_config.set", json!({"setting":42})),
        ("core_config.set", json!({"setting":"read_heat=true", "scope":{}})),
        ("core_config.result", json!({"setting":"read_heat=true"})),
        ("core_config.result", json!({"source_profile":"another-profile"})),
        ("core_config.result", json!({"scope":{}})),
    ] {
        let captured = capture_request(&WireRequest {v:1, id:"desktop-a7e71cb4-0016-4a1d-bc18-8f83816078ec".into(), method:method.into(), params});
        assert_ne!(captured.exit, 0);
        assert!(matches!(captured.envelope.status, respire_commands::output::Status::Fail));
    }
    assert!(arguments(&request("auth.unknown", &[])).is_err());
    assert!(arguments(&request("auth.login", &[("user", "synthetic-user")])).is_err());
    assert!(arguments(&request("auth.reset", &[("unexpected", "value")])).is_err());
    assert!(arguments(&request(
        "auth.recover",
        &[
            ("kdf_salt", "salt"),
            ("wrapped_urk", "wrap"),
            ("urk_nonce", "nonce")
        ]
    ))
    .is_err());
    Ok(())
}

#[test]
fn pending_maintenance_continuation_reaches_original_transaction_validation() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
                None => std::env::remove_var("RSRS_DATA_DIR"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("RSRS_DATA_DIR"));
    let directory = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", directory.path());
    let marker = directory.path().join("runtime/autostart/maintenance.json");
    std::fs::create_dir_all(marker.parent().context("test marker parent")?)?;
    let bytes = serde_json::to_vec(&json!({
        "schema": 1, "id": "fixture-installation", "token": uuid::Uuid::new_v4().to_string(),
        "phase": "Paused", "expected_profile": directory.path().canonicalize()?,
        "expected_version": env!("CARGO_PKG_VERSION"),
        "owner_pid": std::process::id(), "owner_birth": "fixture-birth"
    }))?;
    std::fs::write(&marker, &bytes)?;
    assert!(crate::autostart::check_direct_access().is_err());
    let request = WireRequest {
        v: 1,
        id: "resume-transport".into(),
        method: "auth.login.continue".into(),
        params: json!({"operation_id": "not-an-operation", "source_profile": directory.path(),
                       "continuation": {}, "super_password": "fixture-super"}),
    };
    let error = continue_request(&request)
        .err()
        .context("foreign operation was accepted")?;
    assert!(
        format!("{error:#}").contains("invalid account operation ID"),
        "{error:#}"
    );
    let request = WireRequest {
        method: "auth.key.continue".into(),
        params: json!({"operation_id": uuid::Uuid::new_v4().to_string(),
                       "source_profile": directory.path(), "input": {}}),
        ..request
    };
    let error = continue_request(&request)
        .err()
        .context("unverified Runtime was accepted")?;
    assert!(
        format!("{error:#}").contains("model control Runtime manifest is missing"),
        "{error:#}"
    );
    assert!(crate::autostart::check_direct_access().is_err());
    assert_eq!(std::fs::read(&marker)?, bytes);
    Ok(())
}

#[test]
fn existing_failure_scrubber_removes_synthetic_values() {
    let mut value =
        json!({"errors":["synthetic-secret", {"detail":"prefix synthetic-secret suffix"}]});
    let mut values = Vec::new();
    secret_values(
        &json!({"continuation":r#"{"account_secret":"synthetic-secret"}"#,
        "input":{"super_password":"synthetic-super"}}),
        &mut values,
    );
    assert!(values.iter().any(|secret| secret == "synthetic-secret"));
    assert!(values.iter().any(|secret| secret == "synthetic-super"));
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    scrub(&mut value, &refs);
    assert!(!value.to_string().contains("synthetic-secret"));
}
