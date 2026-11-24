/// Install placement: release archives name the binary with the full triple,
/// but the managed layout stores it under the bare [`RUNTIME_BINARY`]. The
/// rename stays inside the staging directory and never changes file bytes, so
/// the verified checksum still applies.
fn place_managed_binary(stage: &Path, binary_file: &str) -> Result<()> {
    if binary_file == RUNTIME_BINARY {
        return Ok(());
    }
    anyhow::ensure!(
        safe_entry_name(binary_file).is_some(),
        "unexpected release binary name: {}",
        binary_file
    );
    let full = stage.join(binary_file);
    anyhow::ensure!(full.is_file(), "archive did not contain {}", binary_file);
    let bare = stage.join(RUNTIME_BINARY);
    anyhow::ensure!(
        !bare.exists(),
        "archive contains both release and managed binary names"
    );
    fs::rename(&full, &bare).with_context(|| format!("failed to place {}", bare.display()))
}

// ---------------------------------------------------------------------------
// Install transactions
// ---------------------------------------------------------------------------

fn install_lock() -> Result<respire_spawn::FileLock> {
    create_private_dir(&runtime_dir())?;
    respire_spawn::FileLock::acquire(&runtime_dir().join(".install.lock"), INSTALL_LOCK_WAIT)
        .context("another process is installing the runtime")
}

