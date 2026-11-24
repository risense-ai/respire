{
        // The completed import fixture is separate from the following missing/invalid database cases.
        std::fs::remove_file(crate::service::database_path(&root)?)?;
        // Exercise local rejection through the real prepare/login path, without remote requests.
        private_write(&root.join("session.json"), &old_bytes)?;
        let local_identity = HostAuthIdentity::Bound { account_id: digest(b"import-target"),
            session_sha256: digest(&old_bytes) };
        let old_keys = session_material_keys(&old_session, &old_alias, &old_backend)?;
        let (nonce, ciphertext) = old_keys.encrypt_content(&serde_json::to_string(&json!({
            "kind":"Context","tags":"","title":"retained","content":"local rejection fixture",
            "user":"import-target","computer":"fixture","project":"","created_at":"","updated_at":""
        }))?)?;
        let local_database = crate::service::database_path(&root)?;
        assert!(!local_database.exists());
        let (wrong_code, _, wrong_salt, wrong_wrap, wrong_nonce) = crate::auth::new_vault_v4()?;
        let wrong_vault = json!({"version":4,"kdf_salt":wrong_salt,
            "wrapped_urk":wrong_wrap,"urk_nonce":wrong_nonce});
        let mut clean_journal = load_journal()?;
        for case in ["io", "unverifiable", "mismatch"] {
            let database = rusqlite::Connection::open(&local_database)?;
            database.execute_batch("CREATE TABLE memories(ciphertext TEXT, nonce TEXT, deleted INTEGER);")?;
            database.execute("INSERT INTO memories VALUES(?1,?2,?3)",
                rusqlite::params![ciphertext, nonce, i64::from(case == "unverifiable")])?;
            drop(database);
            if case == "io" { std::fs::write(&local_database, b"invalid database")?; }
            let database_before = std::fs::read(&local_database)?;
            let local_id = uuid::Uuid::new_v4().to_string();
            let (mut local, local_index, _) = admit_account_with_intent(&ownership, &local_identity,
                &local_id, &local_id, false, "https://never-contact.invalid", "import-target",
                Some((false, false, false)))?;
            local.operations[local_index].phase = AuthPhase::Authorized;
            let account = local.operations[local_index].account.as_mut().context("local rejection receipt missing")?;
            account.authorization = Some(json!({"user":"import-target","token":"synthetic-unused-token"}));
            account.vault = Some(wrong_vault.clone());
            save_journal(&mut local)?;
            let checkpoint = std::fs::read(recovery_path())?;
            let local_params: HostAuthLoginParams = serde_json::from_value(json!({
                "schema_version":1,"binding":{"installation_root":root,"profile":root,
                    "launch_id":uuid::Uuid::new_v4().to_string(),"expected_account_id":digest(b"import-target")},
                "identity":local_identity,"addr":"https://never-contact.invalid","user":"import-target",
                "authorization":{"kind":"browser","token":"synthetic-unused-token","session_id":null},
                "super_password":wrong_code
            }))?;
            if case == "mismatch" {
                let original_material = host_material(&old_bytes)?;
                let rejected = PreparedLogin::prepare_candidate_with_material("https://never-contact.invalid",
                    &json!({"user":"import-target","token":"synthetic-unused-token"}),
                    wrong_code.clone(), wrong_vault.clone(), None, false, false, None,
                    Some(&original_material)).err().context("key mismatch accepted")?;
                let typed = rejected.downcast_ref::<LocalKeyMaterialRejected>().context("key mismatch was not typed")?;
                std::fs::write(crate::service::client_config_path(), b"{\"changed\":true}")?;
                assert!(login::reject_local_key_material(&ownership, &mut local, local_index,
                    typed, &Some(config_before.clone())).is_err());
                assert_eq!(std::fs::read(recovery_path())?, checkpoint);
                std::fs::write(crate::service::client_config_path(), &config_before)?;
                assert_eq!(control_login(&ownership, &local_id, &local_id, &local_params)?.outcome,
                    HostAuthAccountOutcome::RolledBack);
                assert!(load_journal()?.active.is_none());
                let terminal = std::fs::read(recovery_path())?;
                assert_eq!(control_account_result(&ownership, &local_identity, &local_id, &local_id)?.outcome,
                    HostAuthAccountOutcome::RolledBack);
                assert_eq!(control_login(&ownership, &local_id, &local_id, &local_params)?.outcome,
                    HostAuthAccountOutcome::RolledBack);
                assert_eq!(std::fs::read(recovery_path())?, terminal);
            } else {
                let failure = control_login(&ownership, &local_id, &local_id, &local_params)
                    .err().context("unverifiable local material was accepted")?;
                assert!(!failure.is::<LocalKeyMaterialRejected>());
                assert_eq!(control_account_result(&ownership, &local_identity, &local_id, &local_id)?.outcome,
                    HostAuthAccountOutcome::Unknown);
                assert!(load_journal()?.active.is_some());
                assert_eq!(std::fs::read(recovery_path())?, checkpoint);
            }
            assert_eq!(std::fs::read(root.join("session.json"))?, old_bytes);
            assert_eq!(std::fs::read(crate::service::client_config_path())?, config_before);
            assert_eq!(std::fs::read(&local_database)?, database_before);
            assert_eq!(crate::keystore::read_imported(&old_alias, "super", &old_backend)?, code);
            save_journal(&mut clean_journal)?;
            std::fs::remove_file(&local_database)?;
        }
        drop(ownership);
        drop(keyring);
        identity::assert_existing_recovery(&root)?;
        keys::assert_existing_recovery(&root)?;
        space::assert_existing_recovery(&root)?;
        Ok(())
}
