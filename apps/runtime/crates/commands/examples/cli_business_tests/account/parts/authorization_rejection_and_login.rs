{
        if support::legacy_cli()?.is_some() {
            let guard=super::snapshot_lifecycle::Fixture::new()?;
            let protected=guard.protected_state()?;
            *mode.lock().map_err(|error|error.to_string())?="access_denied".to_owned();
            let denied=execute(&["--json","login","--oauth","--addr",&addr,
                "--dashboard",&addr,"--no-open"])?;
            assert!(!denied.status.success());
            assert!(std::fs::read(root.join("client.json"))?==config_bytes);
            assert!(std::fs::read(original.join("session.json"))?==session_bytes);
            assert!(!requests.lock().map_err(|error|error.to_string())?.is_empty());
            *mode.lock().map_err(|error|error.to_string())?="authorized".to_owned();
            let wrong=crypto::generate_secret_key();
            let rejected=execute(&["--json","login","--oauth","--addr",&addr,
                "--dashboard",&addr,"--no-open","--super",&wrong])?;
            assert!(!rejected.status.success());
            assert!(std::fs::read(root.join("client.json"))?==config_bytes);
            assert!(std::fs::read(original.join("session.json"))?==session_bytes);
            *mode.lock().map_err(|error|error.to_string())?="totp".to_owned();
            let totp=execute(&["--json","login","--interactive","--addr",&addr,
                "--user","authorized","--pass=fixture-password","--super",&super_password])?;
            assert!(!totp.status.success());
            assert!(std::fs::read(root.join("client.json"))?==config_bytes);
            assert!(std::fs::read(original.join("session.json"))?==session_bytes);
            *mode.lock().map_err(|error|error.to_string())?="authorized".to_owned();
            let login=execute(&["--json","login","--oauth","--addr",&addr,
                "--dashboard",&addr,"--no-open","--super",&super_password])?;
            assert!(login.status.success(),"old authorized login failed");
            let selected:Value=serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
            let target=std::path::PathBuf::from(selected["data_dir"].as_str().ok_or("old committed profile missing")?);
            assert!(target.starts_with(&root));
            assert_eq!(selected["custom"],"preserve");
            assert_eq!(selected["api_base"],"https://previous.invalid");
            let saved:Value=serde_json::from_slice(&std::fs::read(target.join("session.json"))?)?;
            assert_eq!(saved["user"],"authorized");
            assert!(saved.get("super").is_none()&&saved.get("pass").is_none());
            let owner=support::legacy_owner(&root,&target,runtime_port)?;
            assert!(respire_app::lock::LibraryLock::acquire(&target,Duration::from_millis(80)).is_err());
            // The old default trivial write formats a diary; use its existing ordinary-memory contract.
            let written=execute(&["--json","remember","old authorized private body","--force","--importance","important"])?;
            assert!(written.status.success());
            let written:Value=serde_json::from_slice(&written.stdout)?;
            let id=written["summary"]["id"].as_str().ok_or("old authorized memory ID missing")?;
            let shown=execute(&["--json","show",id])?;
            assert!(shown.status.success());
            let shown:Value=serde_json::from_slice(&shown.stdout)?;
            assert_eq!(shown["details"]["entry"]["content"],"old authorized private body");
            let retained=serde_json::to_value(LocalStore::open(&target.join("rsrs.db"))?.all(true)?)?;
            let retained_config=std::fs::read(root.join("client.json"))?;
            // Stable creates valid new profile names; use its original invalid-name refusal contract.
            assert!(!execute(&["--json","account","use","invalid/account"])? .status.success());
            assert!(std::fs::read(root.join("client.json"))?==retained_config);
            assert_eq!(serde_json::to_value(LocalStore::open(&target.join("rsrs.db"))?.all(true)?)?,retained);
            support::legacy_owner(&root,&target,runtime_port)?;
            let pid=u32::try_from(owner["pid"].as_u64().ok_or("old auth PID missing")?)?;
            // Failure recovery may legitimately replace this old owner. Reverify
            // the current PID instead of sending any signal to the saved PID.
            let current=support::legacy_owner(&root,&target,runtime_port)?;
            let current_pid=u32::try_from(current["pid"].as_u64().ok_or("old auth current PID missing")?)?;
            assert!(execute(&["--runtime-internal","--stop"])?.status.success());
            support::legacy_drained(&target,runtime_port,current_pid)?;
            assert!(!respire_commands::net_rpc::process_is_running(pid)?);
            assert!(execute(&["--json","logout","--full"])?.status.success());
            assert!(!target.join("session.json").exists());
            let recovered=execute(&["--json","login","--oauth","--addr",&addr,
                "--dashboard",&addr,"--no-open","--super",&super_password])?;
            assert!(recovered.status.success());
            let selected:Value=serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
            assert_eq!(selected["data_dir"].as_str(),target.to_str());
            assert_eq!(serde_json::to_value(LocalStore::open(&target.join("rsrs.db"))?.all(true)?)?,retained);
            support::legacy_owner(&root,&target,runtime_port)?;

            // Reuse the original mock vault and real v2 namespace conversion.
            // New account-result/rollback receipts are deliberately not invoked.
            assert!(execute(&["--runtime-internal","--stop"])?.status.success());
            assert!(execute(&["--json","logout","--full"])?.status.success());
            let legacy_super="synthetic-legacy-passphrase";
            migration_super.set(Some(legacy_super));
            let salt=crypto::random_hex(16);
            let (nonce,wrapped)=crypto::wrap_key(&urk,&crypto::derive_super_kek(legacy_super,&salt)?)?;
            let mut legacy=json!({"user":"authorized","addr":addr,"vault_version":2,
                "kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
            *cloud.lock().map_err(|error|error.to_string())?=json!({"version":2,
                "kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
            std::fs::write(target.join("session.json"),serde_json::to_vec(&legacy)?)?;
            let config=std::fs::read(root.join("client.json"))?;
            let incomplete=execute(&["--json","migrate","--vault","--pass=fixture-password","--super",legacy_super])?;
            assert!(!incomplete.status.success());
            assert!(std::fs::read(target.join("session.json"))?==serde_json::to_vec(&legacy)?);
            assert!(std::fs::read(root.join("client.json"))?==config);
            legacy["crypto_namespace"]=json!(crypto::RSRS_PREFIX);
            legacy["wrapped_urk"]=json!(format!("{}{wrapped}",crypto::RSRS_PREFIX));
            std::fs::write(target.join("session.json"),serde_json::to_vec(&legacy)?)?;
            let migrated=execute(&["--json","migrate","--vault","--pass=fixture-password",
                "--super",legacy_super,"--new-super",legacy_super])?;
            assert!(migrated.status.success());
            let current=cloud.lock().map_err(|error|error.to_string())?.clone();
            let keys=SessionKeys::unlock_super(legacy_super,current["kdf_salt"].as_str().ok_or("old migrated salt missing")?,
                current["wrapped_urk"].as_str().ok_or("old migrated wrap missing")?,
                current["urk_nonce"].as_str().ok_or("old migrated nonce missing")?)?;
            assert!(keys.urk==urk);
            assert_eq!(current["version"],2);
            assert!(current["wrapped_urk"].as_str().is_some_and(|value|value.starts_with(crypto::RSRS_PREFIX)));
            assert!(std::fs::read(original.join("session.json"))?==session_bytes);
            assert_eq!(guard.protected_state()?,protected);
            assert!(execute(&["--json","logout","--full"])?.status.success());
            if respire_commands::net_rpc::port_is_open_on(runtime_port) {
                let selected:Value=serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
                let profile=selected["data_dir"].as_str().map(std::path::PathBuf::from)
                    .map_or_else(||root.clone(),|path|path);
                let owner=support::legacy_owner(&root,&profile,runtime_port)?;
                let pid=u32::try_from(owner["pid"].as_u64().ok_or("old final auth PID missing")?)?;
                assert!(execute(&["--runtime-internal","--stop"])?.status.success());
                support::legacy_drained(&profile,runtime_port,pid)?;
            }
            support::legacy_receipt("account::authorization_preserves_and_commits_profiles","old_embedded")?;
            return Ok(());
        }
        let stale_session = br#"{"user":"original","addr":"http://legacy.invalid"}"#;
        std::fs::write(original.join("session.json"), stale_session)?;
        for args in [
            vec!["--json", "login", "--oauth", "--no-open"],
            vec![
                "--json",
                "login",
                "--interactive",
                "--user",
                "fixture",
                "--pass",
                "fixture-password",
            ],
            vec![
                "--json",
                "login",
                "--oauth",
                "--addr",
                "http://explicit.invalid",
                "--no-open",
            ],
            vec![
                "--json",
                "login",
                "--oauth",
                "--addr",
                &addr,
                "--dashboard",
                "http://name:fixture-secret@dashboard.invalid/?fixture-secret",
                "--no-open",
            ],
        ] {
            let output = execute(&args)?;
            assert_eq!(output.status.code(), Some(1));
            let reply: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(reply["summary"]["reason"], "runtime_error");
            let text = reply.to_string();
            assert!(
                text.contains("login requires HTTPS, except loopback fixtures"),
                "{text}"
            );
            let source = if args.contains(&"--dashboard") {
                "OAuth dashboard from --dashboard"
            } else if args.contains(&"--addr") {
                "login server from --addr"
            } else {
                "stored session.json addr"
            };
            assert!(text.contains(source), "{text}");
            assert!(!text.contains("fixture-secret"), "{text}");
            assert!(requests
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty());
            assert_eq!(std::fs::read(root.join("client.json"))?, config_bytes);
            assert_eq!(std::fs::read(original.join("session.json"))?, stale_session);
        }
        for stored in [
            json!({"user":"original"}),
            json!({"user":"original","addr":""}),
            json!({"user":"original","addr":" \t"}),
            json!({"user":"original","addr":"https://self-hosted.invalid"}),
        ] {
            let bytes = serde_json::to_vec(&stored)?;
            std::fs::write(original.join("session.json"), &bytes)?;
            // Dashboard validation stops before networking, proving login origin
            // resolution succeeded without contacting the official/custom server.
            let output = execute(&[
                "--json",
                "login",
                "--oauth",
                "--dashboard",
                "http://dashboard.invalid",
                "--no-open",
            ])?;
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stdout)
                .contains("OAuth dashboard from --dashboard"));
            assert!(requests
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty());
            assert_eq!(std::fs::read(original.join("session.json"))?, bytes);
            assert_eq!(std::fs::read(root.join("client.json"))?, config_bytes);
        }
        std::fs::write(original.join("session.json"), stale_session)?;
        // An explicit loopback origin can override a stale server without changing
        // the account when authorization is denied. Other address settings are ignored.
        *mode.lock().map_err(|error| error.to_string())? = "access_denied".to_owned();
        let output = execute(&[
            "--json",
            "login",
            "--oauth",
            "--addr",
            &addr,
            "--dashboard",
            &addr,
            "--no-open",
        ])?;
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("browser authorization was denied")
        );
        assert_eq!(
            *requests.lock().map_err(|error| error.to_string())?,
            vec!["/oauth/device/code", "/oauth/token"]
        );
        assert_eq!(std::fs::read(root.join("client.json"))?, config_bytes);
        assert_eq!(std::fs::read(original.join("session.json"))?, stale_session);
        // The stored loopback fixture also works without --addr.
        let loopback_session = serde_json::to_vec(&json!({"user":"original", "addr":addr}))?;
        std::fs::write(original.join("session.json"), &loopback_session)?;
        let output = execute(&[
            "--json",
            "login",
            "--oauth",
            "--dashboard",
            &addr,
            "--no-open",
        ])?;
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("browser authorization was denied")
        );
        assert_eq!(
            std::fs::read(original.join("session.json"))?,
            loopback_session
        );
        assert_eq!(std::fs::read(root.join("client.json"))?, config_bytes);
        std::fs::write(original.join("session.json"), session_bytes)?;
        for reply in [
            "access_denied",
            "expired_token",
            "invalid_grant",
            "unexpected_error",
        ] {
            *mode.lock().map_err(|error| error.to_string())? = reply.to_owned();
            let output = execute(&[
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
            if output.status.success() {
                return Err(format!("OAuth failure {reply} was accepted").into());
            }
            if std::fs::read(root.join("client.json"))? != config_bytes
                || std::fs::read(original.join("session.json"))? != session_bytes
            {
                return Err("rejected authorization changed the original account".into());
            }
        }
        *mode.lock().map_err(|error| error.to_string())? = "authorized".to_owned();
        for args in [
            vec![
                "--json",
                "login",
                "--oauth",
                "--addr",
                &addr,
                "--dashboard",
                &addr,
                "--no-open",
                // Explicit empty input prevents fallback to an old target keyring entry.
                "--super=",
            ],
            vec![
                "--json",
                "login",
                "--interactive",
                "--addr",
                &addr,
                "--user",
                "authorized",
                "--pass=fixture-password",
                "--super=wrong-super",
            ],
        ] {
            let output = execute(&args)?;
            if output.status.success() {
                return Err("missing or incorrect super password was accepted".into());
            }
            if std::fs::read(root.join("client.json"))? != config_bytes
                || std::fs::read(original.join("session.json"))? != session_bytes
            {
                return Err("failed vault verification changed the original account".into());
            }
            if args.contains(&"--super=") {
                recover_account(&output, Some("super_password"))?;
            } else {
                recover_account(&output, None)?;
            }
        }
        *mode.lock().map_err(|error| error.to_string())? = "totp".to_owned();
        let totp = execute(&[
            "--json",
            "login",
            "--interactive",
            "--addr",
            &addr,
            "--user",
            "authorized",
            "--pass=fixture-password",
            "--super",
            &super_password,
        ])?;
        if totp.status.success() || std::fs::read(root.join("client.json"))? != config_bytes {
            return Err("headless TOTP changed the account before authorization".into());
        }
        recover_account(&totp, Some("totp"))?;
        *mode.lock().map_err(|error| error.to_string())? = "authorized".to_owned();
        let before_login: Value = serde_json::from_slice(
            &std::fs::read(root.join(".login-recovery.json"))?,
        )?;
        let prior_operations: Vec<String> = before_login["operations"]
            .as_array().ok_or("account receipt history missing before login")?
            .iter().map(|record| record["operation_id"].as_str()
                .map(str::to_owned).ok_or("prior account operation ID missing"))
            .collect::<Result<_, _>>()?;
        let authorized = execute(&[
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
        if !authorized.status.success() {
            authorization_failure_evidence(&root, &original, &authorized);
            return Err(format!(
                "authorized OAuth login failed: {} {}",
                String::from_utf8_lossy(&authorized.stdout),
                String::from_utf8_lossy(&authorized.stderr)
            )
            .into());
        }
        eprintln!("account-checkpoint oauth_completed");
        let envelope: Value = serde_json::from_slice(&authorized.stdout)?;
        if envelope["status"] != "ok" || envelope["summary"]["user"] != "authorized" {
            return Err("OAuth login did not report the verified account".into());
        }
        let selected: Value = serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
        let target = std::path::PathBuf::from(
            selected["data_dir"]
                .as_str()
                .ok_or("selected directory missing")?,
        );
        if !target.starts_with(&root)
            || selected["custom"] != "preserve"
            || selected["api_base"] != "https://previous.invalid"
        {
            return Err("login changed unrelated API preferences or escaped the fixture".into());
        }
        let saved_bytes = std::fs::read(target.join("session.json"))?;
        let saved: Value = serde_json::from_slice(&saved_bytes)?;
        if saved["user"] != "authorized"
            || saved.get("super").is_some()
            || saved.get("pass").is_some()
        {
            return Err("headless login persisted an unexpected credential".into());
        }
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/account/parts/authorization_migration_and_commit.rs"))
}
