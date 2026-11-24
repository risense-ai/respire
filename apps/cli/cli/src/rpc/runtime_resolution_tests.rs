use super::*;

#[test]
fn startup_errors_propagate_and_cli_cannot_pass_as_runtime() -> Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow::anyhow!("test environment lock poisoned: {error}"))?;
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    let keys = [
        "RSRS_DATA_DIR",
        "RSRS_BIN_DIR",
        "RSRS_CORE_TEST_MODE",
        "RSRS_RUNTIME_BIN",
        "ONEMEMORY_RUNTIME_BIN",
        "RESPIRE_RUNTIME_BIN",
        "RSRS_RPC_PORT",
    ];
    let _restore = Restore(
        keys.iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect(),
    );
    let directory = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", directory.path());
    std::env::set_var("RSRS_BIN_DIR", directory.path().join("bin"));
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    for key in &keys[3..6] {
        std::env::remove_var(key);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);

    // Idle asset maintenance must never resolve or spawn the selected runtime.
    std::env::set_var("RSRS_RUNTIME_BIN", directory.path().join("missing-runtime"));
    let config_path = respire_client_host::paths::client_config_path();
    change_host_assets(|| {
        std::fs::write(&config_path, b"{}")?;
        Ok(())
    })?;
    let error = change_host_assets(|| {
        std::fs::write(&config_path, b"{\"changed\":true}")?;
        bail!("synthetic asset failure")
    })
    .err()
    .context("asset failure was swallowed")?;
    anyhow::ensure!(format!("{error:#}") == "synthetic asset failure");
    anyhow::ensure!(std::fs::read(&config_path)? == b"{}");
    anyhow::ensure!(!endpoint_path().exists() && !directory.path().join("bin").exists());
    std::fs::remove_file(&config_path)?;
    let error = change_host_assets(|| bail!("synthetic absent-config failure"))
        .err()
        .context("absent-config asset failure was swallowed")?;
    anyhow::ensure!(format!("{error:#}") == "synthetic absent-config failure");
    anyhow::ensure!(!config_path.exists() && !endpoint_path().exists());
    let endpoint = endpoint_path();
    std::fs::create_dir_all(endpoint.parent().context("test endpoint parent missing")?)?;
    std::fs::write(&endpoint, br#"{"v":1,"pid":0}"#)?;
    let mut changed = false;
    anyhow::ensure!(change_host_assets(|| {
        changed = true;
        Ok(())
    })
    .is_err());
    anyhow::ensure!(!changed && endpoint.exists() && !config_path.exists());
    let endpoint_before = std::fs::read(&endpoint)?;
    let error = restart_runtime(Duration::from_secs(1))
        .err()
        .context("restart accepted an owner without persisted recovery evidence")?;
    anyhow::ensure!(format!("{error:#}").contains("RSRS_RUNTIME_BIN does not name a file"),
        "restart did not verify the replacement before retiring the endpoint: {error:#}");
    anyhow::ensure!(
        std::fs::read(&endpoint)? == endpoint_before
            && !config_path.exists()
            && !directory.path().join("bin").exists(),
        "rejected restart changed stale ownership or started materialization"
    );
    std::fs::remove_file(&endpoint)?;
    {
        use sha2::{Digest, Sha256};
        let _takeover = crate::runtime_policy::takeover_lock()?;
        let image = directory.path().join("recovery-runtime");
        std::fs::write(&image, b"isolated recovery image")?;
        let image = image.canonicalize()?;
        std::env::set_var("RSRS_RUNTIME_BIN", &image);
        let root = directory.path().canonicalize()?;
        let profile = respire_client_host::config::control_roots()?.1.canonicalize()?;
        let account = respire_client_host::identity::session_user_of_dir(&profile)?.unwrap_or_default();
        let account = if account.is_empty() { profile.display().to_string() } else { account };
        let digest = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
        let version = &crate::runtime_fetch::pinned()?.required;
        let birth_prefix = if cfg!(windows) { "windows-filetime-" }
            else if cfg!(target_os = "linux") { "linux-starttime-" } else { "darwin-lstart-" };
        let launch = root.join("runtime/autostart/launch");
        std::fs::create_dir_all(&launch)?;
        for mode in ["normal", "readonly", "off"] {
            std::fs::write(&config_path, serde_json::to_vec(&json!({"service_mode":mode}))?)?;
            let instance = uuid::Uuid::new_v4().to_string();
            let grant = serde_json::to_string(&json!({
                "schema_version":1, "protocol_version":PROTOCOL_V,
                "nonce":instance, "instance_id":instance,
                "installation_root":root, "installation_id":digest(root.display().to_string().as_bytes()),
                "profile":profile, "account_id":digest(account.as_bytes()), "sync_generation":0,
                "image_path":image, "image_sha256":digest(b"isolated recovery image"),
                "supervisor_pid":u32::MAX-1, "supervisor_birth":"456", "managed_id":null,
                "registration_sha256":null, "maintenance":null,
                "port":crate::net_rpc::rpc_port(), "issued_at_ms":1, "expires_at_ms":2,
            }))?;
            let receipt = serde_json::to_vec(&json!({
                "schema_version":2, "protocol_version":PROTOCOL_V, "state":"consumed",
                "grant_json":grant, "grant_sha256":digest(grant.as_bytes()), "registration_json":null,
                "pid":u32::MAX, "process_start_id":"123",
            }))?;
            let claim = launch.join(format!("launch-{instance}.claimed"));
            std::fs::write(&claim, &receipt)?;
            let record = json!({
                "schema_version":2, "server":"respire", "v":PROTOCOL_V, "bin":version,
                "runtime_version":version, "url":crate::net_rpc::rpc_base_url(),
                "pid":u32::MAX, "process_start_id":format!("{birth_prefix}123"),
                "instance_id":instance, "installation_root":root, "data_dir":profile,
                "installation_id":digest(root.display().to_string().as_bytes()),
                "account_id":digest(account.as_bytes()), "generation":7,
                "exe":image, "image_path":image, "image_sha256":digest(b"isolated recovery image"),
                "managed_id":null, "launch_receipt_sha256":digest(&receipt),
            });
            for (field, invalid) in [
                ("generation", json!(0)), ("account_id", json!("foreign-account")),
                ("image_sha256", json!("f".repeat(64))), ("process_start_id", json!("reused-pid")),
                ("launch_receipt_sha256", json!("f".repeat(64))), ("pid", json!(std::process::id())),
            ] {
                let mut changed = record.clone();
                changed[field] = invalid;
                let bytes = serde_json::to_vec(&changed)?;
                std::fs::write(&endpoint, &bytes)?;
                anyhow::ensure!(crate::autostart::StaleOwner::prepare_locked().is_err(), "accepted changed {field} in {mode}");
                anyhow::ensure!(std::fs::read(&endpoint)? == bytes && std::fs::read(&claim)? == receipt);
            }
            let bytes = serde_json::to_vec(&record)?;
            std::fs::write(&endpoint, &bytes)?;
            let session = profile.join("session.json");
            std::fs::write(&session, b"invalid account metadata")?;
            anyhow::ensure!(crate::autostart::StaleOwner::prepare_locked().is_err(),
                "unreadable account identity became an anonymous profile in {mode}");
            anyhow::ensure!(std::fs::read(&endpoint)? == bytes && std::fs::read(&claim)? == receipt);
            std::fs::remove_file(session)?;
            let stale = crate::autostart::StaleOwner::prepare_locked()?;
            std::fs::write(&endpoint, b"changed after verification")?;
            anyhow::ensure!(stale.remove_endpoint_locked().is_err());
            anyhow::ensure!(endpoint.exists() && std::fs::read(&claim)? == receipt);
            std::fs::write(&endpoint, &bytes)?;
            crate::autostart::StaleOwner::prepare_locked()?.remove_endpoint_locked()?;
            anyhow::ensure!(!endpoint.exists() && std::fs::read(&claim)? == receipt
                && !root.join("rsrs.db").exists(), "recovery touched the store or consumed tombstone in {mode}");
        }
        std::fs::remove_file(&config_path)?;
    }
    std::env::remove_var("RSRS_RUNTIME_BIN");
    let cli = std::env::current_exe()?;
    let health: crate::net_rpc::Health = serde_json::from_value(json!({
        "server": "rsrs", "bin": env!("CARGO_PKG_VERSION"), "pid": 42, "v": PROTOCOL_V,
        "exe": cli, "data_dir": respire_client_host::config::control_roots()?.1,
        "runtime_version": crate::runtime_fetch::pinned()?.required,
        "store_ready": true,
    }))?;
    anyhow::ensure!(
        !runtime_acceptable(&health)?,
        "unselected CLI passed as a Runtime"
    );

    // Invalid explicit selections fail without a download or materialized shim.
    std::env::set_var("RSRS_RUNTIME_BIN", directory.path().join("missing-runtime"));
    anyhow::ensure!(resolve_runtime_executable().is_err());
    anyhow::ensure!(!directory.path().join("bin").exists());
    std::env::remove_var("RSRS_RUNTIME_BIN");
    std::fs::create_dir_all(directory.path().join("bin"))?;
    std::fs::write(
        crate::runtime_fetch::manifest_path(),
        serde_json::to_vec(&json!({
            "version": crate::runtime_fetch::pinned()?.required, "sha256": "1".repeat(64),
        }))?,
    )?;
    let error = resolve_runtime_executable()
        .err()
        .context("legacy proof was accepted")?;
    anyhow::ensure!(error.is::<crate::runtime_fetch::ManagedRuntimeRepairRequired>());
    let error = runtime_acceptable(&health)
        .err()
        .context("legacy proof was accepted during runtime reuse")?;
    anyhow::ensure!(error.is::<crate::runtime_fetch::ManagedRuntimeRepairRequired>());
    let (root, profile) = respire_client_host::config::control_roots()?;
    let mut independent = health.clone();
    independent.exe = directory.path().join("independent-runtime").display().to_string();
    independent.installation_root = root.display().to_string();
    independent.data_dir = profile.display().to_string();
    independent.runtime_version = "3.1.4".into();
    independent.managed_id = Some("external-host".into());
    independent.generation = 1;
    independent.account_id = "account-identity".into();
    independent.instance_id = "instance-identity".into();
    anyhow::ensure!(reusable_business_runtime(&independent)?, "scoped RPC must not depend on the local install receipt");
    independent.store_ready = false;
    independent.credential_state = "required".into();
    assert!(reusable_business_runtime(&independent)?);
    independent.data_dir = directory.path().join("other-library").display().to_string();
    anyhow::ensure!(!reusable_business_runtime(&independent)?, "RPC selected a different library");
    std::fs::remove_file(crate::runtime_fetch::manifest_path())?;

    // Even an explicit override cannot turn this CLI into a Runtime.
    std::env::set_var("RSRS_RUNTIME_BIN", &cli);
    anyhow::ensure!(explicit_cli_runtime(&cli)?);
    anyhow::ensure!(resolve_runtime_executable().is_err());
    anyhow::ensure!(
        !runtime_acceptable(&health)?,
        "CLI override passed as a Runtime"
    );
    Ok(())
}
