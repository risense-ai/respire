//! Pending-only ingress owned by the Runtime. Transfer never receives a local
//! pathname from the client and never holds the active store or asset lock.
use super::OwnedAssets;
use crate::model_removal::Binding;
use anyhow::{ensure, Context, Result};
use respire_app::lock::LibraryLock;
use respire_spawn::owner_credential::ModelStage;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(in crate::model_removal) struct PreparedPair {
    owner: Binding,
    namespace: PathBuf,
    #[cfg(test)]
    stage: PathBuf,
    model: String,
    weights: Pending,
    tokenizer: Pending,
    platform: ModelStage,
}

/// A complete, flushed Runtime-owned pair, ready for a bounded publish.
pub(in crate::model_removal) struct SealedPair(PreparedPair);

struct Pending {
    file: Option<File>,
    size: u64,
    written: u64,
    expected: String,
    digest: Sha256,
}

impl Pending {
    fn create(
        stage: &mut ModelStage,
        name: &'static str,
        size: u64,
        expected: String,
    ) -> Result<Self> {
        ensure!(size > 0, "invalid_asset_pin");
        let file = stage.create_file(name)?;
        Ok(Self {
            file: Some(file),
            size,
            written: 0,
            expected,
            digest: Sha256::new(),
        })
    }

    fn append(&mut self, mut input: &[u8]) -> Result<()> {
        let next = self
            .written
            .checked_add(u64::try_from(input.len())?)
            .context("model_asset_size_overflow")?;
        ensure!(next <= self.size, "model_asset_size_exceeded");
        let file = self.file.as_mut().context("model_stage_closed")?;
        while !input.is_empty() {
            let count = file.write(input)?;
            ensure!(count > 0, "model_stage_write_stalled");
            self.digest.update(&input[..count]);
            self.written += count as u64;
            input = &input[count..];
        }
        Ok(())
    }

    fn seal(&self) -> Result<()> {
        ensure!(self.written == self.size, "model_asset_incomplete");
        let actual = format!("{:x}", self.digest.clone().finalize());
        ensure!(
            actual.eq_ignore_ascii_case(&self.expected),
            "model_asset_digest_mismatch"
        );
        let file = self.file.as_ref().context("model_stage_closed")?;
        ensure!(file.metadata()?.len() == self.size, "model_stage_changed");
        Ok(())
    }
}

pub(super) fn weight_asset(path: &Path) -> Result<&'static str> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("onnx") => Ok("model_quantized.onnx"),
        Some("safetensors") => Ok("model.safetensors"),
        _ => anyhow::bail!("unsupported_managed_weight_format"),
    }
}

impl OwnedAssets {
    pub(in crate::model_removal) fn prepare_builtin_pair(
        &self,
        lease: &LibraryLock,
        owner: &Binding,
        model: &str,
    ) -> Result<PreparedPair> {
        self.verify_ownership(lease)?;
        ensure!(self.1 == owner.canonical_profile, "model_scope_changed");
        let pinned = respire_app::embedding::builtin_registry()?
            .models
            .remove(model)
            .context("unknown_managed_model")?;
        let weights = pinned.weights.context("managed_weights_pin_missing")?;
        let tokenizer = pinned.tokenizer.context("managed_tokenizer_pin_missing")?;
        let weight_name = weight_asset(&weights.path)?;
        let stage_name = format!(".incoming-{}", Uuid::new_v4());
        let mut platform = ModelStage::create(&self.0, &stage_name)?;
        let weights = Pending::create(&mut platform, weight_name, weights.size, weights.sha256)?;
        let tokenizer = Pending::create(
            &mut platform,
            "tokenizer.json",
            tokenizer.size,
            tokenizer.sha256,
        )?;
        Ok(PreparedPair {
            owner: owner.clone(),
            namespace: self.0.clone(),
            #[cfg(test)]
            stage: self.0.join(stage_name),
            model: model.to_owned(),
            weights,
            tokenizer,
            platform,
        })
    }
}

impl PreparedPair {
    pub(in crate::model_removal) fn pinned_lengths(&self) -> (u64, u64) {
        (self.weights.size, self.tokenizer.size)
    }

    pub(in crate::model_removal) fn write_weights(&mut self, bytes: &[u8]) -> Result<()> {
        self.weights.append(bytes)
    }

    pub(in crate::model_removal) fn write_tokenizer(&mut self, bytes: &[u8]) -> Result<()> {
        self.tokenizer.append(bytes)
    }

