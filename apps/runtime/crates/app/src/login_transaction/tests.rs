    use super::*;

    fn host_material(bytes: &[u8]) -> Result<respire_protocol::host_control::HostAuthSessionMaterial> {
        use respire_protocol::host_control::{HostAuthSessionMaterial, HostAuthVaultFactors as Factors};
        let session: Value = serde_json::from_slice(bytes)?;
        let alias = session["keyring_account"].as_str().context("host fixture owner missing")?;
        let backend = session["keyring_backend"].as_str().context("host fixture backend missing")?;
        let factors = match session["vault_version"].as_i64() {
            Some(1) => Factors::V1 { password: crate::keystore::read_imported_v1_password(alias, backend)?,
                account_secret: crate::keystore::read_imported(alias, "legacy-secret",
                    session["legacy_secret_backend"].as_str().context("host fixture legacy backend missing")?)? },
            Some(2) => Factors::V2 { super_password: crate::keystore::read_imported(alias, "super", backend)? },
            Some(3) => Factors::V3 { super_password: crate::keystore::read_imported(alias, "super", backend)?,
                secret_key: crate::keystore::read_imported(alias, "legacy-secret",
                    session["legacy_secret_backend"].as_str().context("host fixture legacy backend missing")?)? },
            Some(4) => Factors::V4 { super_password: crate::keystore::read_imported(alias, "super", backend)? },
            _ => anyhow::bail!("host fixture vault version missing"),
        };
        Ok(HostAuthSessionMaterial { session_sha256: digest(bytes), factors })
    }

    // The fixture acts as the host: Runtime receives only its explicit material.
    fn host_persist_prepared(prepared: &mut PreparedLogin) -> Result<()> {
        let mut backend = None;
        if prepared.session["vault_version"] != 1 {
            backend = Some(crate::keystore::import_credential(&prepared.alias, "super", &prepared.super_password)?);
        }
        if let Some(password) = &prepared.login_password {
            backend = Some(crate::keystore::import_credential(&prepared.alias, "pass", password)?);
        }
        let backend = backend.context("host fixture primary factor missing")?;
        prepared.session["keyring_account"] = json!(prepared.alias);
        prepared.session["keyring_backend"] = json!(backend);
        if let Some(secret) = &prepared.legacy_secret {
            prepared.session["legacy_secret_backend"] = json!(crate::keystore::import_credential(&prepared.alias, "legacy-secret", secret)?);
        }
        let bytes = serde_json::to_vec_pretty(&prepared.session)?;
        let supplied = host_material(&bytes)?;
        assert_eq!(login::supplied_keys(&bytes, Some(&supplied))?.urk, prepared.material_keys()?.urk);
        Ok(())
    }

    fn control_account_result(ownership: &AuthOwnership, identity: &HostAuthIdentity, id: &str, key: &str)
        -> Result<HostAuthAccountResult> {
        let journal = load_journal()?;
        let record = journal.operations.iter().find(|record| record.operation_id == id);
        let material = record.filter(|record| record.account.as_ref().is_some_and(|account| !account.register)
            && matches!(record.phase, AuthPhase::Prepared | AuthPhase::AwaitingVerification | AuthPhase::Committed))
            .and_then(|record| record.candidate.as_deref()).map(host_material).transpose()?;
        super::control_account_result(ownership, identity, id, key, material.as_ref())
    }

    fn control_login(ownership: &AuthOwnership, id: &str, key: &str, params: &HostAuthLoginParams)
        -> Result<HostAuthAccountResult> {
        let mut params: HostAuthLoginParams = serde_json::from_value(serde_json::to_value(params)?)?;
        if params.material.is_none() {
            params.material = Some(respire_protocol::host_control::HostAuthLoginMaterial {
                keyring_account: format!("login-{}", uuid::Uuid::parse_str(id)?.simple()), keyring_backend: "native".into(),
                source: None,
                target_original: read_original(&login_directory(&params.addr, &params.user)?)?.as_deref()
                    .filter(|bytes| serde_json::from_slice::<Value>(bytes).is_ok_and(|session| session.get("vault_version").is_some()))
                    .map(host_material).transpose()?,
            });
        }
        Ok(super::control_login(ownership, id, key, &params)?.receipt)
    }

    #[test]
    fn interrupted_login_restores_existing_and_new_profiles() -> Result<()> {
        let _guard = crate::test_lock::guard();
        let _credentials = crate::migration::memory_keyring_for_test();
        let root = tempfile::tempdir()?;
        let previous_root = crate::env::var_os("RSRS_DATA_DIR");
        let previous_super = crate::env::var_os("RSRS_SUPER");
        std::env::set_var("RSRS_DATA_DIR", root.path());
        let code = crate::memory::crypto::generate_secret_key();
        std::env::set_var("RSRS_SUPER", &code);
        let result = (|| -> Result<()> {
            let config = br#"{"custom":"preserve","api_base":"https://previous.invalid"}"#;
            let old = br#"{"user":"existing","token":"old-token"}"#;
            std::fs::write(crate::service::client_config_path(), config)?;
            let existing = crate::service::account_dir("existing")?;
            std::fs::create_dir_all(&existing)?;
            std::fs::write(existing.join("session.json"), old)?;
            let (salt, wrapped, nonce) = crate::auth::wrap_with_v4(&code, &[42; 32])?;
            let vault = json!({"version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
            let case_dir = crate::service::account_dir("shiningray")?;
            std::fs::create_dir_all(&case_dir)?;
            std::fs::write(case_dir.join("session.json"), br#"{"user":"shiningray","addr":"https://old.invalid"}"#)?;
            let cloud_dir = login_directory("https://cloud.invalid", "ShiningRay")?;
            ensure!(cloud_dir != case_dir && !cloud_dir.exists(), "case collision reused source");
            let second_server = login_directory("https://cloud.invalid", "shiningray")?;
            ensure!(second_server != case_dir, "server change reused source");
            ensure!(login_directory("https://old.invalid", "shiningray")? == case_dir, "original identity was not reused");
            for user in ["existing", "new-account"] {
                let mut prepared = PreparedLogin::prepare_with_vault("https://fixture.invalid", &json!({"user":user,"token":"new-token"}), code.clone(), vault.clone(), None)?;
                let directory = prepared.directory.clone();
                host_persist_prepared(&mut prepared)?;
                prepared.commit()?;
                ensure!(recovery_pending(), "commit did not retain recovery state");
                #[cfg(unix)] {
                    use std::os::unix::fs::PermissionsExt;
                    ensure!(std::fs::metadata(recovery_path())?.permissions().mode() & 0o777 == 0o600, "recovery is not private");
                }
                let record = std::fs::read_to_string(recovery_path())?;
                ensure!(!record.contains(&code), "recovery persisted the super password");
                drop(prepared); // Simulate process loss: only the durable record remains.
                recover_interrupted()?;
                recover_interrupted()?; // Recovery must be idempotent.
                ensure!(std::fs::read(crate::service::client_config_path())? == config, "configuration changed");
                ensure!(std::fs::read(existing.join("session.json"))? == old, "existing session changed");
                if user == "new-account" { ensure!(!directory.exists(), "failed new account remained selectable"); }
                ensure!(!recovery_pending(), "completed recovery retained its journal");
            }
            for version in [1, 3] {
                let primary = if version == 1 { String::new() } else { "original-v3-super".to_owned() };
                let factor = if version == 1 { crate::memory::crypto::generate_account_secret() }
                    else { crate::memory::crypto::generate_secret_key() };
                let salt = crate::memory::crypto::random_hex(16);
                let urk = [version as u8; 32];
                let kek = if version == 1 { crate::memory::crypto::derive_kek(&primary, &factor, &salt)? }
                    else { crate::memory::crypto::derive_vault_kek(&primary, &factor, &salt)? };
                let (nonce, wrapped) = crate::memory::crypto::wrap_key(&urk, &kek)?;
                let mut source = json!({"user":"existing","addr":"https://fixture.invalid","vault_version":version,
                    "token":"old-token","kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
                source[if version == 1 { "pass" } else { "super" }] = json!(primary);
                source[if version == 1 { "secret" } else { "secret_key" }] = json!(factor);
                let original = serde_json::to_vec(&source)?;
                // Only the historical fixture writes factors; production writes must refuse them.
                std::fs::write(existing.join("session.json"), &original)?;
                assert!(private_write(&existing.join("session.json"), &original).is_err());
                // Simulate the host's custody step before Runtime freezes the source identity.
                let host_alias = format!("legacy-host-prepared-{version}");
                let primary_backend = crate::keystore::import_credential(&host_alias,
                    if version == 1 { "pass" } else { "super" }, &primary)?;
                let legacy_backend = crate::keystore::import_credential(&host_alias, "legacy-secret", &factor)?;
                let mut host_session = source.clone();
                for field in ["pass", "super", "secret", "secret_key"] {
                    host_session.as_object_mut().context("fixture session is an object")?.remove(field);
                }
                host_session["keyring_account"] = json!(host_alias);
                host_session["keyring_backend"] = json!(primary_backend);
                host_session["legacy_secret_backend"] = json!(legacy_backend);
                let host_snapshot = serde_json::to_vec(&host_session)?;
                private_write(&existing.join("session.json"), &host_snapshot)?;
                let keys = crate::auth::unlock_session_keys(&source, &primary, Some(&primary), "existing")?;
                let (item_nonce, ciphertext) = keys.encrypt_content(&serde_json::to_string(&json!({
                    "kind":"Context","tags":"","title":"retained","content":"rollback fixture",
                    "user":"existing","computer":"fixture","project":"","created_at":"","updated_at":""
                }))?)?;
                let database_path = crate::service::database_path(&existing)?;
                let database = rusqlite::Connection::open(&database_path)?;
                database.execute_batch("CREATE TABLE memories(ciphertext TEXT, nonce TEXT, deleted INTEGER);")?;
                database.execute("INSERT INTO memories VALUES(?1,?2,0)", rusqlite::params![ciphertext, item_nonce])?;
                drop(database);
                let database_before = std::fs::read(&database_path)?;
                let mut authorized = json!({"user":"existing","token":"new-token"});
                if version == 1 { authorized["pass"] = json!(primary); authorized["secret"] = json!(factor); }
                else { authorized["secret_key"] = json!(factor); }
                let vault = json!({"version":version,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
                let mut prepared = PreparedLogin::prepare_with_vault("https://fixture.invalid", &authorized,
                    if version == 1 { String::new() } else { primary.clone() }, vault, None)?;
                host_persist_prepared(&mut prepared)?;
                prepared.commit()?;
                let stored_snapshot: Value = serde_json::from_slice(&std::fs::read(recovery_path())?)?;
                assert_eq!(serde_json::from_value::<Vec<u8>>(stored_snapshot["active"]["original"].clone())?, host_snapshot);
                assert!(!String::from_utf8(std::fs::read(recovery_path())?)?.contains(&factor));
                let mut pending = load_journal()?;
                rollback_record_inner(&mut pending, false)?;
                let restored: Value = serde_json::from_slice(&std::fs::read(existing.join("session.json"))?)?;
                assert!(["pass", "super", "secret", "secret_key"].iter().all(|field| restored.get(*field).is_none()));
                assert_eq!(restored["user"], source["user"]);
                assert_eq!(restored["wrapped_urk"], source["wrapped_urk"]);
                assert_eq!(restored["urk_nonce"], source["urk_nonce"]);
                assert_eq!(restored["kdf_salt"], source["kdf_salt"]);
                let restored_keys = session_material_keys(&restored,
                    restored["keyring_account"].as_str().context("restored alias missing")?,
                    restored["keyring_backend"].as_str().context("restored backend missing")?)?;
                assert_eq!(restored_keys.urk, urk);
                assert!(library_accepts_keys(&existing, &restored_keys)?);
                assert_eq!(std::fs::read(&database_path)?, database_before);
                drop(pending); // Resume after the migrated session write, before retirement.
                recover_interrupted()?;
                recover_interrupted()?;
                assert_eq!(std::fs::read(&database_path)?, database_before);
                assert!(!recovery_pending());
                std::fs::remove_file(database_path)?;
            }
            let canonical_root = root.path().canonicalize()?;
            let historical_candidate = serde_json::to_vec(&json!({"vault_version":3,
                "secret_key":"historical-candidate-factor","wrapped_urk":"unchanged-wrap"}))?;
            let historical_sha256 = digest(&historical_candidate);
            let before_rejected_snapshot = if recovery_path().try_exists()? {
                Some(std::fs::read(recovery_path())?)
            } else { None };
            let mut retained = Journal { schema_version: 2, active: None, operations: vec![AuthRecord {
                operation_id: uuid::Uuid::new_v4().to_string(), idempotency_key: "retained".into(),
                fingerprint: keygen_fingerprint(&canonical_root, &canonical_root, false)?,
                root: canonical_root.clone(), profile: canonical_root, original_sha256: None,
                candidate: Some(historical_candidate.clone()), candidate_backup: None,
                candidate_sha256: Some(historical_sha256), restored: None, alias: "login-retained".into(), backend: None,
                phase: AuthPhase::RolledBack, account: None, identity: None, key_operation: None, space_operation: None, profile_remove: None,
            }] };
            let rejected = save_journal(&mut retained);
            assert!(rejected.is_err_and(|error| error.to_string().contains("host_preparation_required")));
            assert_eq!(recovery_path().try_exists()?, before_rejected_snapshot.is_some());
            if let Some(before) = before_rejected_snapshot { assert_eq!(std::fs::read(recovery_path())?, before); }
            let retained_backend = crate::keystore::import_credential("login-retained", "super", &code)?;
            let retained_candidate = serde_json::to_vec(&json!({"vault_version":4,
                "kdf_salt":vault["kdf_salt"], "wrapped_urk":vault["wrapped_urk"], "urk_nonce":vault["urk_nonce"],
                "keyring_account":"login-retained", "keyring_backend":retained_backend}))?;
            retained.operations[0].candidate_sha256 = Some(digest(&retained_candidate));
            retained.operations[0].candidate = Some(retained_candidate);
            retained.operations[0].backend = Some(retained_backend.into());
            save_journal(&mut retained)?;
            let mut prepared = PreparedLogin::prepare_with_vault("https://fixture.invalid", &json!({"user":"completed-account","token":"new-token"}), code.clone(), vault, None)?;
            host_persist_prepared(&mut prepared)?;
            prepared.commit()?;
            prepared.complete()?;
            ensure!(!recovery_pending() && load_journal()?.operations.len() == 1, "login completion lost the auth receipt");
            Ok(())
        })();
        match previous_root { Some(value) => std::env::set_var("RSRS_DATA_DIR", value), None => std::env::remove_var("RSRS_DATA_DIR") }
        match previous_super { Some(value) => std::env::set_var("RSRS_SUPER", value), None => std::env::remove_var("RSRS_SUPER") }
        result
    }

include!("tests/auth_admission.rs");
include!("tests/registration.rs");
