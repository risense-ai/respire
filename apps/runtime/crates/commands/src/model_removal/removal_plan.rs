//! Validate the large immutable model files off the serving worker. A typed
//! receipt may be committed only after Runtime rechecks the live inventory and
//! that both names still identify the exact files vetted by the verifier.
use super::*;
use anyhow::ensure;
use respire_spawn::owner_credential::same_model_file;

pub(crate) struct PendingRemoval {
    binding: Binding,
    request: Request,
    platform: ModelRemovalRoot,
    files: Vec<OwnedFile>,
}

pub(crate) struct VerifiedRemoval(PendingRemoval);

impl OwnedFile {
    fn open(root: &ModelRemovalRoot, pin: ManagedPin) -> Result<Self> {
        let leaf = pin
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid managed asset leaf")?;
        let file = root.open_model_asset(leaf)?;
        let metadata = file.metadata()?;
        ensure!(metadata.len() == pin.size, "asset_identity_changed");
        Ok(Self {
            pin,
            file,
            modified: metadata.modified()?,
        })
    }

    pub(super) fn unchanged(&self) -> Result<()> {
        let metadata = self.file.metadata()?;
        ensure!(
            metadata.len() == self.pin.size && metadata.modified()? == self.modified,
            "asset_identity_changed"
        );
        Ok(())
    }

    pub(super) fn still_names(&self, root: &ModelRemovalRoot) -> Result<()> {
        self.unchanged()?;
        let current = root.open_model_asset(self.leaf()?)?;
        ensure!(
            same_model_file(&self.file, &current)?,
            "asset_identity_changed"
        );
        Ok(())
    }

    pub(super) fn leaf(&self) -> Result<&str> {
        self.pin
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid managed asset leaf")
    }
}

impl PendingRemoval {
    pub(super) fn prepare(
        authority: &impl Authority,
        binding: &Binding,
        request: Request,
    ) -> Result<Self> {
        let (platform, files) = with_inventory(authority, binding, &request, |root, pins| {
            let platform = ModelRemovalRoot::open(root, &request.model_id)?;
            let files = pins
                .into_iter()
                .map(|pin| OwnedFile::open(&platform, pin))
                .collect::<Result<Vec<_>>>()?;
            authority.check(binding)?;
            Ok((platform, files))
        })?;
        Ok(Self {
            binding: binding.clone(),
            request,
            platform,
            files,
        })
    }

    pub(crate) fn verify(mut self, checkpoint: impl Fn() -> Result<()>) -> Result<VerifiedRemoval> {
        for file in &mut self.files {
            file.unchanged()?;
            verify_file(&mut file.file, file.pin.size, &file.pin.sha256, &checkpoint)?;
            file.unchanged()?;
        }
        checkpoint()?;
        Ok(VerifiedRemoval(self))
    }
}

impl VerifiedRemoval {
    pub(crate) fn model_id(&self) -> &str {
        &self.0.request.model_id
    }

    pub(super) fn commit(self, authority: &impl Authority, current: &Binding) -> Result<Receipt> {
        let PendingRemoval {
            binding,
            request,
            platform,
            files,
        } = self.0;
        ensure!(&binding == current, "model_scope_changed");
        with_inventory(authority, current, &request, |current_root, pins| {
            platform.reauthorize(current_root)?;
            ensure!(pins.len() == files.len(), "asset_identity_changed");
            for (pin, file) in pins.iter().zip(&files) {
                ensure!(pin == &file.pin, "asset_identity_changed");
                file.still_names(&platform)?;
            }
            authority.check(current)?;
            transact(&platform, &request.model_id, files, || {
                authority.check(current)
            })
        })
    }
}

#[cfg(test)]
pub(super) fn uninstall(
    authority: &impl Authority,
    binding: &Binding,
    request: Request,
) -> Result<Receipt> {
    PendingRemoval::prepare(authority, binding, request)?
        .verify(|| authority.check(binding))?
        .commit(authority, binding)
}
