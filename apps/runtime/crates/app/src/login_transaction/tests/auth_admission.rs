    #[test]
    fn auth_admission_recovers_without_regenerating_and_retains_receipts() -> Result<()> {
        assert!(require_persistent_auth_backend("kernel").is_err());
        require_persistent_auth_backend("native")?;
        require_persistent_auth_backend("secret_service")?;
        let isolated = crate::test_lock::Isolate::new()?;
        let keyring = crate::migration::memory_keyring_for_test();
        let root = isolated.path().canonicalize()?;
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let previous_config = br#"{"custom":"before-auth"}"#;
        let current_config = br#"{"custom":"changed-outside-auth"}"#;
        let mut journal = Journal { schema_version: 2, active: Some(Recovery {
            directory: root.clone(), original: None, config: Some(previous_config.to_vec()), alias: alias.clone(),
            backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(), directory_existed: true, operation_id: Some(id.clone()), owns_config: false, ..Recovery::default()
        }), operations: vec![AuthRecord {
            operation_id: id.clone(), idempotency_key: id.clone(), fingerprint: keygen_fingerprint(&root, &root, false)?,
            root: root.clone(), profile: root.clone(), original_sha256: None, candidate: None, candidate_backup: None, candidate_sha256: None, restored: None,
            alias, backend: None, phase: AuthPhase::Admitted, account: None, identity: None, key_operation: None, space_operation: None, profile_remove: None,
        }] };
        save_journal(&mut journal)?;
        std::fs::write(crate::service::client_config_path(), current_config)?;
        ownership.recover(&HostAuthIdentity::Absent)?;
        ownership.recover(&HostAuthIdentity::Absent)?;
        assert_eq!(std::fs::read(crate::service::client_config_path())?, current_config);
        let result = control_keygen(&ownership, &HostAuthIdentity::Absent, &id, &id, false, true, None)?;
        assert_eq!(result.outcome, HostAuthOutcome::RolledBack);
        assert!(result.super_password.is_none());
        assert!(!root.join("session.json").exists());
        assert!(control_keygen(&ownership, &HostAuthIdentity::Absent, &id, &id, true, false, None).is_err());
        assert!(control_keygen(&ownership, &HostAuthIdentity::Absent, &uuid::Uuid::new_v4().to_string(), &id, false, false, None).is_err());
        journal = load_journal()?;
        assert!(journal.active.is_none());
        assert_eq!(journal.operations.len(), 1);

        // Decode an actual legacy record, then preserve terminal auth receipts across login completion.
        let active = Recovery { directory: root.clone(), original: None, config: None,
            alias: format!("login-{}", uuid::Uuid::new_v4().simple()), backends: Vec::new(),
            pass_backends: Vec::new(), totp_backends: Vec::new(), directory_existed: true, operation_id: None, owns_config: false, ..Recovery::default() };
        let mut legacy = serde_json::to_value(&active)?;
        legacy.as_object_mut().context("legacy recovery must be an object")?.remove("operation_id");
        private_write(&recovery_path(), &serde_json::to_vec(&legacy)?)?;
        assert!(load_journal()?.active.is_some());
        journal.active = Some(active);
        save_journal(&mut journal)?;
        drop(ownership);
        recover_interrupted()?;
        assert!(!crate::service::client_config_path().exists(), "legacy login did not restore its absent configuration");
        assert!(!recovery_pending());
        assert_eq!(load_journal()?.operations.len(), 1);

        let ownership = AuthOwnership::acquire(&root, &root)?;
        let account_id = uuid::Uuid::new_v4().to_string();
        let (mut account_journal, account_index, fresh) = admit_account(&ownership, &HostAuthIdentity::Absent,
            &account_id, &account_id, true, "https://never-contact.invalid", "receipt-user")?;
        assert!(fresh);
        assert!(!admit_account(&ownership, &HostAuthIdentity::Absent, &account_id, &account_id,
            true, "https://never-contact.invalid", "receipt-user")?.2);
        assert!(admit_account(&ownership, &HostAuthIdentity::Absent, &account_id, &account_id,
            true, "https://other.invalid", "receipt-user").is_err());
        account_journal.operations[account_index].phase = AuthPhase::RemotePending;
        save_journal(&mut account_journal)?;
        let params = HostAuthRegisterParams {
            schema_version: 1, binding: respire_protocol::ControlBinding {
                installation_root: root.clone(), profile: root.clone(), launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
            }, identity: HostAuthIdentity::Absent, addr: "https://never-contact.invalid".into(), user: "receipt-user".into(),
            password: "must-not-be-durable".into(), super_password: Some("must-not-be-durable-either".into()), material: None,
        };
        // A replay must return before either network access or credential creation.
        assert_eq!(control_register(&ownership, &account_id, &account_id, &params)?.outcome, HostAuthAccountOutcome::Unknown);
        ownership.recover(&HostAuthIdentity::Absent)?;
        drop(ownership);
        assert!(recover_interrupted().is_err(), "account admission entered keygen recovery");
        let ownership = AuthOwnership::acquire(&root, &root)?;
        assert_eq!(control_account_result(&ownership, &HostAuthIdentity::Absent, &account_id, &account_id)?.outcome,
            HostAuthAccountOutcome::Unknown);
        assert!(control_account_result(&ownership, &HostAuthIdentity::Absent, &account_id, "wrong-key").is_err());
        assert!(!String::from_utf8(std::fs::read(recovery_path())?)?.contains("must-not-be-durable"));
        account_journal.operations[account_index].phase = AuthPhase::Admitted;
        let owned_config = br#"{"custom":"account-before"}"#;
        let active = account_journal.active.as_mut().context("account recovery is missing")?;
        active.owns_config = true;
        active.config = Some(owned_config.to_vec());
        save_journal(&mut account_journal)?;
        std::fs::write(crate::service::client_config_path(), serde_json::to_vec(&json!({"custom":"account-before","data_dir":root}))?)?;
        assert_eq!(control_account_rollback(ownership, &HostAuthIdentity::Absent, &account_id, &account_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);
        assert_eq!(std::fs::read(crate::service::client_config_path())?, owned_config);
        assert!(!recovery_pending());
        assert_eq!(load_journal()?.operations.len(), 2);

        // A serving candidate must pass receipt checks before the host supplies credentials.
        {
            let durable_before = std::fs::read(recovery_path())?;
            let mut locked_journal: Journal = serde_json::from_slice(&serde_json::to_vec(&account_journal)?)?;
            let record = &mut locked_journal.operations[account_index];
            let account = record.account.as_mut().context("locked account receipt missing")?;
            account.target = Some(root.clone());
            let candidate = serde_json::to_vec(&json!({
                "user": account.user, "addr": account.addr, "keyring_account": record.alias
            }))?;
            record.phase = AuthPhase::AwaitingVerification;
            record.backend = Some("secret_service".to_owned());
            record.candidate_sha256 = Some(digest(&candidate));
            record.candidate = Some(candidate.clone());
            let active = locked_journal.active.as_mut().context("locked account recovery missing")?;
            active.directory = root.clone();
            active.alias = locked_journal.operations[account_index].alias.clone();
            private_write(&root.join("session.json"), &candidate)?;
            save_journal(&mut locked_journal)?;
            assert!(!auth_recovery_pending()?, "locked account startup required host credentials");
            verify_account_candidate(&locked_journal.operations[account_index])?;
            assert!(account_result(&locked_journal.operations[account_index])?.super_password.is_none());
            locked_journal.operations[account_index].account.as_mut()
                .context("locked account receipt missing")?.register = false;
            assert!(account_material_keys(&locked_journal.operations[account_index], true, None).is_err(),
                "login candidate verification must still require explicit account material");
            private_write(&root.join("session.json"), br#"{"user":"changed-owner"}"#)?;
            assert!(auth_recovery_pending().is_err(), "startup accepted a changed account candidate");
            std::fs::remove_file(root.join("session.json"))?;
            private_write(&recovery_path(), &durable_before)?;
        }

        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            let mut ordinary_root = match root.components().next() {
                Some(Component::Prefix(prefix)) => match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
                    _ => anyhow::bail!("canonical account fixture requires a local drive"),
                },
                _ => anyhow::bail!("canonical account fixture requires an absolute root"),
            };
            ordinary_root.extend(root.components().skip(2).map(|part| part.as_os_str()));
            assert_eq!(ordinary_root.canonicalize()?, root);
            let ordinary_target = ordinary_root.join("canonical-account-target");
            std::fs::create_dir(&ordinary_target)?;
            let canonical_target = ordinary_target.canonicalize()?;
            assert_ne!(ordinary_target, canonical_target);
            let candidate = serde_json::to_vec(&json!({"user":"receipt-user","addr":"https://never-contact.invalid"}))?;
            private_write(&ordinary_target.join("session.json"), &candidate)?;
            let durable_before = std::fs::read(recovery_path())?;
            let mut target_journal = load_journal()?;
            let record = &mut target_journal.operations[account_index];
            record.phase = AuthPhase::AwaitingVerification;
            record.candidate = Some(candidate.clone());
            record.candidate_sha256 = Some(digest(&candidate));
            record.account.as_mut().context("canonical account receipt missing")?.target = Some(ordinary_target.clone());
            assert_eq!(owned_account_index(&target_journal, &root, &canonical_target, &account_id, &account_id)?, account_index);
            target_journal.operations[account_index].account.as_mut().context("canonical account receipt missing")?.target = Some(canonical_target.clone());
            assert_eq!(owned_account_index(&target_journal, &root, &canonical_target, &account_id, &account_id)?, account_index);
            assert!(owned_account_index(&target_journal, &root, &canonical_target, &account_id, "wrong-key").is_err());
            private_write(&ordinary_target.join("session.json"), br#"{"user":"changed-owner"}"#)?;
            assert!(owned_account_index(&target_journal, &root, &canonical_target, &account_id, &account_id).is_err());
            assert_eq!(std::fs::read(recovery_path())?, durable_before);
            assert_eq!(std::fs::read(crate::service::client_config_path())?, owned_config);
            assert!(!root.join("session.json").exists());
            std::fs::remove_file(ordinary_target.join("session.json"))?;
            std::fs::remove_dir(ordinary_target)?;
        }

        // Login intents are admission identity, not supplemental credentials.
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let login_id = uuid::Uuid::new_v4().to_string();
        let (mut login_journal, login_index, _) = admit_account_with_intent(&ownership,
            &HostAuthIdentity::Absent, &login_id, &login_id, false,
            "https://never-contact.invalid", "receipt-user", Some((false, false, false)))?;
        let admitted_fingerprint = login_journal.operations[login_index].fingerprint.clone();
        let admitted_bytes = std::fs::read(recovery_path())?;
        for changed in [(true, false, false), (false, true, false), (true, true, false), (false, false, true)] {
            assert!(admit_account_with_intent(&ownership, &HostAuthIdentity::Absent,
                &login_id, &login_id, false, "https://never-contact.invalid", "receipt-user", Some(changed)).is_err());
            assert_eq!(std::fs::read(recovery_path())?, admitted_bytes);
        }
        let mut login_params: HostAuthLoginParams = serde_json::from_value(json!({
            "schema_version":1,"binding":params.binding,"identity":{"kind":"absent"},
            "addr":"https://never-contact.invalid","user":"receipt-user",
            "authorization":{"kind":"browser","token":"synthetic-unused-token"},"super_password":"",
            "continuation":{"legacy_password":"must-not-be-durable","account_secret":"must-not-be-durable",
                "source_secret":"must-not-be-durable","migrate_local":false,"migrate_vault":false}
        }))?;
        let challenge = serde_json::from_value(json!({"ticket":"synthetic-private-ticket","auth_salt":"synthetic-salt"}))?;
        let payload = login::encode_challenge(&login_journal.operations[login_index], challenge)?;
        let challenge_sha = digest(payload.as_bytes());
        login::verify_challenge(&login_journal.operations[login_index], &challenge_sha, &payload)?;
        login_journal.operations[login_index].fingerprint = "different-intent".into();
        assert!(login::verify_challenge(&login_journal.operations[login_index], &challenge_sha, &payload).is_err());
        login_journal.operations[login_index].fingerprint = admitted_fingerprint.clone();
        login_journal.operations[login_index].operation_id = uuid::Uuid::new_v4().to_string();
        assert!(login::verify_challenge(&login_journal.operations[login_index], &challenge_sha, &payload).is_err());
        login_journal.operations[login_index].operation_id = login_id.clone();
        assert!(login::verify_challenge(&login_journal.operations[login_index], &digest(b"changed"), &payload).is_err());
        let mut input_record = serde_json::to_value(&login_journal.operations[login_index])?;
        input_record["account"]["login"]["challenge"] = json!({"sha256":challenge_sha});
        login_journal.operations[login_index] = serde_json::from_value(input_record)?;
        login_journal.active.as_mut().context("challenge recovery missing")?.totp_backends.push("native".into());
        login_journal.operations[login_index].phase = AuthPhase::LoginInput;
        login_params.authorization = HostAuthAuthorization::Password { password: String::new(), totp: None };
        save_journal(&mut login_journal)?;
        let missing_totp = control_login(&ownership, &login_id, &login_id, &login_params)?;
        assert_eq!(missing_totp.outcome, HostAuthAccountOutcome::AwaitingInput);
        assert_eq!(missing_totp.login.context("TOTP input status missing")?.required_inputs, ["totp"]);
        login_params.authorization = HostAuthAuthorization::Password { password: String::new(), totp: Some("invalid".into()) };
        assert_eq!(control_login(&ownership, &login_id, &login_id, &login_params)?.outcome, HostAuthAccountOutcome::AwaitingInput);
        assert!(!String::from_utf8(std::fs::read(recovery_path())?)?.contains("synthetic-private-ticket"));
        login_journal.active.as_mut().context("challenge recovery missing")?.totp_backends.clear();
        login_journal.operations[login_index].phase = AuthPhase::RemotePending;
        save_journal(&mut login_journal)?;
        // An uncertain authorization returns its receipt before any network or replacement factor.
        assert_eq!(control_login(&ownership, &login_id, &login_id, &login_params)?.outcome, HostAuthAccountOutcome::Unknown);
        login_journal.operations[login_index].phase = AuthPhase::Authorized;
        let account = login_journal.operations[login_index].account.as_mut().context("login test receipt missing")?;
        account.authorization = Some(json!({"user":"receipt-user","token":"synthetic-retained-token"}));
        account.vault = Some(json!({"version":4}));
        save_journal(&mut login_journal)?;
        let supplemental = control_login(&ownership, &login_id, &login_id, &login_params)?;
        assert_eq!(supplemental.outcome, HostAuthAccountOutcome::AwaitingInput);
        assert_eq!(supplemental.login.context("input status missing")?.required_inputs, ["super_password"]);
        assert_eq!(load_journal()?.operations[login_index].fingerprint, admitted_fingerprint);
        assert!(!String::from_utf8(std::fs::read(recovery_path())?)?.contains("must-not-be-durable"));
        // v1 rejects presence, including an explicitly empty super parameter.
        let mut v1_journal = load_journal()?;
        v1_journal.operations[login_index].account.as_mut().context("v1 receipt missing")?.vault = Some(json!({"version":1}));
        save_journal(&mut v1_journal)?;
        let before_explicit_super = std::fs::read(recovery_path())?;
        assert_eq!(login_params.super_password.as_deref(), Some(""));
        assert!(control_login(&ownership, &login_id, &login_id, &login_params).is_err());
        assert_eq!(std::fs::read(recovery_path())?, before_explicit_super);
        login_params.super_password = None;
        let input = login_params.continuation.as_mut().context("v1 input missing")?;
        input.legacy_password = None;
        input.account_secret = None;
        let v1_input = control_login(&ownership, &login_id, &login_id, &login_params)?;
        assert_eq!(v1_input.outcome, HostAuthAccountOutcome::AwaitingInput);
        assert!(v1_input.login.context("v1 input status missing")?.required_inputs.contains(&"account_secret".to_owned()));
        let omitted = serde_json::to_value(&login_params)?;
        assert!(omitted.get("super_password").is_none());
        assert!(serde_json::from_value::<HostAuthLoginParams>(omitted)?.super_password.is_none());
        login_params.continuation.as_mut().context("continuation missing")?.migrate_local = true;
        assert!(control_login(&ownership, &login_id, &login_id, &login_params).is_err());
        let mut merge = login::LoginProgress::new(false, false, true);
        assert!(merge.confirm_merge(&root, true).is_err());
        merge.offer_merge(&root)?;
        merge.confirm_merge(&root, false)?;
        let confirmed = serde_json::to_vec(&merge)?;
        assert!(merge.confirm_merge(&root, true).is_err());
        assert_eq!(serde_json::to_vec(&merge)?, confirmed);
        assert!(merge.offer_merge(&root.join("another-target")).is_err());
        assert_eq!(serde_json::to_vec(&merge)?, confirmed);
        // Migration publication must retain the password confirmed before host custody acknowledgement.
        let migration_password = "synthetic-original-migration-password";
        let migration_salt = crate::memory::crypto::derive_auth_salt("receipt-user")?;
        let authentication = json!({"salt":migration_salt,
            "pass_hash":crate::memory::crypto::derive_pass_hash(migration_password, &migration_salt)?});
        let mut migration_value = serde_json::to_value(&login_journal)?;
        migration_value["operations"][login_index]["account"]["login"]["migrate_vault"] = json!(true);
        migration_value["operations"][login_index]["account"]["login"]["migration_salt"] = json!(migration_salt);
        let mut migration_journal: Journal = serde_json::from_value(migration_value)?;
        let migration_session = json!({"user":"receipt-user","auth_salt":migration_salt});
        let candidate_sha256 = digest(&serde_json::to_vec_pretty(&migration_session)?);
        login::bind_migration_auth(&mut migration_journal.operations[login_index], &candidate_sha256,
            Some(&authentication), false)?;
        checkpoint_account_candidate(&mut migration_journal, login_index, &migration_session)?;
        save_journal(&mut migration_journal)?;
        let mut migration_journal = load_journal()?;
        login::bind_migration_auth(&mut migration_journal.operations[login_index], &candidate_sha256,
            Some(&authentication), true)?;
        let confirmed_migration = serde_json::to_vec(&migration_journal)?;
        let durable_migration = std::fs::read(recovery_path())?;
        let durable_text = String::from_utf8(durable_migration.clone())?;
        assert!(!durable_text.contains(migration_password) && !durable_text.contains("pass_hash")
            && !durable_text.contains(authentication["pass_hash"].as_str().context("fixture authentication hash missing")?));
        let mut changed_authentication = authentication.clone();
        changed_authentication["pass_hash"] = json!(crate::memory::crypto::derive_pass_hash("synthetic-replacement-password", &migration_salt)?);
        for acknowledging in [false, true] {
            let rejected = login::bind_migration_auth(&mut migration_journal.operations[login_index], &candidate_sha256,
                Some(&changed_authentication), acknowledging).err().context("migration password replacement accepted")?;
            assert!(rejected.to_string().contains("differs from the original login candidate"));
            assert_eq!(serde_json::to_vec(&migration_journal)?, confirmed_migration);
        }
        assert!(login::bind_migration_auth(&mut migration_journal.operations[login_index], &digest(b"different-candidate"),
            Some(&authentication), true).is_err());
        let rejected = login::finish_migration(&mut migration_journal, login_index, Some("synthetic-replacement-password"))
            .err().context("completion accepted a password outside confirmed host custody")?;
        assert_eq!(rejected.to_string(), "migration password differs from the confirmed login material; no publication attempted");
        assert_eq!(serde_json::to_vec(&migration_journal)?, confirmed_migration);
        assert_eq!(std::fs::read(recovery_path())?, durable_migration);
        // Old admitted records cannot bootstrap evidence from a completion request.
        let mut missing_evidence = serde_json::to_value(&migration_journal)?;
        missing_evidence["operations"][login_index]["account"]["login"].as_object_mut()
            .context("migration fixture progress missing")?.remove("migration_auth_sha256");
        let mut missing_evidence: Journal = serde_json::from_value(missing_evidence)?;
        assert!(login::bind_migration_auth(&mut missing_evidence.operations[login_index], &candidate_sha256,
            Some(&authentication), true).is_err());
        let rejected = login::finish_migration(&mut missing_evidence, login_index, Some(migration_password))
            .err().context("completion accepted missing original authentication evidence")?;
        assert_eq!(rejected.to_string(), "host_recovery_required: original migration authentication evidence is missing");
        assert_eq!(std::fs::read(recovery_path())?, durable_migration);
        // Unknown authorization cannot restart password/browser authorization.
        login_params.continuation.as_mut().context("continuation missing")?.migrate_local = false;
        login_journal.operations[login_index].phase = AuthPhase::RemoteUnknown;
        save_journal(&mut login_journal)?;
        assert_eq!(control_login(&ownership, &login_id, &login_id, &login_params)?.outcome,
            HostAuthAccountOutcome::Unknown);
        login_journal.operations[login_index].phase = AuthPhase::Authorized;
        save_journal(&mut login_journal)?;
        assert_eq!(control_account_rollback(ownership, &HostAuthIdentity::Absent, &login_id, &login_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);

        // Pre-continuation receipts keep their original fingerprint and result-only behavior.
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let legacy_id = uuid::Uuid::new_v4().to_string();
        let (legacy_journal, _, _) = admit_account(&ownership, &HostAuthIdentity::Absent,
            &legacy_id, &legacy_id, false, "https://never-contact.invalid", "receipt-user")?;
        let legacy_bytes = std::fs::read(recovery_path())?;
        let legacy = control_login(&ownership, &legacy_id, &legacy_id, &login_params)?;
        assert_eq!(legacy.outcome, HostAuthAccountOutcome::Unknown);
        assert!(legacy.login.is_none());
        assert_eq!(std::fs::read(recovery_path())?, legacy_bytes);
        assert_eq!(load_journal()?.operations.len(), legacy_journal.operations.len());
        assert_eq!(control_account_rollback(ownership, &HostAuthIdentity::Absent, &legacy_id, &legacy_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);

        keygen_overwrite_guard(&root, None, false)?;
        let database_path = crate::service::database_path(&root)?;
        assert!(!database_path.exists(), "overwrite guard initialized a store");
        let database = rusqlite::Connection::open(&database_path)?;
        database.execute_batch("CREATE TABLE memories(deleted INTEGER); INSERT INTO memories VALUES(0);")?;
        drop(database);
        assert!(keygen_overwrite_guard(&root, None, false).is_err());
        let original_database = std::fs::read(&database_path)?;
        assert!(!root.join("session.json").exists());
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let refusal_id = uuid::Uuid::new_v4().to_string();
        let previous_receipts = load_journal()?.operations.len();
        let material = keygen_existing_protocol::host_material()?;
        let refused = control_keygen(&ownership, &HostAuthIdentity::Absent, &refusal_id, &refusal_id, false, false, Some(&material))?;
        assert_eq!(refused.outcome, HostAuthOutcome::RolledBack);
        assert!(refused.super_password.is_none());
        assert!(refused.session_sha256.is_none());
        let refused_journal = load_journal()?;
        assert!(refused_journal.active.is_none());
        assert_eq!(refused_journal.operations.len(), previous_receipts + 1);
        let record = &refused_journal.operations[previous_receipts];
        assert!(record.phase == AuthPhase::RolledBack);
        assert!(record.candidate.is_none() && record.candidate_sha256.is_none() && record.backend.is_none());
        let receipt_bytes = std::fs::read(recovery_path())?;
        assert_eq!(control_keygen(&ownership, &HostAuthIdentity::Absent, &refusal_id, &refusal_id, false, true, None)?.outcome,
            HostAuthOutcome::RolledBack);
        assert_eq!(control_keygen(&ownership, &HostAuthIdentity::Absent, &refusal_id, &refusal_id, false, false, Some(&material))?.outcome,
            HostAuthOutcome::RolledBack);
        assert!(control_keygen(&ownership, &HostAuthIdentity::Absent, &refusal_id, &refusal_id, true, false, None).is_err());
        assert_eq!(std::fs::read(recovery_path())?, receipt_bytes);
        assert_eq!(std::fs::read(&database_path)?, original_database);
        assert!(!root.join("session.json").exists());
        drop(ownership);
        keygen_overwrite_guard(&root, None, true)?;
        std::fs::remove_file(&database_path)?;
        std::fs::write(&database_path, b"invalid database")?;
        assert!(keygen_overwrite_guard(&root, None, false).is_err());
        std::fs::remove_file(database_path)?;

        std::fs::write(root.join("session.json"), b"{")?;
        assert!(verify_auth_identity(&root, &root, &HostAuthIdentity::Absent).is_err());
        std::fs::write(root.join("session.json"), b"{}")?;
        assert!(verify_auth_identity(&root, &root, &HostAuthIdentity::Absent).is_err());
        assert!(verify_auth_identity(&root, &root, &HostAuthIdentity::Unbound {
            operation_id: id, session_sha256: digest(b"{}"),
        }).is_err());

        // Missing source factors remain on the same admitted login without opening its store.
        let source_user = format!("source-{}", uuid::Uuid::new_v4().simple());
        let source_password = uuid::Uuid::new_v4().to_string();
        let source_secret = crate::memory::crypto::generate_account_secret();
        let source_salt = crate::memory::crypto::random_hex(16);
        let source_kek = crate::memory::crypto::derive_kek(&source_password, &source_secret, &source_salt)?;
        let (source_nonce, source_wrap) = crate::memory::crypto::wrap_key(&crate::memory::crypto::generate_key(), &source_kek)?;
        let source_alias = format!("legacy-host-source-{}", uuid::Uuid::new_v4().simple());
        // The host retains the account secret, while import still requires the user's original password.
        let source_legacy_backend = crate::keystore::import_credential(&source_alias, "legacy-secret", &source_secret)?;
        crate::keystore::import_credential(&source_alias, "pass", &source_password)?;
        let source_bytes = serde_json::to_vec(&json!({"user":source_user,"addr":"https://source.invalid",
            "vault_version":1,"keyring_account":source_alias,"keyring_backend":source_legacy_backend,
            "legacy_secret_backend":source_legacy_backend,
            "kdf_salt":source_salt,"wrapped_urk":source_wrap,"urk_nonce":source_nonce}))?;
        std::fs::write(root.join("session.json"), &source_bytes)?;
        let source_store = crate::transport::local::LocalStore::open(&crate::service::database_path(&root)?)?;
        source_store.meta_set("source_fixture", "existing-library")?;
        drop(source_store);
        let source_database_before = std::fs::read(crate::service::database_path(&root)?)?;
        let source_identity = HostAuthIdentity::Bound { account_id: digest(source_user.as_bytes()), session_sha256: digest(&source_bytes) };
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let same_id = uuid::Uuid::new_v4().to_string();
        let (mut same_account, same_index, _) = admit_account_with_intent(&ownership, &source_identity,
            &same_id, &same_id, false, "https://source.invalid", &source_user, Some((true, false, false)))?;
        let (same_super, _, same_salt, same_wrap, same_nonce) = crate::auth::new_vault_v4()?;
        same_account.operations[same_index].phase = AuthPhase::Authorized;
        let same = same_account.operations[same_index].account.as_mut().context("same account receipt missing")?;
        same.authorization = Some(json!({"user":source_user,"token":"synthetic-unused-token"}));
        same.vault = Some(json!({"version":4,"kdf_salt":same_salt,"wrapped_urk":same_wrap,"urk_nonce":same_nonce}));
        save_journal(&mut same_account)?;
        let same_params: HostAuthLoginParams = serde_json::from_value(json!({
            "schema_version":1,"binding":{"installation_root":root,"profile":root,
                "launch_id":uuid::Uuid::new_v4().to_string(),"expected_account_id":digest(source_user.as_bytes())},
            "identity":source_identity,"addr":"https://source.invalid","user":source_user,
            "authorization":{"kind":"browser","token":"synthetic-unused-token"},"super_password":same_super,
            "continuation":{"migrate_local":true,"migrate_vault":false,"offer_local_merge":false}
        }))?;
        let same_rejected = control_login(&ownership, &same_id, &same_id, &same_params).err()
            .context("same-profile migration was not rejected")?;
        assert_eq!(same_rejected.to_string(), "source and destination are the same account; no migration needed");
        assert_eq!(std::fs::read(root.join("session.json"))?, source_bytes);
        assert_eq!(control_account_rollback(ownership, &source_identity, &same_id, &same_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let import_id = uuid::Uuid::new_v4().to_string();
        let (mut importing, index, _) = admit_account_with_intent(&ownership, &source_identity,
            &import_id, &import_id, false, "https://never-contact.invalid", "import-target", Some((true, false, false)))?;
        let (target_super, _, salt, wrapped, nonce) = crate::auth::new_vault_v4()?;
        importing.operations[index].phase = AuthPhase::Authorized;
        let account = importing.operations[index].account.as_mut().context("import receipt missing")?;
        account.authorization = Some(json!({"user":"import-target","token":"synthetic-unused-token"}));
        account.vault = Some(json!({"version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}));
        save_journal(&mut importing)?;
        let mut import_params: HostAuthLoginParams = serde_json::from_value(json!({
            "schema_version":1,"binding":{"installation_root":root,"profile":root,
                "launch_id":uuid::Uuid::new_v4().to_string(),"expected_account_id":digest(source_user.as_bytes())},
            "identity":source_identity,"addr":"https://never-contact.invalid","user":"import-target",
            "authorization":{"kind":"browser","token":"synthetic-unused-token"},"super_password":target_super,
            "continuation":{"migrate_local":true,"migrate_vault":false,"offer_local_merge":false}
        }))?;
        assert!(!login_directory("https://never-contact.invalid", "import-target")?.try_exists()?,
            "first-login regression requires a missing target directory");
        let missing_source = control_login(&ownership, &import_id, &import_id, &import_params)
            .err().context("missing explicit source material was accepted")?;
        assert_eq!(missing_source.to_string(), "explicit host session material is required");
        assert_eq!(std::fs::read(root.join("session.json"))?, source_bytes);
        assert_eq!(std::fs::read(crate::service::database_path(&root)?)?, source_database_before);
        import_params.continuation.as_mut().context("import input missing")?.source_secret = Some("synthetic-wrong-source-password".into());
        assert!(control_login(&ownership, &import_id, &import_id, &import_params).is_err());
        assert_eq!(std::fs::read(root.join("session.json"))?, source_bytes);
        assert_eq!(control_account_rollback(ownership, &source_identity, &import_id, &import_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);

        use crate::sync::SyncControl as _;
        let sync_control = login::ImportSyncControl::default();
        sync_control.progress("pull");
        assert!(sync_control.remote::<()>(|| Err(anyhow!("synthetic read failure"))).is_err());
        assert!(!sync_control.remote_effect_possible.get());
        sync_control.progress("push");
        assert!(sync_control.remote::<()>(|| Err(anyhow!("synthetic lost write response"))).is_err());
        sync_control.progress("pull");
        sync_control.remote(|| Ok(()))?;
        assert!(sync_control.remote_effect_possible.get(), "a later read erased a possible write");
        let unclassified = login::ImportSyncControl::default();
        unclassified.remote(|| Ok(()))?;
        assert!(unclassified.remote_effect_possible.get());

        // Exercise the production compensation against a real candidate and the shared test keyring.
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let rollback_id = uuid::Uuid::new_v4().to_string();
        let config_before = std::fs::read(crate::service::client_config_path())?;
        let (mut pending, index, _) = admit_account_with_intent(&ownership, &source_identity,
            &rollback_id, &rollback_id, false, "https://never-contact.invalid", "import-target", Some((true, false, false)))?;
        pending.operations[index].phase = AuthPhase::Authorized;
        save_journal(&mut pending)?;
        let (code, _, salt, wrapped, nonce) = crate::auth::new_vault_v4()?;
        let mut prepared = PreparedLogin::prepare_with_vault("https://never-contact.invalid",
            &json!({"user":"import-target","token":"synthetic-compensation-token"}), code.clone(),
            json!({"version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}), None)?;
        let old_alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let old_backend = crate::keystore::import_credential_checkpoint(&old_alias, "super", &code,
            require_persistent_auth_backend)?;
        assert_eq!(old_backend, "native", "app and domain must share the test keyring backend");
        let mut old_session = prepared.session.clone();
        old_session["keyring_account"] = json!(old_alias);
        old_session["keyring_backend"] = json!(old_backend);
        let old_bytes = serde_json::to_vec(&old_session)?;
        std::fs::create_dir_all(&prepared.directory)?;
        private_write(&prepared.directory.join("session.json"), &old_bytes)?;
        prepared.original = Some(old_bytes.clone());
        let database = crate::service::database_path(&prepared.directory)?;
        let store = crate::transport::local::LocalStore::open(&database)?;
        store.meta_set("compensation_test", "retain-existing-database")?;
        drop(store);
        let database_before = std::fs::read(&database)?;
        prepared.alias = pending.operations[index].alias.clone();
        host_persist_prepared(&mut prepared)?;
        let candidate_material = host_material(&serde_json::to_vec_pretty(&prepared.session)?)?;
        pending.operations[index].account.as_mut().and_then(|account| account.login.as_mut())
            .context("compensation login missing")?.original_target_key =
            Some((digest(&old_bytes), digest(&login::supplied_keys(&old_bytes, Some(&host_material(&old_bytes)?))?.urk)));
        commit_account(&ownership, &mut pending, index, &mut prepared)?;
        assert_ne!(std::fs::read(crate::service::client_config_path())?, config_before);
        let target = prepared.directory.clone();
        #[cfg(windows)]
        assert_ne!(target, target.canonicalize()?, "regression requires an ordinary Windows target path");
        let selection_checkpoint = std::fs::read(recovery_path())?;
        let missing_snapshot = login::resume_import(&ownership, &mut pending, index, Some(&candidate_material)).err()
            .context("import without a snapshot unexpectedly continued")?;
        assert_eq!(missing_snapshot.to_string(), "admitted encrypted import snapshot missing",
            "resume_import did not pass selection, target ownership and candidate validation");
        assert_eq!(std::fs::read(recovery_path())?, selection_checkpoint,
            "selection validation changed the durable operation before its snapshot was verified");
        assert_eq!(serde_json::to_value(&pending.operations[index])?["account"]["login"]["sync_started"], json!(false));
        let original_material = original_import_material(&pending.operations[index],
            pending.active.as_ref().context("compensation recovery missing")?)?;
        let mut same_target: Journal = serde_json::from_slice(&selection_checkpoint)?;
        let source_spelling = std::path::absolute(isolated.path())?;
        same_target.active.as_mut().context("same target recovery missing")?.directory = source_spelling.clone();
        same_target.operations[index].account.as_mut().context("same target receipt missing")?.target = Some(source_spelling);
        let same_rejected = rollback_import_before_copy(&ownership, &mut same_target, index, None,
            original_material.as_deref(), Some(&candidate_material)).err().context("source target compensation was not rejected")?;
        assert_eq!(same_rejected.to_string(), "import cannot prove an unpublished owned target; retain the original operation");
        assert_eq!(std::fs::read(recovery_path())?, selection_checkpoint);
        let mut changed = serde_json::to_value(&pending.operations[index])?;
        changed["account"]["login"]["import_started"] = json!(true);
        let original_record = std::mem::replace(&mut pending.operations[index], serde_json::from_value(changed)?);
        let lock = crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?;
        let checkpoint = std::fs::read(recovery_path())?;
        assert!(rollback_import_before_copy(&ownership, &mut pending, index, Some(lock), original_material.as_deref(), Some(&candidate_material)).is_err());
        assert_eq!(std::fs::read(recovery_path())?, checkpoint);
        pending.operations[index] = original_record;
        let (other_code, _, other_salt, other_wrap, other_nonce) = crate::auth::new_vault_v4()?;
        let other_alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let other_backend = crate::keystore::import_credential_checkpoint(&other_alias, "super", &other_code,
            require_persistent_auth_backend)?;
        let mut other_session = old_session.clone();
        other_session["keyring_account"] = json!(other_alias);
        other_session["keyring_backend"] = json!(other_backend);
        other_session["kdf_salt"] = json!(other_salt);
        other_session["wrapped_urk"] = json!(other_wrap);
        other_session["urk_nonce"] = json!(other_nonce);
        pending.active.as_mut().context("compensation recovery missing")?.original = Some(serde_json::to_vec(&other_session)?);
        let other_bytes = serde_json::to_vec(&other_session)?;
        pending.operations[index].account.as_mut().and_then(|account| account.login.as_mut())
            .context("compensation login missing")?.original_target_key =
            Some((digest(&other_bytes), digest(&login::supplied_keys(&other_bytes, Some(&host_material(&other_bytes)?))?.urk)));
        let other_material = original_import_material(&pending.operations[index],
            pending.active.as_ref().context("compensation recovery missing")?)?;
        assert_ne!(other_material, original_material);
        let candidate_config = std::fs::read(crate::service::client_config_path())?;
        let candidate_session = std::fs::read(target.join("session.json"))?;
        let lock = crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?;
        let rejected = rollback_import_before_copy(&ownership, &mut pending, index, Some(lock), other_material.as_deref(), Some(&candidate_material));
        assert!(rejected.err().is_some_and(|error| error.to_string().contains("target keys differ")));
        assert_eq!(std::fs::read(recovery_path())?, checkpoint);
        assert_eq!(std::fs::read(crate::service::client_config_path())?, candidate_config);
        assert_eq!(std::fs::read(target.join("session.json"))?, candidate_session);
        pending.active.as_mut().context("compensation recovery missing")?.original = Some(old_bytes.clone());
        pending.operations[index].account.as_mut().and_then(|account| account.login.as_mut())
            .context("compensation login missing")?.original_target_key =
            Some((digest(&old_bytes), original_material.clone().context("original key digest missing")?));
        std::fs::write(crate::service::client_config_path(), b"{\"changed\":true}")?;
        let lock = crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?;
        assert!(rollback_import_before_copy(&ownership, &mut pending, index, Some(lock), original_material.as_deref(), Some(&candidate_material)).is_err());
        assert_eq!(std::fs::read(recovery_path())?, checkpoint);
        // Restore the exact candidate config, not an unrelated external edit.
        let active = pending.active.as_ref().context("compensation recovery missing")?;
        let mut candidate_config: Value = serde_json::from_slice(active.config.as_deref().context("source config missing")?)?;
        candidate_config["data_dir"] = json!(target.to_string_lossy());
        std::fs::write(crate::service::client_config_path(), serde_json::to_vec(&candidate_config)?)?;
        let lock = crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?;
        rollback_import_before_copy(&ownership, &mut pending, index, Some(lock), original_material.as_deref(), Some(&candidate_material))?;
        assert!(target.is_dir(), "compensation quarantined an existing target");
        assert_eq!(std::fs::read(target.join("session.json"))?, old_bytes);
        assert_eq!(std::fs::read(database)?, database_before);
        assert_eq!(crate::keystore::read_imported(&old_alias, "super", &old_backend)?, code);
        assert_eq!(std::fs::read(crate::service::client_config_path())?, config_before);
        assert_eq!(std::fs::read(root.join("session.json"))?, source_bytes);
        assert!(load_journal()?.active.is_none());
        let terminal = std::fs::read(recovery_path())?;
        assert_eq!(control_account_result(&ownership, &source_identity, &rollback_id, &rollback_id)?.outcome,
            HostAuthAccountOutcome::RolledBack);
        assert_eq!(control_login(&ownership, &rollback_id, &rollback_id, &import_params)?.outcome,
            HostAuthAccountOutcome::RolledBack);
        assert_eq!(std::fs::read(recovery_path())?, terminal, "terminal query resumed import or authentication");
        include!("auth_admission_local_rejection.rs")
    }

