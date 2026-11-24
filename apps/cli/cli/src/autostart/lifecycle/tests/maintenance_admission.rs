#[test]
fn offline_keygen_admission_failure_cannot_create_maintenance() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK.lock().map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
            for (name, value) in [("RSRS_DATA_DIR", &self.0), ("RSRS_RUNTIME_BOOT_TOKEN", &self.1)] {
                match value { Some(value) => std::env::set_var(name, value), None => std::env::remove_var(name) }
            }
        }
    }
    let _restore = Restore(std::env::var_os("RSRS_DATA_DIR"), std::env::var_os("RSRS_RUNTIME_BOOT_TOKEN"));
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::remove_var("RSRS_RUNTIME_BOOT_TOKEN");
    ACTIVE.with(|slot| *slot.borrow_mut() = None);
    let mut admitted = false;
    let result = begin_offline_keygen_maintenance(&root, &root, |token, coordinator, digest| {
        assert!(!marker_path(&root).exists() && !control_path(&root).exists());
        assert!(!token.is_empty() && !coordinator.is_empty() && digest.len() == 64);
        admitted = true;
        anyhow::bail!("synthetic admission publication failure")
    });
    assert!(admitted && result.is_err());
    assert!(!marker_path(&root).exists() && !control_path(&root).exists() && current_boot_token().is_none());
    assert!(verify_offline_keygen_marker(&root, &root, "absent", "absent", "absent").is_err());
    Ok(())
}

#[test]
fn prepared_offline_keygen_recovers_absent_or_exact_paused_marker_only() -> Result<()> {
    let _env=crate::runtime_fetch::runtime_fetch_tests::env_scope(&[
        "RSRS_DATA_DIR","RSRS_RUNTIME_BOOT_TOKEN"])?;
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR",&root);
    std::env::remove_var("RSRS_RUNTIME_BOOT_TOKEN");
    assert!(current_boot_token().is_none());
    let token=uuid::Uuid::new_v4().to_string();
    let coordinator=format!("host-{}",hash(root.display().to_string().as_bytes()));
    let resume=|token:&str,digest:&str,allow_absent:bool|
        resume_offline_keygen_prelaunch_maintenance(&root,&root,token,&coordinator,digest,None,allow_absent,|_,_| Ok(()));
    assert!(resume(&token,&"a".repeat(64),false).is_err());
    assert!(!marker_path(&root).exists());
    // Prepared admission exists, but its initial marker publish did not.
    let mut maintenance=resume(&token,&"a".repeat(64),true)?;
    let binding=maintenance.auth_launch_binding(&root)?.context("prepared binding missing")?;
    let digest=binding["marker_sha256"].as_str().context("prepared digest missing")?.to_owned();
    assert_eq!(current_boot_token().as_deref(),Some(token.as_str()));
    drop(maintenance); // The owner drain failed; no auth exchange was sent.
    let bytes=fs::read(marker_path(&root))?;
    assert!(resume(&token,"foreign",true).is_err());
    assert!(resume(&uuid::Uuid::new_v4().to_string(),&digest,true).is_err());
    assert_eq!(fs::read(marker_path(&root))?,bytes);
    let maintenance=resume(&token,&digest,true)?;
    assert_eq!(current_boot_token().as_deref(),Some(token.as_str()));
    drop(maintenance);
    let mut changed=marker(&root)?.context("prepared marker missing")?;
    changed.phase=Phase::Starting;
    let changed_bytes=serde_json::to_vec(&changed)?;
    fs::write(marker_path(&root),&changed_bytes)?;
    assert!(resume(&token,&hash(&changed_bytes),true).is_err());
    assert_eq!(fs::read(marker_path(&root))?,changed_bytes);
    assert!(current_boot_token().is_none());
    Ok(())
}

#[test]
fn foreground_supervisor_retries_are_bounded_and_reset_only_after_stability() -> Result<()> {
    let mut failures = 0;
    assert!(retry_supervised_child(
        &mut failures,
        Duration::from_secs(1)
    ));
    assert!(retry_supervised_child(
        &mut failures,
        Duration::from_secs(1)
    ));
    assert!(!retry_supervised_child(
        &mut failures,
        Duration::from_secs(1)
    ));
    assert!(retry_supervised_child(
        &mut failures,
        Duration::from_secs(120)
    ));
    assert_eq!(failures, 1);
    Ok(())
}

#[test]
fn registration_captures_nonsecret_opt_outs_and_legacy_names_canonically() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
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
        "RSRS_NO_AUTOSYNC",
        "ONEMEMORY_NO_AUTOSYNC",
        "RSRS_UPDATE_CHECK",
    ];
    let _restore = Restore(
        keys.into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    let root = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", root.path());
    std::env::set_var("RSRS_BIN_DIR", root.path().join("bin"));
    std::env::remove_var("RSRS_NO_AUTOSYNC");
    std::env::set_var("ONEMEMORY_NO_AUTOSYNC", "1");
    std::env::set_var("RSRS_UPDATE_CHECK", "0");
    let config = new_registration()?;
    anyhow::ensure!(
        config
            .environment
            .get("RSRS_NO_AUTOSYNC")
            .map(String::as_str)
            == Some("1")
    );
    anyhow::ensure!(
        config
            .environment
            .get("RSRS_UPDATE_CHECK")
            .map(String::as_str)
            == Some("0")
    );
    anyhow::ensure!(!config
        .environment
        .keys()
        .any(|key| key.starts_with("ONEMEMORY_")));
    anyhow::ensure!(
        !control_path(root.path()).exists(),
        "capturing settings registered startup"
    );
    Ok(())
}
