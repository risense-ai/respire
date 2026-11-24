//! Tests for the managed runtime fetcher: version validation, whitelist extraction,
//! stage verification and offline installation against fixture archives.
use super::*;

/// Lifecycle fixtures use a fully verified synthetic selection, never an
/// unproven old installation that the migration guard is supposed to preserve.
pub(crate) fn install_lifecycle_fixture() -> Result<PathBuf> {
    let version = &pinned()?.required;
    let triple = release_triple()?;
    let binary = managed_binary_path(version);
    let directory = binary.parent().context("fixture directory missing")?;
    fs::create_dir_all(directory)?;
    let payload = b"synthetic runtime; never executed";
    fs::write(&binary, payload)?;
    let resource = b"synthetic provider; never loaded";
    fs::create_dir_all(directory.join("lib"))?;
    fs::write(directory.join("lib/provider.bin"), resource)?;
    let core = serde_json::json!({"schema_version":1, "target":triple,
        "files":[{"path":"lib/provider.bin", "sha256":sha256_hex(resource)}]}).to_string();
    fs::write(directory.join(CORE_MANIFEST_FILE), &core)?;
    let name = release_binary_name(&triple);
    let metadata = serde_json::json!({
        "schema_version":2, "runtime_version":version, "tag":format!("v{version}"),
        "target_triple":triple, "archive_file":format!("rsrs-runtime-{triple}.tar.gz"),
        "archive_sha256":"0".repeat(64), "binary_file":name, "binary_sha256":sha256_hex(payload),
        "source_commit":"0".repeat(40), "compat":{"min_cli":"1.0.0", "max_cli":"*"},
        "protocol_range":{"minimum":1, "maximum":1},
        "capabilities":{"business_executor":"1", "mcp_server":"1"},
        "files":[{"path":name, "size":payload.len(), "sha256":sha256_hex(payload)},
            {"path":CORE_MANIFEST_FILE, "size":core.len(), "sha256":sha256_hex(core.as_bytes())},
            {"path":"lib/provider.bin", "size":resource.len(), "sha256":sha256_hex(resource)}],
    }).to_string();
    let parsed: BuildMetadata = serde_json::from_str(&metadata)?;
    publish_manifest(&ManagedManifest::from_release(&parsed, metadata.as_bytes())?)?;
    anyhow::ensure!(managed_runtime_path()?.as_ref() == Some(&binary));
    Ok(binary)
}

