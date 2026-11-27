use super::*;
use serde_json::json;

#[test]
fn preserves_unknown_keys_and_profile_boundaries() -> Result<()> {
    let isolated = crate::test_lock::Isolate::new()?;
    let profile = isolated.path().join("accounts/test");
    fs::create_dir_all(&profile)?;
    fs::write(
        client_config_path(),
        serde_json::to_vec(&json!({"data_dir":profile,"unknown":{"nested":true}}))?,
    )?;
    fs::write(
        profile.join("agent.json"),
        br#"{"diary_mode":"concise","unrelated":[1,2]}"#,
    )?;
    let ownership = LibraryLock::acquire(&profile, Duration::ZERO)?;
    let mut transaction = HostConfigTransaction::begin(&ownership)?;
    transaction.client["lang"] = json!("zh");
    transaction.client["model_mirror"] = json!("https://hf-mirror.net");
    transaction.client["proxy_url"] = Value::Null;
    transaction.agent["diary_mode"] = json!("verbose");
    transaction.commit()?;
    let client = read_client_config_for_write()?;
    assert_eq!(client["unknown"], json!({"nested":true}));
    assert_eq!(client["lang"], "zh");
    assert_eq!(client["model_mirror"], "https://hf-mirror.net");
    let agent = object(read_bytes(&profile.join("agent.json"))?.as_deref())?;
    assert_eq!(agent["unrelated"], json!([1, 2]));
    assert_eq!(agent["diary_mode"], "verbose");
    assert!(!isolated.path().join("agent.json").exists());
    assert!(!journal_path().exists());
    Ok(())
}

#[test]
fn malformed_or_unowned_configuration_is_not_overwritten() -> Result<()> {
    let isolated = crate::test_lock::Isolate::new()?;
    let other = tempfile::tempdir()?;
    let wrong = LibraryLock::acquire(other.path(), Duration::ZERO)?;
    assert!(HostConfigTransaction::begin(&wrong).is_err());
    let owner = LibraryLock::acquire(isolated.path(), Duration::ZERO)?;
    let busy = client_config_lock()?;
    assert!(HostConfigTransaction::begin(&owner).is_err());
    drop(busy);
    fs::write(client_config_path(), b"{")?;
    assert!(HostConfigTransaction::begin(&owner).is_err());
    assert_eq!(fs::read(client_config_path())?, b"{");
    fs::write(client_config_path(), b"{}")?;
    fs::write(isolated.path().join("agent.json"), b"[]")?;
    assert!(HostConfigTransaction::begin(&owner).is_err());
    assert_eq!(fs::read(isolated.path().join("agent.json"))?, b"[]");
    assert!(!journal_path().exists());
    Ok(())
}

#[cfg(windows)]
#[test]
fn second_file_publication_failure_rolls_back_exact_bytes() -> Result<()> {
    let isolated = crate::test_lock::Isolate::new()?;
    let client = b"{ \"unknown\": [1,2] }\r\n";
    let agent = b"{ \"diary_mode\": \"concise\", \"unknown\": true }\r\n";
    fs::write(client_config_path(), client)?;
    let agent_path = isolated.path().join("agent.json");
    fs::write(&agent_path, agent)?;
    let owner = LibraryLock::acquire(isolated.path(), Duration::ZERO)?;
    let mut transaction = HostConfigTransaction::begin(&owner)?;
    transaction.client["lang"] = json!("zh");
    transaction.agent["diary_mode"] = json!("verbose");
    let original_permissions = fs::metadata(&agent_path)?.permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&agent_path, readonly)?;
    let result = transaction.commit();
    fs::set_permissions(&agent_path, original_permissions)?;
    assert!(result.is_err());
    assert_eq!(fs::read(client_config_path())?, client);
    assert_eq!(fs::read(&agent_path)?, agent);
    assert!(!journal_path().exists());
    Ok(())
}

