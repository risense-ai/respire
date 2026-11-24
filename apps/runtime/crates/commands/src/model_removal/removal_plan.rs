//! Validate the large immutable model files off the serving worker. A typed
//! receipt may be committed only after Runtime rechecks the live inventory and
//! that both names still identify the exact files vetted by the verifier.
use super::*;
use anyhow::ensure;
use respire_spawn::owner_credential::{open_model_file, same_model_file};

pub(crate) struct PendingRemoval {
    binding: Binding,
    request: Request,
    root: PathBuf,
    files: Vec<OwnedFile>,
}

pub(crate) struct VerifiedRemoval(PendingRemoval);

impl OwnedFile {
    fn open(pin: ManagedPin) -> Result<Self> {
        let file = open_model_file(&pin.path)?;
        let metadata = file.metadata()?;
        ensure!(metadata.len() == pin.size, "asset_identity_changed");
        Ok(Self {
            pin,
            file,
            modified: metadata.modified()?,
        })
    }

    fn unchanged(&self) -> Result<()> {
        let metadata = self.file.metadata()?;
        ensure!(
            metadata.len() == self.pin.size && metadata.modified()? == self.modified,
            "asset_identity_changed"
        );
        Ok(())
    }

    pub(super) fn still_names(&self, path: &Path) -> Result<()> {
        self.unchanged()?;
        let current = open_model_file(path)?;
        ensure!(
            same_model_file(&self.file, &current)?,
            "asset_identity_changed"
        );
        Ok(())
    }
}

impl PendingRemoval {
    pub(super) fn prepare(
        authority: &impl Authority,
        binding: &Binding,
        request: Request,
    ) -> Result<Self> {
        let (root, files) = with_inventory(authority, binding, &request, |root, pins| {
            let files = pins
                .into_iter()
                .map(OwnedFile::open)
                .collect::<Result<Vec<_>>>()?;
            authority.check(binding)?;
            Ok((root.to_path_buf(), files))
        })?;
        Ok(Self {
            binding: binding.clone(),
            request,
            root,
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
            root,
            files,
        } = self.0;
        ensure!(&binding == current, "model_scope_changed");
        with_inventory(authority, current, &request, |current_root, pins| {
            ensure!(
                root == current_root && pins.len() == files.len(),
                "asset_identity_changed"
            );
            for (pin, file) in pins.iter().zip(&files) {
                ensure!(pin == &file.pin, "asset_identity_changed");
                file.still_names(&pin.path)?;
            }
            authority.check(current)?;
            transact(current_root, current, &request.model_id, files, || {
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
