use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending { id: String, label: String, readonly: bool, claim: Option<String> }

impl Pending {
    fn view(&self) -> SpaceMemberView {
        SpaceMemberView { member_id: self.id.to_string(), display_name: self.label.clone(),
            role: InviteRole::for_readonly(self.readonly), status: "pending" }
    }
}

struct Slot { path: PathBuf, account_login: bool }

fn slot(profile: &Path, account: &str) -> Result<Option<Slot>> {
    let Some(bytes) = read_optional(&profile.join("session.json"))? else { return Ok(None) };
    let session: Value = serde_json::from_slice(&bytes)?;
    ensure!(session["user"].as_str() == Some(account), "space owner changed");
    let Some(addr) = session["addr"].as_str().filter(|s| !s.trim().is_empty()) else { return Ok(None) };
    if session["token"].as_str().is_none_or(|s| s.trim().is_empty()) { return Ok(None); }
    if !ureq::get(addr).request_url().is_ok_and(|url| matches!(url.scheme(), "http" | "https")) { return Ok(None); }
    let scope = digest(&serde_json::to_vec(&(profile, account, addr, digest(&bytes)))?);
    // The unlocked session digest is rechecked by the serving dispatcher.
    // Account login persists auth_salt; delegated native join omits it.
    Ok(Some(Slot { path: profile.join(format!(".space-pending-{scope}.json")), account_login: session["auth_salt"].as_str().is_some_and(|s| !s.is_empty()) }))
}

fn rows(path: &Path) -> Result<Vec<Pending>> {
    use std::io::Read;
    safe_path(path)?;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "invalid pending index");
    let rows: Vec<Pending> = serde_json::from_slice(&bytes)?;
    ensure!(rows.len() <= 128, "invalid pending invitation count");
    let mut ids = std::collections::HashSet::new();
    for row in &rows {
        validate_label(&row.label)?;
        ensure!(uuid::Uuid::parse_str(&row.id).is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == row.id) && ids.insert(&row.id), "invalid pending invitation");
    }
    Ok(rows)
}

// A crash after native commit can leave the pending index intact. Reconcile only
// terminal records for this exact owner, profile, operation ID, key and intent.
fn remaining(path: &Path, profile: &Path, account: &str) -> Result<Vec<Pending>> {
    let mut pending = rows(path)?;
    if pending.is_empty() { return Ok(pending); }
    let root = crate::service::main_data_dir().canonicalize()?;
    let journal = load_journal_metadata()?;
    let owner = digest(account.as_bytes());
    pending.retain(|row| !journal.operations.iter().any(|record| {
        if record.phase != AuthPhase::Committed || record.profile != profile || record.root != root
            || row.claim.as_ref() != Some(&claim(record)) { return false; }
        record.space_operation.as_ref().is_some_and(|op| {
            matches!(&op.source_identity, HostAuthIdentity::Bound { account_id, session_sha256 }
                if account_id == &owner && record.original_sha256.as_ref() == Some(session_sha256))
                && op.intent == (HostSpaceOperation::Invite { note: Some(row.label.clone()), readonly: row.readonly })
                && op.session_id.is_some()
        })
    }));
    Ok(pending)
}

pub fn pending_invite(profile: &Path, lock: &crate::lock::LibraryLock, account: &str,
    request: SpaceInviteRequest, admit: impl FnOnce() -> bool) -> Result<Option<SpaceMemberView>> {
    ensure!(lock.protects(profile)?, "space ownership changed");
    crate::service::ensure_writable()?;
    validate_label(&request.identifier)?;
    let Some(path) = slot(profile, account)? else { return Ok(None) };
    if !path.account_login { return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()); }
    let mut pending = remaining(&path.path, profile, account)?;
    let readonly = request.role == InviteRole::Viewer;
    if let Some(row) = pending.iter().find(|row| row.label == request.identifier && row.readonly == readonly) {
        ensure!(admit(), "invite admission changed");
        return Ok(Some(row.view()));
    }
    ensure!(pending.len() < 128, "pending invitation limit reached");
    let row = Pending { id: uuid::Uuid::new_v4().to_string(), label: request.identifier, readonly, claim: None };
    let view = row.view();
    pending.push(row);
    let bytes = serde_json::to_vec(&pending)?;
    ensure!(bytes.len() <= 64 * 1024 && admit(), "invite admission unavailable");
    private_write(&path.path, &bytes)?;
    Ok(Some(view))
}

pub fn owned_members(profile: &Path, lock: &crate::lock::LibraryLock, account: &str) -> Result<Value> {
    ensure!(lock.protects(profile)?, "space ownership changed");
    let mut result = list_space_members(profile)?;
    if let Some(path) = slot(profile, account)?.filter(|s| s.account_login) {
        let items = result["members"].as_array_mut().context("invalid member list")?;
        for row in remaining(&path.path, profile, account)? { items.push(serde_json::to_value(row.view())?); }
    }
    Ok(result)
}

fn claim(record: &AuthRecord) -> String {
    // IDs and key cannot contain NUL. Fingerprint already binds root, profile, intent and source.
    digest(format!("{}\0{}\0{}", record.operation_id, record.idempotency_key, record.fingerprint).as_bytes())
}

// Called once for a newly authorized native operation, under its original profile lock.
pub(in crate::login_transaction::space) fn bind_native(record: &AuthRecord, journal: &Journal) -> Result<()> {
    let Some(op) = record.space_operation.as_ref() else { return Ok(()) };
    let HostSpaceOperation::Invite { note: Some(label), readonly } = &op.intent else { return Ok(()) };
    let session: Value = serde_json::from_slice(&read_original(&record.profile)?.context("space session missing")?)?;
    let Some(slot) = slot(&record.profile, field(&session, "user")?)? else { return Ok(()) };
    let mut rows = rows(&slot.path)?;
    let Some(row) = rows.iter_mut().find(|row| &row.label == label && row.readonly == *readonly) else { return Ok(()) };
    if let Some(previous) = &row.claim {
        if journal.operations.iter().any(|op| op.phase != AuthPhase::RolledBack && claim(op) == *previous) {
            return Ok(());
        }
    }
    row.claim = Some(claim(record));
    private_write(&slot.path, &serde_json::to_vec(&rows)?)
}

pub(in crate::login_transaction::space) fn finish(ownership: &AuthOwnership, id: &str, key: &str,
    params: &HostSpaceParams) -> Result<()> {
    let claim = digest(format!("{id}\0{key}\0{}", fingerprint(&ownership.root, &ownership.profile, &params.source_identity, &params.operation)?).as_bytes());
    let session: Value = serde_json::from_slice(&read_original(&ownership.profile)?.context("space session missing")?)?;
    let Some(path) = slot(&ownership.profile, field(&session, "user")?)? else { return Ok(()) };
    let mut pending = rows(&path.path)?;
    let Some(index) = pending.iter().position(|row| row.claim.as_ref() == Some(&claim)) else { return Ok(()) };
    let row = &pending[index];
    if params.operation != (HostSpaceOperation::Invite { note: Some(row.label.clone()), readonly: row.readonly }) { return Ok(()); }
    committed_space_invite_member(ownership, id, key, params)?;
    pending.remove(index);
    private_write(&path.path, &serde_json::to_vec(&pending)?)
}

#[cfg(test)]
#[path = "pending_test.rs"]
mod pending_test;
