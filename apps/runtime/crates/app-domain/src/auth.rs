//! auth — authentication and session (shared by CLI and the web console)
//!
//! Read/write session.json, unlock, register, login. Register = key material + server-issued token;
//! login = verify existing key material and fill in a token. Crypto domain is separate from auth:
//!   unlocking the local library uses session pass (KEK chain); server auth uses PBKDF2(pass, deterministic auth_salt).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use crate::memory::crypto;
use crate::memory::SessionKeys;

pub fn session_file() -> Result<PathBuf> {
    Ok(crate::service::data_dir().join("session.json"))
}

pub fn read_session_json() -> Result<serde_json::Value> {
    let path = session_file()?;
    let text = std::fs::read_to_string(&path).map_err(|_| anyhow!("no local session"))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn write_session_json(data: &serde_json::Value) -> Result<PathBuf> {
    let path = session_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(data)?)?;
    Ok(path)
}

/// Session device name (server sessions.device_name): same hostname source as device_tag,
/// so the two do not disagree (old impl had no syscall fallback and was always "CLI" on mac — fixed 2026-09-20).
fn device_name() -> String {
    crate::service::host_name().unwrap_or_else(|| "CLI".to_owned())
}

/// Manage independent network sessions; never send decryption keys to the server.
pub fn sessions(revoke: Option<&str>) -> Result<serde_json::Value> {
    let mut data = read_session_json()?;
    let addr = data["addr"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("cloud address is not configured"))?;
    let token = data["token"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("no cloud token; log in first"))?;
    let base = addr.trim_end_matches('/');
    let response = if let Some(id) = revoke {
        let id = uuid::Uuid::parse_str(id).map_err(|_| anyhow!("session id must be a full UUID"))?;
        ureq::post(&format!("{base}/api/self/sessions/{id}/revoke"))
            .set("Authorization", &format!("Bearer {token}")).send_json(serde_json::json!({}))?
    } else {
        ureq::get(&format!("{base}/api/self/sessions"))
            .set("Authorization", &format!("Bearer {token}")).call()?
    };
    let reply: serde_json::Value = response.into_json()?;
    if revoke.is_some() && reply["revoked"].as_bool() == Some(true)
        && reply["session_id"] == data["session_id"] {
        if let Some(fields) = data.as_object_mut() {
            fields.remove("token");
            fields.remove("session_id");
        }
        write_session_json(&data)?;
    }
    Ok(reply)
}

/// Log out. Default: clear token/session_id to drop the cloud (keep addr, identity, key material —
/// login without --addr reconnects). --full: delete session.json (forget local identity; reconnect with the five-piece kit or register).
pub fn logout(full: bool) -> Result<()> {
    let path = session_file()?;
    if full {
        let user = read_session_json()
            .ok()
            .and_then(|d| d["user"].as_str().map(|s| s.to_owned()))
            .unwrap_or_default();
        crate::keystore::delete_super(&user);
        crate::keystore::delete_login_pass(&user);
        match std::fs::remove_file(&path) {
            Ok(_) => println!("forgot local identity: {} deleted (reconnect with register or the five-piece kit)", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("no session ({} does not exist)", path.display()),
            Err(e) => return Err(anyhow::anyhow!("clear failed: {e}")),
        }
        return Ok(());
    }
    match read_session_json() {
        Err(_) => println!("no session ({} does not exist)", path.display()),
        Ok(mut data) => {
            if let Some(o) = data.as_object_mut() {
                o.remove("token");
                o.remove("session_id");
            }
            write_session_json(&data)?;
            println!("cloud session logged out: token cleared (addr/identity/key material kept; login without --addr reconnects; local library still works)");
        }
    }
    Ok(())
}

/// Unlock session keys from local session.json.
pub fn load_local_session() -> Result<SessionKeys> {
    load_local_session_with_owner().map(|(keys, _)| keys)
}

/// Unlock keys and retain the profile-owner alias from the same file snapshot.
/// This is local configuration identity, not proof of remote account authentication.
pub fn load_local_session_with_owner() -> Result<(SessionKeys, Option<String>)> {
    let data = read_session_json()?;
    if !crate::credential_io::is_private_control() && !cfg!(any(test, feature = "test-support")) {
        let supplied = RUNTIME_SESSION.lock().map_err(|_| anyhow!("runtime session lock poisoned"))?;
        let supplied = supplied.as_ref().ok_or_else(|| anyhow::Error::new(
            SessionCredentialRequired("runtime_locked: supply private key material from the CLI or Client2")))?;
        anyhow::ensure!(supplied.profile == crate::service::data_dir().canonicalize()?
            && supplied.session == session_digest(&data)?, "runtime_locked: session changed; host must supply its key again");
        return Ok((supplied.keys.clone(), supplied.owner.clone()));
    }
    unlock_local_session_snapshot(&data)
}

struct RuntimeSession {
    profile: PathBuf,
    session: String,
    keys: SessionKeys,
    owner: Option<String>,
    authority_epoch: uuid::Uuid,
}

static RUNTIME_SESSION: std::sync::Mutex<Option<RuntimeSession>> = std::sync::Mutex::new(None);

fn session_digest(data: &serde_json::Value) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(data)?)))
}

