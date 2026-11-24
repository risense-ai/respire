    #[test]
    fn registration_requires_host_material_before_remote_or_credential_effects() -> Result<()> {
        for material in [None, Some(respire_protocol::HostAuthKeygenMaterial {
            kdf_salt: "invalid".into(), wrapped_urk: "invalid".into(), urk_nonce: "invalid".into(),
            super_password: "synthetic-host-super".into(),
            keyring_account: format!("login-{}", uuid::Uuid::new_v4().simple()), keyring_backend: "native".into(),
        })] {
            let isolated = crate::test_lock::Isolate::new()?;
            let _keyring = crate::migration::memory_keyring_for_test();
            let root = isolated.path().canonicalize()?;
            let ownership = AuthOwnership::acquire(&root, &root)?;
            let id = uuid::Uuid::new_v4().to_string();
            let params = HostAuthRegisterParams {
                schema_version: 1, binding: respire_protocol::ControlBinding {
                    installation_root: root.clone(), profile: root.clone(),
                    launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
                }, identity: HostAuthIdentity::Absent, addr: "https://never-contact.invalid".into(),
                user: "synthetic-register".into(), password: "synthetic-host-password".into(),
                super_password: None, material,
            };
            assert!(control_register(&ownership, &id, &id, &params).is_err());
            let journal = load_journal()?;
            assert!(journal.operations[0].phase == AuthPhase::Admitted);
            assert!(journal.operations[0].backend.is_none());
            assert!(journal.active.as_ref().is_some_and(|active|
                active.backends.is_empty() && active.pass_backends.is_empty()));
            assert!(!root.join("session.json").exists());
            let durable = String::from_utf8(std::fs::read(recovery_path())?)?;
            assert!(!durable.contains("synthetic-host-super") && !durable.contains("synthetic-host-password"));
            assert_eq!(control_register(&ownership, &id, &id, &params)?.outcome, HostAuthAccountOutcome::Unknown);
        }
        Ok(())
    }

    #[test]
    fn registration_commits_and_rolls_back_without_runtime_credential_custody() -> Result<()> {
        let isolated = crate::test_lock::Isolate::new()?;
        let _keyring = crate::migration::memory_keyring_for_test();
        let root = isolated.path().canonicalize()?;
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let (secret, _, salt, wrapped, nonce) = crate::auth::new_vault_v4()?;
        let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let addr = format!("http://{}", server.server_addr());
        let worker = std::thread::spawn(move || -> std::result::Result<tiny_http::Server, String> {
            for register in [false, true] {
                let request = server.recv_timeout(std::time::Duration::from_secs(20))
                    .map_err(|error| error.to_string())?.ok_or("registration fixture received no request")?;
                let body = if register {
                    assert_eq!(request.method(), &tiny_http::Method::Post);
                    assert_eq!(request.url(), "/register");
                    json!({"token":"synthetic-register-token","session_id":"synthetic-register-session"})
                } else {
                    assert_eq!(request.method(), &tiny_http::Method::Get);
                    assert!(request.url().starts_with("/auth/salt?"));
                    json!({"salt":"11".repeat(16)})
                };
                request.respond(tiny_http::Response::from_string(body.to_string()))
                    .map_err(|error| error.to_string())?;
            }
            Ok(server)
        });
        let params = HostAuthRegisterParams {
            schema_version: 1, binding: respire_protocol::ControlBinding {
                installation_root: root.clone(), profile: root.clone(),
                launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
            }, identity: HostAuthIdentity::Absent, addr, user: "synthetic-register".into(),
            password: "synthetic-host-password".into(), super_password: None,
            material: Some(respire_protocol::HostAuthKeygenMaterial {
                kdf_salt: salt, wrapped_urk: wrapped, urk_nonce: nonce, super_password: secret.clone(),
                keyring_account: alias.clone(), keyring_backend: "native".into(),
            }),
        };
        let registered = control_register(&ownership, &id, &id, &params);
        let server = worker.join().map_err(|_| anyhow::anyhow!("registration fixture panicked"))?
            .map_err(|error| anyhow::anyhow!(error))?;
        let registered = registered?;
        assert_eq!(registered.outcome, HostAuthAccountOutcome::AwaitingVerification);
        assert!(registered.super_password.is_none());
        assert!(crate::keystore::read_imported(&alias, "super", "native").is_err());
        assert!(crate::keystore::read_imported(&alias, "pass", "native").is_err());
        let bytes = std::fs::read(root.join("session.json"))?;
        let session: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(session["keyring_account"], alias);
        assert_eq!(session["wrapped_urk"], params.material.as_ref().context("host material missing")?.wrapped_urk);
        let identity = HostAuthIdentity::Bound { account_id: digest(params.user.as_bytes()), session_sha256: digest(&bytes) };
        assert!(is_registration_receipt(&root, &root, &id, &id)?);
        assert!(control_account_result(&ownership, &identity, &id, &id)?.super_password.is_none());
        let journal = load_journal()?;
        assert!(journal.active.as_ref().is_some_and(|active|
            active.backends.is_empty() && active.pass_backends.is_empty()));
        let durable = String::from_utf8(std::fs::read(recovery_path())?)?;
        assert!(!durable.contains(&secret) && !durable.contains("synthetic-host-password"));
        assert_eq!(control_account_rollback(ownership, &identity, &id, &id)?.outcome, HostAuthAccountOutcome::RolledBack);
        assert!(!root.join("session.json").exists());
        assert!(server.recv_timeout(std::time::Duration::from_millis(50))?.is_none(),
            "registration result or rollback repeated remote authentication");
        Ok(())
    }

    #[test]
    fn prepared_auth_candidate_requires_its_original_host_material() -> Result<()> {
        let isolated = crate::test_lock::Isolate::new()?;
        let keyring = crate::migration::memory_keyring_for_test();
        let root = isolated.path().canonicalize()?;
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let candidate = serde_json::to_vec(&json!({"vault_version":4,"kdf_salt":"invalid",
            "wrapped_urk":"invalid","urk_nonce":"invalid","keyring_account":alias,
            "keyring_backend":"unavailable-test-backend","auth_operation_id":id}))?;
        let identity = HostAuthIdentity::Unbound { operation_id: id.clone(), session_sha256: digest(&candidate) };
        let mut journal = Journal { schema_version: 2, active: Some(Recovery {
            directory: root.clone(), original: None, config: None, alias: alias.clone(),
            backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(), directory_existed: true, operation_id: Some(id.clone()), owns_config: false, ..Recovery::default()
        }), operations: vec![AuthRecord {
            operation_id: id.clone(), idempotency_key: id, fingerprint: keygen_fingerprint(&root, &root, false)?,
            root: root.clone(), profile: root.clone(), original_sha256: None,
            candidate: Some(candidate.clone()), candidate_backup: None, candidate_sha256: Some(digest(&candidate)), restored: None,
            alias, backend: Some("unavailable-test-backend".into()), phase: AuthPhase::Prepared, account: None, identity: None, key_operation: None, space_operation: None, profile_remove: None,
        }] };
        save_journal(&mut journal)?;
        std::fs::write(root.join("session.json"), &candidate)?;
        assert!(ownership.recover(&identity).is_err());
        assert!(load_journal()?.active.is_some());
        assert!(verify_auth_identity(&root, &root, &identity).is_err());
        assert_eq!(std::fs::read(root.join("session.json"))?, candidate);
        let (super_password, _, kdf_salt, wrapped_urk, urk_nonce) = crate::auth::new_vault_v4()?;
        let material = respire_protocol::HostAuthKeygenMaterial { super_password, kdf_salt,
            wrapped_urk, urk_nonce, keyring_account: journal.operations[0].alias.clone(),
            keyring_backend: "native".into() };
        let candidate = serde_json::to_vec(&json!({"vault_version":4,"kdf_salt":material.kdf_salt,
            "wrapped_urk":material.wrapped_urk,"urk_nonce":material.urk_nonce,
            "keyring_account":material.keyring_account,"keyring_backend":material.keyring_backend,
            "auth_operation_id":journal.operations[0].operation_id}))?;
        let record = &mut journal.operations[0];
        record.candidate_sha256 = Some(digest(&candidate));
        record.candidate = Some(candidate.clone());
        record.backend = Some(material.keyring_backend.clone());
        let identity = HostAuthIdentity::Unbound { operation_id: record.operation_id.clone(),
            session_sha256: digest(&candidate) };
        save_journal(&mut journal)?;
        std::fs::write(root.join("session.json"), &candidate)?;
        let original_journal = std::fs::read(recovery_path())?;
        assert!(ownership.recover_keygen(&identity, None).is_err());
        assert_eq!(std::fs::read(recovery_path())?, original_journal);
        let mut invalid = material.clone();
        invalid.super_password = "wrong-host-material".into();
        assert!(ownership.recover_keygen(&identity, Some(&invalid)).is_err());
        assert_eq!(std::fs::read(recovery_path())?, original_journal);
        ownership.recover_keygen(&identity, Some(&material))?;
        let journal = load_journal()?;
        assert!(journal.active.is_none());
        assert_eq!(auth_result(&journal.operations[0], Some(&material))?.outcome, HostAuthOutcome::Committed);
        verify_locked_auth_identity(&root, &root, &identity)?;
        verify_auth_identity(&root, &root, &identity)?;
        assert_eq!(std::fs::read(root.join("session.json"))?, candidate);
        Ok(())
    }
