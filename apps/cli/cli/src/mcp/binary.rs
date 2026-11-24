use super::*;

/// Native daemon directory. Default: `~/.rsrs/bin` (profile root, not the
/// current account `data_dir()`). Isolation: `RSRS_DATA_DIR` / `RSRS_BIN_DIR`.
pub use respire_client_host::paths::bin_dir;

pub fn stable_bin_path() -> PathBuf {
    let name = if cfg!(windows) { "rsrs.exe" } else { "rsrs" };
    bin_dir().join(name)
}

/// Copy this process's native binary to `stable_bin_path()` when missing or stale.
pub fn materialize_bin() -> Result<PathBuf, String> {
    if let Some(selected) = crate::embedding::selection() {
        return selected.verify().map(|runtime| runtime.path().to_path_buf())
            .map_err(|error| error.to_string());
    }
    crate::runtime_policy::require_host("runtime binary installation")
        .map_err(|error| error.to_string())?;
    let dest = stable_bin_path();
    let src = std::env::current_exe().map_err(|err| format!("cannot locate this binary: {err}"))?;
    if same_path(&src, &dest) {
        return Ok(dest);
    }
    // cargo test binaries live under target/*/deps. Never overwrite the user
    // ~/.rsrs/bin with a test harness. Sweep and unit tests isolate data_dir.
    if respire::env::var("RSRS_BIN_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_none()
        && is_cargo_test_bin(&src)
    {
        return Ok(src);
    }
    install_binary(&src, &dest)?;
    Ok(dest)
}

// The CLI is standalone; Runtime and its native libraries use their own verified installer.
pub(crate) fn install_binary(src: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().ok_or("CLI destination has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    if !bin_needs_refresh(src, dest)? {
        return Ok(());
    }
    // Publish complete bytes atomically; truncating a live Unix image can fail or corrupt it.
    let temporary = parent.join(format!(".rsrs-{}", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o700);
        }
        let mut output = options.open(&temporary)?;
        std::io::copy(&mut std::fs::File::open(src)?, &mut output)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        output.sync_all()?;
        drop(output);
        anyhow::ensure!(!bin_needs_refresh(src, &temporary).map_err(anyhow::Error::msg)?,
            "staged CLI bytes differ from the invoking executable");
        if dest.try_exists()? {
            respire_spawn::durable::replace_file(&temporary, dest)?;
        } else {
            respire_spawn::durable::publish_new_file(&temporary, dest)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("cannot install CLI at {}: {error:#}", dest.display()))
}

pub fn exe_matches_dest(exe: &str, dest: &Path) -> bool {
    if exe.trim().is_empty() {
        return false;
    }
    same_path(Path::new(exe), dest)
}

pub fn bin_needs_refresh(src: &Path, dest: &Path) -> Result<bool, String> {
    if !dest.try_exists().map_err(|error| error.to_string())? {
        return Ok(true);
    }
    Ok(file_sha256(src)? != file_sha256(dest)?)
}

fn is_cargo_test_bin(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.contains("/deps/") || text.contains("/incremental/")
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    fn canon(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
    canon(a) == canon(b)
}

pub(super) fn file_sha256(path: &Path) -> Result<[u8; 32], String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    reader_sha256(file)
}

pub(super) fn reader_sha256(mut file: impl Read) -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    // Bound transient memory rather than allocating one executable-sized Vec.
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}