/// Accept explicit host input only. Never consult or write a native key store.
pub fn supply_runtime_session(input: respire_protocol::credentials::RuntimeUnlock) -> Result<()> {
    supply_runtime_session_inner(input, None)
}

pub fn supply_runtime_session_v2(input: respire_protocol::credentials::RuntimeUnlockV2) -> Result<()> {
    use respire_protocol::credentials::{RuntimeUnlock, CREDENTIAL_SCHEMA_V, RUNTIME_UNLOCK_SCHEMA_V2};
    anyhow::ensure!(input.schema_version == RUNTIME_UNLOCK_SCHEMA_V2, "unsupported runtime unlock schema");
    let legacy = RuntimeUnlock {
        schema_version: CREDENTIAL_SCHEMA_V,
        profile: input.profile,
        session_sha256: input.session_sha256,
        instance_id: input.instance_id,
        generation: input.generation,
        super_password: input.super_password,
        login_password: input.login_password,
        legacy_secret: input.legacy_secret,
        provider_credentials: Vec::new(),
    };
    supply_runtime_session_inner(legacy, Some(input.provider_update))
}

fn supply_runtime_session_inner(mut input: respire_protocol::credentials::RuntimeUnlock,
    update: Option<respire_protocol::credentials::ProviderCredentialUpdate>) -> Result<()> {
    use sha2::{Digest, Sha256};
    anyhow::ensure!(input.schema_version == respire_protocol::credentials::CREDENTIAL_SCHEMA_V
        && input.profile.is_absolute()
        && !input.profile.components().any(|part| part == std::path::Component::ParentDir)
        && input.profile.canonicalize()? == crate::service::data_dir().canonicalize()?,
        "unlock profile mismatch");
    let bytes = std::fs::read(session_file()?)?;
    anyhow::ensure!(format!("{:x}", Sha256::digest(&bytes)) == input.session_sha256,
        "unlock session revision mismatch");
    let data: serde_json::Value = serde_json::from_slice(&bytes)?;
    let user = data["user"].as_str().unwrap_or("");
    let mut material = data.clone();
    let version = data["vault_version"].as_u64().unwrap_or(1);
    let fields = material.as_object_mut().context("session must contain an object")?;
    for name in ["super", "secret_key", "pass", "secret"] { fields.remove(name); }
    if let Some(value) = &input.legacy_secret {
        if version == 3 { material["secret_key"] = serde_json::json!(value); }
        else if version < 2 { material["secret"] = serde_json::json!(value); }
    }
    let keys = unlock_session_keys(&material, input.login_password.as_deref().unwrap_or(""),
        input.super_password.as_deref(), user)?;
    anyhow::ensure!(std::fs::read(session_file()?)? == bytes, "session changed during unlock");
    let session = RuntimeSession {
        profile: input.profile.canonicalize()?, session: session_digest(&data)?, keys,
        owner: (!user.trim().is_empty()).then(|| user.to_owned()),
        authority_epoch: uuid::Uuid::new_v4(),
    };
    let mut current = RUNTIME_SESSION.lock().map_err(|_| anyhow!("runtime session lock poisoned"))?;
    match update {
        Some(update) => crate::credential_io::apply_provider_update(update)?,
        None => crate::credential_io::supply_credentials(std::mem::take(&mut input.provider_credentials))?,
    }
    *current = Some(session);
    Ok(())
}

