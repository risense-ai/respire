use super::{current_runtime_session, SessionKeys, RUNTIME_SESSION};
use crate::{
    core_sdk::profiles::Selection, memory::search::Embedder, transport::local::LocalStore,
};
use anyhow::Result;

/// Only typed library operations can use the verified vault.
pub struct RuntimeLibrary<'a>(pub(super) &'a SessionKeys);

pub fn with_runtime_library<T>(
    canonical_profile: &std::path::Path,
    action: impl FnOnce(&str, uuid::Uuid, RuntimeLibrary<'_>) -> T,
) -> Result<Option<T>> {
    let unlocked = RUNTIME_SESSION
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime session lock poisoned"))?;
    let Some(session) = unlocked.as_ref() else {
        return Ok(None);
    };
    let Some(owner) = session.owner.as_deref() else {
        return Ok(None);
    };
    if !current_runtime_session(session, canonical_profile)? {
        return Ok(None);
    }
    Ok(Some(action(
        owner,
        session.authority_epoch,
        RuntimeLibrary(&session.keys),
    )))
}

/// Keeps the verified keys private while a switch runs between vault checks.
pub struct ModelSwitchVault(SessionKeys);

impl RuntimeLibrary<'_> {
    pub fn model_switch_vault(&self) -> ModelSwitchVault {
        ModelSwitchVault(self.0.clone())
    }
}

impl ModelSwitchVault {
    pub fn from_session(keys: SessionKeys) -> Self {
        Self(keys)
    }

    pub fn build_main(
        &self,
        store: &LocalStore,
        embedder: &impl Embedder,
        target: &Selection,
        progress: impl FnMut(usize, usize) -> Result<()>,
    ) -> Result<usize> {
        store.build_candidate_index_with_progress(&self.0, embedder, target, progress)
    }

    pub fn build_diary(
        &self,
        store: &LocalStore,
        embedder: &impl Embedder,
        check: impl FnMut() -> Result<()>,
        commit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
        progress: impl FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        crate::diary::rebuild_with_progress(store, &self.0, embedder, check, commit, progress)
    }
}
