fn check_path(path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute() && path.to_str().is_some(),
        "model maintenance path must be absolute Unicode"
    );
    let mut prefix = PathBuf::new();
    for component in path.components() {
        anyhow::ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "model path has dot components"
        );
        if let Component::Normal(name) = component {
            let text = name.to_str().context("model path must be Unicode")?;
            anyhow::ensure!(
                !text.contains([':', '\\']) && !text.ends_with(['.', ' ']),
                "ambiguous model path"
            );
        }
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "model maintenance cannot follow symlinks"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "model maintenance cannot follow reparse points"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn local_m3_directory() -> Result<PathBuf> {
    let home = || {
        dirs::home_dir()
            .or_else(|| respire_client_host::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| respire_client_host::env::var_os("USERPROFILE").map(PathBuf::from))
    };
    let raw = respire_client_host::env::var_os("RSRS_M3_DIR");
    let path = if let Some(raw) = raw {
        let raw = raw.to_str().context("RSRS_M3_DIR must be Unicode")?.trim();
        anyhow::ensure!(!raw.is_empty(), "RSRS_M3_DIR must not be empty");
        match raw.strip_prefix("~/") {
            Some(rest) => home().context("model override home missing")?.join(rest),
            None => PathBuf::from(raw),
        }
    } else if let Some(root) = respire_client_host::env::var_os("RSRS_DATA_DIR") {
        PathBuf::from(root).join("models/bge-m3")
    } else {
        home()
            .context("model home missing")?
            .join(".rsrs/models/bge-m3")
    };
    Ok(std::path::absolute(path)?)
}

fn remove_m3(root: &Path, profile: &Path, target: &Path) -> Result<bool> {
    let local = local_m3_directory()?;
    anyhow::ensure!(
        target == local
            && !target
                .to_string_lossy()
                .replace('\\', "/")
                .contains("/usr/lib/respire/models")
            && target.parent().is_some(),
        "uninstall target is outside the exact local model destination"
    );
    check_path(target)?;
    for protected in [
        Some(root.to_path_buf()),
        Some(profile.to_path_buf()),
        dirs::home_dir(),
    ]
    .into_iter()
    .flatten()
    {
        anyhow::ensure!(
            target != protected && !protected.starts_with(target),
            "refusing to remove an installation/profile/home ancestor"
        );
    }
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir(),
            "model uninstall target is not a directory"
        ),
    }
    let canonical = target.canonicalize()?;
    for protected in [
        Some(root.to_path_buf()),
        Some(profile.to_path_buf()),
        dirs::home_dir(),
    ]
    .into_iter()
    .flatten()
    {
        let protected = protected.canonicalize()?;
        anyhow::ensure!(
            canonical != protected && !protected.starts_with(&canonical),
            "canonical model target is an installation/profile/home ancestor"
        );
    }
    let mut pending = vec![target.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            check_path(&path)?;
            anyhow::ensure!(
                path.canonicalize()?.starts_with(&canonical),
                "model deletion escapes its exact target"
            );
            let metadata = fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                metadata.is_dir() || metadata.is_file(),
                "model deletion refuses special files"
            );
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    check_path(target)?;
    anyhow::ensure!(
        target.canonicalize()? == canonical,
        "model uninstall target changed"
    );
    fs::remove_dir_all(target)?;
    Ok(true)
}

#[cfg(not(windows))]
fn install_catalog(_root: &Path, pin: Option<&ModelCatalogPin>) -> Result<Vec<String>> {
    anyhow::ensure!(pin.is_none(), "Windows ML plan is invalid on this host");
    Ok(Vec::new())
}

#[cfg(windows)]
fn install_catalog(root: &Path, pin: Option<&ModelCatalogPin>) -> Result<Vec<String>> {
    let pin = pin.context("Windows ML catalog plan missing")?;
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        anyhow::bail!("unsupported Windows ML architecture")
    };
    let directory = root.join("engines/winml-2.4.89");
    let dll = directory.join("Microsoft.Windows.AI.MachineLearning.dll");
    anyhow::ensure!(
        pin.version == "2.4.89"
            && pin.package_source == PACKAGE_SOURCE
            && pin.package_sha256 == PACKAGE_SHA256
            && pin.destination == dll
            && pin.archive_member
                == format!("runtimes/win-{arch}/native/Microsoft.Windows.AI.MachineLearning.dll"),
        "catalog plan differs from local official pin/architecture/destination"
    );
    check_path(&dll)?;
    fs::create_dir_all(&directory)?;
    let package = directory.join(".winml-2.4.89.nupkg");
    check_path(&package)?;
    respire_commands::model_download::download_with_fallback(
        &[PACKAGE_SOURCE.into()],
        &package,
        PACKAGE_SHA256,
        false,
        Default::default(),
        None,
    )?;
    let mut archive = zip::ZipArchive::new(fs::File::open(&package)?)?;
    for (member, destination, limit) in [
        (
            pin.archive_member.as_str(),
            dll.clone(),
            64 * 1024 * 1024u64,
        ),
        ("license.txt", directory.join("license.txt"), 1024 * 1024u64),
    ] {
        let file = archive.by_name(member)?;
        anyhow::ensure!(
            file.is_file() && file.size() <= limit,
            "catalog package member is not a bounded regular file"
        );
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= limit,
            "catalog package member exceeds bound"
        );
        if destination == dll {
            anyhow::ensure!(
                bytes.len() as u64 == pin.dll_size && hash(&bytes) == pin.dll_sha256,
                "catalog DLL differs from the official host pin"
            );
        }
        publish(&destination, &bytes)?;
    }
    // Only the host invokes Windows ML's installer. A provider manifest is not
    // proof that an unpinned hardware payload is safe or ready for inference.
    let providers = respire_spawn::install_accelerators(&dll)?;
    publish(
        &root.join("engines/providers.json"),
        &serde_json::to_vec_pretty(&providers)?,
    )?;
    Ok(providers.into_keys().collect())
}

#[cfg(windows)]
fn publish(target: &Path, bytes: &[u8]) -> Result<()> {
    check_path(target)?;
    let parent = target.parent().context("catalog target parent missing")?;
    let staged = parent.join(format!(".catalog-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        check_path(target)?;
        if target.try_exists()? {
            respire_spawn::durable::replace_file(&staged, target)?;
        } else {
            respire_spawn::durable::publish_new_file(&staged, target)?;
        }
        Ok(())
    })();
    if result.is_err() && staged.try_exists()? {
        fs::remove_file(staged)?;
    }
    result
}
