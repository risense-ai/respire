#[test]
fn version_charset_rejects_separators_and_malformed_shapes() {
    for version in ["1.0.21-dev.1", "1.2.3+build.7", "2.0.0", "1.0.0-alpha-1"] {
        assert!(valid_version(version), "{version:?} should be valid");
    }
    for version in [
        "",
        "1/2",
        "a\\b",
        "C:evil",
        "1 2",
        "a\nb",
        "x@y",
        "v1.2.3",
        "1.2",
        "1.2.x",
        "=2.0.0",
        "1.2.3+",
        "1.2.3-alpha..1",
    ] {
        assert!(!valid_version(version), "{version:?} should be rejected");
    }
}

#[test]
fn entry_names_reject_traversal_and_separators() {
    for name in ["lib/x.json", "./lib/x.json", "rsrs-runtime", "a/b/c/"] {
        assert!(
            safe_entry_name(name).is_some(),
            "{name:?} should be accepted"
        );
    }
    for name in [
        "../escape",
        "a/../../escape",
        "/absolute",
        "back\\slash",
        "drive:C",
        "a//b",
        "a/./b",
        "",
        ".",
    ] {
        assert!(
            safe_entry_name(name).is_none(),
            "{name:?} should be rejected"
        );
    }
}

#[test]
fn extraction_whitelist_matches_client2_semantics() -> Result<()> {
    let triple = match release_triple() {
        Ok(triple) => triple,
        Err(_) => return Ok(()),
    };
    let dir = tempfile::tempdir()?;
    let (archive, _) = release_fixture(&triple)?;
    let archive_path = dir.path().join("runtime.tar.gz");
    fs::write(&archive_path, &archive)?;
    let stage = dir.path().join("stage");
    extract_archive(&archive_path, &stage)?;
    anyhow::ensure!(stage.join(RUNTIME_BINARY).is_file());
    anyhow::ensure!(stage.join(CORE_MANIFEST_FILE).is_file());
    anyhow::ensure!(stage.join("lib/provider.json").is_file());
    // Entry names that fail the whitelist are rejected outright.
    let hostile = gz(&tar_bytes(&[("../escape", b'0', 0o600, b"x")]))?;
    fs::write(dir.path().join("hostile.tar.gz"), &hostile)?;
    anyhow::ensure!(
        extract_archive(
            &dir.path().join("hostile.tar.gz"),
            &dir.path().join("hostile")
        )
        .is_err(),
        "traversal entry was accepted"
    );
    anyhow::ensure!(!dir.path().join("escape").exists());
    for (name, typeflag) in [("C:\\bad", b'0'), ("link-target", b'2'), ("fifo", b'6')] {
        let entry = gz(&tar_bytes(&[(name, typeflag, 0o600, b"x")]))?;
        fs::write(dir.path().join("hostile2.tar.gz"), &entry)?;
        anyhow::ensure!(
            extract_archive(
                &dir.path().join("hostile2.tar.gz"),
                &dir.path().join("hostile2")
            )
            .is_err(),
            "unsafe entry {name} with flag {typeflag} was accepted"
        );
    }
    // A truncated archive never produces a usable stage.
    let truncated = gz(&tar_fixture_truncated())?;
    fs::write(dir.path().join("short.tar.gz"), &truncated)?;
    anyhow::ensure!(
        extract_archive(&dir.path().join("short.tar.gz"), &dir.path().join("short")).is_err()
    );
    Ok(())
}

fn tar_fixture_truncated() -> Vec<u8> {
    let mut raw = tar_bytes(&[("rsrs-runtime", b'0', 0o755, b"payload")]);
    raw.truncate(raw.len() - 600);
    raw
}

#[test]
fn stage_verification_enforces_manifest_and_binary_hash() -> Result<()> {
    let triple = match release_triple() {
        Ok(triple) => triple,
        Err(_) => return Ok(()),
    };
    let dir = tempfile::tempdir()?;
    let (archive, binary_hash) = release_fixture(&triple)?;
    let archive_path = dir.path().join("runtime.tar.gz");
    fs::write(&archive_path, &archive)?;
    let stage = dir.path().join("stage");
    extract_archive(&archive_path, &stage)?;
    let binary = RUNTIME_BINARY.to_owned();
    let verified = verify_stage(&stage, &binary, &triple, &binary_hash)?;
    anyhow::ensure!(verified == stage.join(&binary));
    anyhow::ensure!(verify_stage(&stage, &binary, &triple, &"0".repeat(64)).is_err());
    anyhow::ensure!(verify_stage(&stage, &binary, "other-triple", &binary_hash).is_err());
    anyhow::ensure!(verify_stage(&stage, "missing-binary", &triple, &binary_hash).is_err());
    // A manifest listing a foreign path fails the whitelist.
    fs::write(
        stage.join(CORE_MANIFEST_FILE),
        serde_json::json!({
            "schema_version": 1,
            "target": triple,
            "files": [{"path": "../outside", "sha256": sha256_hex(b"x")}],
        })
        .to_string(),
    )?;
    anyhow::ensure!(verify_stage(&stage, &binary, &triple, &binary_hash).is_err());
    Ok(())
}

#[test]
fn install_placement_renames_the_full_triple_binary() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let stage = dir.path().join("stage");
    fs::create_dir_all(&stage)?;
    let full = release_binary_name("x86_64-pc-windows-msvc");
    let bare = RUNTIME_BINARY;
    anyhow::ensure!(full != bare, "platform constant must use the bare name");
    fs::write(stage.join(&full), b"binary bytes")?;
    place_managed_binary(&stage, &full)?;
    anyhow::ensure!(stage.join(bare).is_file());
    anyhow::ensure!(!stage.join(&full).exists());
    anyhow::ensure!(fs::read(stage.join(bare))? == b"binary bytes");
    fs::write(stage.join(&full), b"again")?;
    anyhow::ensure!(place_managed_binary(&stage, "../escape").is_err());
    anyhow::ensure!(place_managed_binary(&stage, "missing-binary").is_err());
    Ok(())
}
