use super::*;

fn request() -> Result<SpaceInviteRequest> {
    Ok(serde_json::from_value(
        json!({"identifier":"tablet","role":"viewer"}),
    )?)
}

#[test]
fn exact_private_commit_retires_pending_invite() -> Result<()> {
    // Use a child process so even an assertion failure cannot alter another test's profile.
    const CHILD: &str = "RESPIRE_TEST_CARD010_INVITE";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir()?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "exact_private_commit_retires_pending_invite",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RSRS_DATA_DIR", directory.path())
            .status()?;
        ensure!(status.success(), "isolated invite test failed");
        return Ok(());
    }
    let root = crate::service::main_data_dir().canonicalize()?;
    let ownership = AuthOwnership::acquire(&root, &root)?;
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|err| anyhow!(err.to_string()))?;
    let addr = format!("http://{}", server.server_addr());
    let (factor, _, salt, wrapped, nonce) = crate::auth::new_vault_v4()?;
    let original = serde_json::to_vec(&json!({"user":"fixture","auth_salt":"fixture-salt","addr":addr,"token":"synthetic",
        "vault_version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}))?;
    private_write(&root.join("session.json"), &original)?;
    let identity = HostAuthIdentity::Bound {
        account_id: digest(b"fixture"),
        session_sha256: digest(&original),
    };
    let mut params = HostSpaceParams {
        schema_version: HOST_SPACE_SCHEMA_V,
        identity: identity.clone(),
        source_identity: identity,
        operation: request()?.into_host_operation()?,
        binding: respire_protocol::ControlBinding {
            installation_root: root.clone(),
            profile: root.clone(),
            launch_id: uuid::Uuid::new_v4().to_string(),
            expected_account_id: Some(digest(b"fixture")),
        },
        invite_code: None,
        material: Some(HostSpaceMaterial {
            super_password: factor,
            keyring_account: "login-synthetic".into(),
            keyring_backend: "native".into(),
            legacy_secret: None,
        }),
    };
    let row = pending_invite(&root, &ownership._root_lock, "fixture", request()?, || true)?.context("missing pending member")?;
    assert!(pending_invite(&root, &ownership._root_lock, "foreign", request()?, || true).is_err());
    let mut replacement: Value = serde_json::from_slice(&original)?;
    replacement["token"] = json!("replacement");
    private_write(&root.join("session.json"), &serde_json::to_vec(&replacement)?)?;
    assert_eq!(owned_members(&root, &ownership._root_lock, "fixture")?, json!({"members":[]}));
    replacement.as_object_mut().unwrap().remove("auth_salt");
    private_write(&root.join("session.json"), &serde_json::to_vec(&replacement)?)?;
    assert!(pending_invite(&root, &ownership._root_lock, "fixture", request()?, || true).is_err());
    private_write(&root.join("session.json"), &original)?;
    let id = uuid::Uuid::new_v4().to_string();
    assert_ne!(id, row.member_id);
    let key = id.as_str();
    let member_id = uuid::Uuid::new_v4().to_string();
    let remote_id = member_id.clone();
    let worker = std::thread::spawn(move || -> Result<()> {
        let mut req = server
            .recv_timeout(std::time::Duration::from_secs(10))?
            .context("missing POST")?;
        assert_eq!(req.method(), &tiny_http::Method::Post);
        assert_eq!(req.url(), "/api/self/sessions");
        let body: Value = serde_json::from_reader(req.as_reader())?;
        assert_eq!(body, json!({"device_name":"member-tablet","readonly":true}));
        req.respond(tiny_http::Response::from_string(
            json!({"session_id":remote_id,"token":"synthetic-member-token"}).to_string(),
        ))?;
        assert!(server
            .recv_timeout(std::time::Duration::from_millis(200))?
            .is_none());
        Ok(())
    });
    // An orphan claim can survive interruption before the native journal save.
    let slot = slot(&root, "fixture")?.context("missing owner slot")?;
    let original_journal = read_optional(&recovery_path())?;
    let (journal, index) = admit(&ownership, &id, key, &params)?;
    let expected = claim(&journal.operations[index]);
    assert_eq!(rows(&slot.path)?[0].claim.as_ref(), Some(&expected));
    // Recreate the durable state between the claim write and the journal write.
    restore(&recovery_path(), original_journal.as_deref())?;
    let (replayed, index) = admit(&ownership, &id, key, &params)?;
    assert_eq!(claim(&replayed.operations[index]), expected);
    assert_eq!(rows(&slot.path)?[0].claim.as_ref(), Some(&expected));
    // A second restart after journal publication now takes the owned_index path.
    let mut issuance = control_space(&ownership, &id, key, &params, false)?;
    ensure!(issuance.outcome == HostSpaceOutcome::Pending, "private issuance incomplete");
    let list = || owned_members(&root, &ownership._root_lock, "fixture");
    assert_eq!(list()?["members"][0]["status"], "pending");
    params.invite_code = issuance.invite_code.take();
    assert!(control_space(&ownership, &id, "wrong-key", &params, true).is_err());
    for _ in 0..2 {
        assert!(control_space(&ownership, &id, key, &params, true)?.outcome == HostSpaceOutcome::Committed);
        assert_eq!(list()?, json!({"members":[{"member_id":member_id,"display_name":"member-tablet","role":"viewer","status":"active"}]}));
    }
    worker.join().map_err(|_| anyhow!("loopback worker panicked"))??;
    Ok(())
}
