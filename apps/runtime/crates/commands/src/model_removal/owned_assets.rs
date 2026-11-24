mod staging;
pub(super) use staging::{PreparedPair, SealedPair};

use anyhow::{ensure, Result};
use respire_app::{core_sdk::profiles::ModelProfile, embedding, lock::LibraryLock};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Paths which cannot alias the installation-wide legacy model downloader.
pub(super) struct OwnedAssets(PathBuf, PathBuf);

impl OwnedAssets {
    pub(super) fn open(installation: &Path, profile: &Path) -> Result<Self> {
        Self::for_profile(installation, profile, false)
    }

    pub(super) fn provision(installation: &Path, profile: &Path) -> Result<Self> {
        Self::for_profile(installation, profile, true)
    }

    fn for_profile(installation: &Path, profile: &Path, create: bool) -> Result<Self> {
        checked_directory(installation)?;
        checked_directory(profile)?;
        let root = embedding::runtime_owned_namespace(installation, profile)?;
        let runtime = installation.join("runtime");
        let parent = runtime.join("model-assets-v1");
        for directory in [&runtime, &parent, &root] {
            if create {
                match fs::create_dir(directory) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => return Err(error.into()),
                }
            }
            checked_directory(directory)?;
        }
        Ok(Self(root, profile.to_owned()))
    }

    /// A profile namespace is independent of other profiles in the same
    /// installation. The publisher and remover use one lock in that namespace.
    pub(super) fn acquire_ownership(&self, wait: Duration) -> Result<LibraryLock> {
        checked_directory(&self.0)?;
        let directory = self.0.join(".ownership");
        match fs::create_dir(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        checked_directory(&directory)?;
        let db = directory.join("lock.db");
        match fs::symlink_metadata(&db) {
            Ok(_) => checked_entry(&db, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let guard = LibraryLock::acquire(&directory, wait)?;
        self.verify_ownership(&guard)?;
        Ok(guard)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    pub(super) fn model_dir(&self, model: &str) -> Result<PathBuf> {
        let path = self.model_path(model)?;
        checked_directory(&path)?;
        Ok(path)
    }

    fn model_path(&self, model: &str) -> Result<PathBuf> {
        ensure!(
            respire_spawn::owner_credential::model_proof::valid_model_id(model),
            "invalid_managed_model_id"
        );
        Ok(self.0.join(model))
    }

    /// Reconstruct an installed profile only from the Runtime's release pins.
    /// Host overrides remain valid for legacy inference but never grant removal
    /// ownership of a custom or shared artifact path.
    pub(super) fn installed_builtin(
        &self,
        guard: &LibraryLock,
        model: &str,
    ) -> Result<ModelProfile> {
        let mut registry = embedding::builtin_registry()?;
        let pins = registry
            .models
            .remove(model)
            .ok_or_else(|| anyhow::anyhow!("unknown_managed_model"))?;
        self.project_pinned_profile(guard, pins)
    }

    fn project_pinned_profile(
        &self,
        guard: &LibraryLock,
        mut pins: ModelProfile,
    ) -> Result<ModelProfile> {
        self.verify_ownership(guard)?;
        let root = self.model_dir(&pins.id)?;
        let weights = pins
            .weights
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("managed_weights_pin_missing"))?;
        let filename = staging::weight_asset(&weights.path)?;
        let tokenizer = pins
            .tokenizer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("managed_tokenizer_pin_missing"))?;
        for (resource, name) in [(weights, filename), (tokenizer, "tokenizer.json")] {
            let path = root.join(name);
            super::regular_owned_path(&root, &path)?;
            ensure!(
                fs::metadata(&path)?.len() == resource.size,
                "managed_asset_size_mismatch"
            );
            resource.path = path;
        }
        Ok(pins)
    }

    /// The writer and remover must carry the same live profile-level
    /// lock while registering, publishing, checking, or removing artifacts.
    pub(super) fn verify_ownership(&self, guard: &LibraryLock) -> Result<()> {
        let directory = self.0.join(".ownership");
        checked_directory(&directory)?;
        checked_entry(&directory.join("lock.db"), false)?;
        ensure!(guard.protects(&directory)?, "managed_asset_lock_mismatch");
        Ok(())
    }
}

fn checked_directory(path: &Path) -> Result<()> {
    checked_entry(path, true)
}

fn checked_entry(path: &Path, directory: bool) -> Result<()> {
    ensure!(path.is_absolute(), "asset_root_not_canonical");
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        (if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        }) && !metadata.file_type().is_symlink(),
        "asset_not_owned"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "asset_reparse_point"
        );
    }
    ensure!(path.canonicalize()? == path, "asset_root_not_canonical");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_cannot_share_managed_assets_or_lock() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let installation = temporary.path().canonicalize()?;
        let profile_b = installation.join("second test profile");
        fs::create_dir(&profile_b)?;
        let profile_b = profile_b.canonicalize()?;
        let a = OwnedAssets::provision(&installation, &installation)?;
        let b = OwnedAssets::provision(&installation, &profile_b)?;
        let guard_a = a.acquire_ownership(Duration::ZERO)?;
        let guard_b = b.acquire_ownership(Duration::ZERO)?;
        assert_ne!(a.path(), b.path());
        assert!(b.verify_ownership(&guard_a).is_err());
        assert!(a.verify_ownership(&guard_b).is_err());
        let builtin = "bge-small-zh-v1.5";
        let path = a.model_path(builtin)?;
        fs::create_dir(&path)?;
        fs::write(path.join("tokenizer.json"), b"synthetic owned words")?;
        assert_eq!(a.model_dir(builtin)?, path);
        assert!(b.model_dir(builtin).is_err());
        assert_eq!(
            fs::read(path.join("tokenizer.json"))?,
            b"synthetic owned words"
        );
        Ok(())
    }
}
