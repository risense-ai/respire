use super::*;
use anyhow::Result;

#[test]
fn backend_probe_host_keeps_serving_and_private_admission_separate() -> Result<()> {
    assert!(probe_needs_serving_admission(BackendProbeHost::Serving as usize)?);
    assert!(!probe_needs_serving_admission(BackendProbeHost::PrivateControl as usize)?);
    assert!(probe_needs_serving_admission(0).is_err());
    assert!(probe_needs_serving_admission(usize::MAX).is_err());
    Ok(())
}

#[test]
fn backend_load_is_not_attempted_for_incomplete_files() {
    assert!(read_probe_frame(std::io::Cursor::new(((PROBE_INPUT_LIMIT + 1) as u32).to_be_bytes())).is_err());
    assert!(read_probe_frame(std::io::Cursor::new([0u8, 0, 0, 2, b'{'])).is_err());
    let mut lease = std::io::Cursor::new([0u8, 0, 0, 2, b'{', b'}', b'x']);
    assert!(read_probe_frame(&mut lease).is_ok());
    assert_eq!(lease.position(), 6, "lifetime watcher owns everything after the one request frame");
    assert!(read_probe_frame(std::io::Cursor::new([0u8, 0, 0, 2, b'{', b'}'])).is_ok());
    let status = with_backend_load(AssetsStatus::default(), || {
        panic!("incomplete files must not enter native backend loading")
    });
    assert!(!status.complete);
    assert!(status.backend_load.is_some_and(|backend| {
        backend.state == BackendLoadState::NotRun && backend.evidence.is_none()
    }));
}

#[test]
fn verified_files_do_not_hide_backend_failure_or_malformed_evidence() -> Result<()> {
    let mut unverified = required_assets_status(&[])?;
    unverified.complete = false;
    unverified.assets.push(AssetRequirement {
        name: "windows-npu-provider".into(), version: None, sha256: None,
        suggested_source: "fixture".into(), state: AssetState::Incompatible, path: None,
        expected_size: None, actual_size: None, actual_sha256: None, reason: Some("unverified".into()),
    });
    let unverified = with_backend_load(unverified, || anyhow::bail!("must not probe an unverified provider"));
    assert!(!unverified.complete && unverified.missing.is_empty());
    assert!(unverified.backend_load.is_some_and(|backend| backend.state == BackendLoadState::Failed
        && backend.reason.as_deref() == Some("npu_provider_unverified")));
    #[cfg(windows)]
    {
        use std::process::{Command, Stdio};
        let shell = PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot missing")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = Command::new(shell);
        command.args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = OwnedProbe(respire_spawn::spawn_probe_helper(&mut command)?);
        assert!(child.0.try_wait()?.is_none());
        let started = std::time::Instant::now();
        child.retire()?;
        assert!(child.0.try_wait()?.is_some());
        assert!(started.elapsed() < std::time::Duration::from_secs(6));
        child.retire()?;
    }
    #[cfg(unix)]
    {
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("60").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut child = OwnedProbe(respire_spawn::spawn_probe_helper(&mut command)?);
        assert!(child.0.try_wait()?.is_none());
        let started = std::time::Instant::now();
        child.retire()?;
        assert!(child.0.try_wait()?.is_some());
        assert!(started.elapsed() < std::time::Duration::from_secs(6));
        child.retire()?;
    }
    for malformed in [false, true] {
        let status = with_backend_load(required_assets_status(&[])?, || {
            if malformed {
                Ok(serde_json::json!({"ready": true}))
            } else {
                anyhow::bail!("selected native backend failed to load")
            }
        });
        assert!(!status.complete);
        assert!(status.missing.is_empty());
        assert!(status.backend_load.is_some_and(|backend| {
            backend.state == BackendLoadState::Failed
                && backend.reason.is_some()
                && backend.evidence.is_none()
        }));
    }
    Ok(())
}

#[test]
fn picker_inventory_reports_presence_without_claiming_checksum_validation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("weights");
    assert_eq!(variant_file_state(&path, 3)?, "missing");
    std::fs::write(&path, b"abc")?;
    assert_eq!(variant_file_state(&path, 4)?, "invalid_size");
    assert_eq!(variant_file_state(&path, 3)?, "present");
    assert_eq!(variant_file_state(dir.path(), 3)?, "invalid_size");
    Ok(())
}

