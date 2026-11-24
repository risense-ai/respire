#[cfg(test)]
pub(super) fn assert_existing_recovery(root: &Path) -> Result<()> {
    let ownership = AuthOwnership::acquire(root, root)?;
    let secret = crate::memory::crypto::generate_secret_key();
    let (salt, wrapped, nonce) = crate::auth::wrap_with_v4(&secret, &crate::memory::crypto::generate_key())?;
    let original = serde_json::to_vec(&json!({"user":"reset-fixture", "addr":"https://never-contact.invalid",
        "token":"synthetic-unused-token", "vault_version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}))?;
    private_write(&root.join("session.json"), &original)?;
    let identity = HostAuthIdentity::Bound { account_id: digest(b"reset-fixture"), session_sha256: digest(&original) };
    let mut params = HostKeyParams { schema_version: HOST_KEY_SCHEMA_V,
        binding: respire_protocol::ControlBinding { installation_root: root.to_owned(), profile: root.to_owned(),
            launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: Some(digest(b"reset-fixture")) },
        identity: identity.clone(), source_identity: identity, operation: HostKeyOperation::SuperReset { addr: None },
        input: Some(HostKeyInput { super_password: Some(secret.clone()), ..HostKeyInput::default() }) };
    let mut invalid = serde_json::to_value(&params)?;
    invalid["argv"] = json!(["unsupported"]);
    assert!(serde_json::from_value::<HostKeyParams>(invalid).is_err());
    let id = uuid::Uuid::new_v4().to_string();
    let (mut journal, index) = admit(&ownership, &id, &id, &params)?;
    let mut candidate: Value = serde_json::from_slice(&original)?;
    candidate["keyring_account"] = json!(journal.operations[index].alias);
    candidate["keyring_backend"] = json!("kernel");
    journal.operations[index].backend = Some("kernel".to_owned());
    checkpoint_account_candidate(&mut journal, index, &candidate)?;
    let op = operation_mut(&mut journal, index)?;
    op.target = Some(root.to_owned()); op.target_original_sha256 = Some(digest(&original));
    journal.operations[index].phase = AuthPhase::RemotePending;
    save_journal(&mut journal)?;
    let pending = std::fs::read(recovery_path())?;
    // An unrecoverable credential fails before GET/POST and cannot become success.
    assert!(control_key_result(&ownership, &id, &id, &params).is_err());
    assert!(control_key(&ownership, &id, &id, &params).is_err());
    assert_eq!(std::fs::read(recovery_path())?, pending);
    assert_eq!(std::fs::read(root.join("session.json"))?, original);
    params.operation = HostKeyOperation::SuperReset { addr: Some("https://other.invalid".into()) };
    assert!(control_key_result(&ownership, &id, &id, &params).is_err());
    params.operation = HostKeyOperation::SuperReset { addr: None };
    // Before any remote effect, a failed checkpoint is explicit and compensatable.
    journal.operations[index].phase = AuthPhase::Admitted;
    save_journal(&mut journal)?;
    let admitted = std::fs::read(recovery_path())?;
    assert!(control_key(&ownership, &id, &id, &params).is_err());
    assert_eq!(std::fs::read(recovery_path())?, admitted);
    assert!(control_key_result(&ownership, &id, "foreign-key", &params).is_err());
    assert!(control_key_rollback(ownership, &id, &id, &params)?.outcome == HostKeyOutcome::RolledBack);
    assert_eq!(std::fs::read(root.join("session.json"))?, original);
    assert!(!auth_recovery_pending()?);
    fivekeys::assert_existing_recovery(root)?;
    Ok(())
}
