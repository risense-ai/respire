use anyhow::{Context, Result};
use serde_json::{json, Value};

#[test]
fn host_control_dto_roundtrip_rejects_unknown_fields() -> Result<()> {
    use respire_protocol::{HostControlOperation, HostControlParams, RpcRequest, HOST_CONTROL_SCHEMA_V};

    let wire = json!({
        "schema_version": HOST_CONTROL_SCHEMA_V,
        "binding": {
            "installation_root": "fixture-root",
            "profile": "fixture-profile",
            "launch_id": "12345678-1234-4234-8234-123456789abc",
            "expected_account_id": null,
        },
    });
    let params: HostControlParams = serde_json::from_value(wire.clone())?;
    assert_eq!(serde_json::to_value(&params)?, wire);
    assert!(params.binding.expected_account_id.is_none());
    let operation = HostControlOperation::MemoryRevision;
    assert_eq!(serde_json::to_value(operation)?, json!("memory_revision"));
    assert_eq!(serde_json::from_value::<HostControlOperation>(json!("memory_revision"))?, operation);
    assert!(serde_json::from_value::<HostControlOperation>(json!("exec")).is_err());
    let request: RpcRequest = serde_json::from_value(json!({
        "v": 1,
        "id": "12345678-1234-4234-8234-123456789abc",
        "method": operation.method(),
        "args": [serde_json::to_string(&params)?],
    }))?;
    let roundtrip: RpcRequest = serde_json::from_slice(&serde_json::to_vec(&request)?)?;
    assert!(roundtrip.scope.is_none());
    assert_eq!(roundtrip.method, "control.memory_revision");
    assert_eq!(roundtrip.args, request.args);
    let mut unknown = wire.clone();
    unknown["argv"] = json!(["--direct"]);
    assert!(serde_json::from_value::<HostControlParams>(unknown).is_err());
    let mut unknown = wire;
    unknown["binding"]["generation"] = json!(0);
    assert!(serde_json::from_value::<HostControlParams>(unknown).is_err());
    use respire_protocol::{HostAuthIdentity, HostAuthKeygenParams, HOST_AUTH_SCHEMA_V};
    let mut auth = json!({
        "schema_version": HOST_AUTH_SCHEMA_V,
        "binding": serde_json::to_value(&params.binding)?,
        "identity": {"kind":"absent"},
        "force": false,
    });
    let parsed: HostAuthKeygenParams = serde_json::from_value(auth.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, auth);
    auth["identity"] = json!({"kind":"unbound","operation_id":"12345678-1234-4234-8234-123456789abc","session_sha256":"a".repeat(64)});
    let parsed: HostAuthKeygenParams = serde_json::from_value(auth.clone())?;
    assert!(matches!(&parsed.identity, HostAuthIdentity::Unbound { .. }));
    assert_eq!(serde_json::to_value(parsed)?, auth);
    auth["material"] = json!({
        "kdf_salt":"fixture-salt", "wrapped_urk":"fixture-wrap", "urk_nonce":"fixture-nonce",
        "super_password":"private-pipe-material", "keyring_account":"login-fixture", "keyring_backend":"fixture-backend",
    });
    let parsed: HostAuthKeygenParams = serde_json::from_value(auth.clone())?;
    assert_eq!(serde_json::to_value(&parsed)?, auth);
    assert!(!format!("{parsed:?}").contains("private-pipe-material"));
    let mut invalid_material = auth.clone();
    invalid_material["material"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<HostAuthKeygenParams>(invalid_material).is_err());
    for field in ["kdf_salt", "wrapped_urk", "urk_nonce", "super_password", "keyring_account", "keyring_backend"] {
        let mut missing = auth.clone();
        missing["material"].as_object_mut().context("material is an object")?.remove(field);
        assert!(serde_json::from_value::<HostAuthKeygenParams>(missing).is_err());
    }
    auth["identity"]["generation"] = json!(0);
    assert!(serde_json::from_value::<HostAuthKeygenParams>(auth.clone()).is_err());
    assert!(serde_json::from_value::<HostAuthIdentity>(json!({"kind":"anonymous"})).is_err());
    use respire_protocol::host_control::{HostAuthRegisterParams, HostAuthLoginParams, HostAuthAccountParams};
    let register = json!({"schema_version":1,"binding":params.binding,"identity":{"kind":"absent"},
        "addr":"https://example.invalid","user":"roundtrip-user","password":"private-pipe-only","super_password":null});
    let parsed: HostAuthRegisterParams = serde_json::from_value(register.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, register);
    let mut prepared_register = register.clone();
    prepared_register["material"] = auth["material"].clone();
    let parsed: HostAuthRegisterParams = serde_json::from_value(prepared_register.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, prepared_register);
    prepared_register["material"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<HostAuthRegisterParams>(prepared_register).is_err());
    let mut unknown = register.clone();
    unknown["argv"] = json!(["keygen"]);
    assert!(serde_json::from_value::<HostAuthRegisterParams>(unknown).is_err());
    let mut login = register;
    let object = login.as_object_mut().context("login parameters are an object")?;
    object.remove("password");
    object.insert("authorization".into(), json!({"kind":"password","password":"private-pipe-only","totp":null}));
    object.insert("super_password".into(), json!("recovery-code"));
    let parsed: HostAuthLoginParams = serde_json::from_value(login.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, login);
    let mut prepared_login = login.clone();
    prepared_login["material"] = json!({
        "keyring_account":"login-fixture", "keyring_backend":"native",
        "source":{"session_sha256":"a".repeat(64),
            "factors":{"version":"v1","password":"","account_secret":"fixture-source-secret"}},
        "target_original":null,
    });
    prepared_login["challenge"] = json!({"payload":"private-bound-challenge","sha256":"b".repeat(64)});
    prepared_login["custody_ack"] = json!({"candidate_sha256":"c".repeat(64)});
    let parsed: HostAuthLoginParams = serde_json::from_value(prepared_login.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, prepared_login);
    for field in ["material", "challenge", "custody_ack"] {
        let mut unknown = prepared_login.clone();
        unknown[field]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<HostAuthLoginParams>(unknown).is_err());
    }
    for factors in [
        json!({"version":"v1","password":"fixture-pass","account_secret":"fixture-secret"}),
        json!({"version":"v2","super_password":"fixture-super"}),
        json!({"version":"v3","super_password":"fixture-super","secret_key":"fixture-secret"}),
        json!({"version":"v4","super_password":"fixture-super"}),
    ] {
        let parsed: respire_protocol::host_control::HostAuthVaultFactors = serde_json::from_value(factors.clone())?;
        assert_eq!(serde_json::to_value(parsed)?, factors);
        let snapshot = json!({
            "schema_version":1,"binding":params.binding,"operation_id":"fixture-operation",
            "request_fingerprint":"d".repeat(64),"sync_generation":0,"snapshot_id":"fixture-snapshot",
            "action":"restore","material":{"session_sha256":"a".repeat(64),"factors":factors},
        });
        let parsed: respire_protocol::maintenance_control::MaintenanceControlParams =
            serde_json::from_value(snapshot.clone())?;
        assert_eq!(serde_json::to_value(&parsed)?, snapshot);
        assert!(!format!("{parsed:?}").contains("fixture-super"));
        assert!(!format!("{parsed:?}").contains("fixture-pass"));
        assert!(!format!("{parsed:?}").contains("fixture-secret"));
        let mut unknown_snapshot = snapshot.clone();
        unknown_snapshot["material"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<respire_protocol::maintenance_control::MaintenanceControlParams>(unknown_snapshot).is_err());
        let mut missing_snapshot = snapshot;
        missing_snapshot["material"].as_object_mut().context("snapshot material is an object")?.remove("session_sha256");
        assert!(serde_json::from_value::<respire_protocol::maintenance_control::MaintenanceControlParams>(missing_snapshot).is_err());
        let migration = json!({"candidates":[{"session_sha256":"a".repeat(64),"factors":factors}],
            "custody":{"keyring_account":"fixture-migration","keyring_backend":"native",
                "legacy_secret_backend":null,"login_password_retained":true}});
        let parsed: respire_protocol::migration_control::HostMigrationMaterial =
            serde_json::from_value(migration.clone())?;
        assert_eq!(serde_json::to_value(parsed)?, migration);
        let mut unknown_migration = migration.clone();
        unknown_migration["custody"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<respire_protocol::migration_control::HostMigrationMaterial>(unknown_migration).is_err());
        let mut missing_migration = migration;
        missing_migration.as_object_mut().context("migration material is an object")?.remove("candidates");
        assert!(serde_json::from_value::<respire_protocol::migration_control::HostMigrationMaterial>(missing_migration).is_err());
        let mut unknown = factors.clone();
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<respire_protocol::host_control::HostAuthVaultFactors>(unknown).is_err());
        let required = if factors["version"] == "v1" { "account_secret" } else { "super_password" };
        let mut missing = factors;
        missing.as_object_mut().context("vault factors are an object")?.remove(required);
        assert!(serde_json::from_value::<respire_protocol::host_control::HostAuthVaultFactors>(missing).is_err());
    }
    login["authorization"]["exec"] = json!(true);
    assert!(serde_json::from_value::<HostAuthLoginParams>(login).is_err());
    let query = json!({"schema_version":1,"binding":params.binding,"identity":{"kind":"absent"}});
    let parsed: HostAuthAccountParams = serde_json::from_value(query.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, query);
    let mut unknown = query;
    unknown["verified"] = json!(true);
    assert!(serde_json::from_value::<HostAuthAccountParams>(unknown).is_err());
    assert_eq!(serde_json::to_value(respire_protocol::migration_control::HostMigrationOutcome::CredentialsRequired)?,
        json!("credentials_required"));
    let key_input = json!({"password":null,"super_password":null,"account_secret":null,"totp":null,
        "material":{"keyring_account":"fixture-key","keyring_backend":"native",
            "reset":auth["material"],"challenge":"private-key-continuation"}});
    let parsed: respire_protocol::key_control::HostKeyInput = serde_json::from_value(key_input.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, key_input);
    for field in ["keyring_account", "keyring_backend"] {
        let mut missing = key_input.clone();
        missing["material"].as_object_mut().context("key material is an object")?.remove(field);
        assert!(serde_json::from_value::<respire_protocol::key_control::HostKeyInput>(missing).is_err());
    }
    let mut unknown = key_input.clone();
    unknown["material"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<respire_protocol::key_control::HostKeyInput>(unknown).is_err());
    let mut unknown = key_input;
    unknown["material"]["reset"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<respire_protocol::key_control::HostKeyInput>(unknown).is_err());
    let event = json!({"plugin_event":1,"id":"12345678-1234-4234-8234-123456789abc",
        "event":"post-sync","data":{"pulled":2,"pushed":3}});
    let parsed: respire_protocol::host_plugin::HostPluginEventRequest = serde_json::from_value(event.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, event);
    let mut unknown = event.clone();
    unknown["data"]["command"] = json!("arbitrary-shell");
    assert!(serde_json::from_value::<respire_protocol::host_plugin::HostPluginEventRequest>(unknown).is_err());
    let mut unknown = event;
    unknown["event"] = json!("exec");
    assert!(serde_json::from_value::<respire_protocol::host_plugin::HostPluginEventRequest>(unknown).is_err());
    for result in [json!({"status":"completed","warnings":[]}), json!({"status":"failed","error":"fixture failure"})] {
        let ack = json!({"plugin_event_ack":1,"id":"12345678-1234-4234-8234-123456789abc","result":result});
        let parsed: respire_protocol::host_plugin::HostPluginEventAck = serde_json::from_value(ack.clone())?;
        assert_eq!(serde_json::to_value(parsed)?, ack);
        let mut unknown = ack;
        unknown["result"]["retry"] = json!(true);
        assert!(serde_json::from_value::<respire_protocol::host_plugin::HostPluginEventAck>(unknown).is_err());
    }
    Ok(())
}