    pub(in crate::model_removal) fn seal(mut self) -> Result<SealedPair> {
        self.weights.seal()?;
        self.tokenizer.seal()?;
        drop(self.weights.file.take());
        drop(self.tokenizer.file.take());
        self.platform.seal()?;
        Ok(SealedPair(self))
    }
}

impl SealedPair {
    pub(in crate::model_removal) fn commit(
        mut self,
        assets: &OwnedAssets,
        lease: &LibraryLock,
        current: &Binding,
        checkpoint: impl Fn() -> Result<()>,
    ) -> Result<PathBuf> {
        let stage = &mut self.0;
        ensure!(
            &stage.owner == current && stage.namespace == assets.0,
            "model_scope_changed"
        );
        assets.verify_ownership(lease)?;
        let destination = assets.model_path(&stage.model)?;
        stage.platform.publish(&assets.0, &stage.model, || {
            checkpoint().map_err(io::Error::other)
        })?;
        Ok(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_removal::secure_temp;
    use std::{io::Read, time::Duration};

    const FIXTURE_MODEL: &str = "bge-base-zh-v1.5";

    fn owner(profile: &Path) -> Binding {
        Binding {
            owner: "synthetic-model-test".into(),
            canonical_profile: profile.to_owned(),
            instance: Uuid::new_v4(),
            generation: 1,
            vault_epoch: Uuid::new_v4(),
        }
    }

    fn synthetic_pair(assets: &OwnedAssets, owner: &Binding, valid: bool) -> Result<PreparedPair> {
        const MODEL: &[u8] = b"isolated model weights";
        const TOKENIZER: &[u8] = b"isolated tokenizer";
        let name = format!(".incoming-{}", Uuid::new_v4());
        let mut platform = ModelStage::create(&assets.0, &name)?;
        let mut weights = Pending::create(
            &mut platform,
            "model_quantized.onnx",
            MODEL.len() as u64,
            format!(
                "{:x}",
                Sha256::digest(if valid { MODEL } else { b"wrong pin" })
            ),
        )?;
        let mut tokenizer = Pending::create(
            &mut platform,
            "tokenizer.json",
            TOKENIZER.len() as u64,
            format!("{:x}", Sha256::digest(TOKENIZER)),
        )?;
        weights.append(MODEL)?;
        tokenizer.append(TOKENIZER)?;
        Ok(PreparedPair {
            owner: owner.clone(),
            namespace: assets.0.clone(),
            stage: assets.0.join(name),
            model: FIXTURE_MODEL.into(),
            weights,
            tokenizer,
            platform,
        })
    }

    #[test]
    fn atomic_publication_rejects_digest_scope_and_precommit_cancel_without_replacing() -> Result<()>
    {
        let (_directory, root) = secure_temp()?;
        let assets = OwnedAssets::provision(&root, &root)?;
        let original = owner(&root);
        let malformed = synthetic_pair(&assets, &original, false)?;
        let malformed_path = malformed.stage.clone();
        assert!(malformed.seal().is_err());
        assert!(!malformed_path.exists());
        let lease = assets.acquire_ownership(Duration::ZERO)?;
        let destination = assets.model_path(FIXTURE_MODEL)?;
        std::fs::create_dir(&destination)?;
        let mut switched = original.clone();
        switched.generation += 1;
        let stale = synthetic_pair(&assets, &original, true)?;
        let stale_path = stale.stage.clone();
        assert_eq!(
            stale
                .seal()?
                .commit(&assets, &lease, &switched, || Ok(()))
                .unwrap_err()
                .to_string(),
            "model_scope_changed"
        );
        assert!(!stale_path.exists());
        let cancelled = synthetic_pair(&assets, &original, true)?;
        let cancelled_path = cancelled.stage.clone();
        assert!(cancelled
            .seal()?
            .commit(&assets, &lease, &original, || {
                anyhow::bail!("model_operation_cancelled")
            })
            .is_err());
        assert!(!cancelled_path.exists());
        assert!(destination.read_dir()?.next().is_none());
        let staged = synthetic_pair(&assets, &original, true)?;
        let stage_path = staged.stage.clone();
        assert_eq!(
            staged
                .seal()?
                .commit(&assets, &lease, &original, || Ok(()))?,
            destination
        );
        assert!(!stage_path.exists());
        assert_eq!(
            std::fs::read(destination.join("model_quantized.onnx"))?,
            b"isolated model weights"
        );
        assert_eq!(
            std::fs::read(destination.join("tokenizer.json"))?,
            b"isolated tokenizer"
        );
        let duplicate = synthetic_pair(&assets, &original, true)?;
        let duplicate_path = duplicate.stage.clone();
        assert!(duplicate
            .seal()?
            .commit(&assets, &lease, &original, || Ok(()))
            .is_err());
        assert!(!duplicate_path.exists());
        assert_eq!(destination.read_dir()?.count(), 2);
        Ok(())
    }

    #[test]
    fn pending_removal_recovery_blocks_new_and_already_sealed_publication() -> Result<()> {
        let (_directory, root) = secure_temp()?;
        let assets = OwnedAssets::provision(&root, &root)?;
        let original = owner(&root);
        let sealed = synthetic_pair(&assets, &original, true)?.seal()?;
        let lease = assets.acquire_ownership(Duration::ZERO)?;
        let recovery = assets.0.join(".runtime-model-removal-v1");
        std::fs::create_dir(&recovery)?;
        std::fs::write(
            recovery.join("journal.json"),
            b"isolated pending transaction",
        )?;
        assert!(assets
            .prepare_builtin_pair(&lease, &original, FIXTURE_MODEL)
            .is_err());
        assert!(sealed
            .commit(&assets, &lease, &original, || Ok(()))
            .is_err());
        assert!(!assets.model_path(FIXTURE_MODEL)?.exists());
        assert_eq!(
            std::fs::read(recovery.join("journal.json"))?,
            b"isolated pending transaction"
        );
        assert!(assets.0.read_dir()?.all(|entry| {
            entry.is_ok_and(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".incoming-")
            })
        }));
        Ok(())
    }

