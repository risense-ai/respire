//! Adapter for the already-locked Core Runtime store. No request paths,
//! profiles, registry entries or downloaded pins are trusted from the client.
use super::{
    owned_assets::{OwnedAssets, PreparedPair},
    Active, Authority, Binding, Receipt, Request, Scope, State,
};
use anyhow::{ensure, Context, Result};
use respire_app::{embedding, lock::LibraryLock, transport::local::LocalStore};
use std::{path::PathBuf, time::Duration};

struct ServingAuthority<'a, C> {
    binding: &'a Binding,
    installation_root: PathBuf,
    store: &'a LocalStore,
    library: &'a LibraryLock,
    model: &'a str,
    live_checkpoint: C,
}

impl<C: Fn() -> Result<()>> Authority for ServingAuthority<'_, C> {
    fn with_current<T>(
        &self,
        binding: &Binding,
        action: impl FnOnce(State<'_>) -> Result<T>,
    ) -> Result<T> {
        self.check(binding)?;
        ensure!(
            self.library.protects(&binding.canonical_profile)?,
            "serving_library_scope_mismatch"
        );
        // Report a live or pending selection before opening removal files.
        // The removal transaction repeats selection and shared-asset checks.
        let selection = self.store.active_selection()?;
        let pending = self.store.pending_selection()?;
        ensure!(
            selection
                .as_ref()
                .map_or("m3", |selected| selected.model.as_str())
                != self.model
                && pending
                    .as_ref()
                    .is_none_or(|selected| selected.model != self.model),
            "model_in_use"
        );
        let managed = OwnedAssets::open(&self.installation_root, &binding.canonical_profile)?;
        let assets = managed.acquire_ownership(Duration::ZERO)?;
        let installed = managed.installed_builtin(&assets, self.model)?;
        // Include configured entries for shared-path checks, but replace just
        // the uninstall candidate with the verified, pinned managed entry.
        let mut registry = embedding::registry()?;
        let active = match selection {
            Some(selected) => Active::Explicit(selected),
            None => Active::ImplicitM3(
                registry
                    .models
                    .get("m3")
                    .cloned()
                    .context("serving default model unavailable")?,
            ),
        };
        registry.models.insert(installed.id.clone(), installed);
        self.check(binding)?;
        action(State {
            binding: binding.clone(),
            registry,
            installation_root: self.installation_root.clone(),
            asset_ownership: &assets,
            scopes: vec![Scope {
                profile: &binding.canonical_profile,
                ownership: self.library,
                active,
                pending,
            }],
        })
    }

    fn check(&self, binding: &Binding) -> Result<()> {
        ensure!(self.binding == binding, "model_scope_changed");
        (self.live_checkpoint)()
    }
}

/// Unpublished transfer tied to the exact NativeOwner identity. The buffer
/// accepts bytes, not paths, and owns no active store or asset lock.
pub(crate) struct StagedModelTransfer(PreparedPair);

/// Only the off-worker verifier can create this publishable value.
pub(crate) struct SealedModelTransfer(super::owned_assets::SealedPair);

impl StagedModelTransfer {
    pub(crate) fn seal(self) -> Result<SealedModelTransfer> {
        Ok(SealedModelTransfer(self.0.seal()?))
    }

    pub(crate) fn pinned_lengths(&self) -> (u64, u64) {
        self.0.pinned_lengths()
    }

    pub(crate) fn write_weights(&mut self, bytes: &[u8]) -> Result<()> {
        self.0.write_weights(bytes)
    }

    pub(crate) fn write_tokenizer(&mut self, bytes: &[u8]) -> Result<()> {
        self.0.write_tokenizer(bytes)
    }
}

fn verified_installation(binding: &Binding, library: &LibraryLock) -> Result<PathBuf> {
    let roots = respire_app::service::resolved_roots()?;
    ensure!(
        roots.account_data_root.canonicalize()? == binding.canonical_profile
            && library.protects(&binding.canonical_profile)?,
        "model_scope_changed"
    );
    roots.installation_root.canonicalize().map_err(Into::into)
}

/// Call only while holding a verified live owner-store scope. Locks are
/// dropped before the caller receives any transfer bytes.
pub(crate) fn prepare_pair_in_fenced_scope(
    binding: &Binding,
    library: &LibraryLock,
    model: &str,
) -> Result<StagedModelTransfer> {
    let installation = verified_installation(binding, library)?;
    let assets = OwnedAssets::provision(&installation, &binding.canonical_profile)?;
    let lease = assets.acquire_ownership(Duration::ZERO)?;
    Ok(StagedModelTransfer(
        assets.prepare_builtin_pair(&lease, binding, model)?,
    ))
}

/// Revalidate the owner scope after transfer, then publish one complete pair.
pub(crate) fn publish_pair_in_fenced_scope(
    binding: &Binding,
    library: &LibraryLock,
    stage: SealedModelTransfer,
    checkpoint: impl Fn() -> Result<()>,
) -> Result<()> {
    checkpoint()?;
    let installation = verified_installation(binding, library)?;
    let assets = OwnedAssets::open(&installation, &binding.canonical_profile)?;
    let lease = assets.acquire_ownership(Duration::ZERO)?;
    stage.0.commit(&assets, &lease, binding, checkpoint)?;
    Ok(())
}

/// The caller must hold the verified owner-store scope during each short
/// phase. Hashing the returned removal plan never owns this scope.
fn removal_authority<'a, C: Fn() -> Result<()>>(
    binding: &'a Binding,
    store: &'a LocalStore,
    library: &'a LibraryLock,
    model: &'a str,
    live_checkpoint: C,
) -> Result<ServingAuthority<'a, C>> {
    Ok(ServingAuthority {
        binding,
        installation_root: verified_installation(binding, library)?,
        store,
        library,
        model,
        live_checkpoint,
    })
}

pub(crate) fn prepare_removal_in_fenced_scope(
    binding: &Binding,
    store: &LocalStore,
    library: &LibraryLock,
    request: Request,
    checkpoint: impl Fn() -> Result<()>,
) -> Result<super::PendingRemoval> {
    let model = request.model_id.clone();
    let authority = removal_authority(binding, store, library, &model, checkpoint)?;
    super::PendingRemoval::prepare(&authority, binding, request)
}

pub(crate) fn commit_removal_in_fenced_scope(
    binding: &Binding,
    store: &LocalStore,
    library: &LibraryLock,
    removal: super::VerifiedRemoval,
    checkpoint: impl Fn() -> Result<()>,
) -> Result<Receipt> {
    let model = removal.model_id().to_owned();
    let authority = removal_authority(binding, store, library, &model, checkpoint)?;
    removal.commit(&authority, binding)
}
