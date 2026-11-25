use super::RuntimeLibrary;
use crate::{lock::LibraryLock, memory::crypto, service, transport::local::LocalStore};
use anyhow::{ensure, Context, Result};
use respire_spawn::owner_credential::LocalSpaceNamespace;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[path = "device_space_state.rs"]
mod state;
pub use state::{DeviceSpacePatch, DeviceSpaceRetirement};

const DOMAIN: &[u8] = b"rsrs:device-local-space-owner:1";
const LIMIT: usize = 4096;
const CATALOG_LIMIT: usize = 256;
const LOCK_WAIT: Duration = Duration::from_millis(150);

#[derive(Serialize)]
pub struct DeviceSpace {
    id: String,
    name: String,
    role: &'static str,
    active: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u8,
    installation: String,
    profile: PathBuf,
    account: String,
    space: String,
    name: String,
}

#[derive(Debug)]
pub struct DeviceSpaceAuthorityChanged;

impl std::fmt::Display for DeviceSpaceAuthorityChanged {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("local space authority changed")
    }
}

impl std::error::Error for DeviceSpaceAuthorityChanged {}

pub fn valid_device_space_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 200 && !name.chars().any(char::is_control)
}

fn id_from_path(raw: &str) -> Option<Uuid> {
    let leaf = raw.strip_prefix("local-")?;
    let id = Uuid::parse_str(leaf).ok()?;
    (id.get_version_num() == 4 && id.simple().to_string() == leaf).then_some(id)
}

fn item(id: Uuid, name: String) -> DeviceSpace {
    DeviceSpace {
        id: format!("local-{}", id.simple()),
        name,
        role: "owner",
        active: false,
    }
}

