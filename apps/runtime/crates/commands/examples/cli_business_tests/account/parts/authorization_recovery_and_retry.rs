{
        // Inspect only local checkpoints; account-result can resume an unfinished operation.
        let journal = || -> Result<Value, Box<dyn std::error::Error>> {
            Ok(serde_json::from_slice(&std::fs::read(root.join(".login-recovery.json"))?)?)
        };
        let local_rows = || -> Result<Value, Box<dyn std::error::Error>> {
            Ok(serde_json::to_value(LocalStore::open(&target.join("rsrs.db"))?.all(true)?)?)
        };
        let checkpoint = |before: &Value, session: &[u8], config: &[u8], data: &Value,
            migrating: bool, phase: &str| -> Result<Value, Box<dyn std::error::Error>> {
            let after = journal()?;
            let prior = before["operations"].as_array().ok_or("prior account history missing")?;
            let operations = after["operations"].as_array().ok_or("account history missing")?;
            if before["schema_version"] != 2 || !before["active"].is_null()
                || after["schema_version"] != 2 || operations.len() != prior.len() + 1
                || operations.get(..prior.len()) != Some(prior.as_slice()) {
                return Err("rejected account operation did not retain exactly one new receipt".into());
            }
            let record = &operations[prior.len()];
            let id = record["operation_id"].as_str().ok_or("rejected operation ID missing")?;
            let parsed = uuid::Uuid::parse_str(id)?;
            let source = target.canonicalize()?;
            let identity = respire_protocol::HostAuthIdentity::Bound {
                account_id: format!("{:x}", Sha256::digest(b"authorized")),
                session_sha256: format!("{:x}", Sha256::digest(session)),
            };
            let base = format!("{:x}", Sha256::digest(serde_json::to_vec(&json!({
                "schema_version":1,"register":false,"root":root.canonicalize()?,
                "source":source,"identity":identity,"addr":addr,"user":"authorized",
            }))?));
            let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(
                &(&base, (false, migrating, false)))?));
            let account = &record["account"];
            let login = &account["login"];
            if parsed.get_version_num() != 4 || parsed.to_string() != id
                || prior.iter().any(|old| old["operation_id"] == id)
                || record["idempotency_key"] != id || record["fingerprint"] != fingerprint
                || record["root"] != serde_json::to_value(root.canonicalize()?)?
                || record["profile"] != serde_json::to_value(&source)?
                || record["original_sha256"] != format!("{:x}", Sha256::digest(session))
                || record["phase"] != phase
                || !record["alias"].as_str().is_some_and(|alias| alias.starts_with("login-") && alias.len() > 6)
                || record["candidate"] != Value::Null || record["candidate_sha256"] != Value::Null
                || record["backend"] != Value::Null || account["target"] != Value::Null
                || account["register"] != false || account["user"] != "authorized" || account["addr"] != addr
                || account["source_identity"] != serde_json::to_value(identity)?
                || !account["authorization"].is_object()
                || account["published_vault"] != false || account["vault_pending"] != false
                || login["migrate_local"] != false || login["migrate_vault"] != migrating
                || login["offer_local_merge"] != false || login["required_inputs"] != json!([])
                || login["snapshot"] != Value::Null || login["import_started"] != false
                || login["sync_started"] != false || login["import"] != Value::Null || login["sync"] != Value::Null
                || login["publication_pending"] != false || login["publication_done"] != false
                || login["vault_done"] != false || login["password_done"] != false
                || std::fs::read(target.join("session.json"))? != session
                || std::fs::read(root.join("client.json"))? != config || local_rows()? != *data {
                return Err("rejected account operation changed its binding, checkpoint or local material".into());
            }
            if phase == "authorized" {
                let active = &after["active"];
                let original: Vec<u8> = serde_json::from_value(active["original"].clone())?;
                if active["operation_id"] != id || active["alias"] != record["alias"]
                    || active["directory"] != serde_json::to_value(source)? || original != session
                    || active["directory_existed"] != true || active["owns_config"] != false
                    || active["config"] != Value::Null || active["backends"] != json!([])
                    || active["pass_backends"] != json!([]) || active["totp_backends"] != json!([]) {
                    return Err("rejected account operation does not own an untouched recovery checkpoint".into());
                }
            } else if phase != "rolled_back" || !after["active"].is_null() {
                return Err("automatic account rejection did not durably clear active recovery".into());
            }
            Ok(record.clone())
        };
        let rollback_rejected = |output: &Output, before: &Value, session: &[u8], config: &[u8], data: &Value|
            -> Result<(), Box<dyn std::error::Error>> {
            use respire_protocol::host_control::{HostAuthAccountOutcome, HostAuthAccountResult};
            assert!(!output.status.success());
            let record = checkpoint(before, session, config, data, true, "authorized")?;
            let operation = record["operation_id"].as_str().ok_or("rollback operation missing")?;
            let failure: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(failure["summary"]["reason"], "account_outcome_unknown");
            assert_eq!(failure["details"]["operation_id"], operation);
            assert_eq!(failure["details"]["source_profile"], serde_json::to_value(target.canonicalize()?)?);
            assert_eq!(failure["details"]["register"], false);
            let requests_before = requests.lock().map_err(|error| error.to_string())?.clone();
            let cloud_before = cloud.lock().map_err(|error| error.to_string())?.clone();
            let source = target.canonicalize()?;
            let source_arg = source.to_str().ok_or("rollback source is not UTF-8")?;
            let output = execute(&["--json", "runtime", "account-rollback", "--operation-id",
                operation, "--source-profile", source_arg, "--confirm"])?;
            let envelope: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(envelope["command"], "auth-account");
            let receipt: HostAuthAccountResult = serde_json::from_value(envelope["summary"].clone())?;
            assert_eq!(receipt.operation_id, operation);
            assert_eq!(receipt.source_profile, source);
            assert_eq!(receipt.outcome, HostAuthAccountOutcome::RolledBack);
            let mut expected = record;
            expected["phase"] = json!("rolled_back");
            if checkpoint(before, session, config, data, true, "rolled_back")? != expected {
                return Err("explicit rollback changed original operation evidence".into());
            }
            assert_eq!(*requests.lock().map_err(|error| error.to_string())?, requests_before,
                "rollback repeated authentication, vault publication or import requests");
            if *cloud.lock().map_err(|error| error.to_string())? != cloud_before {
                return Err("explicit rollback changed the remote vault".into());
            }
            Ok(())
        };
        let restart_migration = |session: &[u8], config: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
            let restarted = execute(&["--json", "runtime", "restart"])?;
            assert!(restarted.status.success(), "migration source did not resume after explicit rollback");
            let envelope: Value = serde_json::from_slice(&restarted.stdout)?;
            assert_eq!(envelope["summary"]["state"], "ready");
            assert_eq!(std::fs::canonicalize(envelope["summary"]["data_dir"].as_str()
                .ok_or("restarted migration source missing")?)?, target.canonicalize()?);
            assert!(execute(&["--json", "--client-only", "status"])?.status.success(),
                "restarted migration source failed scoped owner verification");
            let health = runtime_health(runtime_port, "restart-target")?;
            assert_eq!(health["store_ready"], true);
            assert_eq!(health["account_id"], format!("{:x}", Sha256::digest(b"authorized")));
            assert_eq!(std::fs::canonicalize(health["data_dir"].as_str()
                .ok_or("restarted owner profile missing")?)?, target.canonicalize()?);
            assert_eq!(std::fs::read(target.join("session.json"))?, session);
            assert_eq!(std::fs::read(root.join("client.json"))?, config);
            Ok(())
        };
        let mismatch_before = journal()?;
        let mismatch_rows = local_rows()?;
        let mismatch_salt = crypto::random_hex(16);
        let (mismatch_nonce, mismatch_wrap) = crypto::wrap_key(
            &[44_u8; 32],
            &crypto::derive_kek_v4(&super_password, &mismatch_salt)?,
        )?;
        *cloud.lock().map_err(|error| error.to_string())? = json!({"version":4,"kdf_salt":mismatch_salt,"wrapped_urk":mismatch_wrap,"urk_nonce":mismatch_nonce});
        let rejected = execute(&[
            "--json",
            "login",
            "--oauth",
            "--addr",
            &addr,
            "--dashboard",
            &addr,
            "--no-open",
            "--super",
            &super_password,
        ])?;
        assert_eq!(
            rejected.status.code(),
            Some(1),
            "same-identity key mismatch was accepted"
        );
        let rejected_record = checkpoint(&mismatch_before, &retained_session, &retained_config,
            &mismatch_rows, false, "rolled_back")?;
        let rejected_envelope: Value = serde_json::from_slice(&rejected.stdout)?;
        assert_eq!(rejected_envelope["summary"]["reason"], "runtime_error");
        let rejected_id = rejected_record["operation_id"].as_str().ok_or("automatic rollback operation missing")?;
        assert!(rejected_envelope["errors"].as_array().ok_or("automatic rollback error missing")?
            .iter().filter_map(Value::as_str).any(|message| message.contains(
                &format!("account operation rolled back; operation_id={rejected_id} "))),
            "key mismatch did not report its automatic original-operation rollback");
        assert_eq!(
            std::fs::read(target.join("session.json"))?,
            retained_session
        );
        assert_eq!(std::fs::read(root.join("client.json"))?, retained_config);
        *cloud.lock().map_err(|error| error.to_string())? = cloud_snapshot;
        let logged_out = execute(&["--json", "logout", "--full"])?;
        assert!(logged_out.status.success(), "{}", String::from_utf8_lossy(&logged_out.stdout));
        assert!(!target.join("session.json").exists());
        let logged_out_health = runtime_health(runtime_port, "full-logout")?;
        assert_eq!(logged_out_health["store_ready"], false, "full logout left the database open");
        let recovered = execute(&[
            "--json",
            "login",
            "--oauth",
            "--addr",
            &addr,
            "--dashboard",
            &addr,
            "--no-open",
            "--super",
            &super_password,
        ])?;
        assert!(
            recovered.status.success(),
            "{}",
            String::from_utf8_lossy(&recovered.stdout)
        );
        let recovered: Value = serde_json::from_slice(&recovered.stdout)?;
        assert_eq!(
            recovered["summary"]["dir"].as_str(),
            target.to_str(),
            "full logout selected a fresh library"
        );
        for row in &rows {
            let read = execute(&["--json", "show", &row.id])?;
            assert!(
                read.status.success(),
                "full logout lost local record: {}",
                String::from_utf8_lossy(&read.stdout)
            );
        }
        for row in &rows {
            assert!(execute(&["--json", "purge", &row.id])?.status.success());
        }
        remote_blobs
            .lock()
            .map_err(|error| error.to_string())?
            .clear();
        assert!(execute(&["--runtime-internal", "--stop"])?.status.success());
        std::fs::write(original.join("session.json"), session_bytes)?;
        use_checkpointed_credentials.set(false);

        let legacy_super = "synthetic-legacy-passphrase";
        migration_super.set(Some(legacy_super));
        let salt = crypto::random_hex(16);
        let (nonce, wrapped) =
            crypto::wrap_key(&urk, &crypto::derive_super_kek(legacy_super, &salt)?)?;
        let mut legacy = json!({"user":"authorized","addr":addr,"vault_version":2,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
        std::fs::write(target.join("session.json"), serde_json::to_vec(&legacy)?)?;
        *cloud.lock().map_err(|error| error.to_string())? =
            json!({"version":2,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
        let incomplete_before = journal()?;
        let incomplete_session = std::fs::read(target.join("session.json"))?;
        let migration_config = std::fs::read(root.join("client.json"))?;
        let migration_rows = local_rows()?;
        let incomplete = execute(&[
            "--json",
            "migrate",
            "--vault",
            "--pass=fixture-password",
            "--super",
            legacy_super,
        ])?;
        if incomplete.status.success()
            || std::fs::read(target.join("session.json"))? != serde_json::to_vec(&legacy)?
        {
            return Err("vault-only operation bypassed full library migration".into());
        }
        rollback_rejected(&incomplete, &incomplete_before, &incomplete_session, &migration_config, &migration_rows)?;
        // The real public library conversion is covered by migration_readiness_tests.
        // This authorization fixture begins with its verified v2 namespace wrap.
        legacy["crypto_namespace"] = json!(crypto::RSRS_PREFIX);
        legacy["wrapped_urk"] = json!(format!("{}{wrapped}", crypto::RSRS_PREFIX));
        std::fs::write(target.join("session.json"), serde_json::to_vec(&legacy)?)?;
        restart_migration(&serde_json::to_vec(&legacy)?, &migration_config)?;
        let migrated = execute(&[
            "--json",
            "migrate",
            "--vault",
            "--pass=fixture-password",
            "--super",
            legacy_super,
            "--new-super",
            legacy_super,
        ])?;
        if !migrated.status.success() {
            return Err(format!(
                "explicit vault migration failed: {} {}",
                String::from_utf8_lossy(&migrated.stdout),
                String::from_utf8_lossy(&migrated.stderr)
            )
            .into());
        }
        let current = cloud.lock().map_err(|error| error.to_string())?.clone();
        let keys = SessionKeys::unlock_super(
            legacy_super,
            current["kdf_salt"].as_str().ok_or("salt missing")?,
            current["wrapped_urk"].as_str().ok_or("wrap missing")?,
            current["urk_nonce"].as_str().ok_or("nonce missing")?,
        )?;
        if current["version"] != 2
            || keys.urk != urk
            || !current["wrapped_urk"]
                .as_str()
                .is_some_and(|value| value.starts_with(crypto::RSRS_PREFIX))
        {
            return Err("migration replaced the existing memory key".into());
        }
        let stopped = execute(&["--runtime-internal", "--stop"])?;
        if !stopped.status.success() {
            return Err("could not stop the isolated migration runtime".into());
        }
        // Reproduce a committed cloud wrap with a locally rolled-back legacy
        // session: retry must prove the same URK, never overwrite the cloud key.
        let legacy_bytes = serde_json::to_vec(&legacy)?;
        std::fs::write(target.join("session.json"), &legacy_bytes)?;
        let wrong_before = journal()?;
        let wrong_config = std::fs::read(root.join("client.json"))?;
        let wrong_rows = local_rows()?;
        let wrong = execute(&[
            "--json",
            "migrate",
            "--vault",
            "--pass=fixture-password",
            "--super",
            legacy_super,
            "--new-super=wrong-recovery-code",
        ])?;
        if wrong.status.success() || std::fs::read(target.join("session.json"))? != legacy_bytes {
            return Err("migration resume accepted an incorrect recovery code".into());
        }
        rollback_rejected(&wrong, &wrong_before, &legacy_bytes, &wrong_config, &wrong_rows)?;
        restart_migration(&legacy_bytes, &wrong_config)?;
        let resumed = execute(&[
            "--json",
            "migrate",
            "--vault",
            "--pass=fixture-password",
            "--super",
            legacy_super,
            "--new-super",
            legacy_super,
        ])?;
        if !resumed.status.success() {
            return Err(format!(
                "migration resume failed: {} {}",
                String::from_utf8_lossy(&resumed.stdout),
                String::from_utf8_lossy(&resumed.stderr)
            )
            .into());
        }
        let saved: Value = serde_json::from_slice(&std::fs::read(target.join("session.json"))?)?;
        if saved["vault_version"] != 2
            || *cloud.lock().map_err(|error| error.to_string())? != current
        {
            return Err(
                "migration resume did not commit locally or republished the cloud vault".into(),
            );
        }
        if std::fs::read(original.join("session.json"))? != session_bytes {
            return Err("migration modified the unrelated original account".into());
        }
        let paths = requests.lock().map_err(|error| error.to_string())?;
        if !paths.iter().any(|path| path == "/oauth/token")
            || !paths.iter().any(|path| path == "/api/self/vault")
        {
            return Err("authorization did not exercise polling and vault verification".into());
        }
        Ok(())
}
