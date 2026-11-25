/// Install a release from the default channel (or an explicit version),
/// returning the managed binary path. Only an explicit `mirror` origin changes
/// Runtime download sources. Already-installed matching
/// builds are reused. Concurrent CLI instances serialize on the install lock;
/// the switch into the version directory is
/// atomic and the previous version directory is retained as the rollback
/// point.
/// A damaged selected build requires repair; it never triggers automatic
/// replacement. An explicit trusted offline install can re-establish proof.
pub fn download_and_install(pin: Option<&str>, mirror: Option<&str>) -> Result<PathBuf> {
    download_runtime_release(pin, mirror, default_runtime_channel(), true)
}

pub(crate) fn install_published_runtime(version: Option<&str>, mirror: Option<&str>, channel: Option<RuntimeChannel>) -> Result<PathBuf> {
    download_runtime_release(version, mirror, channel.unwrap_or_else(default_runtime_channel), false)
}

fn download_runtime_release(
    pin: Option<&str>,
    mirror: Option<&str>,
    channel: RuntimeChannel,
    reuse_selected: bool,
) -> Result<PathBuf> {
    anyhow::ensure!(crate::embedding::selection().is_none(),
        "Runtime installation is owned by Client2; the embedded frontend cannot download or replace it");
    if let Some(explicit) = pin {
        anyhow::ensure!(valid_version(explicit), "invalid runtime version: {explicit:?}");
    }
    if reuse_selected {
        if let Some(binary) = selected_install_for_reuse(pin)? {
            return Ok(binary);
        }
    }
    let triple = release_triple()?;
    // Hermetic tests fail explicitly instead of downloading or synthesizing a shim.
    anyhow::ensure!(
        !respire_client_host::env::var("RSRS_CORE_TEST_MODE").is_ok_and(|value| value == "1"),
        "runtime downloads are disabled in core test mode"
    );
    let _lock = install_lock()?;
    if reuse_selected {
        if let Some(binary) = selected_install_for_reuse(pin)? {
            return Ok(binary);
        }
    }
    let release = resolve_runtime_release(pin, channel)?;
    let required = release.version;
    let _operation = model_progress::Operation::begin_download()?;
    let metadata_name = format!("runtime-build-{triple}.json");
    let origins = download_origins(mirror)?;
    // Mirrors carry archives only; metadata remains bound to the official source.
    let metadata_bytes = fetch_metadata(&release.source.browser_url(RELEASE_BASE, &required, &metadata_name))?;
    let metadata: BuildMetadata =
        serde_json::from_slice(&metadata_bytes).context("invalid runtime release metadata")?;
    metadata.validate(&required, &triple)?;
    let manifest = ManagedManifest::from_release(&metadata, &metadata_bytes)?;
    let mut failures = Vec::new();
    for origin in &origins {
        let stage = staging_dir()?;
        let outcome = (|| -> Result<PathBuf> {
            let binary_sha256 = metadata.binary_sha256()?.to_owned();
            let archive_sha256 = metadata.archive_sha256()?.to_owned();
            let archive_name = metadata
                .archive_file
                .clone()
                .unwrap_or_else(|| format!("rsrs-runtime-{triple}.tar.gz"));
            let archive = stage.join(&archive_name);
            let actual = download_archive(
                &release.source.browser_url(origin, &required, &archive_name),
                &archive,
            )?;
            anyhow::ensure!(
                actual.eq_ignore_ascii_case(&archive_sha256),
                "archive checksum failed: found {actual}, expected {archive_sha256}"
            );
            let unpacked = stage.join("unpacked");
            extract_archive(&archive, &unpacked)?;
            verify_stage(&unpacked, &metadata.binary_file, &triple, &binary_sha256)?;
            metadata.verify_files(&unpacked)?;
            // Release archives carry the full-triple binary name; publish the
            // bare managed name before switching the version directory.
            place_managed_binary(&unpacked, &metadata.binary_file)?;
            adopt_or_verify_version_dir(&unpacked, &manifest)?;
            publish_manifest(&manifest)?;
            Ok(managed_binary_path(&required))
        })();
        match finish_install(&stage, outcome) {
            // Running older instances may still load resources. Retire versions only via explicit cleanup.
            Ok(binary) => return Ok(binary),
            Err(error) => {
                // Cancellation and local filesystem failures do not advance to
                // the next source; network/checksum failures do.
                if is_progress_abort(&error) {
                    return Err(error);
                }
                failures.push(format!("{origin}: {error:#}"));
            }
        }
    }
    anyhow::bail!("runtime download failed: {}", failures.join("; "))
}

