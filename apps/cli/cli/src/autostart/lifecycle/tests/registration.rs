#[cfg(target_os = "linux")]
#[test]
fn fresh_autostart_resolves_before_registration_and_remains_installable() -> Result<()> {
    let _env = crate::runtime_fetch::runtime_fetch_tests::env_scope(&[
        "RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE", "RSRS_RUNTIME_BIN",
        "ONEMEMORY_RUNTIME_BIN", "RESPIRE_RUNTIME_BIN", "RSRS_RPC_PORT", "HOME", "USERPROFILE",
    ])?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    fs::create_dir(root.join("home"))?;
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("USERPROFILE", root.join("home"));
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_BIN_DIR", root.join("bin"));
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    for key in ["RSRS_RUNTIME_BIN", "ONEMEMORY_RUNTIME_BIN", "RESPIRE_RUNTIME_BIN"] {
        std::env::remove_var(key);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);
    let error = enable(false).err().context("fresh hermetic download unexpectedly succeeded")?;
    anyhow::ensure!(format!("{error:#}").contains("runtime downloads are disabled"));
    anyhow::ensure!(!control_path(&root).exists() && !marker_path(&root).exists());
    anyhow::ensure!(!crate::mcp::stable_bin_path().exists());

    crate::runtime_fetch::runtime_fetch_tests::install_lifecycle_fixture()?;
    let config = new_registration()?;
    let (_manager, state) = manager::test_adapter::install(&config);
    enable(false)?;
    anyhow::ensure!(state.borrow().registered && state.borrow().enabled && !state.borrow().running);
    anyhow::ensure!(read_registration(&root)?.context("fresh registration missing")?.enabled);
    anyhow::ensure!(!marker_path(&root).exists());
    anyhow::ensure!(!crate::mcp::bin_needs_refresh(&std::env::current_exe()?, &config.executable)
        .map_err(anyhow::Error::msg)?);
    Ok(())
}

#[test]
fn cold_start_archives_stale_endpoint_and_preserves_invalid_legacy_session() -> Result<()> {
    let _env = crate::runtime_fetch::runtime_fetch_tests::env_scope(&[
        "RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE", "RSRS_RPC_PORT",
    ])?;
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_BIN_DIR", root.join("bin"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);

    fs::write(root.join("session.json"), b"invalid legacy session")?;
    let endpoint = root.join("runtime/endpoint.json");
    fs::create_dir_all(endpoint.parent().context("legacy endpoint parent missing")?)?;
    let owner = br#"{"v":1,"pid":0}"#;
    fs::write(&endpoint, owner)?;
    let error = execute(&RuntimeCommand::Start).err().context("invalid legacy session was accepted")?;
    anyhow::ensure!(format!("{error:#}").contains("invalid historical session"));
    anyhow::ensure!(fs::read(root.join("session.json"))? == b"invalid legacy session");
    anyhow::ensure!(!endpoint.exists());
    let runtime_dir = endpoint.parent().context("legacy endpoint parent missing")?;
    let archived = fs::read_dir(runtime_dir)?.collect::<std::io::Result<Vec<_>>>()?;
    let archived_endpoint = archived.iter().find(|entry| entry.file_name().to_string_lossy()
        .starts_with("endpoint.stopped-")).context("stopped endpoint not archived")?;
    anyhow::ensure!(fs::read(archived_endpoint.path())? == owner);
    anyhow::ensure!(root.join("runtime/takeover/lock.db").is_file());
    anyhow::ensure!(!control_path(&root).exists() && !marker_path(&root).exists());
    anyhow::ensure!(!crate::runtime_fetch::manifest_path().exists());

    let _ownership = crate::runtime_policy::takeover_lock()?;
    fs::write(&endpoint, owner)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    let mut config = new_registration()?;
    let unrelated_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    config.environment.insert("RSRS_RPC_PORT".into(), unrelated_listener.local_addr()?.port().to_string());
    let path = control_path(&root);
    let legacy = b"obsolete registration";
    fs::create_dir_all(path.parent().context("registration parent missing")?)?;
    fs::write(&path, legacy)?;
    anyhow::ensure!(archive_stopped_registration_locked(&root, &config).is_err());
    anyhow::ensure!(fs::read(&path)? == legacy);
    drop(listener);
    let library = respire_client_host::lock::LibraryLock::acquire(&root, Duration::ZERO)?;
    anyhow::ensure!(archive_stopped_registration_locked(&root, &config).is_err());
    anyhow::ensure!(fs::read(&path)? == legacy);
    drop(library);
    #[cfg(target_os = "linux")]
    let (_manager, _state) = manager::test_adapter::install(&config);
    archive_stopped_registration_locked(&root, &config)?;
    anyhow::ensure!(!path.exists() && !endpoint.exists());
    let archived = fs::read_dir(path.parent().context("registration parent missing")?)?
        .collect::<std::io::Result<Vec<_>>>()?;
    anyhow::ensure!(archived.len() == 1 && fs::read(archived[0].path())? == legacy);

    config.enabled = false;
    config.manually_stopped = true;
    save(&mut config)?;
    let disabled = fs::read(&path)?;
    anyhow::ensure!(prepare_cold_restart_locked()?, "stopped disabled registration forced managed recovery");
    anyhow::ensure!(fs::read(&path)? == disabled && !marker_path(&root).exists());
    let _library = respire_client_host::lock::LibraryLock::acquire(&root, Duration::ZERO)?;
    anyhow::ensure!(prepare_cold_restart_locked().is_err(), "independent restart ignored the real library lock");
    anyhow::ensure!(fs::read(&path)? == disabled && !marker_path(&root).exists());
    Ok(())
}