    #[test]
    fn pending_content_is_not_visible_and_cannot_seal() -> Result<()> {
        let (directory, root) = secure_temp()?;
        let _keep = directory;
        let assets = OwnedAssets::provision(&root, &root)?;
        let lease = assets.acquire_ownership(Duration::ZERO)?;
        let original = owner(&root);
        let mut stage = assets.prepare_builtin_pair(&lease, &original, FIXTURE_MODEL)?;
        let temporary = stage.stage.clone();
        stage.write_weights(b"incomplete")?;
        stage.write_tokenizer(b"incomplete")?;
        assert!(assets.installed_builtin(&lease, FIXTURE_MODEL).is_err());
        let error = stage.seal().err().expect("partial model cannot seal");
        assert_eq!(error.to_string(), "model_asset_incomplete");
        assert!(!temporary.exists());
        assert!(assets.installed_builtin(&lease, FIXTURE_MODEL).is_err());
        Ok(())
    }

    #[test]
    fn sealed_ingress_releases_lock_and_rechecks_real_catalog_pair() -> Result<()> {
        let Some(source) = std::env::var_os("RESPIRE_TEST_REAL_MODEL_FIXTURE") else {
            return Ok(());
        };
        let source = PathBuf::from(source);
        ensure!(
            source.is_absolute(),
            "absolute external fixture path required"
        );
        let (directory, root) = secure_temp()?;
        let _keep = directory;
        let assets = OwnedAssets::provision(&root, &root)?;
        let original = owner(&root);
        let mut stage = {
            let lease = assets.acquire_ownership(Duration::ZERO)?;
            assets.prepare_builtin_pair(&lease, &original, FIXTURE_MODEL)?
        };
        let stage_path = stage.stage.clone();
        for (name, is_weights) in [("model_quantized.onnx", true), ("tokenizer.json", false)] {
            let mut file = File::open(source.join(name))?;
            let mut chunk = [0; 16 * 1024];
            loop {
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                if is_weights {
                    stage.write_weights(&chunk[..n])?;
                } else {
                    stage.write_tokenizer(&chunk[..n])?;
                }
            }
        }
        assert_eq!(
            stage.write_tokenizer(&[0]).unwrap_err().to_string(),
            "model_asset_size_exceeded"
        );
        let sealed = stage.seal()?;
        let lease = assets.acquire_ownership(Duration::ZERO)?;
        assert!(assets.installed_builtin(&lease, FIXTURE_MODEL).is_err());
        let installed = sealed.commit(&assets, &lease, &original, || Ok(()))?;
        assert!(!stage_path.exists());
        assert!(installed.join("tokenizer.json").is_file());
        assert!(assets.installed_builtin(&lease, FIXTURE_MODEL).is_ok());
        Ok(())
    }
}
