    use super::*;
    use crate::memory::crypto;

    fn lock_dir() -> std::sync::MutexGuard<'static, ()> {
        crate::test_lock::guard()
    }

    #[test]
    fn auth_salt_deterministic() -> Result<()> {
        assert_eq!(crypto::derive_auth_salt("Alice")?, crypto::derive_auth_salt("alice")?);
        assert_ne!(crypto::derive_auth_salt("Alice")?, crypto::derive_auth_salt("Bob")?);
        assert_eq!(validate_totp_code(" 000000 ")?, "000000");
        for invalid in ["", "00000", "0000000", "00000x"] { assert!(validate_totp_code(invalid).is_err()); }
        let reply = authorization_reply("https://fixture.invalid/", "fixture-user", "fixture-salt",
            &serde_json::json!({"token":"synthetic-token","session_id":"synthetic-session"}))?;
        assert_eq!(reply["addr"], "https://fixture.invalid");
        assert_eq!(reply["user"], "fixture-user");
        assert_eq!(reply["session_id"], "synthetic-session");
        assert!(reply.get("pass").is_none() && reply.get("ticket").is_none());
        assert!(serde_json::from_value::<PasswordAuthorizationChallenge>(serde_json::json!({
            "ticket":"synthetic-ticket","auth_salt":"fixture-salt","extra":true})).is_err());
        Ok(())
    }

    #[test]
    fn wrap_v3_cannot_unlock_with_super_alone() -> Result<()> {
        let super_pass = crypto::generate_secret_key();
        let urk = crypto::generate_key();
        let (salt, wrapped, nonce) = wrap_with_v4(&super_pass, &urk)?;
        let (secret, salt3, wrapped3, nonce3) = {
            let sk = crypto::generate_secret_key();
            let kdf_salt = crypto::random_hex(16);
            let kek = crypto::derive_vault_kek("master-pass", &sk, &kdf_salt)?;
            let (n, w) = crypto::wrap_key(&urk, &kek)?;
            (sk, kdf_salt, w, n)
        };
        crate::memory::SessionKeys::unlock_vault("master-pass", &secret, &salt3, &wrapped3, &nonce3)?;
        assert!(crate::memory::SessionKeys::unlock_vault(
            "wrong-pass",
            &secret,
            &salt3,
            &wrapped3,
            &nonce3
        )
        .is_err());
        assert!(crate::memory::SessionKeys::unlock_super("master-pass", &salt3, &wrapped3, &nonce3).is_err());
        // v4 roundtrip: correct super password unlocks, wrong key does not
        crate::memory::SessionKeys::unlock_v4(&super_pass, &salt, &wrapped, &nonce)?;
        assert!(crate::memory::SessionKeys::unlock_v4("A3-000000-000000-000000-000000-000000-000000", &salt, &wrapped, &nonce).is_err());
        assert_ne!(super_pass, "master-pass");
        let _ = salt3;
        Ok(())
    }

    #[test]
    fn unlock_session_keys_v3_and_v2() -> Result<()> {
        let _guard = lock_dir();
        let super_pass = crypto::generate_secret_key();
        let urk = crypto::generate_key();
        let (salt, wrapped, nonce) = wrap_with_v4(&super_pass, &urk)?;
        let v4 = serde_json::json!({
            "wrapped_urk": wrapped,
            "urk_nonce": nonce,
            "kdf_salt": salt,
            "vault_version": 4,
            "secret_key": super_pass,
        });
        unlock_session_keys(&v4, "login-pass", None, "u")?;
        assert!(unlock_session_keys(&v4, "login-pass", Some("A3-000000-000000-000000-000000-000000-000000"), "u").is_err());

        let (secret, salt, wrapped, nonce) = {
            let sk = crypto::generate_secret_key();
            let kdf_salt = crypto::random_hex(16);
            let kek = crypto::derive_vault_kek("master-pass", &sk, &kdf_salt)?;
            let urk = crypto::generate_key();
            let (n, w) = crypto::wrap_key(&urk, &kek)?;
            (sk, kdf_salt, w, n)
        };
        let v3 = serde_json::json!({
            "wrapped_urk": wrapped,
            "urk_nonce": nonce,
            "kdf_salt": salt,
            "vault_version": 3,
            "super": "master-pass",
            "secret_key": secret,
        });
        unlock_session_keys(&v3, "login-pass", None, "u")?;
        assert!(unlock_session_keys(&v3, "login-pass", Some("wrong-pass"), "u").is_err());

        let salt2 = crypto::random_hex(16);
        let kek = crypto::derive_super_kek("master-pass", &salt2)?;
        let urk = crypto::generate_key();
        let (nonce2, wrapped2) = crypto::wrap_key(&urk, &kek)?;
        let v2 = serde_json::json!({
            "wrapped_urk": wrapped2,
            "urk_nonce": nonce2,
            "kdf_salt": salt2,
            "vault_version": 2,
            "super": "master-pass",
        });
        unlock_session_keys(&v2, "login-pass", None, "u")?;
        assert!(unlock_session_keys(&v2, "login-pass", Some("wrong-pass"), "u").is_err());
        Ok(())
    }

    /// A replaced session file cannot supply an alias for previously read key material.
    #[test]
    fn local_session_owner_and_keys_share_one_snapshot() -> Result<()> {
        let _iso = crate::test_lock::Isolate::new()?;
        assert!(load_local_session_with_owner().is_err());
        let salt = crypto::random_hex(16);
        let kek = crypto::derive_super_kek("synthetic-super", &salt)?;
        let first_urk = [11; 32];
        let (nonce, wrapped) = crypto::wrap_key(&first_urk, &kek)?;
        let first = serde_json::json!({"user":"snapshot-owner", "pass":"unused",
            "super":"synthetic-super", "vault_version":2, "kdf_salt":salt,
            "wrapped_urk":wrapped, "urk_nonce":nonce});
        write_session_json(&first)?;
        let snapshot = read_session_json()?;
        let second_urk = [12; 32];
        let (nonce, wrapped) = crypto::wrap_key(&second_urk, &kek)?;
        let mut second = first.clone();
        second["user"] = serde_json::json!("replacement-owner");
        second["wrapped_urk"] = serde_json::json!(wrapped);
        second["urk_nonce"] = serde_json::json!(nonce);
        write_session_json(&second)?;
        let (keys, owner) = unlock_local_session_snapshot(&snapshot)?;
        assert!(keys.urk == first_urk);
        assert_eq!(owner.as_deref(), Some("snapshot-owner"));
        let (keys, owner) = load_local_session_with_owner()?;
        assert!(keys.urk == second_urk);
        assert_eq!(owner.as_deref(), Some("replacement-owner"));
        for user in [serde_json::Value::Null, serde_json::json!(""), serde_json::json!(" ")] {
            second["user"] = user;
            write_session_json(&second)?;
            let (keys, owner) = load_local_session_with_owner()?;
            assert!(keys.urk == second_urk && owner.is_none());
            assert!(load_local_session()?.urk == second_urk);
        }
        Ok(())
    }

    #[test]
    fn unlock_v2_keeps_session_authentication_independent_of_provider_failure() -> Result<()> {
        use respire_protocol::credentials::{ProviderCredentialStatus, ProviderCredentialUpdate,
            ProviderUnavailableCode, RuntimeUnlockV2, RUNTIME_UNLOCK_SCHEMA_V2};
        use sha2::{Digest, Sha256};
        let _iso = crate::test_lock::Isolate::new()?;
        let result = (|| -> Result<()> {
            let salt = crypto::random_hex(16);
            let kek = crypto::derive_super_kek("synthetic-super", &salt)?;
            let expected = [17; 32];
            let (nonce, wrapped) = crypto::wrap_key(&expected, &kek)?;
            write_session_json(&serde_json::json!({"user":"provider-fixture", "vault_version":2,
                "kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce}))?;
            let digest = format!("{:x}", Sha256::digest(std::fs::read(session_file()?)?));
            let input = |password: &str, digest: &str, provider_update| -> Result<RuntimeUnlockV2> {
                Ok(RuntimeUnlockV2 { schema_version:RUNTIME_UNLOCK_SCHEMA_V2,
                    profile:crate::service::data_dir().canonicalize()?, session_sha256:digest.into(),
                    instance_id:"fixture".into(), generation:1,
                    super_password:Some(password.into()), login_password:None, legacy_secret:None,
                    provider_update })
            };
            supply_runtime_session_v2(input("synthetic-super", &digest,
                ProviderCredentialUpdate::Unavailable { code:ProviderUnavailableCode::AccessDenied })?)?;
            {
                let supplied = RUNTIME_SESSION.lock().map_err(|_| anyhow!("fixture session lock poisoned"))?;
                let supplied = supplied.as_ref().context("authenticated session was not installed")?;
                assert_eq!(supplied.keys.urk, expected);
                assert_eq!(supplied.owner.as_deref(), Some("provider-fixture"));
            }
            assert_eq!(crate::credential_io::provider_status()?,
                ProviderCredentialStatus::Unavailable { code:ProviderUnavailableCode::AccessDenied });
            assert!(crate::credential_io::ensure_provider_credentials_available().is_err());
            assert!(matches!(crate::credential_io::Entry::new("rsrs", "classify:typesafe", "native")
                .get_password(), Err(respire_protocol::credentials::CredentialError::Unavailable)));
            for (password, revision) in [("wrong-super", digest.as_str()), ("synthetic-super", "wrong-revision")] {
                assert!(supply_runtime_session_v2(input(password, revision,
                    ProviderCredentialUpdate::Replace { credentials:Vec::new() })?).is_err());
                assert_eq!(crate::credential_io::provider_status()?,
                    ProviderCredentialStatus::Unavailable { code:ProviderUnavailableCode::AccessDenied });
            }
            supply_runtime_session_v2(input("synthetic-super", &digest,
                ProviderCredentialUpdate::Replace { credentials:Vec::new() })?)?;
            assert_eq!(crate::credential_io::provider_status()?, ProviderCredentialStatus::Ready);
            Ok(())
        })();
        clear_runtime_session()?;
        result
    }

    #[test]
    fn session_roundtrip_isolated_home() -> Result<()> {
        let _iso = crate::test_lock::Isolate::new()?;
        let secret = crypto::generate_account_secret();
        let kdf_salt = crypto::random_hex(16);
        let kek = crypto::derive_kek("pass", &secret, &kdf_salt)?;
        let urk = crypto::generate_key();
        let (nonce, wrapped) = crypto::wrap_key(&urk, &kek)?;
        write_session_json(&serde_json::json!({
            "pass": "pass", "secret": secret, "kdf_salt": kdf_salt,
            "wrapped_urk": wrapped, "urk_nonce": nonce,
        }))?;
        assert!(load_local_session().is_ok());
        Ok(())
    }

    #[test]
    fn fivekeys_super_opens_v4_and_keeps_old_wrap() -> Result<()> {
        let _guard = lock_dir();
        let dir = tempfile::tempdir()?;
        let saved = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let result = (|| -> Result<()> {
            let super_pass = crypto::generate_secret_key();
            let urk = crypto::generate_key();
            let (salt, wrapped, nonce) = wrap_with_v4(&super_pass, &urk)?;
            fivekeys_login(
                "",
                "fivekeys-v4-test",
                "login-pass-123",
                "",
                &salt,
                &wrapped,
                &nonce,
                &super_pass,
            )?;
            let session = read_session_json()?;
            if session["vault_version"].as_i64() != Some(4) {
                anyhow::bail!("v4 fivekeys wrote vault_version {}", session["vault_version"]);
            }
            crate::memory::SessionKeys::unlock_v4(&super_pass, &salt, &wrapped, &nonce)?;
            if fivekeys_login(
                "",
                "fivekeys-v4-test",
                "login-pass-123",
                "",
                &salt,
                &wrapped,
                &nonce,
                "not-the-super",
            )
            .is_ok()
            {
                anyhow::bail!("wrong super password was accepted for a v4 wrap");
            }

            let old_pass = "old-super-passphrase";
            let old_urk = crypto::generate_key();
            let old_salt = crypto::random_hex(16);
            let kek = crypto::derive_super_kek(old_pass, &old_salt)?;
            let (old_nonce, old_wrapped) = crypto::wrap_key(&old_urk, &kek)?;
            fivekeys_login(
                "",
                "fivekeys-v2-test",
                "login-pass-123",
                "",
                &old_salt,
                &old_wrapped,
                &old_nonce,
                old_pass,
            )?;
            let old = read_session_json()?;
            if old["vault_version"].as_i64() != Some(2) {
                anyhow::bail!("old fivekeys wrote vault_version {}", old["vault_version"]);
            }
            let root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
                dir.path().join("session.json"),
            )?)?;
            if root["user"].as_str() != Some("fivekeys-v4-test") || root["vault_version"].as_i64() != Some(4) {
                anyhow::bail!("fivekeys for another user overwrote the open profile");
            }
            Ok(())
        })();
        match saved {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        result
    }

    #[test]
    fn register_other_user_does_not_unlock_the_open_vault() -> Result<()> {
        let _guard = lock_dir();
        let dir = tempfile::tempdir()?;
        let saved = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let result = (|| -> Result<()> {
            let super_pass = crypto::generate_secret_key();
            let urk = crypto::generate_key();
            let (salt, wrapped, nonce) = wrap_with_v4(&super_pass, &urk)?;
            write_session_json(&serde_json::json!({
                "user": "mainuser",
                "vault_version": 4,
                "kdf_salt": salt,
                "wrapped_urk": wrapped,
                "urk_nonce": nonce,
            }))?;
            let err = register(
                "http://127.0.0.1:1",
                "otheruser",
                "password12",
                "",
            )
            .expect_err("register against a closed port must fail after switching profiles");
            let msg = format!("{err:#}");
            if msg.contains("none in the local keyring") {
                anyhow::bail!("register tried to unlock the open vault: {msg}");
            }
            let root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
                dir.path().join("session.json"),
            )?)?;
            if root["user"] != "mainuser" || root["wrapped_urk"] != wrapped {
                anyhow::bail!("open profile was rewritten");
            }
            let current = crate::service::data_dir();
            let expected = dir.path().join("accounts").join("otheruser");
            if current != expected {
                anyhow::bail!("new account profile is {}, want {}", current.display(), expected.display());
            }
            Ok(())
        })();
        match saved {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        result
    }

    #[test]
    fn register_after_keygen_accepts_the_issued_super() -> Result<()> {
        let _guard = lock_dir();
        let dir = tempfile::tempdir()?;
        let saved = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let result = (|| -> Result<()> {
            let super_pass = crypto::generate_secret_key();
            let urk = crypto::generate_key();
            let (salt, wrapped, nonce) = wrap_with_v4(&super_pass, &urk)?;
            write_session_json(&serde_json::json!({
                "vault_version": 4,
                "kdf_salt": salt,
                "wrapped_urk": wrapped,
                "urk_nonce": nonce,
            }))?;
            let opened = register("http://127.0.0.1:1", "keygenuser", "password12", &super_pass)
                .expect_err("closed port");
            let opened_msg = format!("{opened:#}");
            if opened_msg.contains("none in the local keyring") {
                anyhow::bail!("correct A3 was ignored: {opened_msg}");
            }
            let wrong = register(
                "http://127.0.0.1:1",
                "keygenuser",
                "password12",
                "A3-000000-000000-000000-000000-000000-000000",
            )
            .expect_err("wrong A3");
            let wrong_msg = format!("{wrong:#}");
            if wrong_msg.contains("none in the local keyring") {
                anyhow::bail!("placeholder A3 was treated as a missing key: {wrong_msg}");
            }
            let omitted = register("http://127.0.0.1:1", "keygenuser", "password12", "")
                .expect_err("omitted super");
            let omitted_msg = format!("{omitted:#}");
            let keyring_has_it = crate::keystore::load_super("keygenuser").is_some()
                || crate::keystore::load_super("local").is_some();
            if keyring_has_it && omitted_msg.contains("none in the local keyring") {
                anyhow::bail!("keyring already holds the keygen code but register did not use it: {omitted_msg}");
            }
            if !keyring_has_it && !omitted_msg.contains("none in the local keyring") {
                anyhow::bail!("missing super and empty keyring should say so: {omitted_msg}");
            }
            let root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
                dir.path().join("session.json"),
            )?)?;
            if root["user"].as_str().unwrap_or("").is_empty() == false {
                anyhow::bail!("failed register wrote a user into the keygen profile");
            }
            if root["wrapped_urk"].as_str() != Some(wrapped.as_str()) {
                anyhow::bail!(
                    "failed register rewrote the keygen wrap file={} expect={}",
                    root["wrapped_urk"].as_str().unwrap_or("").chars().take(12).collect::<String>(),
                    wrapped.chars().take(12).collect::<String>()
                );
            }
            Ok(())
        })();
        match saved {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        result
    }
