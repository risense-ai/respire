//! Local confirmation only. This record cannot authorize native account mutation.
use super::*;
use std::{io::Read, time::{SystemTime, UNIX_EPOCH}};

const FILE: &str = ".host-login-pending.json";
const LIFETIME_SECONDS: u64 = 300;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMode { BrowserOauth, Recovery, OfflineKeygen }

#[derive(Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginIntent {
    mode: LoginMode,
    #[serde(default)]
    passphrase_present: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    operation: String,
    binding: String,
    issued: u64,
    intent: LoginIntent,
}

pub enum LoginAdmission { Pending, Conflict, RequiresEmptyProfile }

fn now() -> Result<u64> { Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()) }

fn binding(profile: &Path, session: &[u8], journal: &Journal) -> Result<String> {
    // A subsequent native admission supersedes the local request, even after
    // its active recovery clears without changing the session.
    Ok(digest(&serde_json::to_vec(&(profile, digest(session), journal.operations.len(),
        journal.operations.last().map(|entry| &entry.operation_id)))?))
}

fn current(profile: &Path, expected: &str, now: u64) -> Result<Option<Pending>> {
    let path = profile.join(FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(meta.is_file() && meta.len() <= 2048, "invalid pending login record");
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(2049).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2048, "invalid pending login size");
    let record: Pending = serde_json::from_slice(&bytes)?;
    Ok((record.binding == expected && now.checked_sub(record.issued)
        .is_some_and(|age| age < LIFETIME_SECONDS)).then_some(record))
}

pub fn request_login(profile: &Path, owner: &str, held: &crate::lock::LibraryLock,
    intent: LoginIntent, may_publish: impl FnOnce() -> bool) -> Result<LoginAdmission> {
    ensure!(held.protects(profile)? && profile == crate::service::data_dir().canonicalize()?,
        "selected profile changed");
    crate::service::ensure_writable()?;
    let _lock = auth_journal_lock_with_wait(std::time::Duration::ZERO)?;
    let bytes = read_original(profile)?.context("missing owner session")?;
    let session: Value = serde_json::from_slice(&bytes)?;
    ensure!(!owner.is_empty() && session["user"].as_str() == Some(owner), "session owner changed");
    if intent.mode == LoginMode::OfflineKeygen { return Ok(LoginAdmission::RequiresEmptyProfile); }
    let journal = load_journal_metadata()?;
    if journal.active.is_some() { return Ok(LoginAdmission::Conflict); }
    let binding = binding(profile, &bytes, &journal)?;
    let issued = now()?;
    if let Some(existing) = current(profile, &binding, issued)? {
        if existing.intent != intent { return Ok(LoginAdmission::Conflict); }
        respire_spawn::durable::sync_directory(profile)?;
        return Ok(LoginAdmission::Pending);
    }
    let pending = Pending { operation: uuid::Uuid::new_v4().to_string(), binding, issued, intent };
    let bytes = serde_json::to_vec(&pending)?;
    ensure!(may_publish(), "login request no longer admitted");
    private_write(&profile.join(FILE), &bytes)?;
    Ok(LoginAdmission::Pending)
}

pub(super) fn projection(profile: &Path, session: Option<&[u8]>, journal: &Journal) -> Result<&'static str> {
    let Some(session) = session else { return Ok("none"); };
    Ok(match current(profile, &binding(profile, session, journal)?, now()?)?.map(|p| p.intent.mode) {
        Some(LoginMode::BrowserOauth) => "login_pending",
        Some(LoginMode::Recovery) => "recovery_pending",
        _ => "none",
    })
}

#[cfg(all(test, target_os = "linux"))]
#[path = "pending_login_tests.rs"]
mod tests;
