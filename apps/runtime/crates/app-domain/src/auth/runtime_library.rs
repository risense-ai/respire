use super::{current_runtime_session, SessionKeys, RUNTIME_SESSION};
use crate::{
    core_sdk::profiles::Selection, memory::search::Embedder, transport::local::LocalStore,
};
use anyhow::Result;

/// Only typed library operations can use the verified vault.
pub struct RuntimeLibrary<'a>(pub(super) &'a SessionKeys, &'a str, &'a std::path::Path);

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[path = "device_spaces.rs"]
mod device_spaces;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use device_spaces::{valid_device_space_name, DeviceSpaceAuthorityChanged, DeviceSpacePatch, DeviceSpaceRetirement};

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
        RuntimeLibrary(&session.keys, owner, &session.profile),
    )))
}

/// Keeps the verified keys private while a switch runs between vault checks.
pub struct ModelSwitchVault(SessionKeys);

impl RuntimeLibrary<'_> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(crate) fn backup_account(&self) -> &str {
        self.1
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(crate) fn seal_backup_challenge(&self, challenge: &str) -> Result<(String, String)> {
        self.0.encrypt_content(challenge)
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(crate) fn open_backup_challenge(&self, ciphertext: &str, nonce: &str) -> Result<String> {
        self.0.decrypt_content(ciphertext, nonce)
    }

    /// Decrypt one prepared candidate input under the live Runtime vault.
    /// This returns no key or session context to the isolate.
    pub fn open_model_switch_row(&self, stored: &crate::StoredMemory) -> Result<crate::MemoryEntry> {
        crate::MemoryEngine::open(self.0, stored)
    }

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

#[cfg(all(test, unix))]
#[path = "runtime_backup_tests.rs"]
mod backup_tests;