#[test]
fn config_writer_in_another_process_waits_for_publication() -> Result<()> {
    const CHILD: &str = "RESPIRE_HOST_CONFIG_WRITER";
    if std::env::var_os(CHILD).is_some() {
        assert!(LibraryLock::acquire(&data_dir(), Duration::ZERO).is_err());
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(client_config_path().with_extension("json.lock"))?;
        assert!(matches!(
            probe.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        fs::write(data_dir().join("writer-ready"), b"ready")?;
        set_cure_auto(false)?;
        return Ok(());
    }
    let isolated = crate::test_lock::Isolate::new()?;
    let owner = LibraryLock::acquire(isolated.path(), Duration::ZERO)?;
    let mut transaction = HostConfigTransaction::begin(&owner)?;
    transaction.client["lang"] = json!("zh");
    transaction.agent["diary_mode"] = json!("verbose");
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "service::host_config_transaction::tests::config_writer_in_another_process_waits_for_publication", "--nocapture"])
        .env(CHILD, "1").env("RSRS_DATA_DIR", isolated.path()).spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !isolated.path().join("writer-ready").exists() {
        ensure!(
            std::time::Instant::now() < deadline,
            "config writer did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        child.try_wait()?.is_none(),
        "another config writer crossed the held config lock"
    );
    transaction.commit()?;
    assert!(child.wait()?.success());
    let config = read_client_config_for_write()?;
    assert_eq!(config["lang"], "zh");
    assert_eq!(config["cure_auto"], false);
    assert_eq!(
        object(read_bytes(&isolated.path().join("agent.json"))?.as_deref())?["diary_mode"],
        "verbose"
    );
    Ok(())
}

fn candidate(transaction: &mut HostConfigTransaction<'_>) -> Result<()> {
    transaction.client["lang"] = json!("zh");
    transaction.agent["diary_mode"] = json!("verbose");
    transaction.journal.client_after = Some(serde_json::to_vec_pretty(&transaction.client)?);
    transaction.journal.agent_after = Some(serde_json::to_vec_pretty(&transaction.agent)?);
    Ok(())
}

#[test]
fn recovers_durable_states_after_process_exit() -> Result<()> {
    const CHILD: &str = "RESPIRE_HOST_CONFIG_CRASH_STAGE";
    if let Ok(stage) = std::env::var(CHILD) {
        let owner = LibraryLock::acquire(&data_dir(), Duration::ZERO)?;
        let mut transaction = HostConfigTransaction::begin(&owner)?;
        candidate(&mut transaction)?;
        transaction.journal.save()?;
        if stage != "prepared" {
            replace(
                &client_config_path(),
                transaction.journal.client_after.as_deref(),
            )?;
        }
        if matches!(stage.as_str(), "both" | "committed") {
            replace(
                &data_dir().join("agent.json"),
                transaction.journal.agent_after.as_deref(),
            )?;
        }
        if stage == "committed" {
            transaction.journal.committed = true;
            transaction.journal.save()?;
        }
        std::process::exit(73);
    }
    for stage in ["prepared", "client", "both", "committed"] {
        let isolated = crate::test_lock::Isolate::new()?;
        let profile = isolated.path().join("accounts/test");
        fs::create_dir_all(&profile)?;
        let before = serde_json::to_vec(&json!({"data_dir":profile,"unknown":[1,2]}))?;
        fs::write(client_config_path(), &before)?;
        let result = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "service::host_config_transaction::tests::recovers_durable_states_after_process_exit", "--nocapture"])
            .env(CHILD, stage).env("RSRS_DATA_DIR", isolated.path()).output()?;
        assert_eq!(
            result.status.code(),
            Some(73),
            "{stage}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(journal_path().exists());
        assert!(read_client_config_for_write().is_err());
        assert!(client_config_lock().is_err());
        assert!(write_agent_config_key("diary_mode", &json!("concise")).is_err());
        recover_host_config()?;
        if stage == "committed" {
            assert_eq!(read_client_config_for_write()?["lang"], "zh");
            assert_eq!(
                object(read_bytes(&profile.join("agent.json"))?.as_deref())?["diary_mode"],
                "verbose"
            );
        } else {
            assert_eq!(fs::read(client_config_path())?, before);
            assert!(!profile.join("agent.json").exists());
        }
        recover_host_config()?;
        assert!(!journal_path().exists());
    }
    Ok(())
}

#[test]
fn recovery_keeps_journal_and_every_file_on_conflict() -> Result<()> {
    let isolated = crate::test_lock::Isolate::new()?;
    let owner = LibraryLock::acquire(isolated.path(), Duration::ZERO)?;
    let mut transaction = HostConfigTransaction::begin(&owner)?;
    candidate(&mut transaction)?;
    transaction.journal.save()?;
    replace(
        &client_config_path(),
        transaction.journal.client_after.as_deref(),
    )?;
    let published = fs::read(client_config_path())?;
    fs::write(
        isolated.path().join("agent.json"),
        br#"{"later_writer":true}"#,
    )?;
    drop(transaction);
    drop(owner);
    assert!(recover_host_config().is_err());
    assert_eq!(fs::read(client_config_path())?, published);
    assert_eq!(
        fs::read(isolated.path().join("agent.json"))?,
        br#"{"later_writer":true}"#
    );
    assert!(journal_path().exists());
    Ok(())
}
