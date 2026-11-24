use super::*;

fn original() -> Original {
    Original {
        schema_version: HOST_KEY_SCHEMA_V,
        root: PathBuf::from("/synthetic/root"),
        profile: PathBuf::from("/synthetic/profile"),
        operation_id: "20000000-0000-4000-8000-000000000001".into(),
        key: "20000000-0000-4000-8000-000000000001".into(),
        source_identity: HostAuthIdentity::Bound {
            account_id: "synthetic-account".into(),
            session_sha256: "synthetic-session".into(),
        },
        operation: HostKeyOperation::SuperReset {
            addr: Some("https://synthetic.invalid".into()),
        },
        token: "20000000-0000-4000-8000-000000000002".into(),
        coordinator: "synthetic-coordinator".into(),
        was_serving: false,
        store_ready: false,
        sent: false,
        candidate: None,
        rolled_back: false,
    }
}

fn receipt(original: &Original, outcome: HostKeyOutcome) -> Result<HostKeyResult> {
    Ok(HostKeyResult {
        operation_id: original.operation_id.clone(),
        fingerprint: original.fingerprint()?,
        operation: original.operation.clone(),
        outcome,
        source_profile: original.profile.clone(),
        target_profile: None,
        account_id: None,
        session_sha256: None,
        identity: original.source_identity.clone(),
        required_inputs: Vec::new(),
        verification_required: false,
        super_password: None,
        challenge: None,
    })
}

#[test]
fn unsent_query_rollback_or_continuation_never_admits_effect() -> Result<()> {
    let mut original = original();
    let previous = receipt(&original, HostKeyOutcome::AwaitingInput)?;
    let fresh = Action::Reset {
        operation: original.operation.clone(),
        input: HostKeyInput::default(),
        resume: None,
    };
    assert!(require_unsent_admission(&fresh, &original, &original.source_identity).is_ok());
    for action in [
        Action::Query(&original.operation_id),
        Action::Rollback(&original.operation_id),
        Action::Reset {
            operation: original.operation.clone(),
            input: HostKeyInput::default(),
            resume: Some(&previous),
        },
    ] {
        assert!(
            require_unsent_admission(&action, &original, &original.source_identity).is_err()
        );
    }
    assert!(require_unsent_admission(&fresh, &original, &HostAuthIdentity::Absent).is_err());
    original.sent = true;
    assert!(require_unsent_admission(&fresh, &original, &original.source_identity).is_err());
    Ok(())
}

