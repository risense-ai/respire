    use super::*;

    #[test]
    fn namespace_conversion_registers_witness_functions_and_preserves_original()->Result<()> {
        use crate::transport::MemoryTransport;
        let root=tempfile::tempdir()?;let original=root.path().join("original.db");let staged=root.path().join("staged.db");
        let keys=crate::SessionKeys::from_legacy_urk([7;32])?;
        let (nonce,ciphertext)=keys.encrypt_content("synthetic namespace content")?;
        let mut memory=crate::StoredMemory::new_pending("synthetic-entry".into(),"account".into());
        memory.ciphertext=ciphertext.clone();memory.nonce=nonce;memory.updated_at="2026-01-01T00:00:00Z".into();
        {let store=crate::transport::local::LocalStore::open(&original)?;assert!(store.put(&memory)?);assert_eq!(store.witness_verify()?.count,1);}
        std::fs::copy(&original,&staged)?;
        migrate_namespace_database(&staged,&keys)?;
        let converted=crate::transport::local::LocalStore::open_read_only(&staged)?;
        let rows=converted.all(false)?;assert_eq!(rows.len(),1);
        assert!(rows[0].ciphertext.starts_with(crate::memory::crypto::RSRS_PREFIX));
        assert_eq!(crate::SessionKeys::from_urk([7;32])?.decrypt_content(&rows[0].ciphertext,&rows[0].nonce)?,"synthetic namespace content");
        let proof=converted.witness_verify()?;assert!(proof.intact);assert_eq!(proof.count,2);
        let retained=crate::transport::local::LocalStore::open_read_only(&original)?;
        assert_eq!(retained.all(false)?[0].ciphertext,ciphertext);assert_eq!(retained.witness_verify()?.count,1);
        Ok(())
    }

    #[test]
    fn client_only_never_creates_a_migration_home() -> Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let home = tempfile::tempdir()?;
        plaintext_profile(&home.path().join(".onememory"), "client-fixture")?;
        let variables = [
            "HOME",
            "RSRS_DATA_DIR",
            "RSRS_CLIENT_ONLY",
            DEFAULT_ENV,
        ];
        let saved = variables.map(|name| (name, crate::env::var_os(name)));
        std::env::set_var("HOME", home.path());
        std::env::remove_var("RSRS_DATA_DIR");
        std::env::remove_var(DEFAULT_ENV);
        std::env::set_var("RSRS_CLIENT_ONLY", "yes");
        let result = ensure_default_home();
        let root_changed = crate::env::var_os("RSRS_DATA_DIR").is_some();
        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        result?;
        assert!(!root_changed);
        assert!(!home.path().join(".rsrs").exists());
        assert!(!home.path().join(".rsrs-migration-lock").exists());
        assert!(!home.path().join(".rsrs-migration-staging").exists());
        Ok(())
    }

    fn plaintext_profile(path: &Path, user: &str) -> Result<()> {
        std::fs::create_dir_all(path)?;
        write_json(
            &path.join("session.json"),
            &json!({"user":user,"addr":"https://1memory.ai","token":"fixture-token"}),
        )?;
        Ok(())
    }

    #[test]
    fn committed_wal_and_original_files_survive() -> Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let _keyring = super::readiness_tests::memory_keyring();
        let home = tempfile::tempdir()?;
        let source = home.path().join(".onememory");
        plaintext_profile(&source, "wal-owner")?;
        let urk = crate::memory::crypto::generate_key();
        let db = Connection::open(source.join("onememory.db"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                        CREATE TABLE memories (id TEXT PRIMARY KEY,user TEXT,ciphertext TEXT,nonce TEXT,created_at TEXT,updated_at TEXT,deleted INTEGER NOT NULL DEFAULT 0);
                        INSERT INTO memories VALUES ('committed','wal-owner','','','created','updated',0);")?;
        let data_key = crate::memory::crypto::derive_subkey(&urk, b"onememory:data:v1")?;
        let payload = json!({"kind":"context","tags":"","title":"committed",
                "content":"committed fixture","user":"wal-owner","computer":"fixture",
                "project":"","created_at":"2026-10-04T00:00:00Z","updated_at":"2026-10-04T00:00:00Z"});
        let (nonce, ciphertext) = crate::memory::crypto::encrypt_item(&data_key, &payload.to_string())?;
        db.execute(
            "UPDATE memories SET ciphertext=?1,nonce=?2 WHERE id='committed'",
            rusqlite::params![ciphertext, nonce],
        )?;
        let foreign_key = crate::memory::crypto::generate_key();
        let (foreign_nonce, foreign_ciphertext) =
            crate::memory::crypto::encrypt_item(&foreign_key, "old local fixture")?;
        db.execute(
            "INSERT INTO memories VALUES ('old-local','local',?1,?2,'created','updated',0)",
            rusqlite::params![foreign_ciphertext, foreign_nonce],
        )?;
        db.execute("UPDATE memories SET deleted=1 WHERE id='old-local'", [])?;
        for index in 1..5 {
            db.execute(
                "INSERT INTO memories VALUES (?1,'local',?2,?3,'created','updated',1)",
                rusqlite::params![
                    format!("old-local-{index}"),
                    foreign_ciphertext,
                    foreign_nonce
                ],
            )?;
        }
        // Exercise the copied database through the normal store/index path.
        let encrypted_snapshot = home.path().join("encrypted-snapshot.db");
        snapshot_database(&source.join("onememory.db"), &encrypted_snapshot)?;
        validate_schema(&encrypted_snapshot)?;
        let keys = crate::memory::SessionKeys::from_urk(urk)?;
        let store = crate::transport::local::LocalStore::open(&encrypted_snapshot)?;
        use crate::transport::MemoryTransport;
        let rows = store.all(true)?;
        assert_eq!(rows.len(), 6);
        let tombstone = rows
            .iter()
            .find(|row| row.id == "old-local")
            .ok_or_else(|| anyhow!("copied tombstone missing"))?;
        assert!(crate::memory::MemoryEngine::open(&keys, tombstone).is_err());
        let embedder = crate::memory::search::HashingEmbedder::default();
        assert_eq!(store.rebuild_index(&keys, &embedder, "m3")?, 1);
        assert!(!store.index_pending("m3")?);
        assert_eq!(store.all(false)?.len(), 1);
        assert_eq!(
            (tombstone.ciphertext.clone(), tombstone.nonce.clone()),
            (foreign_ciphertext.clone(), foreign_nonce.clone())
        );
        // An encrypted profile without usable session material must remain
        // retryable instead of being published with a completion receipt.
        assert!(migrate_home(home.path()).is_err());
        assert!(!home.path().join(".rsrs").exists());
        std::fs::remove_file(source.join("session.json"))?;
        assert!(migrate_home(home.path()).is_err());
        assert!(!home.path().join(".rsrs").exists());
        let secret = crate::memory::crypto::generate_secret_key();
        let salt = crate::memory::crypto::random_hex(16);
        let kek = crate::memory::crypto::derive_kek_v4(&secret, &salt)?;
        let (urk_nonce, wrapped_urk) = crate::memory::crypto::wrap_key(&urk, &kek)?;
        write_json(&source.join("session.json"), &json!({
            "user":"wal-owner","addr":"https://1memory.ai","token":"fixture-token",
            "vault_version":4,"secret_key":secret,"kdf_salt":salt,
            "urk_nonce":urk_nonce,"wrapped_urk":wrapped_urk
        }))?;
        std::fs::create_dir_all(source.join("runtime"))?;
        std::fs::write(source.join("runtime/token"), "must-not-copy")?;
        std::fs::create_dir_all(source.join("models"))?;
        std::fs::write(source.join("models/model.fixture"), "model")?;
        let original_session = std::fs::read(source.join("session.json"))?;
        migrate_home(home.path())?;
        let target = home.path().join(".rsrs");
        let snapshot = Connection::open(target.join("onememory.db"))?;
        assert_eq!(
            snapshot.query_row("SELECT id FROM memories WHERE id='committed'", [], |row| {
                row.get::<_, String>(0)
            })?,
            "committed"
        );
        assert_eq!(
            snapshot.query_row(
                "SELECT ciphertext,nonce,deleted FROM memories WHERE id='old-local'",
                [],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?
                ))
            )?,
            (foreign_ciphertext, foreign_nonce, 1)
        );
        assert_eq!(
            std::fs::read(source.join("session.json"))?,
            original_session
        );
        assert!(source.join("runtime/token").is_file());
        assert!(!target.join("runtime").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("models/model.fixture"))?,
            "model"
        );
        let session: Value = serde_json::from_slice(&std::fs::read(target.join("session.json"))?)?;
        assert_eq!(session["addr"], crate::service::DEFAULT_SERVER_ADDR);
        assert_eq!(session["token"], "fixture-token");
        verify_local_session(&session)?;
        for field in ["pass", "super", "secret", "secret_key"] { assert!(session.get(field).is_none()); }
        migrate_home(home.path())?;
        assert!(!target.join("accounts/legacy-onememory").exists());

        // Every historical vault keeps its URK through host-slot migration and namespace conversion.
        for version in 1..=4 {
            let case = tempfile::tempdir()?;
            let source = case.path().join(".onememory");
            plaintext_profile(&source, "wal-owner")?;
            let primary = crate::memory::crypto::generate_secret_key();
            let legacy = crate::memory::crypto::generate_account_secret();
            let salt = crate::memory::crypto::random_hex(16);
            let kek = match version {
                1 => crate::memory::crypto::derive_kek(&primary, &legacy, &salt)?,
                2 => crate::memory::crypto::derive_super_kek(&primary, &salt)?,
                3 => crate::memory::crypto::derive_vault_kek(&primary, &legacy, &salt)?,
                4 => crate::memory::crypto::derive_kek_v4(&primary, &salt)?,
                _ => bail!("unsupported migration fixture version"),
            };
            let (nonce, wrapped) = crate::memory::crypto::wrap_key(&urk, &kek)?;
            let original = serde_json::to_vec(&json!({"user":"wal-owner","vault_version":version,
                "pass":primary,"super":primary,"secret":legacy,
                "secret_key":if version == 4 { &primary } else { &legacy },
                "kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}))?;
            // Only this historical source fixture may contain inline factors.
            std::fs::write(source.join("session.json"), &original)?;
            let db = Connection::open(source.join("onememory.db"))?;
            db.execute_batch("CREATE TABLE memories (id TEXT PRIMARY KEY,user TEXT,ciphertext TEXT,nonce TEXT,created_at TEXT,updated_at TEXT,deleted INTEGER NOT NULL DEFAULT 0)")?;
            let (nonce, ciphertext) = crate::memory::crypto::encrypt_item(&data_key, &payload.to_string())?;
            db.execute("INSERT INTO memories VALUES ('committed','wal-owner',?1,?2,'created','updated',0)",
                rusqlite::params![ciphertext, nonce])?;
            let profiles = legacy_profiles(case.path())?;
            let profile = profiles.first().context("historical migration fixture is missing")?;
            let interrupted = case.path().join("interrupted");
            assert!(snapshot_profile_checkpoint(profile, &interrupted, &profiles, true,
                || Err(anyhow!("synthetic durable checkpoint failure"))).is_err());
            assert!(!interrupted.join("session.json").exists());
            assert_eq!(std::fs::read(source.join("session.json"))?, original);

            // A later config failure retains slot ownership without copying original factors.
            std::fs::write(source.join("client.json"), br#"{"data_dir":"../outside"}"#)?;
            let failed = case.path().join("failed");
            assert!(snapshot_profile_checkpoint(profile, &failed, &profiles, true, || Ok(())).is_err());
            let pending: Value = serde_json::from_slice(&std::fs::read(failed.join(RECEIPT))?)?;
            assert_eq!(pending["complete"], false);
            let references = pending["credential_references"].as_array().context("migration credential references missing")?;
            if version != 1 { assert!(references.iter().any(|reference| reference["slot"] == "super")); }
            assert!(references.iter().any(|reference| reference["slot"] == "pass"));
            if matches!(version, 1 | 3) {
                assert!(references.iter().any(|reference| reference["slot"] == "legacy-secret"));
            }
            for name in ["session.json", RECEIPT] {
                let text = std::fs::read_to_string(failed.join(name))?;
                assert!(!text.contains(&primary));
                assert!(!text.contains(&legacy));
            }
            assert_eq!(std::fs::read(source.join("session.json"))?, original);
            std::fs::remove_file(source.join("client.json"))?;

            let stage = case.path().join("complete");
            snapshot_profile_checkpoint(profile, &stage, &profiles, true, || Ok(()))?;
            let migrated: Value = serde_json::from_slice(&std::fs::read(stage.join("session.json"))?)?;
            for field in ["pass", "super", "secret", "secret_key"] { assert!(migrated.get(field).is_none()); }
            assert_eq!(migrated["vault_version"], version);
            verify_local_session(&migrated)?;
            let alias = migrated["keyring_account"].as_str().context("migrated alias missing")?;
            let restored = crate::auth::unlock_session_keys(&migrated,
                if version == 1 { &primary } else { "" }, Some(&primary), alias)?;
            assert_eq!(restored.urk, urk);
            validate_live_memories(&stage.join("rsrs.db"), Some(&restored))?;
            if matches!(version, 1 | 3) {
                let backend = migrated["legacy_secret_backend"].as_str().context("legacy backend missing")?;
                assert_ne!(backend, "kernel");
                assert_eq!(crate::keystore::read_imported(alias, "legacy-secret", backend)?, legacy);
                crate::keystore::remove_imported(alias, "legacy-secret", backend)?;
                assert!(verify_local_session(&migrated).is_err());
                let wrong_backend = crate::keystore::import_credential(alias, "legacy-secret", "wrong-migration-factor")?;
                let mut wrong = migrated.clone();
                wrong["legacy_secret_backend"] = json!(wrong_backend);
                assert!(verify_local_session(&wrong).is_err());
                crate::keystore::remove_imported(alias, "legacy-secret", wrong_backend)?;
                assert_eq!(crate::keystore::import_credential(alias, "legacy-secret", &legacy)?, backend);
                verify_local_session(&migrated)?;
            }
            let receipt: Value = serde_json::from_slice(&std::fs::read(stage.join(RECEIPT))?)?;
            assert_eq!(receipt["complete"], true);
            assert_eq!(receipt["keyring_account"], migrated["keyring_account"]);
            assert_eq!(receipt["credential_references"], pending["credential_references"]);
            assert_eq!(std::fs::read(source.join("session.json"))?, original);
        }
        Ok(())
    }

    #[test]
    fn both_homes_and_conflicting_profiles_are_retained() -> Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let home = tempfile::tempdir()?;
        let recent = home.path().join(".respire");
        let old = home.path().join(".onememory");
        plaintext_profile(&recent, "recent")?;
        plaintext_profile(&old, "old")?;
        plaintext_profile(&recent.join("accounts/work"), "recent-work")?;
        plaintext_profile(&old.join("accounts/work"), "old-work")?;
        write_json(
            &recent.join("client.json"),
            &json!({"data_dir":recent.join("accounts/work"),"addr":"https://dev.rsrs.rs"}),
        )?;
        migrate_home(home.path())?;
        let target = home.path().join(".rsrs");
        assert!(target.join("session.json").is_file());
        assert!(target.join("accounts/work/session.json").is_file());
        assert!(target
            .join("accounts/legacy-onememory/session.json")
            .is_file());
        assert!(target
            .join("accounts/legacy-onememory-work/session.json")
            .is_file());
        let config: Value = serde_json::from_slice(&std::fs::read(target.join("client.json"))?)?;
        let active_profile = config["data_dir"]
            .as_str()
            .ok_or_else(|| anyhow!("migrated active profile path is missing"))?;
        assert_eq!(Path::new(active_profile), target.join("accounts/work"));
        assert_eq!(config["addr"], "https://dev.rsrs.rs");
        let session_before = std::fs::read(target.join("session.json"))?;
        migrate_home(home.path())?;
        assert_eq!(std::fs::read(target.join("session.json"))?, session_before);
        assert!(old.join("accounts/work/session.json").is_file());
        let fresh_home = tempfile::tempdir()?;
        for label in [".rsrs", ".respire"] {
            let current = fresh_home.path().join(label);
            std::fs::create_dir_all(&current)?;
            std::fs::write(current.join("onememory.db"), b"")?;
            write_json(&current.join("session.json"), &json!({"wrapped_urk":format!("{}fixture",crate::memory::crypto::RSRS_PREFIX),"vault_version":4}))?;
        }
        assert!(legacy_profiles(fresh_home.path())?.is_empty(), "current vaults were reported as legacy");
        Ok(())
    }

    #[test]
    fn incompatible_ciphertext_schema_never_publishes() -> Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let home = tempfile::tempdir()?;
        let source = home.path().join(".onememory");
        plaintext_profile(&source, "old-demo")?;
        let db = Connection::open(source.join("onememory.db"))?;
        db.execute_batch(
            "CREATE TABLE memories (id INTEGER PRIMARY KEY,tag_hashes TEXT,content TEXT);
            INSERT INTO memories VALUES (1,'legacy','preserved');",
        )?;
        assert!(migrate_home(home.path()).is_err());
        assert!(!home.path().join(".rsrs").exists());
        assert_eq!(
            db.query_row("SELECT content FROM memories", [], |row| row
                .get::<_, String>(0))?,
            "preserved"
        );
        assert!(home.path().join(".rsrs-migration-staging").is_dir());
        Ok(())
    }
