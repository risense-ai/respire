use super::{checked_path, configured_scope};
use anyhow::{Context, Result};
use respire_app::{lock::LibraryLock, login_transaction::AuthOwnership};
use std::{fs, path::Path};

/// For a future authenticated, one-shot offline keygen helper only.
/// This does not grant authority to a caller or validate a launch grant.
pub(crate) fn with_configured_scope<T>(
    action: impl FnOnce(&Path, &Path, &AuthOwnership) -> Result<T>,
) -> Result<T> {
    let (root, profile) = configured_scope()?;
    let (_takeover, _lifetime) = acquire_runtime_exclusion(&root)?;
    anyhow::ensure!(
        configured_scope()? == (root.clone(), profile.clone()),
        "offline keygen configured scope changed"
    );
    verify_lock_entries(&root)?;
    if profile != root {
        verify_lock_entries(&profile)?;
    }
    let journal_lock = root.join(".login-recovery.json.lock");
    verify_file_slot(&journal_lock)?;
    let owner = AuthOwnership::try_acquire(&root, &profile)?;
    anyhow::ensure!(
        configured_scope()? == (root.clone(), profile.clone()),
        "offline keygen configured scope changed"
    );
    action(&root, &profile, &owner)
}

fn acquire_runtime_exclusion(root: &Path) -> Result<(LibraryLock, LibraryLock)> {
    let takeover_dir = super::takeover_directory(root)?;
    let takeover =
        LibraryLock::try_acquire(&takeover_dir).context("offline keygen takeover unavailable")?;
    let runtime = root.join("runtime");
    verify_lock_entries(&runtime)?;
    let lifetime = LibraryLock::try_acquire(&runtime)
        .context("offline keygen requires an offline installation")?;
    anyhow::ensure!(
        takeover.protects(&takeover_dir)? && lifetime.protects(&runtime)?,
        "offline keygen runtime exclusion identity changed"
    );
    Ok((takeover, lifetime))
}

fn verify_lock_entries(directory: &Path) -> Result<()> {
    anyhow::ensure!(
        checked_path(directory)? == directory,
        "offline keygen lock directory changed"
    );
    for name in ["lock.db", "lock.db-journal", "lock.db-wal", "lock.db-shm"] {
        verify_file_slot(&directory.join(name))?;
    }
    Ok(())
}

fn verify_file_slot(path: &Path) -> Result<()> {
    anyhow::ensure!(
        checked_path(path)? == path,
        "offline keygen lock path changed"
    );
    match fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(metadata.is_file(), "offline keygen lock is not a file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_startup_and_lifetime_exclusions_are_distinct_and_refused() -> Result<()> {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("runtime-keygen-exclusion-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        let temp = Fixture(path);
        let root = temp.0.canonicalize()?;
        let takeover_dir = root.join("runtime/takeover");
        let old = LibraryLock::try_acquire(&takeover_dir)?;
        assert!(acquire_runtime_exclusion(&root).is_err());
        drop(old);
        let lifetime = LibraryLock::try_acquire(&root.join("runtime"))?;
        assert!(acquire_runtime_exclusion(&root).is_err());
        // An unsuccessful attempt on the lifetime lock must release takeover.
        drop(LibraryLock::try_acquire(&takeover_dir)?);
        drop(lifetime);
        let both = acquire_runtime_exclusion(&root)?;
        assert!(LibraryLock::try_acquire(&takeover_dir).is_err());
        assert!(LibraryLock::try_acquire(&root.join("runtime")).is_err());
        drop(both);
        assert!(acquire_runtime_exclusion(&root).is_ok());
        Ok(())
    }
}
