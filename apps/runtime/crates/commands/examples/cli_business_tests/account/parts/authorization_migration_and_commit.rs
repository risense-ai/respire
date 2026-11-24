{
        // Private auth retains a credential reference bound to this new committed receipt.
        // Compare private bytes in memory; failures must not print credential material.
        use sha2::{Digest, Sha256};
        let journal: Value = serde_json::from_slice(
            &std::fs::read(root.join(".login-recovery.json"))?,
        )?;
        let new_operations: Vec<_> = journal["operations"].as_array()
            .ok_or("account receipt history missing after login")?
            .iter().filter(|record| record["operation_id"].as_str()
                .is_some_and(|id| !prior_operations.iter().any(|prior| prior == id)))
            .collect();
        if journal["schema_version"] != 2 || !journal["active"].is_null()
            || new_operations.len() != 1 {
            return Err("login did not retain exactly one completed new operation".into());
        }
        let record = new_operations[0];
        let operation_id = record["operation_id"].as_str().ok_or("login operation ID missing")?;
        let parsed = uuid::Uuid::parse_str(operation_id)?;
        let alias = record["alias"].as_str().filter(|value| !value.is_empty())
            .ok_or("login credential reference missing")?;
        let backend = record["backend"].as_str().ok_or("login credential backend missing")?;
        let candidate: Vec<u8> = serde_json::from_value(record["candidate"].clone())?;
        let session_sha256 = format!("{:x}", Sha256::digest(&saved_bytes));
        if parsed.get_version_num() != 4 || parsed.to_string() != operation_id
            || record["idempotency_key"] != operation_id || record["phase"] != "committed"
            || record["account"]["register"] != false
            || record["account"]["user"] != "authorized" || record["account"]["addr"] != addr
            || std::path::Path::new(record["root"].as_str().ok_or("login receipt root missing")?).canonicalize()? != root.canonicalize()?
            || std::path::Path::new(record["profile"].as_str().ok_or("login receipt source missing")?).canonicalize()? != original.canonicalize()?
            || std::path::Path::new(record["account"]["target"].as_str().ok_or("login receipt target missing")?).canonicalize()? != target.canonicalize()?
            || saved["keyring_account"] != alias || saved["keyring_backend"] != backend
            || !matches!(backend, "native" | "secret_service")
            || candidate != saved_bytes || record["candidate_sha256"] != session_sha256
        {
            return Err("login session does not match its committed credential receipt".into());
        }
        eprintln!("account-checkpoint receipt_verified");
        let stopped = execute(&["--runtime-internal", "--stop"])?;
        if !stopped.status.success() {
            return Err("could not stop the isolated OAuth runtime".into());
        }
        eprintln!("account-checkpoint oauth_runtime_stopped");

        // Seed an encrypted cloud-only record, absent from this target's local store.
        assert!(execute(&[
            "--json",
            "remember",
            "Synthetic cloud peer body",
            "--title",
            "Cloud peer",
            "--importance",
            "important",
            "--force"
        ])?
        .status
        .success());
        eprintln!("account-checkpoint cloud_memory_seeded");
        let peer_store = LocalStore::open(&target.join("rsrs.db"))?;
        eprintln!("account-checkpoint cloud_store_opened");
        let peers = peer_store.all(false)?;
        let peer = peers.first().ok_or("cloud fixture row missing")?;
        remote_blobs
            .lock()
            .map_err(|error| error.to_string())?
            .push(serde_json::to_value(peer)?);
        drop(peer_store);
        assert!(execute(&["--runtime-internal", "--stop"])?.status.success());
        for name in ["rsrs.db", "rsrs.db-wal", "rsrs.db-shm"] {
            let path = target.join(name);
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }

        eprintln!("account-checkpoint cloud_fixture_staged");
        // The source owns its recovery code; target --super is never a source factor.
        let source_urk = [43_u8; 32];
        let source_secret = crypto::generate_secret_key();
        if source_secret == super_password {
            return Err("source and target fixture credentials must be distinct".into());
        }
        let source_backend = respire_app::keystore::import_credential_checkpoint(
            &source_alias, "super", &source_secret, |backend| {
                anyhow::ensure!(matches!(backend, "native" | "secret_service"),
                    "source fixture requires persistent credentials");
                source_backends.push(backend.to_owned());
                Ok(())
            },
        )?;
        let checkpoint = respire_app::keystore::read_imported(
            &source_alias, "super", source_backend,
        )?;
        if checkpoint != source_secret {
            return Err("source credential checkpoint readback mismatch".into());
        }
        let source_salt = crypto::random_hex(16);
        let (source_nonce, source_wrap) = crypto::wrap_key(
            &source_urk,
            &crypto::derive_kek_v4(&source_secret, &source_salt)?,
        )?;
        let source_session = serde_json::to_vec(
            &json!({"user":"original","addr":"https://previous.invalid",
            "keyring_account":source_alias,"keyring_backend":source_backend,
            "vault_version":4,"kdf_salt":source_salt,"wrapped_urk":source_wrap,"urk_nonce":source_nonce}),
        )?;
        std::fs::write(original.join("session.json"), &source_session)?;
        use_checkpointed_credentials.set(true);
        let selected = execute(&["--json", "account", "use", "original"])?;
        assert!(
            selected.status.success(),
            "{}",
            String::from_utf8_lossy(&selected.stdout)
        );
        let created = execute(&[
            "--json",
            "remember",
            "Synthetic cross-server transfer body",
            "--title",
            "Transfer fixture",
            "--importance",
            "important",
            "--force",
        ])?;
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stdout)
        );
        assert!(execute(&[
            "--json",
            "remember",
            "Synthetic cloud peer body",
            "--title",
            "Source peer",
            "--importance",
            "important",
            "--force"
        ])?
        .status
        .success());
        let transfer = [
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
            "--migrate-local",
        ];
        let source_config = std::fs::read(root.join("client.json"))?;
        let target_session = std::fs::read(target.join("session.json"))?;
        let before_import: Value = serde_json::from_slice(
            &std::fs::read(root.join(".login-recovery.json"))?,
        )?;
        let prior_imports = before_import["operations"].as_array()
            .ok_or("account history missing before import")?;
        let target_empty = || -> Result<bool, Box<dyn std::error::Error>> {
            let path = target.join("rsrs.db");
            if !path.try_exists()? { return Ok(true); }
            let database = rusqlite::Connection::open_with_flags(path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
            Ok(database.query_row("SELECT NOT EXISTS(SELECT 1 FROM memories)
                AND NOT EXISTS(SELECT 1 FROM sync_outbox)
                AND NOT EXISTS(SELECT 1 FROM sync_resolution_outbox)", [], |row| row.get(0))?)
        };
        if before_import["schema_version"] != 2 || !before_import["active"].is_null()
            || !target_empty()? {
            return Err("import rejection fixture requires its original empty target/outbox".into());
        }
        *mode.lock().map_err(|error| error.to_string())? = "sync_unavailable".to_owned();
        let rejected = execute(&transfer)?;
        assert_eq!(rejected.status.code(), Some(1));
        let after_import: Value = serde_json::from_slice(
            &std::fs::read(root.join(".login-recovery.json"))?,
        )?;
        let operations = after_import["operations"].as_array()
            .ok_or("account history missing after rejected import")?;
        if after_import["schema_version"] != 2 || !after_import["active"].is_null()
            || operations.len() != prior_imports.len() + 1
            || operations.get(..prior_imports.len()) != Some(prior_imports.as_slice()) {
            return Err("rejected import did not retain one new terminal operation and its history".into());
        }
        let record = &operations[prior_imports.len()];
        let operation_id = record["operation_id"].as_str().ok_or("import operation ID missing")?;
        let parsed = uuid::Uuid::parse_str(operation_id)?;
        let bound_root: std::path::PathBuf = serde_json::from_value(record["root"].clone())?;
        let bound_source: std::path::PathBuf = serde_json::from_value(record["profile"].clone())?;
        let identity = respire_protocol::HostAuthIdentity::Bound {
            account_id: format!("{:x}", Sha256::digest(b"original")),
            session_sha256: format!("{:x}", Sha256::digest(&source_session)),
        };
        let base = format!("{:x}", Sha256::digest(serde_json::to_vec(&json!({
            "schema_version":1,"register":false,"root":bound_root,"source":bound_source,
            "identity":identity,"addr":addr,"user":"authorized",
        }))?));
        let fingerprint = format!("{:x}", Sha256::digest(
            serde_json::to_vec(&(&base, (true, false, false)))?,
        ));
        let login = &record["account"]["login"];
        if parsed.get_version_num() != 4 || parsed.to_string() != operation_id
            || prior_imports.iter().any(|prior| prior["operation_id"] == operation_id)
            || record["idempotency_key"] != operation_id || record["fingerprint"] != fingerprint
            || record["phase"] != "rolled_back" || bound_root != root.canonicalize()?
            || bound_source != original.canonicalize()?
            || record["original_sha256"] != format!("{:x}", Sha256::digest(&source_session))
            || record["account"]["register"] != false || record["account"]["user"] != "authorized"
            || record["account"]["addr"] != addr
            || record["account"]["source_identity"] != serde_json::to_value(&identity)?
            || std::path::Path::new(record["account"]["target"].as_str()
                .ok_or("import receipt target missing")?).canonicalize()? != target.canonicalize()?
            || login["migrate_local"] != true || login["migrate_vault"] != false
            || login["offer_local_merge"] != false || login["import_started"] != false
            || login.get("import") != Some(&Value::Null)
            || login["sync_started"] != true
            || login["sync"] != json!({"status":"failed","reason":"target_sync_failed_before_copying"})
            || !target_empty()? || std::fs::read(target.join("session.json"))? != target_session {
            return Err("rejected import did not prove bound rollback before copying source data".into());
        }
        assert_eq!(std::fs::read(root.join("client.json"))?, source_config);
        assert_eq!(
            std::fs::read(original.join("session.json"))?,
            source_session
        );
        *mode.lock().map_err(|error| error.to_string())? = "authorized".to_owned();
        let prior_copy_operations = operations;
        let source_rows = serde_json::to_value(LocalStore::open(&original.join("rsrs.db"))?.all(true)?)?;
        let copied = execute(&transfer)?;
        assert_eq!(
            copied.status.code(),
            Some(2),
            "{} {}",
            String::from_utf8_lossy(&copied.stdout),
            String::from_utf8_lossy(&copied.stderr)
        );
        let copied: Value = serde_json::from_slice(&copied.stdout)?;
        assert_eq!(copied["status"], "warn");
        assert_eq!(copied["summary"]["ok"], true);
        assert_eq!(copied["summary"]["user"], "authorized");
        assert_eq!(copied["summary"]["addr"], addr);
        assert_eq!(copied["summary"]["import"]["imported"], 1, "{copied}");
        assert_eq!(
            copied["summary"]["import"]["duplicates"], 1,
            "cloud-only duplicate was not pulled before import"
        );
        assert_eq!(copied["summary"]["import"]["skipped"], 1);
        let pending_sync = json!({"status":"pending","reason":"post_import_sync_failed"});
        assert_eq!(copied["summary"]["sync"], pending_sync);
        // Import is durable before post-sync; a sync warning is not an unknown login.
        let copy_journal: Value = serde_json::from_slice(
            &std::fs::read(root.join(".login-recovery.json"))?,
        )?;
        let copy_operations = copy_journal["operations"].as_array()
            .ok_or("account history missing after committed import")?;
        if copy_journal["schema_version"] != 2 || !copy_journal["active"].is_null()
            || copy_operations.len() != prior_copy_operations.len() + 1
            || copy_operations.get(..prior_copy_operations.len()) != Some(prior_copy_operations.as_slice()) {
            return Err("import warning did not retain one new terminal operation and its history".into());
        }
        let copy_record = &copy_operations[prior_copy_operations.len()];
        let copy_id = copy_record["operation_id"].as_str().ok_or("committed import ID missing")?;
        let parsed = uuid::Uuid::parse_str(copy_id)?;
        let copy_login = &copy_record["account"]["login"];
        let candidate: Vec<u8> = serde_json::from_value(copy_record["candidate"].clone())?;
        let copied_session_bytes = std::fs::read(target.join("session.json"))?;
        let copied_session: Value = serde_json::from_slice(&copied_session_bytes)?;
        let alias = copy_record["alias"].as_str().filter(|value| !value.is_empty())
            .ok_or("committed import credential reference missing")?;
        let backend = copy_record["backend"].as_str().ok_or("committed import backend missing")?;
        if parsed.get_version_num() != 4 || parsed.to_string() != copy_id
            || prior_copy_operations.iter().any(|prior| prior["operation_id"] == copy_id)
            || copy_record["idempotency_key"] != copy_id || copy_record["fingerprint"] != fingerprint
            || copy_record["phase"] != "committed" || copy_record["root"] != record["root"]
            || copy_record["profile"] != record["profile"]
            || copy_record["original_sha256"] != format!("{:x}", Sha256::digest(&source_session))
            || copy_record["account"]["source_identity"] != serde_json::to_value(&identity)?
            || copy_record["account"]["register"] != false || copy_record["account"]["user"] != "authorized"
            || copy_record["account"]["addr"] != addr
            || std::path::Path::new(copy_record["account"]["target"].as_str()
                .ok_or("committed import target missing")?).canonicalize()? != target.canonicalize()?
            || copy_login["migrate_local"] != true || copy_login["migrate_vault"] != false
            || copy_login["offer_local_merge"] != false || copy_login["import_started"] != true
            || copy_login["sync_started"] != true || copy_login["import"]["recovered"] != false
            || copy_login["import"]["report"] != copied["summary"]["import"] || copy_login["sync"] != pending_sync
            || candidate != copied_session_bytes
            || copy_record["candidate_sha256"] != format!("{:x}", Sha256::digest(&copied_session_bytes))
            || copied_session["user"] != "authorized" || copied_session["addr"] != addr
            || copied_session["keyring_account"] != alias || copied_session["keyring_backend"] != backend
            || !matches!(backend, "native" | "secret_service")
            || copied_session.get("super").is_some() || copied_session.get("pass").is_some() {
            return Err("import warning does not match its committed candidate, owner and durable result".into());
        }
        let mut copy_config: Value = serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
        let source_config_value: Value = serde_json::from_slice(&source_config)?;
        if std::path::Path::new(copy_config["data_dir"].as_str().ok_or("import selection missing")?).canonicalize()?
                != target.canonicalize()?
            || std::path::Path::new(copied["summary"]["dir"].as_str().ok_or("import response directory missing")?).canonicalize()?
                != target.canonicalize()? {
            return Err("committed import did not select its bound target".into());
        }
        copy_config["data_dir"] = source_config_value["data_dir"].clone();
        if copy_config != source_config_value || std::fs::read(original.join("session.json"))? != source_session
            || serde_json::to_value(LocalStore::open(&original.join("rsrs.db"))?.all(true)?)? != source_rows {
            return Err("committed import changed source data/session or unrelated configuration".into());
        }
        let owner = runtime_health(runtime_port, "import-target")?;
        let official = runtime_bin()?;
        let installed = root.join("bin/runtime")
            .join(official.parent().and_then(std::path::Path::file_name).ok_or("official Runtime version missing")?)
            .join(official.file_name().ok_or("official Runtime filename missing")?).canonicalize()?;
        if owner["store_ready"] != true || owner["account_id"] != format!("{:x}", Sha256::digest(b"authorized"))
            || std::path::Path::new(owner["data_dir"].as_str().ok_or("import owner profile missing")?).canonicalize()? != target.canonicalize()?
            || std::path::Path::new(owner["exe"].as_str().ok_or("import owner image missing")?).canonicalize()? != installed
            || owner["image_sha256"] != format!("{:x}", Sha256::digest(std::fs::read(&installed)?))
            || !owner["pid"].as_u64().is_some_and(|pid| pid > 0)
            || !owner["instance_id"].as_str().is_some_and(|id| !id.is_empty())
            || !owner["generation"].is_u64() {
            return Err("import warning lost its ready managed target owner".into());
        }
        assert!(execute(&["--json", "--client-only", "status"])?.status.success(),
            "committed import failed the existing scoped owner verification");
        let db = LocalStore::open(&target.join("rsrs.db"))?;
        let rows = db.all(false)?;
        assert_eq!(rows.len(), 2);
        let cloud_snapshot = cloud.lock().map_err(|error| error.to_string())?.clone();
        let keys = SessionKeys::unlock_v4(
            &super_password,
            cloud_snapshot["kdf_salt"]
                .as_str()
                .ok_or("missing cloud salt")?,
            cloud_snapshot["wrapped_urk"]
                .as_str()
                .ok_or("missing cloud wrap")?,
            cloud_snapshot["urk_nonce"]
                .as_str()
                .ok_or("missing cloud nonce")?,
        )?;
        let entries = rows
            .iter()
            .map(|row| respire_app::memory::MemoryEngine::open(&keys, row))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(entries
            .iter()
            .any(|entry| entry.content == "Synthetic cross-server transfer body"));
        assert!(entries
            .iter()
            .any(|entry| entry.id == peer.id && entry.content == "Synthetic cloud peer body"));
        drop(db);
        assert!(execute(&["--json", "account", "use", "original"])?
            .status
            .success());
        let retry = execute(&transfer)?;
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stdout)
        );
        let retry: Value = serde_json::from_slice(&retry.stdout)?;
        assert_eq!(retry["summary"]["import"]["imported"], 0, "{retry}");
        assert_eq!(retry["summary"]["import"]["duplicates"], 2);
        assert_eq!(
            remote_blobs
                .lock()
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert_eq!(
            std::fs::read(original.join("session.json"))?,
            source_session
        );
        let retained_session = std::fs::read(target.join("session.json"))?;
        let retained_config = std::fs::read(root.join("client.json"))?;
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/account/parts/authorization_recovery_and_retry.rs"))
}