#[test]
fn asset_status_reports_real_file_and_compatibility_states() -> Result<()> {
    let profile_dir = tempfile::tempdir()?;
    let weights = profile_dir.path().join("profile-weights");
    let tokenizer = profile_dir.path().join("profile-tokenizer");
    std::fs::write(&weights, b"abc")?;
    std::fs::write(&tokenizer, b"abc")?;
    let digest = format!("{:x}", Sha256::digest(b"abc"));
    let resource = |path: &Path| serde_json::json!({"path":path,"size":3,"sha256":digest});
    let selection = serde_json::from_value(serde_json::json!({
        "model":"fixture-profile","backend":"fixture-cpu","engine":"cpu",
        "profile":{"id":"fixture-profile","embedding":{"model_id":"fixture","revision":"pinned",
            "dimensions":1,"max_tokens":4,"pooling":"cls","normalize":true,"query_prefix":"","document_prefix":""},
            "onnx":{"input_ids":"input_ids"},"weights":resource(&weights),"tokenizer":resource(&tokenizer)},
        "backend_profile":{"id":"fixture-cpu","backend":"onnx"}
    }))?;
    let snapshot = ProbeSelection::capture_choice(Some(selection))?;
    let profile_status = file_assets_status_for(&snapshot)?;
    assert!(profile_status.complete);
    assert_eq!(profile_status.assets.len(), 2);
    assert_eq!(profile_status.assets[0].path.as_deref(), weights.to_str());
    assert!(profile_status.assets.iter().all(|asset| asset.name.starts_with("profile-")));
    let identity = snapshot.identity()?;
    let mut changed = snapshot.clone();
    changed.profile.as_mut().context("fixture profile missing")?.generation = "changed".into();
    assert_ne!(identity, changed.identity()?);
    changed = snapshot.clone();
    changed.profile.as_mut().context("fixture profile missing")?.profile.weights.as_mut()
        .context("fixture weights missing")?.path = profile_dir.path().join("different-model");
    assert_ne!(identity, changed.identity()?);
    assert!(!file_assets_status_for(&changed)?.complete);
    std::fs::write(&weights, b"bad")?;
    assert!(!file_assets_status_for(&snapshot)?.complete);
    let complete = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let corrupt = "3608bca1e44ea6c4d268eb6db02260269892c0b42b86bbf1e77a6fa16c3c9282";
    let specs = [
        ("missing", None, complete, 3, false),
        ("incomplete", Some(b"ab".as_slice()), complete, 3, false),
        ("short-digest", Some(b"abc".as_slice()), corrupt, 3, false),
        ("overlong", Some(b"abcd".as_slice()), complete, 3, false),
        ("complete", Some(b"abc".as_slice()), complete, 3, false),
        ("incompatible", Some(b"abc".as_slice()), complete, 3, true),
    ];
    let directory = tempfile::tempdir()?;
    let mut expected = Vec::new();
    for (name, bytes, sha256, size, incompatible) in specs {
        let path = directory.path().join(name);
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes)?;
        }
        expected.push(RequiredAsset {
            name: "test-asset",
            version: "test-version",
            path,
            size,
            sha256: sha256.to_owned(),
            suggested_source: "fixture",
            incompatible_reason: incompatible.then(|| "unsupported on this host".to_owned()),
        });
    }
    let status = required_assets_status(&expected)?;
    assert!(!status.complete);
    assert_eq!(status.assets.len(), 6);
    assert_eq!(status.missing.len(), 5);
    for (name, expected_state) in [
        ("missing", AssetState::Missing),
        ("incomplete", AssetState::Incomplete),
        ("short-digest", AssetState::Corrupt),
        ("overlong", AssetState::Corrupt),
        ("complete", AssetState::Complete),
        ("incompatible", AssetState::Incompatible),
    ] {
        let asset = status
            .assets
            .iter()
            .find(|asset| asset.path == Some(directory.path().join(name).display().to_string()))
            .ok_or_else(|| anyhow!("missing report for {name}"))?;
        assert_eq!(asset.state, expected_state, "{name}");
    }
    let complete_report = status.assets.last().context("status reports are present")?;
    assert_eq!(complete_report.actual_size, Some(3));
    assert_eq!(complete_report.actual_sha256.as_deref(), Some(complete));
    Ok(())
}

#[test]
fn missing_explicit_cpu_variant_does_not_load_the_owned_quantized_container() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
    let keys = [
        "RSRS_M3_DIR",
        "RSRS_DATA_DIR",
        "RSRS_ENGINE",
        "RSRS_MODEL_VARIANT",
        "RSRS_CPU_OWNED_ORT_EXPERIMENT",
    ];
    let _restore = Restore(
        keys.into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("onnx"))?;
    std::fs::write(
        dir.path().join("onnx/model_quantized.onnx"),
        b"baseline presence only",
    )?;
    std::fs::write(dir.path().join("tokenizer.json"), b"{}")?;
    std::env::set_var("RSRS_M3_DIR", dir.path());
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_ENGINE", "cpu");
    std::env::set_var("RSRS_MODEL_VARIANT", "fp16");
    std::env::remove_var("RSRS_CPU_OWNED_ORT_EXPERIMENT");
    let error = respire_app::memory::bge::BgeEmbedder::load()
        .err()
        .ok_or_else(|| anyhow!("missing explicit fp16 must fail"))?;
    let message = format!("{error:#}");
    assert!(message.contains("model_fp16.onnx"), "{message}");
    assert!(message.contains("--model fp16"), "{message}");
    assert!(!message.contains("model_quantized.cpu-v1.ort"), "{message}");
    Ok(())
}

