//! Vault-sealed compound metadata and REST-only active selection.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const STATE_CONTEXT: &[u8] = b"rsrs:device-local-space-state:1";
const STATE_LIMIT: usize = 100_000;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpacePatch {
    pub name: Option<String>,
    pub active: Option<bool>,
}

pub enum DeviceSpaceRetirement {
    Missing,
    Active,
    Retired,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopedState {
    version: u8,
    installation: String,
    owner: String,
    profile: PathBuf,
    active: Option<String>,
    names: BTreeMap<String, String>,
}

impl ScopedState {
    pub(super) fn project(&self, item: &mut DeviceSpace) {
        item.active = self.active.as_deref() == Some(item.id.as_str());
        if let Some(name) = self.names.get(&item.id) {
            item.name.clone_from(name);
        }
    }
}

impl RuntimeLibrary<'_> {
    pub(super) fn read_space_state(
        &self, area: &LocalSpaceNamespace, installation: [u8; 16],
    ) -> Result<((String, [u8; 32]), ScopedState)> {
        let key = crypto::derive_subkey(&self.0.urk, STATE_CONTEXT)?;
        let installation = Uuid::from_bytes(installation).to_string();
        let mut scope = Sha256::new();
        scope.update(key);
        scope.update(serde_json::to_vec(&(&installation, self.1, self.2))?);
        let slot = hex::encode(&scope.finalize()[..16]);
        let state = match area.state_record(&slot)? {
            None => ScopedState {
                version: 1, installation: installation.clone(), owner: self.1.to_owned(), profile: self.2.to_owned(),
                active: None, names: BTreeMap::new(),
            },
            Some(record) => {
                let [nonce, sealed] = serde_json::from_slice::<[String; 2]>(&record)?;
                ensure!(nonce.len() == 24 && sealed.len() <= 2 * (STATE_LIMIT + 16),
                    "invalid private local space state shape");
                let bytes = crypto::decrypt_bytes(&key, &sealed, &nonce)?;
                ensure!(bytes.len() <= STATE_LIMIT, "local space state exceeded limits");
                serde_json::from_slice::<ScopedState>(&bytes)?
            }
        };
        ensure!(
            state.version == 1 && state.installation == installation
                && state.owner == self.1 && state.profile == self.2
                && state.names.len() <= CATALOG_LIMIT
                && state.names.iter().all(|(id, name)| id_from_path(id).is_some() && valid_device_space_name(name))
                && state.active.as_deref().is_none_or(|id| id_from_path(id).is_some()),
            "private local space state was not authorized for the serving Vault",
        );
        Ok(((slot, key), state))
    }

    /// Missing state leaves legacy routing exactly unchanged; corrupted or
    /// stale state never authorizes falling back from a selected child.
    pub fn active_device_space(&self, ownership: &LibraryLock) -> Result<Option<String>> {
        let Some(area) = LocalSpaceNamespace::open_configured(
            &self.device_space_installation(ownership)?,
        )? else { return Ok(None) };
        let Some(installation) = area.existing_installation()? else { return Ok(None) };
        let Some(raw) = self.read_space_state(&area, installation)?.1.active else {
            return Ok(None);
        };
        let id = id_from_path(&raw).context("invalid active device local space ID")?;
        let claim_key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        ensure!(self.read_device_space(&area, installation, &claim_key, id)?.is_some(),
            "active device local space receipt is missing");
        Ok(Some(raw))
    }

