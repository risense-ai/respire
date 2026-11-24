//! Exact detached launch grant and identity fixtures.
use super::*;

pub(super) fn run() -> Result<()> {
    let offline = std::env::temp_dir().canonicalize()?
        .join(format!("respire-offline-fixture-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&offline)?;
    let offline_result = (|| -> Result<()> {
        let (mut context, mut supervised) = fixture();
        context.root = offline.clone();
        let control = offline.join("runtime/autostart");
        fs::create_dir_all(&control)?;
        fs::write(control.join("registration.json"), b"obsolete registration")?;
        verify_authority(None, &context)?;
        supervised.managed_id = Some("third-party-supervisor".to_owned());
        verify_authority(Some(&supervised), &context)?;
        assert!(verify_authority_for_mode(None, &context, false).is_err());
        let marker = control.join("maintenance.json");
        fs::write(&marker, b"unverified active maintenance")?;
        assert!(verify_authority(None, &context).is_err());
        assert!(verify_authority(Some(&supervised), &context).is_err());
        fs::remove_file(marker)?;
        let session_path = offline.join("session.json");
        assert!(serving_unbound_identity(&offline)?.is_none());
        let mut session = json!({"vault_version":4,"kdf_salt":"fixture-salt",
            "wrapped_urk":"fixture-wrap","urk_nonce":"fixture-nonce"});
        fs::write(&session_path, serde_json::to_vec(&session)?)?;
        assert!(serving_unbound_identity(&offline)?.is_none());
        session["keyring_account"] = json!("login-fixture");
        session["keyring_backend"] = json!("native");
        session["auth_operation_id"] = json!("12345678-1234-4234-8234-123456789abc");
        let bytes = serde_json::to_vec(&session)?;
        fs::write(&session_path, &bytes)?;
        assert!(matches!(serving_unbound_identity(&offline)?,
            Some(respire_protocol::HostAuthIdentity::Unbound { operation_id, session_sha256 })
                if operation_id == "12345678-1234-4234-8234-123456789abc" && session_sha256 == hash(&bytes)));
        for (field, value) in [("auth_operation_id", Value::Null),
            ("auth_operation_id", json!("wrong-operation")), ("keyring_backend", json!("kernel")),
            ("user", json!("")), ("extra", json!("unexpected"))] {
            let mut changed = session.clone();
            changed[field] = value;
            fs::write(&session_path, serde_json::to_vec(&changed)?)?;
            assert!(serving_unbound_identity(&offline).is_err());
        }
        session["user"] = json!("fixture-account");
        fs::write(&session_path, serde_json::to_vec(&session)?)?;
        assert!(serving_unbound_identity(&offline)?.is_none());
        Ok(())
    })();
    fs::remove_dir_all(&offline)?;
    offline_result?;
    let (context, grant) = fixture();
    validate_grant(&grant, &context, false, &grant.nonce, 2000)?;
    validate_control(Some(&grant), &context, None, None, None, None)?;
    let value = serde_json::to_value(&grant)?;
    for (field, replacement) in [
        ("schema_version", json!(2)),
        ("protocol_version", json!(0)),
        ("nonce", json!("wrong")),
        ("instance_id", json!("wrong")),
        ("installation_root", json!("other-root")),
        ("installation_id", json!("other")),
        ("profile", json!("other-profile")),
        ("account_id", json!("other")),
        ("sync_generation", json!(1)),
        ("image_path", json!("other-image")),
        ("image_sha256", json!("c".repeat(64))),
        ("managed_id", json!("unexpected")),
        ("port", json!(19170)),
        ("issued_at_ms", json!(3000)),
        ("expires_at_ms", json!(1999)),
        ("expires_at_ms", json!(62_000)),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        let changed: LaunchGrant = serde_json::from_value(changed)?;
        assert!(
            validate_grant(&changed, &context, false, &grant.nonce, 2000).is_err(),
            "accepted changed {field}"
        );
    }
    assert!(validate_grant(&grant, &context, true, &grant.nonce, 2000).is_err());
    let mut detached = value.clone();
    let fields = detached.as_object_mut().context("fixture grant is not an object")?;
    fields.remove("supervisor_pid");
    fields.remove("supervisor_birth");
    let detached: LaunchGrant = serde_json::from_value(detached)?;
    validate_grant(&detached, &context, false, &grant.nonce, 2000)?;
    verify_authority(Some(&detached), &context)?;
    assert!(verify_authority_for_mode(Some(&detached), &context, false).is_err());
    assert!(validate_nonce("../../outside").is_err());
    assert!(validate_nonce("12345678-1234-1234-8234-123456789abc").is_err());

    let mut binding = respire_protocol::ControlBinding {
        installation_root: context.root.clone(),
        profile: context.profile.clone(),
        launch_id: grant.instance_id.clone(),
        expected_account_id: Some(context.account_id.clone()),
    };
    validate_control_binding(&binding, &context, &grant, Some(&context.account_id))?;
    let params = respire_protocol::HostControlParams {
        schema_version: respire_protocol::HOST_CONTROL_SCHEMA_V,
        binding: binding.clone(),
    };
    let mut request: respire_protocol::RpcRequest = serde_json::from_value(json!({
        "v": respire_commands::rpc_worker::PROTOCOL_V,
        "id": grant.instance_id,
        "method": respire_protocol::HostControlOperation::MemoryRevision.method(),
        "args": [serde_json::to_string(&params)?],
    }))?;
    assert!(request.scope.is_none());
    assert!(crate::host_control::validate_request(&request).is_ok());
    request.method = respire_protocol::HostControlOperation::ModelInstallPlan.method().into();
    assert!(crate::host_control::validate_request(&request).is_err());
    let mut model_params = respire_protocol::HostModelParams {
        schema_version: respire_protocol::HOST_MODEL_SCHEMA_V,
        binding: binding.clone(),
        intent: respire_protocol::ModelInstallIntent::default(),
        selection_sha256: None,
    };
    request.args = vec![serde_json::to_string(&model_params)?];
    assert!(crate::host_control::validate_request(&request).is_ok());
    request.method = respire_protocol::HostControlOperation::ModelInstallVerify.method().into();
    assert!(crate::host_control::validate_request(&request).is_err());
    model_params.selection_sha256 = Some("a".repeat(64));
    request.args = vec![serde_json::to_string(&model_params)?];
    assert!(crate::host_control::validate_request(&request).is_ok());
    model_params.selection_sha256 = Some("A".repeat(64));
    request.args = vec![serde_json::to_string(&model_params)?];
    assert!(crate::host_control::validate_request(&request).is_err());
    model_params.selection_sha256 = Some("a".repeat(64));
    request.args = vec![serde_json::to_string(&model_params)?];
    request.operation_id = Some(grant.instance_id.clone());
    assert!(crate::host_control::validate_request(&request).is_err());
    request.operation_id = None;
    request.method = "control.arbitrary".into();
    assert!(crate::host_control::validate_request(&request).is_err());
    request.method = respire_protocol::model_maintenance::MODEL_MAINTENANCE_METHOD.into();
    assert!(crate::host_control::validate_request(&request).is_err());
    request.method = respire_protocol::HostControlOperation::MemoryRevision.method().into();
    request.args = vec![serde_json::to_string(&params)?];
    request.scope = Some(respire_protocol::RuntimeRequestScope {
        account_id: context.account_id.clone(),
        instance_id: grant.instance_id.clone(),
        generation: 1,
    });
    assert!(crate::host_control::validate_request(&request).is_err());
    let secret_params = respire_protocol::secret_control::HostSecretReadParams {
        schema_version: respire_protocol::secret_control::HOST_SECRET_SCHEMA_V,
        binding: binding.clone(),
        identity: respire_protocol::HostAuthIdentity::Bound {
            account_id: context.account_id.clone(), session_sha256: "a".repeat(64),
        },
        reveal: true,
    };
    let mut secret_request: respire_protocol::RpcRequest = serde_json::from_value(json!({
        "v": respire_commands::rpc_worker::PROTOCOL_V,
        "id": grant.instance_id,
        "method": respire_protocol::secret_control::HOST_SECRET_READ_METHOD,
        "args": [serde_json::to_string(&secret_params)?],
    }))?;
    assert!(crate::host_control::validate_request(&secret_request).is_err());
    secret_request.operation_id = Some(grant.instance_id.clone());
    assert!(crate::host_control::validate_request(&secret_request).is_err());
    secret_request.operation_id = None;
    let mut extra = serde_json::to_value(&secret_params)?;
    extra["unexpected"] = json!(true);
    secret_request.args = vec![serde_json::to_string(&extra)?];
    assert!(crate::host_control::validate_request(&secret_request).is_err());
    let sessions = respire_protocol::identity_control::HostIdentitySessionListParams {
        schema_version: respire_protocol::identity_control::HOST_IDENTITY_SCHEMA_V,
        binding: binding.clone(),
        identity: secret_params.identity.clone(),
    };
    let mut sessions_request = secret_request.clone();
    sessions_request.method = respire_protocol::identity_control::HOST_IDENTITY_SESSION_LIST_METHOD.into();
    sessions_request.args = vec![serde_json::to_string(&sessions)?];
    assert!(crate::identity_control::IdentityRequest::parse(&sessions_request).is_ok());
    assert!(crate::host_control::validate_request(&sessions_request).is_err());
    sessions_request.operation_id = Some(grant.instance_id.clone());
    assert!(crate::identity_control::IdentityRequest::parse(&sessions_request).is_err());
    sessions_request.operation_id = None;
    let mut extra = serde_json::to_value(&sessions)?;
    extra["unexpected"] = json!(true);
    sessions_request.args = vec![serde_json::to_string(&extra)?];
    assert!(crate::identity_control::IdentityRequest::parse(&sessions_request).is_err());
    let value = serde_json::to_value(&binding)?;
    for (field, replacement) in [
        ("installation_root", json!("other-root")),
        ("profile", json!("other-profile")),
        ("launch_id", json!("other-launch")),
        ("expected_account_id", json!("other-account")),
        ("expected_account_id", Value::Null),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        let changed = serde_json::from_value(changed)?;
        assert!(
            validate_control_binding(&changed, &context, &grant, Some(&context.account_id)).is_err(),
            "accepted changed control binding {field}"
        );
    }
    binding.expected_account_id = None;
    validate_control_binding(&binding, &context, &grant, None)?;
    assert!(validate_control_binding(&binding, &context, &grant, Some(&context.account_id)).is_err());
    let mut auth: respire_protocol::RpcRequest = serde_json::from_value(json!({
        "v": respire_commands::rpc_worker::PROTOCOL_V,
        "id": grant.instance_id,
        "operation_id": grant.instance_id,
        "idempotency_key": grant.instance_id,
        "method": respire_protocol::HOST_AUTH_KEYGEN_METHOD,
        "args": [serde_json::to_string(&respire_protocol::HostAuthKeygenParams {
            schema_version: respire_protocol::HOST_AUTH_SCHEMA_V,
            binding, identity: respire_protocol::HostAuthIdentity::Absent, force: false, material: None,
        })?],
    }))?;
    assert!(crate::host_control::validate_auth_request(&auth).is_ok());
    assert!(crate::host_control::validate_request(&auth).is_err());
    let mut account_request = auth.clone();
    account_request.method = respire_protocol::host_control::HOST_AUTH_REGISTER_METHOD.into();
    account_request.args = vec![serde_json::to_string(&respire_protocol::host_control::HostAuthRegisterParams {
        schema_version: respire_protocol::HOST_AUTH_SCHEMA_V,
        binding: params.binding.clone(),
        identity: respire_protocol::HostAuthIdentity::Absent,
        addr: "https://example.invalid".into(), user: "fixture-user".into(),
        password: "fixture-only-password".into(), super_password: None, material: None,
    })?];
    assert!(crate::auth_control::AccountRequest::parse(&account_request).is_ok());
    account_request.operation_id = None;
    assert!(crate::auth_control::AccountRequest::parse(&account_request).is_err());
    account_request.operation_id = auth.operation_id.clone();
    account_request.args.push("unexpected".into());
    assert!(crate::auth_control::AccountRequest::parse(&account_request).is_err());
    let mut changed = auth.clone();
    changed.operation_id = None;
    assert!(crate::host_control::validate_auth_request(&changed).is_err());
    changed = auth.clone();
    changed.idempotency_key = None;
    assert!(crate::host_control::validate_auth_request(&changed).is_err());
    auth.scope = Some(respire_protocol::RuntimeRequestScope {
        account_id: context.account_id.clone(), instance_id: grant.instance_id, generation: 1,
    });
    assert!(crate::host_control::validate_auth_request(&auth).is_err());
    use respire_protocol::identity_write_control::{HostIdentityWriteOperation,
        HostIdentityWriteOutcome, HostIdentityWriteResult};
    let launched = respire_protocol::HostAuthIdentity::Bound {
        account_id: context.account_id.clone(), session_sha256: "c".repeat(64),
    };
    let mut receipt = HostIdentityWriteResult {
        operation_id: auth.id.clone(), fingerprint: "d".repeat(64),
        operation: HostIdentityWriteOperation::Logout { full: true },
        outcome: HostIdentityWriteOutcome::Committed,
        path: context.profile.join("session.json"), session_found: true,
        session_sha256: None, identity: respire_protocol::HostAuthIdentity::Absent,
        revoke: None,
    };
    let mut after_logout = context.clone();
    after_logout.account_id = hash(context.profile.display().to_string().as_bytes());
    validate_identity_write_context(&context, &launched, &after_logout, &receipt)?;
    for field in ["root", "profile", "account", "generation", "image", "digest", "port"] {
        let mut foreign = after_logout.clone();
        match field {
            "root" => foreign.root = PathBuf::from("foreign-root"),
            "profile" => foreign.profile = PathBuf::from("foreign-profile"),
            "account" => foreign.account_id = "e".repeat(64),
            "generation" => foreign.sync_generation += 1,
            "image" => foreign.image_path = PathBuf::from("foreign-runtime"),
            "digest" => foreign.image_sha256 = "f".repeat(64),
            "port" => foreign.port += 1,
            _ => unreachable!("fixed launch context field"),
        }
        assert!(validate_identity_write_context(&context, &launched, &foreign, &receipt).is_err(), "{field}");
    }
    receipt.operation = HostIdentityWriteOperation::SessionRevoke { session_id: auth.id.clone() };
    assert!(validate_identity_write_context(&context, &launched, &after_logout, &receipt).is_err());
    receipt.operation = HostIdentityWriteOperation::Logout { full: true };
    receipt.path = context.root.join("foreign-session.json");
    assert!(validate_identity_write_context(&context, &launched, &after_logout, &receipt).is_err());
    receipt.path = context.profile.join("session.json");
    receipt.identity = launched.clone();
    receipt.session_sha256 = Some("c".repeat(64));
    assert!(validate_identity_write_context(&context, &launched, &context, &receipt).is_err());
    receipt.operation = HostIdentityWriteOperation::Logout { full: false };
    validate_identity_write_context(&context, &launched, &context, &receipt)?;
    receipt.session_sha256 = None;
    assert!(validate_identity_write_context(&context, &launched, &context, &receipt).is_err());
    use respire_protocol::key_control::{HostKeyOperation, HostKeyOutcome, HostKeyResult};
    let mut key_receipt = HostKeyResult { operation_id: auth.id.clone(), fingerprint: "e".repeat(64),
        operation: HostKeyOperation::Fivekeys { addr: String::new(), user: "fivekeys-fixture".into(),
            kdf_salt: "synthetic-salt".into(), wrapped_urk: "synthetic-wrap".into(), urk_nonce: "synthetic-nonce".into(), use_super: true },
        outcome: HostKeyOutcome::Pending, source_profile: context.profile.clone(),
        target_profile: Some(context.root.join("accounts/fivekeys-fixture")),
        account_id: Some(hash(b"fivekeys-fixture")), session_sha256: Some("f".repeat(64)),
        identity: respire_protocol::HostAuthIdentity::Bound { account_id: hash(b"fivekeys-fixture"), session_sha256: "f".repeat(64) },
        required_inputs: Vec::new(), verification_required: true, super_password: None, challenge: None };
    let mut target = context.clone();
    target.profile = key_receipt.target_profile.clone().context("fivekeys target missing")?;
    target.account_id = hash(b"fivekeys-fixture");
    validate_key_result_context(&context, &launched, &target, &key_receipt)?;
    assert!(validate_key_result_context(&context, &launched, &context, &key_receipt).is_err());
    for field in ["root", "profile", "account", "generation", "image", "sha", "port"] {
        let mut foreign = target.clone();
        match field {
            "root" => foreign.root = target.root.join("other"),
            "profile" => foreign.profile = target.profile.join("other"),
            "account" => foreign.account_id.push('x'),
            "generation" => foreign.sync_generation += 1,
            "image" => foreign.image_path.push("other"),
            "sha" => foreign.image_sha256.push('x'),
            _ => foreign.port += 1,
        }
        assert!(validate_key_result_context(&context, &launched, &foreign, &key_receipt).is_err(), "{field}");
    }
    if let HostKeyOperation::Fivekeys { user, .. } = &mut key_receipt.operation { user.clear(); }
    key_receipt.account_id = None;
    key_receipt.identity = respire_protocol::HostAuthIdentity::Unbound {
        operation_id: key_receipt.operation_id.clone(), session_sha256: "f".repeat(64) };
    target.account_id = hash(target.profile.display().to_string().as_bytes());
    validate_key_result_context(&context, &launched, &target, &key_receipt)?;
    key_receipt.identity = respire_protocol::HostAuthIdentity::Absent;
    assert!(validate_key_result_context(&context, &launched, &target, &key_receipt).is_err());
    key_receipt.identity = respire_protocol::HostAuthIdentity::Unbound {
        operation_id: "foreign-operation".into(), session_sha256: "f".repeat(64) };
    assert!(validate_key_result_context(&context, &launched, &target, &key_receipt).is_err());
    key_receipt.identity = respire_protocol::HostAuthIdentity::Unbound {
        operation_id: key_receipt.operation_id.clone(), session_sha256: "f".repeat(64) };
    if let HostKeyOperation::Fivekeys { addr, .. } = &mut key_receipt.operation {
        *addr = "https://never-contact.invalid".into();
    }
    assert!(validate_key_result_context(&context, &launched, &target, &key_receipt).is_err());
    key_receipt.operation = HostKeyOperation::SuperReset { addr: None };
    assert!(validate_key_result_context(&context, &launched, &target, &key_receipt).is_err());
    Ok(())
}
