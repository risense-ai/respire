use super::{current_runtime_session, SessionKeys, RUNTIME_SESSION};
use crate::{
    core_sdk::profiles::Selection, memory::search::Embedder, transport::local::LocalStore,
};
use anyhow::Result;

/// Only typed library operations can use the verified vault.
pub struct RuntimeLibrary<'a>(pub(super) &'a SessionKeys, &'a str, &'a std::path::Path);

#[path = "backup_vault.rs"]
mod backup_vault;

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

pub fn open_runtime_model_switch_row(
    canonical_profile: &std::path::Path,
    stored: &crate::StoredMemory,
) -> Result<Option<crate::MemoryEntry>> {
    let unlocked = RUNTIME_SESSION
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime session lock poisoned"))?;
    let Some(session) = unlocked.as_ref() else {
        return Ok(None);
    };
    if !current_runtime_session(session, canonical_profile)? {
        return Ok(None);
    }
    Ok(Some(crate::MemoryEngine::open(&session.keys, stored)?))
}

/// Keeps the verified keys private while a switch runs between vault checks.
pub struct ModelSwitchVault(SessionKeys);

impl RuntimeLibrary<'_> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{clear_runtime_session, new_vault_v4, supply_runtime_session, write_session_json};
    use respire_protocol::credentials::{RuntimeUnlock, CREDENTIAL_SCHEMA_V};
    use serde_json::json;
    use sha2::{Digest, Sha256};

    #[test]
    fn offline_session_can_open_model_switch_rows_until_cleared() -> Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let (super_password, urk, kdf_salt, wrapped_urk, urk_nonce) = new_vault_v4()?;
        let path = write_session_json(&json!({
            "vault_version": 4,
            "kdf_salt": kdf_salt,
            "wrapped_urk": wrapped_urk,
            "urk_nonce": urk_nonce,
        }))?;
        let profile = crate::service::data_dir().canonicalize()?;
        let entry: crate::MemoryEntry = serde_json::from_value(json!({
            "id": "offline-row", "kind": "Context", "tags": [], "title": "Offline title",
            "content": "Offline content", "user": "local", "computer": "fixture", "project": "",
            "created_at": "2026-10-27T09:00:00Z", "updated_at": "2026-10-27T09:00:00Z",
            "emotion": 0, "parent_id": "", "importance": "trivial",
        }))?;
        let keys = SessionKeys::from_urk(urk)?;
        let stored = crate::MemoryEngine::seal_for_indexing(&keys, &entry, "local")?;
        supply_runtime_session(RuntimeUnlock {
            schema_version: CREDENTIAL_SCHEMA_V,
            profile: profile.clone(),
            session_sha256: format!("{:x}", Sha256::digest(std::fs::read(path)?)),
            instance_id: uuid::Uuid::new_v4().to_string(),
            generation: 1,
            super_password: Some(super_password),
            login_password: None,
            legacy_secret: None,
            provider_credentials: Vec::new(),
        })?;

        let owner_library = with_runtime_library(&profile, |_, _, _| ());
        let opened = open_runtime_model_switch_row(&profile, &stored);
        clear_runtime_session()?;

        assert!(owner_library?.is_none());
        let opened = opened?.ok_or_else(|| anyhow::anyhow!("offline model switch row unavailable"))?;
        assert_eq!(opened.title, entry.title);
        assert_eq!(opened.content, entry.content);
        assert!(open_runtime_model_switch_row(&profile, &stored)?.is_none());
        Ok(())
    }
}