/// Run a serving-only operation against the already verified, unlocked local
/// owner, without loading a keyring or exposing unlocked key material. The
/// caller must hold the Runtime identity fence and must not re-enter session I/O.
pub fn with_runtime_session_owner<T>(
    canonical_profile: &std::path::Path,
    action: impl FnOnce(&str, uuid::Uuid) -> T,
) -> Result<Option<T>> {
    with_runtime_library(canonical_profile, |owner, epoch, _| action(owner, epoch))
}

fn current_runtime_session(session: &RuntimeSession, profile: &std::path::Path) -> Result<bool> {
    Ok(session.profile == profile
        && crate::service::data_dir().canonicalize()? == profile
        && session.session == session_digest(&read_session_json()?)?)
}

#[path = "auth/recall.rs"]
mod recall;
#[path = "auth/deepen.rs"]
mod deepen;
pub use deepen::PlannedOutline;
#[path = "auth/runtime_library.rs"]
mod runtime_library;
pub use runtime_library::{with_runtime_library, ModelSwitchVault, RuntimeLibrary};
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use runtime_library::{valid_device_space_name, DeviceSpaceAuthorityChanged};

pub struct RuntimeMemoryPage {
    pub entries: Vec<crate::MemoryEntry>,
    pub next: Option<(String, i64)>,
}

impl RuntimeLibrary<'_> {
    pub fn sync_access<'a>(&'a self, store: &'a crate::transport::local::LocalStore)
        -> crate::sync::SyncLocal<'a>
    {
        crate::sync::SyncLocal::new(store, self.0)
    }

    pub fn memory_exact(&self, store: &crate::transport::local::LocalStore, id: &str)
        -> Result<Option<crate::MemoryEntry>>
    {
        store.active_memory_exact(id)?.map(|row| crate::MemoryEngine::open(self.0, &row)).transpose()
    }

    pub fn create_memory(&self, store: &crate::transport::local::LocalStore, entry: &crate::MemoryEntry)
        -> Result<()>
    {
        self.commit_memory(store, entry, &entry.user, None)
    }

    pub fn update_memory_exact(
        &self,
        store: &crate::transport::local::LocalStore,
        id: &str,
        edit: impl FnOnce(&mut crate::MemoryEntry),
    ) -> Result<Option<crate::MemoryEntry>> {
        let Some(current) = store.active_memory_exact(id)? else { return Ok(None); };
        let mut entry = crate::MemoryEngine::open(self.0, &current)?;
        edit(&mut entry);
        entry.updated_at = store.edit_stamp(id)?;
        self.commit_memory(store, &entry, &current.user, Some(&current.ciphertext))?;
        Ok(Some(entry))
    }

    fn commit_memory(&self, store: &crate::transport::local::LocalStore,
        entry: &crate::MemoryEntry, storage_user: &str, previous: Option<&str>) -> Result<()>
    {
        crate::metadata_gate::ensure_write(store, entry)?;
        crate::metadata_gate::ensure_sources(self.0, store, entry)?;
        let sealed = crate::MemoryEngine::seal_for_indexing(self.0, entry, storage_user)?;
        anyhow::ensure!(store.put_checked(&sealed, previous)?, "memory write was not committed");
        Ok(())
    }

    pub fn memory_page(&self, store: &crate::transport::local::LocalStore,
        limit: usize, after: Option<(&str, i64)>) -> Result<RuntimeMemoryPage>
    {
        anyhow::ensure!((1..=200).contains(&limit), "invalid memory page size");
        let rows = store.active_memory_page(limit + 1, after)?;
        let next = (rows.len() > limit)
            .then(|| (rows[limit - 1].1.updated_at.clone(), rows[limit - 1].0));
        let entries = rows.into_iter().take(limit)
            .map(|(_, row)| crate::MemoryEngine::open(self.0, &row))
            .collect::<Result<Vec<_>>>()?;
        Ok(RuntimeMemoryPage { entries, next })
    }

    pub fn diary_entries(&self, store: &crate::transport::local::LocalStore,
        date: Option<&str>, day_of: impl Fn(&str) -> String) -> Result<Vec<crate::MemoryEntry>>
    {
        store.active_diary_entries(date, day_of)?.iter()
            .map(|row| crate::MemoryEngine::open(self.0, row)).collect()
    }

    pub fn open_memory(&self, row: &crate::StoredMemory) -> Result<crate::MemoryEntry> {
        crate::MemoryEngine::open(self.0, row)
    }

    pub fn append_diary(&self, store: &crate::transport::local::LocalStore, entry: &crate::MemoryEntry,
        previous: &crate::StoredMemory, duplicates: &[&crate::StoredMemory]) -> Result<()>
    {
        crate::metadata_gate::ensure_write(store, entry)?;
        crate::metadata_gate::ensure_sources(self.0, store, entry)?;
        let sealed = crate::MemoryEngine::seal_for_indexing(self.0, entry, &previous.user)?;
        anyhow::ensure!(store.put_diary_checked(&sealed, &previous.ciphertext, duplicates)?,
            "diary write was not committed");
        Ok(())
    }
}