/// Offline manual installation from a local release archive
/// (`rsrs runtime install --file <archive> --version x.y.z`). The archive must
/// still satisfy the extraction whitelist and ship `core-runtime.json`; the
/// adjacent `runtime-build-<triple>.json` must be supplied from a trusted
/// release source. Both archive and binary digests are checked before publish.
/// Managed activation requires schema 2 with a complete file inventory;
/// parsing historical schema-1 metadata alone does not establish this proof.
pub fn install_from_file(archive: &Path, version: &str) -> Result<PathBuf> {
    anyhow::ensure!(crate::embedding::selection().is_none(),
        "Runtime installation is owned by Client2; the embedded frontend cannot replace or remove it");
    anyhow::ensure!(
        archive.is_file(),
        "archive {} does not exist",
        archive.display()
    );
    anyhow::ensure!(
        valid_version(version),
        "invalid runtime version: {version:?}"
    );
    let triple = release_triple()?;
    let metadata_path = archive.with_file_name(format!("runtime-build-{triple}.json"));
    let mut bytes = Vec::new();
    fs::File::open(&metadata_path)
        .with_context(|| {
            format!(
                "offline installation requires trusted metadata {}",
                metadata_path.display()
            )
        })?
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_METADATA_BYTES,
        "release metadata too large"
    );
    let metadata: BuildMetadata = serde_json::from_slice(&bytes)?;
    metadata.validate(version, &triple)?;
    let manifest = ManagedManifest::from_release(&metadata, &bytes)?;
    anyhow::ensure!(
        fs::metadata(archive)?.len() <= MAX_ARCHIVE_BYTES,
        "archive exceeds size limit"
    );
    let _operation = model_progress::Operation::begin_download()?;
    let _lock = install_lock()?;
    let stage = staging_dir()?;
    let outcome = (|| -> Result<()> {
        let unpacked = stage.join("unpacked");
        let staged_archive = stage.join("runtime.tar.gz");
        let mut source = fs::File::open(archive)?.take(MAX_ARCHIVE_BYTES + 1);
        let mut target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_archive)?;
        anyhow::ensure!(
            std::io::copy(&mut source, &mut target)? <= MAX_ARCHIVE_BYTES,
            "archive exceeds size limit"
        );
        target.sync_all()?;
        drop(target);
        anyhow::ensure!(
            sha256_file(&staged_archive)?.eq_ignore_ascii_case(metadata.archive_sha256()?),
            "offline archive checksum mismatch"
        );
        extract_archive(&staged_archive, &unpacked)?;
        let binary_file = metadata.binary_file.clone();
        let binary_hash = metadata.binary_sha256()?.to_owned();
        verify_stage(&unpacked, &binary_file, &triple, &binary_hash)?;
        metadata.verify_files(&unpacked)?;
        place_managed_binary(&unpacked, &binary_file)?;
        adopt_or_verify_version_dir(&unpacked, &manifest)?;
        publish_manifest(&manifest)?;
        Ok(())
    })();
    finish_install(&stage, outcome)?;
    // Keep the prior installation intact while its instance may still be running.
    Ok(managed_binary_path(version))
}

// ---------------------------------------------------------------------------
// Inventory and cleanup
// ---------------------------------------------------------------------------

/// One installed runtime version directory.
#[derive(Clone, Debug)]
pub struct InstalledRuntime {
    pub version: String,
    pub path: PathBuf,
    /// True when `runtime.json` currently selects this version.
    pub current: bool,
}

/// List installed runtime version directories, newest first. Without a
/// manifest the newest installed version counts as current.
pub fn list_installed() -> Result<Vec<InstalledRuntime>> {
    let mut installed = Vec::new();
    for entry in fs::read_dir(runtime_dir()).context("failed to list the runtime directory")? {
        let entry = entry.context("failed to list the runtime directory")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !valid_version(name) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path().join(RUNTIME_BINARY);
        if !path.is_file() {
            continue;
        }
        installed.push(InstalledRuntime {
            version: name.to_owned(),
            path,
            current: false,
        });
    }
    // Newest first; equal comparisons keep a stable relative order.
    installed.sort_by(|a, b| respire_client_host::version_cmp(&b.version, &a.version));
    let current = read_manifest()?
        .map(|manifest| manifest.version)
        .or_else(|| installed.first().map(|entry| entry.version.clone()));
    if let Some(current) = current {
        for entry in &mut installed {
            entry.current = entry.version == current;
        }
    }
    Ok(installed)
}

/// Remove old version directories, keeping the current version and the newest
/// remaining one as the rollback point. Returns the removed versions.
pub fn clean_old() -> Result<Vec<String>> {
    anyhow::ensure!(crate::embedding::selection().is_none(),
        "Runtime installation is owned by Client2; the embedded frontend cannot replace or remove it");
    let _lock = install_lock()?;
    let keep = list_installed()?
        .iter()
        .find(|entry| entry.current)
        .map(|entry| entry.version.clone())
        .context("no managed runtime version is selected")?;
    remove_superseded(&keep)
}

fn remove_superseded(keep: &str) -> Result<Vec<String>> {
    let installed = list_installed()?;
    let mut removed = Vec::new();
    let mut kept_previous = false;
    for entry in installed {
        if entry.version == keep {
            continue;
        }
        if !kept_previous {
            // The newest remaining version stays as the rollback point.
            kept_previous = true;
            continue;
        }
        remove_version_dir(&entry.version)?;
        removed.push(entry.version);
    }
    Ok(removed)
}

fn remove_version_dir(version: &str) -> Result<()> {
    let dir = version_dir(version);
    // Never delete a directory that does not look like a runtime install.
    anyhow::ensure!(
        dir.join(RUNTIME_BINARY).is_file() || dir.join(LEGACY_COMBINED_BINARY).is_file(),
        "refusing to remove {}: not a runtime version directory",
        dir.display()
    );
    fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
