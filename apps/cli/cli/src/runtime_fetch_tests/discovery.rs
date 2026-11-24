#[test]
fn explicit_runtime_override_keeps_its_existing_trust_policy() -> Result<()> {
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_RUNTIME_BIN"])?;
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    // An explicit selection still precedes managed discovery; no executable is run.
    fs::create_dir_all(manifest_path().parent().context("manifest parent missing")?)?;
    fs::write(manifest_path(), b"invalid managed selection")?;
    let explicit = dir.path().join(RUNTIME_BINARY);
    fs::write(&explicit, b"synthetic explicit runtime")?;
    std::env::set_var("RSRS_RUNTIME_BIN", &explicit);
    anyhow::ensure!(resolve_runtime_bin()? == explicit);
    Ok(())
}

#[test]
fn runtime_staging_and_cli_update_preserve_legacy_data_before_launch() -> Result<()> {
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE", "RSRS_RPC_PORT", "RSRS_RUNTIME_BIN"])?;
    std::env::remove_var("RSRS_RUNTIME_BIN");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    std::env::set_var("RSRS_RPC_PORT", listener.local_addr()?.port().to_string());
    drop(listener);
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    fs::create_dir(dir.path().join("bin"))?;
    let legacy = crate::mcp::stable_bin_path();
    let legacy_bytes = b"legacy combined CLI image; not executable";
    fs::write(&legacy, legacy_bytes)?;
    let session = dir.path().join("session.json");
    let library = dir.path().join("rsrs.db");
    fs::write(&session, b"legacy session bytes")?;
    fs::write(&library, b"legacy data bytes")?;
    let endpoint = dir.path().join("runtime/endpoint.json");
    fs::create_dir(dir.path().join("runtime"))?;
    fs::write(&endpoint, br#"{"v":1,"pid":0}"#)?;
    let error = download_and_install(None, None)
        .err()
        .context("hermetic download was allowed")?;
    anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
    let materialized = crate::mcp::materialize_bin().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(materialized == legacy);
    anyhow::ensure!(!crate::mcp::bin_needs_refresh(&std::env::current_exe()?, &legacy)
        .map_err(anyhow::Error::msg)?);
    let error = crate::rpc::restart_runtime(Duration::from_secs(1))
        .err().context("restart was allowed without a verified replacement")?;
    anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
    anyhow::ensure!(fs::read(&session)? == b"legacy session bytes");
    anyhow::ensure!(fs::read(&library)? == b"legacy data bytes");
    anyhow::ensure!(fs::read(&endpoint)? == br#"{"v":1,"pid":0}"#);
    anyhow::ensure!(!runtime_dir().exists() && !manifest_path().exists());
    Ok(())
}

#[test]
fn automatic_install_rejects_non_directory_binary_ancestors_without_writes() -> Result<()> {
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE", "RSRS_RUNTIME_BIN"])?;
    std::env::remove_var("RSRS_RUNTIME_BIN");
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("fresh-installation");
    let blocked = dir.path().join("blocked");
    fs::write(&blocked, b"existing non-directory bytes")?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    // Cover Windows PATH_NOT_FOUND as well as Unix ENOTDIR, including a
    // missing suffix below the obstructing ancestor. Never run/download bytes.
    for binary_dir in [blocked.clone(), blocked.join("missing/nested")] {
        std::env::set_var("RSRS_BIN_DIR", &binary_dir);
        let error = require_automatic_migration_safe()
            .err().context("non-directory binary ancestor passed admission")?;
        anyhow::ensure!(error.is::<LegacyRuntimeMigrationRequired>());
        let error = download_and_install(None, None)
            .err().context("hermetic download was allowed")?;
        anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
        anyhow::ensure!(!root.exists(), "rejected binary path created installation state");
        anyhow::ensure!(fs::read(&blocked)? == b"existing non-directory bytes");
    }
    // A genuinely missing directory chain still admits a fresh install.
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("fresh/missing/bin"));
    require_automatic_migration_safe()?;
    anyhow::ensure!(!root.exists() && !dir.path().join("fresh").exists());
    let selected = install_lifecycle_fixture()?;
    std::env::set_var("RSRS_BIN_DIR", &blocked);
    require_automatic_migration_safe()?;
    anyhow::ensure!(resolve_runtime_bin()? == selected);
    Ok(())
}

#[test]
fn automatic_install_allows_fresh_and_stopped_legacy_cli() -> Result<()> {
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_RPC_PORT", "RSRS_CORE_TEST_MODE", "RSRS_RUNTIME_BIN"])?;
    std::env::remove_var("RSRS_RUNTIME_BIN");
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    let required = &pinned()?.required;
    anyhow::ensure!(selected_install_for_reuse(Some(required))?.is_none());
    anyhow::ensure!(!runtime_dir().exists() && !manifest_path().exists());
    fs::write(dir.path().join("client.json"), br#"{"language":"en"}"#)?;
    anyhow::ensure!(selected_install_for_reuse(Some(required))?.is_none(),
        "fresh preferences were mistaken for an old installation");
    fs::create_dir(dir.path().join("bin"))?;
    let legacy_cli = crate::mcp::stable_bin_path();
    fs::write(&legacy_cli, b"previous CLI outside any release list")?;
    let endpoint = crate::rpc::endpoint_path();
    fs::create_dir_all(endpoint.parent().context("endpoint parent missing")?)?;
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let url = format!("http://{}", port.local_addr()?);
    let mut previous = std::process::Command::new(std::env::current_exe()?)
        .arg("--list").stdout(std::process::Stdio::null()).spawn()?;
    let pid = previous.id();
    anyhow::ensure!(previous.wait()?.success(), "previous process did not exit cleanly");
    let record = serde_json::to_vec(&serde_json::json!({
        "v": 1, "pid": pid, "bin": "1.0.21-dev.3", "url": url,
    }))?;
    fs::write(&endpoint, &record)?;
    anyhow::ensure!(selected_install_for_reuse(Some(required))?.is_none(), "listener affected package selection");
    anyhow::ensure!(require_automatic_migration_safe().is_err(), "unverified launch owner was admitted");
    drop(port);
    anyhow::ensure!(selected_install_for_reuse(Some(required))?.is_none());
    let error = download_and_install(None, None)
        .err()
        .context("hermetic download was allowed")?;
    anyhow::ensure!(!error.is::<LegacyRuntimeMigrationRequired>());
    anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
    anyhow::ensure!(fs::read(&legacy_cli)? == b"previous CLI outside any release list");
    anyhow::ensure!(fs::read(&endpoint)? == record);
    anyhow::ensure!(!runtime_dir().exists() && !manifest_path().exists());
    std::env::set_var("RSRS_RPC_PORT", url.rsplit_once(':').context("fixture port missing")?.1);
    let old_listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let recycled = serde_json::to_vec(&serde_json::json!({
        "v": 1, "pid": std::process::id(), "bin": "1.0.21-dev.3",
        "url": format!("http://{}", old_listener.local_addr()?),
    }))?;
    let binary = install_lifecycle_fixture()?;
    fs::write(&endpoint, &recycled)?;
    let _error = crate::rpc::restart_runtime(Duration::from_secs(1))
        .err().context("the synthetic runtime started")?;
    anyhow::ensure!(!endpoint.exists());
    let archived = fs::read_dir(endpoint.parent().context("endpoint parent missing")?)?
        .collect::<std::io::Result<Vec<_>>>()?;
    let archive = archived.into_iter().find(|entry| entry.file_name().to_string_lossy().starts_with("endpoint.stopped-"))
        .context("endpoint archive missing")?;
    anyhow::ensure!(fs::read(archive.path())? == recycled);
    let directory = binary.parent().context("fixture directory missing")?;
    let legacy = dir.path().join("runtime").join(required);
    fs::create_dir_all(legacy.join("lib"))?;
    for name in [RUNTIME_BINARY, CORE_MANIFEST_FILE, "lib/provider.bin"] {
        fs::copy(directory.join(name), legacy.join(name))?;
    }
    let image = legacy.join(RUNTIME_BINARY);
    anyhow::ensure!(verified_migrated_image(&image.to_string_lossy())? == Some(image.clone()));
    fs::write(legacy.join("lib/provider.bin"), b"tampered")?;
    anyhow::ensure!(verified_migrated_image(&image.to_string_lossy()).is_err());
    Ok(())
}

#[test]
fn automatic_install_preserves_legacy_data_without_requiring_owner_handoff() -> Result<()> {
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE"])?;
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    for relative in [
        "runtime/endpoint.json", "runtime/autostart/registration.json", "client.json",
    ] {
        let dir = tempfile::tempdir()?;
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
        let record = dir.path().join(relative);
        fs::create_dir_all(record.parent().context("legacy record parent missing")?)?;
        fs::write(&record, b"existing legacy state")?;
        let error = download_and_install(None, None)
            .err().context("hermetic download was allowed")?;
        anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"), "old record blocked staging: {relative}");
        anyhow::ensure!(fs::read(&record)? == b"existing legacy state");
        anyhow::ensure!(!manifest_path().exists() && !runtime_dir().join(".install.lock").exists());
        anyhow::ensure!(!crate::mcp::stable_bin_path().exists());
    }
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
    let profile = dir.path().join("accounts/previous");
    fs::create_dir_all(&profile)?;
    for root in [dir.path(), profile.as_path()] {
        for name in ["session.json", "rsrs.db", "onememory.db"] {
            fs::write(root.join(name), b"old account bytes")?;
        }
    }
    fs::write(dir.path().join("client.json"),
        serde_json::json!({"data_dir": profile}).to_string())?;
    anyhow::ensure!(selected_install_for_reuse(None)?.is_none());
    let error = download_and_install(None, None)
        .err().context("source test unexpectedly downloaded a Runtime")?;
    anyhow::ensure!(!error.is::<LegacyRuntimeMigrationRequired>(),
        "stopped account data blocked installation");
    anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
    for root in [dir.path(), profile.as_path()] {
        for name in ["session.json", "rsrs.db", "onememory.db"] {
            anyhow::ensure!(fs::read(root.join(name))? == b"old account bytes");
        }
    }
    anyhow::ensure!(!manifest_path().exists() && !runtime_dir().exists());
    Ok(())
}

#[test]
#[ignore = "requires the independently verified official GNU CLI v1.0.20 binary"]
fn automatic_install_admits_official_cli_1_0_20() -> Result<()> {
    let fixture = PathBuf::from(std::env::var("RSRS_TEST_LEGACY_CLI_BIN")
        .context("set RSRS_TEST_LEGACY_CLI_BIN to the official GNU v1.0.20 binary")?);
    let expected = "c03c131c61819a5f4d132cfe328f7c1da82c2f0b21a9c87d2b8870d1541a4c3e";
    anyhow::ensure!(sha256_file(&fixture)? == expected, "wrong official legacy fixture digest");
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_CORE_TEST_MODE"])?;
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    fs::create_dir(dir.path().join("bin"))?;
    let legacy = crate::mcp::stable_bin_path();
    fs::copy(fixture, &legacy)?;
    let error = download_and_install(None, None)
        .err()
        .context("hermetic download was allowed")?;
    anyhow::ensure!(format!("{error:#}").contains("downloads are disabled"));
    anyhow::ensure!(sha256_file(&legacy)? == expected);
    anyhow::ensure!(!runtime_dir().exists() && !manifest_path().exists());
    Ok(())
}

#[test]
fn release_triple_override_is_validated() -> Result<()> {
    let _env = env_scope(&["RSRS_RUNTIME_TRIPLE"])?;
    std::env::remove_var("RSRS_RUNTIME_TRIPLE");
    if cfg!(target_os = "linux") && matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
        let abi = if cfg!(target_env = "musl") { "musl" } else { "gnu" };
        anyhow::ensure!(
            release_triple()? == format!("{}-unknown-linux-{abi}", std::env::consts::ARCH)
        );
    }
    std::env::set_var("RSRS_RUNTIME_TRIPLE", "aarch64-unknown-linux-musl");
    anyhow::ensure!(release_triple()? == "aarch64-unknown-linux-musl");
    std::env::set_var("RSRS_RUNTIME_TRIPLE", "not-a-triple");
    anyhow::ensure!(release_triple().is_err());
    Ok(())
}

#[test]
fn mirror_override_builds_fallback_origins() -> Result<()> {
    let _env = env_scope(&["RSRS_MIRROR", "RSRS_DATA_DIR"])?;
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    let official = RELEASE_BASE.to_owned();
    let approved = vec![format!("{MIRROR_PREFIX}/{official}"), official.clone()];
    anyhow::ensure!(download_origins(Some(MIRROR_PREFIX))? == approved);
    anyhow::ensure!(
        download_origins(Some(MIRROR_PREFIX.trim_start_matches("https://")))? == approved
    );
    anyhow::ensure!(download_origins(Some(RELEASE_BASE))? == vec![official.clone()]);
    for origin in [
        "https://mirror.example",
        "mirror.example",
        "http://gh-proxy.com",
        "https://user:secret@gh-proxy.com",
        "https://gh-proxy.com/path",
        "https://gh-proxy.com?query=1",
        "https://gh-proxy.com#fragment",
    ] {
        anyhow::ensure!(
            download_origins(Some(origin)).is_err(),
            "unapproved origin accepted: {origin}"
        );
    }
    std::env::remove_var("RSRS_MIRROR");
    anyhow::ensure!(
        download_origins(None)? == vec![official.clone(), format!("{MIRROR_PREFIX}/{official}")]
    );
    Ok(())
}