pub fn clear_runtime_session() -> Result<()> {
    *RUNTIME_SESSION.lock().map_err(|_| anyhow!("runtime session lock poisoned"))? = None;
    crate::credential_io::supply_credentials(Vec::new())?;
    Ok(())
}

/// Keep key material and owner inseparable even if the profile changes after reading.
fn unlock_local_session_snapshot(data: &serde_json::Value) -> Result<(SessionKeys, Option<String>)> {
    let user = data["user"].as_str().unwrap_or("").to_owned();
    let explicit_pass = if data["vault_version"] == 1 {
        crate::env::var("RSRS_PASS").ok().filter(|value| !value.is_empty())
    } else { None };
    let supplied_pass = explicit_pass.or_else(|| data["pass"].as_str().map(ToOwned::to_owned));
    let pass = if data["vault_version"].as_i64().unwrap_or(1) == 1 && supplied_pass.is_none() {
        match data["keyring_backend"].as_str() {
            Some(backend) => crate::keystore::read_imported_v1_password(
                data["keyring_account"].as_str().unwrap_or(&user), backend)?,
            None => crate::keystore::load_login_pass(&user).ok_or_else(|| anyhow::Error::new(
                SessionCredentialRequired("original login password is required")))?,
        }
    } else { supplied_pass.or_else(|| crate::keystore::load_login_pass(&user)).unwrap_or_default() };
    let keys = unlock_session_keys(data, &pass, None, &user)?;
    let owner = if user.trim().is_empty() { None } else { Some(user) };
    Ok((keys, owner))
}

/// Try to unlock the local session; no session → Ok(None) (unregistered; web can guide register).
pub fn try_session() -> Result<Option<SessionKeys>> {
    match load_local_session() {
        Ok(keys) => Ok(Some(keys)),
        Err(_) => Ok(None),
    }
}

pub fn wrap_with_v4(super_pass: &str, urk: &[u8; 32]) -> Result<(String, String, String)> {
    let kdf_salt = crypto::random_hex(16);
    let kek = crypto::derive_kek_v4(super_pass, &kdf_salt)?;
    let (urk_nonce, wrapped_urk) = crypto::wrap_key(urk, &kek)?;
    Ok((kdf_salt, wrapped_urk, urk_nonce))
}

pub fn wrap_with_rsrs(super_pass: &str, urk: &[u8; 32]) -> Result<(String, String, String)> {
    let salt = crypto::random_hex(16);
    let kek = crypto::derive_rsrs_kek(super_pass, &salt)?;
    let (nonce, wrapped) = crypto::wrap_key(urk, &kek)?;
    Ok((salt, format!("{}{wrapped}", crypto::RSRS_PREFIX), nonce))
}

/// v4 key material: generate a super password (A3- recovery code) and a fresh URK.
pub fn new_vault_v4() -> Result<(String, [u8; 32], String, String, String)> {
    let super_pass = crypto::generate_secret_key();
    let urk = crypto::generate_key();
    let (kdf_salt, wrapped_urk, urk_nonce) = wrap_with_rsrs(&super_pass, &urk)?;
    Ok((super_pass, urk, kdf_salt, wrapped_urk, urk_nonce))
}

/// Write v4 material into session: super password goes only to the OS keyring (headless warns and uses RSRS_SUPER); never plaintext on disk.
fn apply_vault_v4(
    data: &mut serde_json::Value,
    user: &str,
    super_pass: &str,
    kdf_salt: String,
    wrapped_urk: String,
    urk_nonce: String,
) -> Result<()> {
    data["kdf_salt"] = serde_json::Value::String(kdf_salt);
    data["wrapped_urk"] = serde_json::Value::String(wrapped_urk);
    data["urk_nonce"] = serde_json::Value::String(urk_nonce);
    data["vault_version"] = serde_json::json!(4);
    // Super password and login password never go to disk
    data.as_object_mut().map(|o| {
        o.remove("secret_key");
        o.remove("super");
        o.remove("pass");
    });
    match crate::keystore::save_super(user, super_pass) {
        Ok(()) => {}
        Err(e) => eprintln!("warning: {e}; this session can still pass --super <super-password>"),
    }
    Ok(())
}