fn publish_manifest(manifest: &ManagedManifest) -> Result<()> {
    shared::save_receipt(manifest)?;
    let path = manifest_path();
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let temporary = path.with_file_name(format!(".runtime-manifest-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if path.is_file() {
            respire_spawn::durable::replace_file(&temporary, &path)?;
        } else {
            respire_spawn::durable::publish_new_file(&temporary, &path)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Publish a staged version directory atomically. An existing directory with
/// the identical build is reused; a different build occupying the same version
/// fails loudly instead of being overwritten.
fn adopt_or_verify_version_dir(stage: &Path, manifest: &ManagedManifest) -> Result<PathBuf> {
    let dest = version_dir(&manifest.version);
    if dest.is_dir() {
        // An explicit install may repair a legacy selection only after verifying
        // the existing bytes against the newly supplied trusted release proof.
        return validate_installed(manifest)
            .context("a different build already occupies this runtime version");
    }
    // Renaming stays inside <runtime/>, so it is atomic on every platform.
    fs::rename(stage, &dest).with_context(|| format!("failed to publish {}", dest.display()))?;
    shared::compact_version(manifest)?;
    respire_spawn::durable::sync_directory(&runtime_dir()).ok();
    Ok(dest.join(RUNTIME_BINARY))
}

fn staging_dir() -> Result<PathBuf> {
    create_private_dir(&runtime_dir())?;
    let stage = runtime_dir().join(format!(".download-{}", uuid::Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&stage)
        .with_context(|| format!("failed to create {}", stage.display()))?;
    Ok(stage)
}

fn finish_install<T>(stage: &Path, outcome: Result<T>) -> Result<T> {
    match (outcome, fs::remove_dir_all(stage)) {
        (result, Ok(())) => result,
        (Ok(installed), Err(cleanup)) => {
            eprintln!(
                "WARN Runtime installed and selected, but failed to remove staging directory {}: {cleanup}",
                stage.display()
            );
            Ok(installed)
        }
        (Err(error), Err(cleanup)) => Err(error.context(format!(
            "failed to remove staging directory {}: {cleanup}",
            stage.display()
        ))),
    }
}

fn is_progress_abort(error: &anyhow::Error) -> bool {
    error.is::<LegacyRuntimeMigrationRequired>()
        || error.is::<ManagedRuntimeRepairRequired>()
        || error
        .downcast_ref::<model_progress::OperationStopped>()
        .is_some()
        || error.downcast_ref::<std::io::Error>().is_some()
}

/// Reuse only the selected package and its own verified bytes.
fn selected_install_for_reuse(required: Option<&str>) -> Result<Option<PathBuf>> {
    let Some(manifest) = read_manifest().context(ManagedRuntimeRepairRequired)? else {
        return Ok(None);
    };
    let binary = validate_installed(&manifest).map_err(|error| {
        if manifest.release.is_none() {
            error.context(LegacyRuntimeMigrationRequired)
        } else {
            error
        }
    })?;
    Ok(required.is_none_or(|version| manifest.version == version).then_some(binary))
}

fn record_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn verify_stopped_endpoint(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(metadata.is_file(), "invalid Runtime endpoint file");
    #[derive(Deserialize)]
    struct Endpoint {
        v: u32,
        pid: u32,
        url: String,
    }
    let endpoint: Endpoint = serde_json::from_slice(&fs::read(path)?)
        .context("invalid Runtime endpoint record")?;
    anyhow::ensure!(endpoint.v == u32::from(PROTOCOL_V) && endpoint.pid > 0,
        "invalid Runtime endpoint identity");
    let address: std::net::SocketAddr = endpoint.url.strip_prefix("http://")
        .context("invalid Runtime endpoint URL")?.parse()?;
    anyhow::ensure!(address.ip().is_loopback() && address.port() > 0,
        "Runtime endpoint must specify a local port");
    anyhow::ensure!(crate::autostart::process_birth(endpoint.pid)?.is_none(),
        "recorded Runtime process {} is still running; stop it before upgrading", endpoint.pid);
    require_runtime_port_closed(address)?;
    Ok(true)
}

pub(crate) fn require_runtime_port_closed(address: std::net::SocketAddr) -> Result<()> {
    match crate::net_rpc::check_connection_to(address) {
        Err(error) if matches!(error.downcast_ref::<crate::runtime_error::RuntimeError>(),
            Some(crate::runtime_error::RuntimeError::Unavailable)) => Ok(()),
        Err(error) => Err(error),
        Ok(()) => anyhow::bail!("Runtime port {} is still occupied; stop it before upgrading", address.port()),
    }
}

/// Call under the installation takeover lock, after confirming no current listener.
pub(crate) fn archive_stopped_endpoint_locked() -> Result<()> {
    if !record_exists(&crate::rpc::endpoint_path())? {
        return Ok(());
    }
    with_stopped_runtime_locked(|_| Ok(()))
}

pub(crate) fn with_stopped_runtime_locked(action: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let path = crate::rpc::endpoint_path();
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let _installation = respire_client_host::lock::LibraryLock::acquire(&root, Duration::ZERO)
        .context("the previous Runtime still holds the installation lock")?;
    let mut _profile = None;
    if profile.try_exists()? {
        let profile = profile.canonicalize()?;
        if profile != root {
            _profile = Some(respire_client_host::lock::LibraryLock::acquire(&profile, Duration::ZERO)
                .context("the previous Runtime still holds the profile lock")?);
        }
    }
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], crate::net_rpc::rpc_port()));
    require_runtime_port_closed(address)?;
    if !record_exists(&path)? {
        return action(&root);
    }
    let parent = path.parent().context("Runtime endpoint directory missing")?;
    anyhow::ensure!(parent.canonicalize()? == root.join("runtime"), "Runtime endpoint directory is outside the installation");
    anyhow::ensure!(fs::symlink_metadata(&path)?.is_file(), "invalid Runtime endpoint file");
    // The endpoint's historical PID and port say nothing about the current
    // listener or the locked library.
    let archive = path.with_file_name(format!("endpoint.stopped-{}.json", uuid::Uuid::new_v4()));
    respire_spawn::durable::publish_new_file(&path, &archive)
        .with_context(|| format!("could not archive the stopped Runtime endpoint at {}", archive.display()))?;
    action(&root)
}

fn verify_install_host() -> Result<()> {
    let legacy = crate::mcp::stable_bin_path();
    // Windows can report NotFound for `file/rsrs.exe`, unlike Unix ENOTDIR.
    // Missing leaf bytes are not evidence that the installation path is usable.
    // Inspect existing ancestors before any takeover lock or lifecycle write.
    for parent in legacy.ancestors().skip(1).filter(|path| !path.as_os_str().is_empty()) {
        match fs::metadata(parent) {
            Ok(metadata) => anyhow::ensure!(metadata.is_dir(), LegacyRuntimeMigrationRequired),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context(LegacyRuntimeMigrationRequired),
        }
    }
    match fs::symlink_metadata(&legacy) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context(LegacyRuntimeMigrationRequired),
        Ok(metadata) => {
            anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(),
                LegacyRuntimeMigrationRequired);
        }
    }
    Ok(())
}

/// Validate historical ownership before changing supervisor or maintenance state.
pub(crate) fn require_automatic_migration_safe() -> Result<()> {
    if let Some(selected) = crate::embedding::selection() {
        // Client2 has already selected this image. Host commands do not migrate
        // or materialize a CLI; installation entry points reject embedding.
        return selected.verify().map(|_| ());
    }
    if let Some(manifest) = read_manifest().context(ManagedRuntimeRepairRequired)? {
        return validate_installed(&manifest).map(|_| ());
    }
    verify_install_host()?;
    shared::migrate_legacy()?;
    if selected_install_for_reuse(None)?.is_none() {
        respire_client_host::config::control_roots().context(LegacyRuntimeMigrationRequired)?;
        let registration = installation_root().join("runtime/autostart/registration.json");
        anyhow::ensure!(
            !record_exists(&registration).context(LegacyRuntimeMigrationRequired)?,
            LegacyRuntimeMigrationRequired
        );
        verify_stopped_endpoint(&crate::rpc::endpoint_path())
            .context(LegacyRuntimeMigrationRequired)?;
    }
    Ok(())
}