/// Serialize env-mutating tests and restore the previous environment on drop.
pub(crate) fn env_scope(keys: &[&'static str]) -> Result<impl Drop> {
    struct Restore {
        _guard: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    Ok(Restore {
        _guard: crate::TEST_ENV_LOCK
            .lock()
            .map_err(|error| anyhow::anyhow!("test environment lock poisoned: {error}"))?,
        saved: keys
            .iter()
            .map(|key| (*key, std::env::var_os(*key)))
            .collect(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Build one ustar header block with a valid checksum field.
fn tar_header(name: &str, size: u64, typeflag: u8, mode: u32) -> [u8; 512] {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    let mode_text = format!("{mode:o}");
    header[100..100 + mode_text.len()].copy_from_slice(mode_text.as_bytes());
    let size_text = format!("{size:o}");
    header[124..124 + size_text.len()].copy_from_slice(size_text.as_bytes());
    header[156] = typeflag;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let mut sum: u64 = 0;
    for (index, byte) in header.iter().enumerate() {
        sum += if (148..156).contains(&index) {
            u64::from(b' ')
        } else {
            u64::from(*byte)
        };
    }
    let checksum = format!("{sum:o}");
    let mut field = [0u8; 8];
    field[..checksum.len()].copy_from_slice(checksum.as_bytes());
    header[148..156].copy_from_slice(&field);
    header
}

/// Assemble a raw tar archive from (name, typeflag, mode, payload) entries.
fn tar_bytes(entries: &[(&str, u8, u32, &[u8])]) -> Vec<u8> {
    let mut raw = Vec::new();
    for (name, typeflag, mode, payload) in entries {
        raw.extend_from_slice(&tar_header(name, payload.len() as u64, *typeflag, *mode));
        raw.extend_from_slice(payload);
        let padding = (512 - (payload.len() as u64 % 512)) % 512;
        raw.extend(std::iter::repeat(0u8).take(padding as usize));
    }
    raw.extend_from_slice(&[0u8; 1024]);
    raw
}

fn gz(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .context("fixture archive compression failed")?;
    encoder
        .finish()
        .context("fixture archive compression failed")
}

/// A complete, verifiable release fixture: binary + core manifest + library.
fn release_fixture(triple: &str) -> Result<(Vec<u8>, String)> {
    const BINARY: &[u8] = b"synthetic rsrs-runtime binary bytes";
    let hash = sha256_hex(BINARY);
    let manifest = serde_json::json!({
        "schema_version": 1,
        "target": triple,
        "files": [{"path": RUNTIME_BINARY, "sha256": hash}],
    });
    let raw = tar_bytes(&[
        ("./", b'5', 0o700, b""),
        ("lib/", b'5', 0o700, b""),
        ("lib/provider.json", b'0', 0o600, b"{}"),
        (
            CORE_MANIFEST_FILE,
            b'0',
            0o600,
            manifest.to_string().as_bytes(),
        ),
        (RUNTIME_BINARY, b'0', 0o755, BINARY),
    ]);
    Ok((gz(&raw)?, hash))
}

include!("runtime_fetch_tests/archive.rs");

#[test]
fn offline_install_publishes_manifest_and_retains_one_rollback() -> Result<()> {
    let triple = match release_triple() {
        Ok(triple) => triple,
        Err(_) => return Ok(()),
    };
    let _env = env_scope(&["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_RUNTIME_BIN", "RSRS_CORE_TEST_MODE"])?;
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_BIN_DIR", dir.path().join("bin"));
    std::env::remove_var("RSRS_RUNTIME_BIN");
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    let binary_name = release_binary_name(&triple);
    let binary_bytes = b"synthetic runtime bytes, never executed";
    let resource_bytes = b"synthetic SDK resource";
    let hash = sha256_hex(binary_bytes);
    let resource_hash = sha256_hex(resource_bytes);
    let core = serde_json::json!({
        "schema_version": 1, "target": triple,
        "files": [
            {"path": "lib/provider.bin", "size": resource_bytes.len(), "sha256": resource_hash},
            {"path": "core-notices/NOTICES.txt", "sha256": sha256_hex(b"notices")},
        ],
    }).to_string();
    let archive = gz(&tar_bytes(&[
        (&binary_name, b'0', 0o700, binary_bytes),
        (CORE_MANIFEST_FILE, b'0', 0o600, core.as_bytes()),
        ("lib/provider.bin", b'0', 0o600, resource_bytes),
        ("core-notices/NOTICES.txt", b'0', 0o600, b"notices"),
    ]))?;
    let archive_path = dir.path().join("runtime.tar.gz");
    let metadata_path = dir.path().join(format!("runtime-build-{triple}.json"));
    let metadata = serde_json::json!({
        "schema_version": 2, "runtime_version": "1.0.21-dev.1", "tag": "v1.0.21-dev.1",
        "target_triple": triple, "archive_file": format!("rsrs-runtime-{triple}.tar.gz"),
        "archive_sha256": sha256_hex(&archive), "binary_file": binary_name, "binary_sha256": hash,
        "source_commit": "0".repeat(40), "compat": {"min_cli": "1.0.0", "max_cli": "*"},
        "protocol_range": {"minimum": 2, "maximum": 2},
        "files": [
            {"path": binary_name, "size": binary_bytes.len(), "sha256": hash},
            {"path": CORE_MANIFEST_FILE, "size": core.len(), "sha256": sha256_hex(core.as_bytes())},
            {"path": "lib/provider.bin", "size": resource_bytes.len(), "sha256": resource_hash},
            {"path": "core-notices/NOTICES.txt", "size": 7, "sha256": sha256_hex(b"notices")},
        ],
    });
    fs::write(&archive_path, &archive)?;
    anyhow::ensure!(
        install_from_file(&archive_path, "1.0.21-dev.1").is_err(),
        "offline metadata is mandatory"
    );
    anyhow::ensure!(read_manifest()?.is_none());
    fs::write(&metadata_path, metadata.to_string())?;
    let binary = install_from_file(&archive_path, "1.0.21-dev.1")?;
    anyhow::ensure!(binary.is_file());
    anyhow::ensure!(!version_dir("1.0.21-dev.1").join("core-notices").exists());
    anyhow::ensure!(version_dir("1.0.21-dev.1").join(CORE_MANIFEST_FILE).is_file());
    // Reusing an older installation leaves its authenticated notices untouched.
    let notices = version_dir("1.0.21-dev.1").join("core-notices/NOTICES.txt");
    fs::create_dir_all(notices.parent().context("notice parent missing")?)?;
    fs::write(&notices, b"notices")?;
    anyhow::ensure!(install_from_file(&archive_path, "1.0.21-dev.1")? == binary);
    anyhow::ensure!(notices.is_file());
    let manifest = read_manifest()?.context("manifest missing after install")?;
    anyhow::ensure!(manifest.version == "1.0.21-dev.1" && manifest.sha256 == hash);
    let proof = manifest.release.as_ref().context("release proof missing")?;
    anyhow::ensure!(proof.metadata_json == metadata.to_string());
    anyhow::ensure!(proof.metadata_sha256 == sha256_hex(metadata.to_string().as_bytes()));
    anyhow::ensure!(managed_runtime_path()?.context("managed path missing")? == binary);
    anyhow::ensure!(download_and_install(Some("1.0.21-dev.1"), None)? == binary);
    // Reusing the verified Runtime neither depends on nor replaces the old CLI.
    fs::create_dir_all(dir.path().join("bin"))?;
    let legacy = crate::mcp::stable_bin_path();
    fs::write(&legacy, b"old combined CLI")?;
    let before = fs::read(manifest_path())?;
    anyhow::ensure!(download_and_install(Some("1.0.21-dev.1"), None)? == binary);
    anyhow::ensure!(fs::read(&legacy)? == b"old combined CLI");
    anyhow::ensure!(fs::read(manifest_path())? == before);
    fs::remove_file(legacy)?;
    let resource = binary
        .parent()
        .context("binary has no parent")?
        .join("lib/provider.bin");
    anyhow::ensure!(fs::read(&resource)? == resource_bytes);
    // Reuse must authenticate the original metadata, not a replacement local
    // manifest that agrees with damaged resource bytes.
    let core_path = binary
        .parent()
        .context("binary has no parent")?
        .join(CORE_MANIFEST_FILE);
    fs::write(&resource, b"damaged resource")?;
    let changed_core = serde_json::json!({
        "schema_version": 1, "target": triple,
        "files": [{"path": "lib/provider.bin", "sha256": sha256_hex(b"damaged resource")}],
    });
    fs::write(&core_path, changed_core.to_string())?;
    let error = managed_runtime_path()
        .err()
        .context("damaged resources were accepted")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    anyhow::ensure!(format!("{error:#}").contains("repair required"));
    let error = resolve_runtime_bin()
        .err()
        .context("discovery ignored managed corruption")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    anyhow::ensure!(format!("{error:#}").contains("repair required"));
    let error = download_and_install(Some("1.0.21-dev.1"), None)
        .err()
        .context("cached install ignored damaged resources")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    let before = fs::read(manifest_path())?;
    let error = download_and_install(Some("1.0.21-dev.2"), None)
        .err()
        .context("a different pin bypassed damaged resource verification")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    anyhow::ensure!(fs::read(manifest_path())? == before);
    fs::write(&core_path, &core)?;
    fs::write(&resource, resource_bytes)?;
    fs::remove_file(&resource)?;
    anyhow::ensure!(managed_runtime_path().is_err());
    fs::write(&resource, resource_bytes)?;
    let extra = resource.with_file_name("unlisted-provider.bin");
    fs::write(&extra, b"unlisted resource")?;
    anyhow::ensure!(managed_runtime_path().is_err());
    fs::remove_file(&extra)?;

    let mut changed = manifest.clone();
    changed
        .release
        .as_mut()
        .context("release proof missing")?
        .metadata_json
        .push(' ');
    publish_manifest(&changed)?;
    anyhow::ensure!(managed_runtime_path().is_err());
    changed = manifest.clone();
    changed.sha256 = "0".repeat(64);
    publish_manifest(&changed)?;
    anyhow::ensure!(managed_runtime_path().is_err());
    for (field, value) in [
        ("runtime_version", serde_json::json!("9.0.0")),
        ("target_triple", serde_json::json!("other-target")),
    ] {
        let mut rebound = metadata.clone();
        rebound[field] = value;
        changed = manifest.clone();
        let proof = changed.release.as_mut().context("release proof missing")?;
        proof.metadata_json = rebound.to_string();
        proof.metadata_sha256 = sha256_hex(proof.metadata_json.as_bytes());
        publish_manifest(&changed)?;
        anyhow::ensure!(
            managed_runtime_path().is_err(),
            "rebound {field} was accepted"
        );
    }
    let mut incomplete = metadata.clone();
    incomplete["files"]
        .as_array_mut()
        .context("release files missing")?
        .retain(|file| file["path"] != CORE_MANIFEST_FILE);
    let incomplete_metadata: BuildMetadata = serde_json::from_value(incomplete.clone())?;
    anyhow::ensure!(
        ManagedManifest::from_release(&incomplete_metadata, incomplete.to_string().as_bytes())
            .is_err(),
        "release proof must authenticate the Core manifest"
    );
    changed = manifest.clone();
    changed.release = None;
    publish_manifest(&changed)?;
    let error = resolve_runtime_bin()
        .err()
        .context("legacy selection silently fell back")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    anyhow::ensure!(format!("{error:#}").contains("repair required"));
    let error = download_and_install(Some("1.0.21-dev.1"), None)
        .err()
        .context("cached install accepted a legacy selection without proof")?;
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    // An explicit trusted reinstall can establish proof for a legacy selection.
    let before = fs::read(manifest_path())?;
    let error = download_and_install(Some("1.0.21-dev.2"), None)
        .err()
        .context("a different pin bypassed legacy migration protection")?;
    anyhow::ensure!(error.is::<LegacyRuntimeMigrationRequired>());
    anyhow::ensure!(error.is::<ManagedRuntimeRepairRequired>());
    anyhow::ensure!(fs::read(manifest_path())? == before);
    anyhow::ensure!(fs::read(&resource)? == resource_bytes);
    anyhow::ensure!(install_from_file(&archive_path, "1.0.21-dev.1")? == binary);
    anyhow::ensure!(read_manifest()?
        .context("repaired selection missing")?
        .release
        .is_some());
    anyhow::ensure!(managed_runtime_path()?.context("repaired path missing")? == binary);
    // Failed archive, binary and resource checks must preserve the selected payload.
    for field in ["archive_sha256", "binary_sha256", "resource_sha256"] {
        let mut invalid = metadata.clone();
        if field == "resource_sha256" {
            invalid["files"][2]["sha256"] = serde_json::json!("0".repeat(64));
        } else {
            invalid[field] = serde_json::json!("0".repeat(64));
            if field == "binary_sha256" {
                invalid["files"][0]["sha256"] = invalid[field].clone();
            }
        }
        fs::write(&metadata_path, invalid.to_string())?;
        anyhow::ensure!(install_from_file(&archive_path, "1.0.21-dev.1").is_err());
        anyhow::ensure!(
            read_manifest()?
                .context("selection lost after rejected install")?
                .sha256
                == hash
        );
        anyhow::ensure!(fs::read(&resource)? == resource_bytes);
    }
    fs::write(&metadata_path, metadata.to_string())?;
    for version in ["1.0.20", "1.0.19", "1.0.21"] {
        let side = version_dir(version);
        fs::create_dir_all(&side)?;
        fs::write(side.join(RUNTIME_BINARY), b"synthetic side runtime")?;
    }
    // Reinstallation retains older payloads; cleanup is an explicit management action.
    anyhow::ensure!(install_from_file(&archive_path, "1.0.21-dev.1")? == binary);
    anyhow::ensure!(list_installed()?.len() == 4);
    let removed = clean_old()?;
    anyhow::ensure!(
        removed == vec!["1.0.20".to_owned(), "1.0.19".to_owned()],
        "unexpected removals: {removed:?}"
    );
    let listed = list_installed()?;
    anyhow::ensure!(listed.len() == 2, "unexpected inventory: {listed:?}");
    anyhow::ensure!(
        listed
            .iter()
            .find(|entry| entry.version == "1.0.21-dev.1")
            .context("current missing")?
            .current
    );
    anyhow::ensure!(clean_old()?.is_empty());
    // Both hosts migrate the same receipt and inventory, preserving old files.
    let client2 = dir.path().join("client2");
    fs::create_dir_all(&client2)?;
    fs::rename(runtime_dir(), client2.join("runtime"))?;
    fs::rename(manifest_path(), client2.join(MANIFEST_FILE))?;
    shared::migrate_legacy()?;
    anyhow::ensure!(managed_runtime_path()?.as_ref() == Some(&binary));
    anyhow::ensure!(client2.join("runtime").join("1.0.21-dev.1").join(RUNTIME_BINARY).is_file());
    shared::migrate_legacy()?;
    let lock = install_lock()?;
    anyhow::ensure!(respire_spawn::FileLock::acquire(&runtime_dir().join(".install.lock"), Duration::ZERO).is_err());
    drop(lock);
    Ok(())
}

include!("runtime_fetch_tests/discovery.rs");

#[test]
fn metadata_validation_rejects_cross_purpose_documents() -> Result<()> {
    let triple = match release_triple() {
        Ok(triple) => triple,
        Err(_) => return Ok(()),
    };
    let base = serde_json::json!({
        "schema_version": 1,
        "runtime_version": "1.0.21-dev.1", "tag": "v1.0.21-dev.1", "triple": triple,
        "sha256": sha256_hex(b"archive"), "binary_sha256": sha256_hex(b"binary"),
        "binary_file": release_binary_name(&triple), "compat": {"min_cli": "1.0.0", "max_cli": "*"},
    });
    let metadata: BuildMetadata = serde_json::from_value(base.clone())?;
    metadata.validate("1.0.21-dev.1", &triple)?;
    let mut no_cli_version = base.clone();
    no_cli_version.as_object_mut().context("metadata is not an object")?.remove("compat");
    serde_json::from_value::<BuildMetadata>(no_cli_version)?.validate("1.0.21-dev.1", &triple)?;
    let mut obsolete_cli_version = base.clone();
    obsolete_cli_version["compat"]["min_cli"] = serde_json::json!("99.0.0");
    serde_json::from_value::<BuildMetadata>(obsolete_cli_version)?.validate("1.0.21-dev.1", &triple)?;
    anyhow::ensure!(
        ManagedManifest::from_release(&metadata, base.to_string().as_bytes()).is_err(),
        "legacy metadata must not become complete managed release proof"
    );
    let mut alias = base.clone();
    alias["archive_sha256"] = alias["sha256"].clone();
    let equal_alias: BuildMetadata = serde_json::from_value(alias.clone())?;
    equal_alias.validate("1.0.21-dev.1", &triple)?;
    alias
        .as_object_mut()
        .context("metadata is not an object")?
        .remove("sha256");
    let canonical: BuildMetadata = serde_json::from_value(alias)?;
    canonical.validate("1.0.21-dev.1", &triple)?;
    anyhow::ensure!(canonical.archive_sha256()? != canonical.binary_sha256()?);
    for (field, value) in [
        ("runtime_version", serde_json::json!("1.0.22")),
        ("tag", serde_json::json!("v1.0.22")),
        ("triple", serde_json::json!("x86_64-unknown-freebsd")),
        ("target_triple", serde_json::json!("other-target")),
        ("sha256", serde_json::json!("zz")),
        ("binary_sha256", serde_json::json!("")),
        ("binary_file", serde_json::json!("../escape")),
        ("binary_file", serde_json::json!(RUNTIME_BINARY)),
    ] {
        let mut changed = base.clone();
        changed[field] = value;
        let changed: BuildMetadata = serde_json::from_value(changed)?;
        anyhow::ensure!(
            changed.validate("1.0.21-dev.1", &triple).is_err(),
            "invalid metadata accepted: {field}"
        );
    }
    let mut conflicting = base.clone();
    conflicting["archive_sha256"] = serde_json::json!(sha256_hex(b"other archive"));
    let conflicting: BuildMetadata = serde_json::from_value(conflicting)?;
    anyhow::ensure!(conflicting.validate("1.0.21-dev.1", &triple).is_err());
    let mut missing = base;
    missing
        .as_object_mut()
        .context("metadata is not an object")?
        .remove("binary_sha256");
    let missing: BuildMetadata = serde_json::from_value(missing)?;
    anyhow::ensure!(missing.validate("1.0.21-dev.1", &triple).is_err());
    Ok(())
}