#[test]
fn journal_contains_only_original_binding_and_rejects_secret_fields() -> Result<()> {
    let original = original();
    let mut value = serde_json::to_value(&original)?;
    let fields = value
        .as_object()
        .context("synthetic journal is not an object")?;
    for forbidden in [
        "input",
        "password",
        "super_password",
        "account_secret",
        "totp",
    ] {
        assert!(!fields.contains_key(forbidden));
    }
    value["super_password"] = json!("synthetic-issued-code");
    assert!(serde_json::from_value::<Original>(value).is_err());
    assert!(canonical(&original.operation_id));
    assert!(!canonical("20000000-0000-1000-8000-000000000001"));
    assert!(!canonical("not-an-operation"));
    let mut custody = original;
    custody.operation_id = uuid::Uuid::new_v4().to_string();
    custody.key = custody.operation_id.clone();
    let kit = crate::auth_material::generate()?;
    custody.operation = HostKeyOperation::Fivekeys { addr: String::new(), user: String::new(),
        kdf_salt: kit.kdf_salt, wrapped_urk: kit.wrapped_urk, urk_nonce: kit.urk_nonce, use_super: true };
    let input = HostKeyInput { password: Some(String::new()), super_password: Some(kit.super_password),
        ..HostKeyInput::default() };
    let (candidate, private) = material::prepare(&custody, &input)?;
    // Offline recovery factors must not reserve the later registration password slot.
    assert_eq!(material::login_password(&custody, &private), None);
    assert_eq!(private.password.as_deref(), Some(""));
    let mut supplied = serde_json::from_value::<HostKeyInput>(serde_json::to_value(&private)?)?;
    supplied.password = Some("synthetic-future-login-password".to_owned());
    assert_eq!(material::login_password(&custody, &supplied), None);
    if let HostKeyOperation::Fivekeys { use_super, .. } = &mut custody.operation {
        *use_super = false;
    }
    assert_eq!(material::login_password(&custody, &private), Some(""));
    if let HostKeyOperation::Fivekeys { addr, user, use_super, .. } = &mut custody.operation {
        *use_super = true;
        *addr = "https://invalid.test".to_owned();
        *user = "synthetic-user".to_owned();
    }
    assert_eq!(material::login_password(&custody, &supplied), supplied.password.as_deref());
    if let HostKeyOperation::Fivekeys { addr, user, .. } = &mut custody.operation {
        addr.clear();
        user.clear();
    }
    custody.candidate = candidate;
    material::validate_public(&custody)?;
    #[cfg(windows)]
    {
        use respire_client_host::host_credentials::key_control as slots;
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let mut owned: Original = serde_json::from_value(serde_json::to_value(&custody)?)?;
        owned.root = root.clone();
        owned.profile = root.clone();
        owned.sent = true;
        let archive = archive_path(&root, &owned.operation_id);
        std::fs::create_dir_all(archive.parent().context("fixture archive parent missing")?)?;
        std::fs::write(archive, serde_json::to_vec(&owned)?)?;
        let material = private.material.as_ref().context("fixture material missing")?;
        let account = &material.keyring_account;
        let backend = &material.keyring_backend;
        let payload = serde_json::to_string(&private)?;
        anyhow::ensure!(slots::read(account, backend, "key-control")?.is_none()
            && slots::read(account, backend, "pass")?.is_none(), "synthetic native slots must be absent");
        let checked = (|| -> Result<()> {
            slots::persist(account, backend, "key-control", &payload)?;
            let receipt = verify_archived_custody(&root, &root, &owned.operation_id, &private)?;
            anyhow::ensure!(receipt.summary["verified"] == true, "native custody not verified");
            // A later registration may legitimately bind the profile and introduce a new password.
            std::fs::write(root.join("session.json"), br#"{"user":"synthetic-registered-user"}"#)?;
            slots::persist(account, backend, "pass", "synthetic-new-registration-password")?;
            verify_archived_custody(&root, &root, &owned.operation_id, &private)?;
            let changed = serde_json::to_string(&supplied)?;
            slots::replace(account, backend, &payload, &changed)?;
            let rejected = verify_archived_custody(&root, &root, &owned.operation_id, &private).is_err();
            slots::replace(account, backend, &changed, &payload)?;
            anyhow::ensure!(rejected, "changed native custody accepted");
            anyhow::ensure!(verify_archived_custody(&root, &root.join("foreign"), &owned.operation_id, &private).is_err(),
                "foreign custody source accepted");
            Ok(())
        })();
        let control_cleanup = slots::remove(account, backend, "key-control", &payload);
        let pass_cleanup = slots::remove(account, backend, "pass", "synthetic-new-registration-password");
        checked?;
        control_cleanup?;
        pass_cleanup?;
    }
    let public = custody.candidate.as_mut().context("host fixture candidate missing")?;
    assert!(public.get("super_password").is_none() && public.get("password").is_none());
    assert!(private.material.is_some());
    public["super_password"] = json!("forbidden-plaintext");
    assert!(material::validate_public(&custody).is_err());
    Ok(())
}

#[test]
fn receipt_requires_original_operation_source_and_fingerprint() -> Result<()> {
    let original = original();
    let mut result = receipt(&original, HostKeyOutcome::Pending)?;
    validate_receipt(&result, &original)?;
    result.fingerprint = "foreign".into();
    assert!(validate_receipt(&result, &original).is_err());
    result.fingerprint = original.fingerprint()?;
    result.operation_id = "20000000-0000-4000-8000-000000000003".into();
    assert!(validate_receipt(&result, &original).is_err());
    result.operation_id = original.operation_id.clone();
    result.source_profile = PathBuf::from("/synthetic/foreign");
    assert!(validate_receipt(&result, &original).is_err());
    Ok(())
}

#[test]
fn terminal_or_applied_key_results_require_host_completion() -> Result<()> {
    let original = original();
    for outcome in [HostKeyOutcome::Unknown, HostKeyOutcome::Pending, HostKeyOutcome::AwaitingInput] {
        assert!(!host_completion_required(&receipt(&original, outcome)?));
    }
    for outcome in [HostKeyOutcome::Committed, HostKeyOutcome::RolledBack] {
        assert!(host_completion_required(&receipt(&original, outcome)?));
    }
    let mut applied = receipt(&original, HostKeyOutcome::Pending)?;
    applied.verification_required = true;
    assert!(host_completion_required(&applied));
    Ok(())
}

#[test]
fn issued_code_is_only_valid_for_exact_committed_super_reset() -> Result<()> {
    let original = original();
    let mut result = receipt(&original, HostKeyOutcome::Pending)?;
    assert!(committed_summary(&result).is_err());
    result.super_password = Some("synthetic-issued-code".into());
    assert!(validate_receipt(&result, &original).is_err());
    result.outcome = HostKeyOutcome::Committed;
    assert!(validate_receipt(&result, &original).is_err());
    result.target_profile = Some(original.profile.clone());
    result.account_id = Some("synthetic-account".into());
    result.session_sha256 = Some("synthetic-session".into());
    validate_receipt(&result, &original)?;
    result.challenge = Some("synthetic-private-challenge".into());
    let summary = committed_summary(&result)?;
    assert_eq!(summary["operation_id"], result.operation_id);
    assert_eq!(summary["source_profile"], serde_json::to_value(&result.source_profile)?);
    assert_eq!(summary["outcome"], "committed");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["super"].as_str(), result.super_password.as_deref());
    assert!(summary.get("challenge").is_none() && summary.get("material").is_none());
    result.challenge = None;
    result.required_inputs.push("password".into());
    assert!(validate_receipt(&result, &original).is_err());
    result.required_inputs.clear();
    result.super_password = Some(String::new());
    assert!(validate_receipt(&result, &original).is_err());
    Ok(())
}