    pub fn patch_device_space(
        &self, ownership: &LibraryLock, raw: &str, patch: DeviceSpacePatch,
        admit: impl FnOnce() -> bool,
    ) -> Result<Option<DeviceSpace>> {
        let Some(id) = id_from_path(raw) else { return Ok(None) };
        ensure!(patch.name.as_deref().is_none_or(valid_device_space_name), "invalid space name");
        let Some(area) = LocalSpaceNamespace::open_configured(
            &self.device_space_installation(ownership)?,
        )? else { return Ok(None) };
        let Some(installation) = area.existing_installation()? else { return Ok(None) };
        let claim_key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        let Some(mut space) = self.read_device_space(&area, installation, &claim_key, id)? else {
            return Ok(None);
        };
        let ((slot, key), mut state) = self.read_space_state(&area, installation)?;
        if let Some(name) = patch.name {
            // Retirement has an independent atomic native boundary; discard
            // old display overrides without resurrecting absent directories.
            let committed = area.candidates()?;
            state.names.retain(|raw, _| raw.strip_prefix("local-")
                .is_some_and(|id| committed.iter().any(|candidate| candidate == id)));
            state.names.insert(space.id.clone(), name);
        }
        match patch.active {
            Some(true) => state.active = Some(space.id.clone()),
            Some(false) if state.active.as_deref() == Some(space.id.as_str()) => state.active = None,
            _ => {}
        }
        state.project(&mut space);
        Self::write_space_state(&area, &slot, &key, &state, admit)?;
        Ok(Some(space))
    }

    /// Only fresh owner-verified local children can be retired. Active
    /// children must first restore the previously selected library.
    pub fn delete_device_space(
        &self, ownership: &LibraryLock, raw: &str, admit: impl FnOnce() -> bool,
    ) -> Result<DeviceSpaceRetirement> {
        let Some(id) = id_from_path(raw) else { return Ok(DeviceSpaceRetirement::Missing) };
        let Some(area) = LocalSpaceNamespace::open_configured(
            &self.device_space_installation(ownership)?,
        )? else { return Ok(DeviceSpaceRetirement::Missing) };
        let Some(installation) = area.existing_installation()? else {
            return Ok(DeviceSpaceRetirement::Missing);
        };
        let claim_key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        let active = self.read_space_state(&area, installation)?.1.active;
        if active.as_deref() == Some(raw) {
            ensure!(self.read_device_space(&area, installation, &claim_key, id)?.is_some(),
                "active local space receipt is missing");
            return Ok(DeviceSpaceRetirement::Active);
        }
        let retired = area.retire(
            &id.simple().to_string(),
            |receipt| self.verify_device_space(receipt, installation, &claim_key, id).is_some(),
            || {
                if admit() {
                    Ok(())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied, DeviceSpaceAuthorityChanged,
                    ))
                }
            },
        )?;
        Ok(if retired { DeviceSpaceRetirement::Retired } else { DeviceSpaceRetirement::Missing })
    }

    /// Restore the already locked original serving profile, never a
    /// caller-named profile. The HTTP layer must validate its existing role.
    pub fn restore_original_space(
        &self, ownership: &LibraryLock, admit: impl FnOnce() -> bool,
    ) -> Result<()> {
        let area = LocalSpaceNamespace::open_configured(
            &self.device_space_installation(ownership)?,
        )?.context("no device local catalog to restore from")?;
        let installation = area.existing_installation()?.context("missing space installation")?;
        let ((slot, key), mut state) = self.read_space_state(&area, installation)?;
        state.active = None;
        Self::write_space_state(&area, &slot, &key, &state, admit)
    }

    fn write_space_state(
        area: &LocalSpaceNamespace, slot: &str, key: &[u8; 32], state: &ScopedState,
        admit: impl FnOnce() -> bool,
    ) -> Result<()> {
        ensure!(state.names.len() <= CATALOG_LIMIT, "too many local space names");
        let bytes = serde_json::to_vec(state)?;
        ensure!(bytes.len() <= STATE_LIMIT, "local space state exceeded limits");
        let (nonce, sealed) = crypto::encrypt_bytes(key, &bytes)?;
        area.replace_state_record(slot, &serde_json::to_vec(&[nonce, sealed])?, || {
            if admit() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied, DeviceSpaceAuthorityChanged,
                ))
            }
        })?;
        Ok(())
    }
}