fn put_vault(addr: &str, token: &str, data: &serde_json::Value) -> Result<()> {
    let base = addr.trim().trim_end_matches('/');
    ureq::post(&format!("{base}/api/self/vault"))
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({
            "kdf_salt": data["kdf_salt"],
            "wrapped_urk": data["wrapped_urk"],
            "urk_nonce": data["urk_nonce"],
            "version": data["vault_version"].as_i64().unwrap_or(3),
        }))
        .map_err(|e| anyhow!("failed to upload key wrap: {e}"))?;
    Ok(())
}

fn fetch_vault(addr: &str, token: &str) -> Result<Option<serde_json::Value>> {
    let base = addr.trim().trim_end_matches('/');
    match ureq::get(&format!("{base}/api/self/vault"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(resp) => resp
            .into_json()
            .map(Some)
            .map_err(|e| anyhow!("failed to parse vault response: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(anyhow!("failed to fetch key wrap: {e}")),
    }
}

/// Live memory count for this user on the server (overwrite protection / prompts).
fn fetch_count(addr: &str, token: &str) -> Result<i64> {
    let base = addr.trim().trim_end_matches('/');
    match ureq::get(&format!("{base}/count"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(resp) => Ok(resp.into_json::<serde_json::Value>()?["count"].as_i64().unwrap_or(0)),
        Err(ureq::Error::Status(404, _)) => Ok(0),
        Err(e) => Err(anyhow!("failed to fetch memory count: {e}")),
    }
}

/// Generate key material (local offline): wrap URK with a random key, store in the OS keyring, never plaintext in session.
///
/// Offline mode has no cloud backup: the keyring is the only copy, so the write must round-trip —
/// the old `save_super(...).ok()` swallowed errors and left the user with a library that could never open.
pub fn keygen() -> Result<(PathBuf, String)> {
    let (super_pass, _urk, kdf_salt, wrapped_urk, urk_nonce) = new_vault_v4()?;
    crate::keystore::save_super("local", &super_pass).map_err(|e| {
        anyhow!(
            "local keyring write failed ({e}) \u{2014} offline mode stores the key only here; \
             a failed write builds a library that cannot be opened. \
             Install a keyring (gnome-keyring / KWallet) and retry, \
             or set RSRS_SUPER on later commands as a bypass"
        )
    })?;
    if crate::keystore::load_super("local").is_none() {
        anyhow::bail!("keyring write did not round-trip \u{2014} offline mode cannot continue; check the keyring service and retry");
    }
    let path = write_session_json(&serde_json::json!({
        "kdf_salt": kdf_salt,
        "wrapped_urk": wrapped_urk,
        "urk_nonce": urk_nonce,
        "vault_version": 4,
    }))?;
    Ok((path, super_pass))
}

pub fn unlock_session_keys(
    data: &serde_json::Value,
    login_pass: &str,
    super_arg: Option<&str>,
    user: &str,
) -> Result<crate::memory::SessionKeys> {
    let wrapped = data["wrapped_urk"].as_str().ok_or_else(|| anyhow!("session missing wrapped_urk"))?;
    let nonce = data["urk_nonce"].as_str().ok_or_else(|| anyhow!("session missing urk_nonce"))?;
    let salt = data["kdf_salt"].as_str().ok_or_else(|| anyhow!("session missing kdf_salt"))?;
    let keys = match data["vault_version"].as_i64() {
        // v4: super password as the single factor — source: --super > OS keyring/env > leftover session field
        Some(v) if v >= 4 => {
            let super_pass = super_arg
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .or_else(|| crate::keystore::load_super(user))
                .or_else(|| data["secret_key"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_owned()))
                .ok_or_else(|| {
                    anyhow::Error::new(SessionCredentialRequired(
                        "super password required: --super <super-password> (A3-\u{2026}, system-generated recovery code). \
                         none in the local keyring \u{2014} on a new machine export from the old device with `rsrs keys-export` and retry"
                    ))
                })?;
            crate::memory::SessionKeys::unlock_v4(&super_pass, salt, wrapped, nonce)
        }
        // v3: original super password + Secret Key, retained during normal login.
        Some(3) => {
            let stored_super = if super_arg.is_none() && data["super"].as_str().is_none() {
                crate::keystore::load_super(user)
            } else { None };
            let super_pass = super_arg
                .or_else(|| data["super"].as_str())
                .or(stored_super.as_deref())
                .ok_or_else(|| anyhow::Error::new(SessionCredentialRequired("super password required")))?;
            let secret_key = legacy_session_factor(data, user, "secret_key")?;
            crate::memory::SessionKeys::unlock_vault(super_pass, &secret_key, salt, wrapped, nonce)
        }
        // v2: super password (passphrase) only
        Some(2) => {
            let stored_super = if super_arg.is_none() && data["super"].as_str().is_none() {
                crate::keystore::load_super(user)
            } else { None };
            let super_pass = super_arg
                .or_else(|| data["super"].as_str())
                .or(stored_super.as_deref())
                .ok_or_else(|| anyhow::Error::new(SessionCredentialRequired("super password required")))?;
            crate::memory::SessionKeys::unlock_super(super_pass, salt, wrapped, nonce)
        }
        // v1: original login password + Account Secret, retained during normal login.
        _ => crate::memory::SessionKeys::unlock(
            login_pass,
            &legacy_session_factor(data, user, "secret")?,
            salt,
            wrapped,
            nonce,
        ),
    }?;
    if data["crypto_namespace"].as_str() == Some(crypto::RSRS_PREFIX) {
        crate::memory::SessionKeys::from_urk(keys.urk)
    } else {
        Ok(keys)
    }
}

fn legacy_session_factor(data: &serde_json::Value, user: &str, field: &str) -> Result<String> {
    if let Some(value) = data[field].as_str().filter(|value| !value.is_empty()) {
        return Ok(value.to_owned());
    }
    let backend = data["legacy_secret_backend"].as_str().context("original legacy factor is missing")?;
    anyhow::ensure!(backend != "kernel", "legacy factor requires a persistent host backend");
    let alias = data["keyring_account"].as_str().unwrap_or(user);
    crate::keystore::read_imported(alias, "legacy-secret", backend)
}

/// Missing material is distinguishable from malformed sessions or failed decryption.
#[derive(Debug)]
pub struct SessionCredentialRequired(pub &'static str);

impl std::fmt::Display for SessionCredentialRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for SessionCredentialRequired {}

/// Current profile already belongs to a different user: switch to that user's
/// profile and return its session. An empty profile stays put, so the first
/// register in an isolated data dir still writes there.
fn session_for_other_user(user: &str) -> Result<serde_json::Value> {
    let data = read_session_json().unwrap_or_else(|_| serde_json::json!({}));
    let cur_user = data["user"].as_str().unwrap_or("").to_owned();
    if cur_user.is_empty() || cur_user == user {
        return Ok(data);
    }
    crate::service::require_profile_change_host()?;
    let main_user = crate::service::session_user_of_dir(&crate::service::main_data_dir());
    if main_user == user {
        crate::service::set_data_dir("")?;
    } else {
        let dir = crate::service::account_dir(user)?;
        std::fs::create_dir_all(&dir)?;
        crate::service::set_data_dir(&dir.to_string_lossy())?;
    }
    eprintln!(
        "switching profile for `{user}` (account `{cur_user}` stays in its own data dir)"
    );
    Ok(read_session_json().unwrap_or_else(|_| serde_json::json!({})))
}

/// Super password for a vault that already exists (keygen, or the same profile).
/// `--super` wins, then `super:<user>`, then the keygen slot `super:local`, then a leftover session field.
fn existing_super(data: &serde_json::Value, passed: &str, user: &str) -> Result<String> {
    if !passed.is_empty() {
        return Ok(passed.to_owned());
    }
    if let Some(found) = crate::keystore::load_super(user) {
        return Ok(found);
    }
    if user != "local" {
        if let Some(found) = crate::keystore::load_super("local") {
            return Ok(found);
        }
    }
    data["secret_key"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "super password required: --super <super-password> (A3-\u{2026}, system-generated recovery code). \
                 none in the local keyring \u{2014} on a new machine export from the old device with `rsrs keys-export` and retry"
            )
        })
}

include!("auth/accounts.rs");
include!("auth/authorization.rs");
#[cfg(test)]
mod tests;