#[test]
fn awaiting_input_cannot_change_the_original_bound_identity() -> Result<()> {
    let original = original();
    let mut result = receipt(&original, HostKeyOutcome::AwaitingInput)?;
    assert!(validate_receipt(&result, &original).is_err());
    result.required_inputs.push("super_password".into());
    validate_receipt(&result, &original)?;
    result.identity = HostAuthIdentity::Absent;
    assert!(validate_receipt(&result, &original).is_err());
    Ok(())
}

#[test]
fn offline_fivekeys_identity_requires_its_original_operation_and_digest() -> Result<()> {
    let mut original = original();
    original.operation = HostKeyOperation::Fivekeys {
        addr: String::new(),
        user: String::new(),
        kdf_salt: "salt".into(),
        wrapped_urk: "wrap".into(),
        urk_nonce: "nonce".into(),
        use_super: true,
    };
    let mut result = receipt(&original, HostKeyOutcome::Pending)?;
    result.verification_required = true;
    result.session_sha256 = Some("a".repeat(64));
    result.identity = HostAuthIdentity::Unbound {
        operation_id: original.operation_id.clone(),
        session_sha256: "a".repeat(64),
    };
    assert!(target_identity_matches(&result));
    result.account_id = Some(crate::launch::hash(b""));
    assert!(!target_identity_matches(&result));
    result.account_id = None;
    result.identity = HostAuthIdentity::Absent;
    assert!(!target_identity_matches(&result));
    result.identity = HostAuthIdentity::Unbound {
        operation_id: "foreign".into(),
        session_sha256: "a".repeat(64),
    };
    assert!(!target_identity_matches(&result));
    result.identity = HostAuthIdentity::Unbound {
        operation_id: original.operation_id.clone(),
        session_sha256: "b".repeat(64),
    };
    assert!(!target_identity_matches(&result));
    result.identity = HostAuthIdentity::Unbound {
        operation_id: original.operation_id,
        session_sha256: "a".repeat(64),
    };
    if let HostKeyOperation::Fivekeys { addr, .. } = &mut result.operation {
        *addr = "https://never-contact.invalid".into();
    }
    assert!(!target_identity_matches(&result));
    result.outcome = HostKeyOutcome::Committed;
    result.verification_required = false;
    result.challenge = Some("synthetic-private-challenge".into());
    let summary = committed_summary(&result)?;
    assert_eq!(summary["operation_id"], result.operation_id);
    assert_eq!(summary["source_profile"], serde_json::to_value(&result.source_profile)?);
    assert_eq!(summary["outcome"], "committed");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["addr"], "https://never-contact.invalid");
    assert_eq!(summary["user"], "");
    for field in ["challenge", "material", "wrapped_urk", "super", "super_password"] {
        assert!(summary.get(field).is_none());
    }
    Ok(())
}
