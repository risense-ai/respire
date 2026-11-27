#[test]
fn uninstall_reviews_removes_and_verifies_only_the_owned_model_tree() -> Result<()> {
    let fixture = Fixture::new()?;
    std::fs::create_dir_all(fixture.model.join("onnx/nested"))?;
    std::fs::write(
        fixture.model.join("onnx/nested/model.onnx"),
        b"synthetic non-model bytes",
    )?;
    let sibling = fixture.root.join("models/keep.txt");
    std::fs::write(&sibling, b"unrelated synthetic model")?;
    let before = std::fs::read(fixture.root.join("inference.json"))?;
    for (action, status, removed) in [("uninstall", "ok", true), ("uninstall-m3", "skip", false)] {
        let (value, receipt) = fixture.receipt(
            action,
            ModelMaintenanceKind::UninstallM3,
            ModelMaintenancePhase::Verify,
        )?;
        assert_eq!(value["status"], status);
        assert_eq!(value["summary"]["removed"], removed);
        assert_eq!(value["summary"]["dir"], json!(fixture.model));
        assert_eq!(
            receipt.plan.uninstall_directory.as_ref(),
            Some(&fixture.model)
        );
        assert!(!receipt.config_applied);
        assert!(!fixture.model.exists());
        assert_eq!(std::fs::read(&sibling)?, b"unrelated synthetic model");
        assert_eq!(std::fs::read(fixture.root.join("inference.json"))?, before);
    }
    Ok(())
}

#[test]
fn rejected_install_intent_preserves_configuration_without_pausing() -> Result<()> {
    let fixture = Fixture::new()?;
    let before = std::fs::read(fixture.root.join("inference.json"))?;
    let (output, value) = fixture.command(&[
        "model", "install-m3", "--mirror", "ftp://invalid.example", "--for-engine", "cpu",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["status"], "fail");
    let error = value["errors"][0].as_str().context("model plan rejection")?;
    assert!(error.contains("--mirror only supports http/https"), "{error}");
    assert_eq!(std::fs::read(fixture.root.join("inference.json"))?, before);
    assert!(!fixture.model.exists(), "rejected plan must not create model files");
    assert!(!fixture.marker().exists());
    fixture.assert_profile_untouched()?;
    Ok(())
}

#[test]
fn cpu_reset_applies_the_reviewed_bytes_without_a_model_or_serving_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    for _ in 0..2 {
        let (value, receipt) = fixture.receipt(
            "reset-cpu",
            ModelMaintenanceKind::ResetCpu,
            ModelMaintenancePhase::Apply,
        )?;
        assert_eq!(value["status"], "ok");
        assert_eq!(
            value["summary"],
            json!({"engine":"cpu", "force_cpu":true,
            "runtime_restarted":false, "stopped_pid":null})
        );
        assert!(receipt.config_applied);
        let bytes = std::fs::read(fixture.root.join("inference.json"))?;
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes)?,
            json!({"engine":"cpu","model":"q4","force_cpu":true})
        );
        assert_eq!(
            receipt.plan.cpu_candidate_sha256.as_deref(),
            Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
        );
        assert!(
            !fixture.model.exists(),
            "CPU reset must not download models"
        );
    }
    let (output, result) = fixture.command(&["config", "--addr", "https://fixture.invalid"])?;
    anyhow::ensure!(output.status.success(), "idle absent config failed: {result}");
    assert_eq!(result["summary"]["addr"], "https://fixture.invalid");
    assert!(!fixture.marker().exists());
    assert!(!fixture.profile.join("session.json").exists());
    assert!(std::net::TcpStream::connect(("127.0.0.1", fixture.port)).is_err());
    let (output, result) = fixture.command(&["config"])?;
    anyhow::ensure!(output.status.success(), "idle config read failed: {result}");
    assert_eq!(result["summary"]["addr"], "https://fixture.invalid");
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn provider_maintenance_verifies_an_empty_non_windows_catalog_without_downloads() -> Result<()> {
    let fixture = Fixture::new()?;
    let before = std::fs::read(fixture.root.join("inference.json"))?;
    let (value, receipt) = fixture.receipt(
        "install-engines",
        ModelMaintenanceKind::InstallEngines,
        ModelMaintenancePhase::Verify,
    )?;
    assert_eq!(value["status"], "ok");
    assert_eq!(value["summary"], json!({"providers":[],"npu_ready":false}));
    assert!(receipt.plan.catalog.is_none());
    assert!(!receipt.config_applied);
    assert_eq!(std::fs::read(fixture.root.join("inference.json"))?, before);
    assert!(!fixture.model.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn refused_link_deletion_preserves_data_and_fences_later_mutation() -> Result<()> {
    let fixture = Fixture::new()?;
    std::fs::create_dir_all(&fixture.model)?;
    let outside = fixture.base.join("outside.txt");
    std::fs::write(&outside, b"outside synthetic marker")?;
    let linked = fixture.model.join("linked.onnx");
    std::os::unix::fs::symlink(&outside, &linked)?;
    let before = std::fs::read(fixture.root.join("inference.json"))?;
    let (output, value) = fixture.run("uninstall")?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["status"], "fail");
    assert_eq!(
        value["errors"],
        json!(["model maintenance cannot follow symlinks"])
    );
    assert!(std::fs::symlink_metadata(&linked)?.file_type().is_symlink());
    assert_eq!(std::fs::read(&outside)?, b"outside synthetic marker");
    assert_eq!(std::fs::read(fixture.root.join("inference.json"))?, before);
    let marker = std::fs::read(fixture.marker())?;
    let paused: Value = serde_json::from_slice(&marker)?;
    assert_eq!(paused["phase"], "Paused");
    assert_eq!(paused["expected_profile"], json!(fixture.profile));
    fixture.assert_profile_untouched()?;

    // Fixing the owned fixture's link cannot silently clear incomplete maintenance.
    std::fs::remove_file(&linked)?;
    let (output, value) = fixture.run("reset-cpu")?;
    assert_eq!(output.status.code(), Some(1));
    assert!(value["errors"][0]
        .as_str()
        .context("fence error")?
        .contains("runtime maintenance is incomplete"));
    assert_eq!(std::fs::read(fixture.marker())?, marker);
    assert_eq!(std::fs::read(fixture.root.join("inference.json"))?, before);
    assert_eq!(std::fs::read(&outside)?, b"outside synthetic marker");
    assert!(fixture.model.is_dir());
    fixture.assert_profile_untouched()?;
    Ok(())
}