#[test]
fn business_request_scope_round_trips_and_rejects_incomplete_identity() -> Result<()> {
    use respire_protocol::RuntimeRequestScope;
    let wire = json!({
        "account_id": "test-account",
        "instance_id": "test-instance",
        "generation": 7,
    });
    let scope: RuntimeRequestScope = serde_json::from_value(wire.clone())?;
    assert!(!scope.account_id.is_empty());
    assert!(!scope.instance_id.is_empty());
    assert!(scope.generation > 0);
    assert_eq!(serde_json::to_value(scope)?, wire);
    for key in ["account_id", "instance_id", "generation"] {
        let mut missing = wire.clone();
        missing.as_object_mut().context("scope object")?.remove(key);
        assert!(serde_json::from_value::<RuntimeRequestScope>(missing).is_err());
        let mut null = wire.clone();
        null[key] = Value::Null;
        assert!(serde_json::from_value::<RuntimeRequestScope>(null).is_err());
    }
    let mut unknown = wire.clone();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<RuntimeRequestScope>(unknown).is_err());
    let mut wrong_generation = wire;
    wrong_generation["generation"] = json!("1");
    assert!(serde_json::from_value::<RuntimeRequestScope>(wrong_generation).is_err());
    Ok(())
}

#[test]
fn stop_receipt_wire_has_typed_terminal_outcomes() -> Result<()> {
    use respire_protocol::{RuntimeStopOutcome, RuntimeStopReceipt};

    let mut wire = json!({
        "schema_version": 1,
        "protocol_version": 1,
        "operation_id": "test-stop",
        "installation_id": "test-installation",
        "instance_id": "test-instance",
        "pid": 42,
        "process_start_id": "test-process",
        "image_path": "/test-runtime",
        "image_sha256": "a".repeat(64),
        "runtime_version": "1.2.3",
        "account_id": "test-account",
        "generation": 7,
        "state": "drained",
        "unfinished": []
    });
    let receipt: RuntimeStopReceipt = serde_json::from_value(wire.clone())?;
    assert_eq!(receipt.state, RuntimeStopOutcome::Drained);
    assert!(receipt.unfinished.is_empty());
    assert_eq!(serde_json::to_value(receipt)?, wire);
    for (state, outcome) in [
        ("forced_timeout", RuntimeStopOutcome::ForcedTimeout),
        ("compatibility_timeout", RuntimeStopOutcome::CompatibilityTimeout),
    ] {
        wire["state"] = json!(state);
        wire["unfinished"] = json!([{"operation_id":"test-write", "result":"aborted"}]);
        let receipt: RuntimeStopReceipt = serde_json::from_value(wire.clone())?;
        assert_eq!(receipt.state, outcome);
        assert_eq!(serde_json::to_value(receipt)?, wire);
    }
    wire["unfinished"][0]["result"] = json!("completed");
    assert!(serde_json::from_value::<RuntimeStopReceipt>(wire).is_err());
    Ok(())
}