impl RuntimeLibrary<'_> {
    fn device_space_installation(&self, ownership: &LibraryLock) -> Result<PathBuf> {
        ensure!(ownership.protects(self.2)?, "selected library ownership changed");
        Ok(service::main_data_dir().canonicalize()?)
    }

    pub fn create_device_space(
        &self,
        ownership: &LibraryLock,
        name: String,
        confirm_current: impl Fn() -> bool,
        admit: impl FnOnce() -> bool,
    ) -> Result<DeviceSpace> {
        ensure!(valid_device_space_name(&name), "invalid local space name");
        if !confirm_current() {
            return Err(DeviceSpaceAuthorityChanged.into());
        }
        let installation = self.device_space_installation(ownership)?;
        let area = LocalSpaceNamespace::open(&installation)?;
        ensure!(
            area.candidates()?.len() < CATALOG_LIMIT,
            "device local space capacity reached"
        );
        let candidate = Uuid::new_v4();
        let owned_installation = area.installation(
            &candidate.simple().to_string(), *candidate.as_bytes(),
        )?;
        let id = Uuid::new_v4();
        let child = area.fresh(&id.simple().to_string())?;
        let path = child.path().canonicalize()?;
        ensure!(path == child.path(), "local space path changed");

        respire_core_sdk::with_index_root(Some(&path), || {
            let child_lock = LibraryLock::acquire(&path, LOCK_WAIT)?;
            ensure!(child_lock.protects(&path)?, "local space library ownership changed");
            let store = LocalStore::open(&path.join("onememory.db"))?;
            ensure!(
                self.memory_page(&store, 1, None)?.entries.is_empty(),
                "device local space cannot adopt existing data",
            );
            drop(store);
            let claim = Claim {
                version: 1,
                installation: Uuid::from_bytes(owned_installation).to_string(),
                profile: self.2.to_owned(),
                account: self.1.to_owned(),
                space: id.to_string(),
                name: name.clone(),
            };
            let bytes = serde_json::to_vec(&claim)?;
            ensure!(bytes.len() <= LIMIT, "device local space metadata is oversized");
            let key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
            let (nonce, sealed) = crypto::encrypt_bytes(&key, &bytes)?;
            // Windows requires staged SQLite children to be closed before the
            // native directory move. The serving profile and Vault stay fenced.
            drop(child_lock);
            child.publish(&serde_json::to_vec(&[nonce, sealed])?, || {
                if confirm_current() && admit() {
                    Ok(())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied, DeviceSpaceAuthorityChanged,
                    ))
                }
            })?;
            Ok(item(id, name))
        })
    }

    pub fn list_device_spaces(&self, ownership: &LibraryLock) -> Result<Vec<DeviceSpace>> {
        let Some(area) = LocalSpaceNamespace::open_existing(
            &self.device_space_installation(ownership)?,
        )? else {
            return Ok(Vec::new());
        };
        let Some(installation) = area.existing_installation()? else {
            return Ok(Vec::new());
        };
        let key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        let state = self.read_space_state(&area, installation)?.1;
        let mut result = Vec::new();
        for candidate in area.candidates()? {
            let id = Uuid::parse_str(&candidate).context("invalid local space candidate")?;
            if id.get_version_num() != 4 {
                continue;
            }
            if let Some(mut found) = self.read_device_space(&area, installation, &key, id)? {
                state.project(&mut found);
                result.push(found);
            }
        }
        Ok(result)
    }

    pub fn get_device_space(
        &self, ownership: &LibraryLock, raw: &str,
    ) -> Result<Option<DeviceSpace>> {
        let Some(id) = id_from_path(raw) else { return Ok(None) };
        let Some(area) = LocalSpaceNamespace::open_existing(
            &self.device_space_installation(ownership)?,
        )? else {
            return Ok(None);
        };
        let Some(installation) = area.existing_installation()? else {
            return Ok(None);
        };
        let key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        let Some(mut found) = self.read_device_space(&area, installation, &key, id)? else {
            return Ok(None);
        };
        self.read_space_state(&area, installation)?.1.project(&mut found);
        Ok(Some(found))
    }

    /// Resolve and hold a committed device-local library, without changing
    /// the active profile or retaining Core index state on this thread.
    pub fn with_device_space_store<T>(
        &self, ownership: &LibraryLock, raw: &str, current: impl Fn() -> bool,
        run: impl FnOnce(&LocalStore) -> T,
    ) -> Result<Option<T>> {
        let Some(id) = id_from_path(raw) else { return Ok(None) };
        let Some(area) = LocalSpaceNamespace::open_existing(
            &self.device_space_installation(ownership)?,
        )? else { return Ok(None) };
        let Some(installation) = area.existing_installation()? else { return Ok(None) };
        let key = crypto::derive_subkey(&self.0.urk, DOMAIN)?;
        let candidate = id.simple().to_string();
        area.with_library_receipt(
            &candidate,
            |receipt| self.verify_device_space(receipt, installation, &key, id).is_some(),
            |path| {
                if !current() {
                    return Err(DeviceSpaceAuthorityChanged.into());
                }
                ensure!(path.canonicalize()? == path, "device local library path changed");
                let lock = LibraryLock::try_acquire_device_space(path)?;
                ensure!(lock.protects(path)?, "device local library ownership changed");
                respire_core_sdk::with_index_root(Some(path), || {
                    let store = LocalStore::open_existing_device_space(&path.join("onememory.db"))?;
                    if !current() {
                        return Err(DeviceSpaceAuthorityChanged.into());
                    }
                    Ok(run(&store))
                })
            },
        )?.transpose()
    }

    fn read_device_space(
        &self,
        area: &LocalSpaceNamespace,
        installation: [u8; 16],
        key: &[u8; 32],
        id: Uuid,
    ) -> Result<Option<DeviceSpace>> {
        let Some(receipt) = area.receipt(&id.simple().to_string())? else {
            return Ok(None);
        };
        Ok(self.verify_device_space(&receipt, installation, key, id))
    }

    fn verify_device_space(
        &self, receipt: &[u8], installation: [u8; 16], key: &[u8; 32], id: Uuid,
    ) -> Option<DeviceSpace> {
        let [nonce, sealed] = serde_json::from_slice::<[String; 2]>(receipt).ok()?;
        if nonce.len() != 24 || sealed.len() > 2 * (LIMIT + 16) {
            return None;
        }
        let Ok(bytes) = crypto::decrypt_bytes(key, &sealed, &nonce) else {
            return None;
        };
        if bytes.len() > LIMIT {
            return None;
        }
        let Ok(claim) = serde_json::from_slice::<Claim>(&bytes) else {
            return None;
        };
        (claim.version == 1
            && claim.installation == Uuid::from_bytes(installation).to_string()
            && claim.profile == self.2
            && claim.account == self.1
            && claim.space == id.to_string()
            && valid_device_space_name(&claim.name))
            .then(|| item(id, claim.name))
    }
}