#[test]
fn managed_launch_budget_is_bounded_persistent_and_registration_scoped() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut config = super::super::tests::fixture(Platform::current()?);
    config.installation = dir.path().canonicalize()?;
    anyhow::ensure!(record_attempt(&config, 100)?);
    anyhow::ensure!(record_attempt(&config, 101)?);
    anyhow::ensure!(record_attempt(&config, 102)?);
    anyhow::ensure!(!record_attempt(&config, 103)?);
    anyhow::ensure!(
        !record_attempt(&config, 99)?,
        "clock reversal must not reopen the budget"
    );
    anyhow::ensure!(record_attempt(&config, 222)?);
    let (value, _) = read_attempts(&config)?;
    anyhow::ensure!(value.starts == vec![222]);
    config.id = "another-installation".into();
    anyhow::ensure!(read_attempts(&config).is_err());
    Ok(())
}

#[test]
fn lifecycle_events_are_bounded_fixed_fields_and_private() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut config = super::super::tests::fixture(Platform::current()?);
    config.installation = dir.path().canonicalize()?;
    event(&config, "starting")?;
    anyhow::ensure!(event(&config, "secret-error-message").is_err());
    let path = control_path(&config.installation).with_file_name("events.jsonl");
    let bytes = fs::read(&path)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(value.as_object().context("event object missing")?.len() == 6);
    anyhow::ensure!(value["event"] == "starting" && value["id"] == config.id);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(fs::metadata(&path)?.permissions().mode() & 0o777 == 0o600);
    }
    fs::write(&path, vec![b' '; 65500])?;
    event(&config, "exit_ok")?;
    anyhow::ensure!(fs::metadata(&path)?.len() < 1024);
    anyhow::ensure!(fs::metadata(path.with_file_name("events.previous.jsonl"))?.len() == 65500);
    let backup = path.with_file_name("events.previous.jsonl");
    let full = vec![b' '; 65500];
    fs::write(&path, &full)?;
    fs::write(&backup, b"previous rotation")?;
    event(&config, "exit_error")?;
    anyhow::ensure!(fs::read(&backup)? == full);
    fs::remove_file(&backup)?;
    fs::create_dir(&backup)?;
    fs::write(&path, &full)?;
    anyhow::ensure!(event(&config, "exit_ok").is_err() && fs::read(&path)? == full);
    Ok(())
}
