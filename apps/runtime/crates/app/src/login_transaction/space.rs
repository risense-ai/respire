//! Membership shares auth admission, fencing and durable credential checkpoints.
use super::*;
use std::io::Read;
use respire_protocol::space_control::*;

const MEMBERS: &str = "space_sessions.json";
const PREFIX: &str = "1mem-invite:";

#[derive(Debug)]
struct RemoteRejected;
impl std::fmt::Display for RemoteRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("space request rejected before a remote effect") }
}
impl std::error::Error for RemoteRejected {}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    session_id: String,
    device_name: String,
    readonly: bool,
    issued_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpaceOperation {
    intent: HostSpaceOperation,
    source_identity: HostAuthIdentity,
    members_original: Option<Vec<u8>>,
    members_candidate: Option<Vec<u8>>,
    kick: Vec<HostSpaceKickItem>,
    session_id: Option<String>,
    code_backend: Option<String>,
    code_sha256: Option<String>,
    issued_at: Option<String>,
    target: Option<PathBuf>,
    credential_ready: bool,
}

fn op(journal: &Journal, index: usize) -> Result<&SpaceOperation> {
    journal.operations[index].space_operation.as_ref().context("space receipt missing")
}
fn op_mut(journal: &mut Journal, index: usize) -> Result<&mut SpaceOperation> {
    journal.operations[index].space_operation.as_mut().context("space receipt missing")
}

fn fingerprint(root: &Path, profile: &Path, source: &HostAuthIdentity, intent: &HostSpaceOperation) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&json!({"schema_version":HOST_SPACE_SCHEMA_V,
        "method":"control.space", "root":root,"profile":profile,"source_identity":source,"operation":intent}))?))
}

fn safe_path(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                ensure!(!metadata.file_type().is_symlink(), "space path must not be a symbolic link");
                #[cfg(windows)] {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(metadata.file_attributes() & 0x400 == 0, "space path must not be a reparse point");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    safe_path(path)?;
    match std::fs::read(path) {
        Ok(bytes) => { ensure!(bytes.len() <= 64 * 1024, "space record exceeds limit"); Ok(Some(bytes)) }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn members(bytes: Option<&[u8]>) -> Result<Vec<Member>> {
    let rows: Vec<Member> = bytes.map(serde_json::from_slice).transpose()?.unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        ensure!(uuid::Uuid::parse_str(&row.session_id)?.to_string() == row.session_id
            && seen.insert(row.session_id.clone()) && row.device_name.len() <= 128
            && row.issued_at.len() <= 128, "invalid or duplicate member record");
    }
    Ok(rows)
}

mod invite_projection;
pub use invite_projection::{committed_space_invite_member, list_space_members, SpaceInviteRequest, SpaceMemberView};

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().filter(|value| !value.trim().is_empty()).context("space credential field missing")
}

fn parse_code(code: &str) -> Result<Value> {
    let raw = code.trim();
    let raw = raw.strip_prefix(PREFIX).unwrap_or(raw);
    ensure!(!raw.is_empty() && raw.len() <= 32 * 1024, "invalid invite code length");
    let bytes = hex::decode(raw).map_err(|_| anyhow!("invalid invite encoding"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid invite payload"))?;
    for key in ["space", "addr", "user", "token", "super"] { field(&value, key)?; }
    let name = field(&value, "space")?.trim();
    ensure!(name.len() <= 40 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)), "invalid space name");
    ensure!(value.get("readonly").is_none_or(Value::is_boolean), "invalid invite readonly flag");
    Ok(value)
}

fn check_code(intent: &HostSpaceOperation, code: &str) -> Result<Value> {
    let HostSpaceOperation::Join { invite_sha256, space, addr, user, readonly } = intent
        else { anyhow::bail!("invite input is only valid for join"); };
    let value = parse_code(code)?;
    ensure!(*invite_sha256 == digest(code.trim().as_bytes()) && field(&value,"space")?.trim() == space
        && field(&value,"addr")?.trim().trim_end_matches('/') == addr && field(&value,"user")?.trim() == user
        && value["readonly"].as_bool().unwrap_or(false) == *readonly, "invite differs from the original space intent");
    Ok(value)
}

fn source_session(record: &AuthRecord) -> Result<Value> {
    let bytes = read_original(&record.profile)?.context("space owner session missing")?;
    ensure!(Some(digest(&bytes)) == record.original_sha256, "space source session changed");
    let session: Value = serde_json::from_slice(&bytes)?;
    for key in ["addr","user","token"] { field(&session,key)?; }
    Ok(session)
}

fn host_material(params: &HostSpaceParams) -> Result<&HostSpaceMaterial> {
    let material = params.material.as_ref().context("space host material missing")?;
    ensure!(material.keyring_account.starts_with("login-") && material.keyring_account.len() <= 128
        && material.keyring_account.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !material.super_password.is_empty(), "invalid space host credential binding");
    require_persistent_auth_backend(&material.keyring_backend)?;
    ensure!(matches!(material.keyring_backend.as_str(),"native" | "secret_service"), "invalid space credential backend");
    Ok(material)
}

fn source_super(session: &Value, params: &HostSpaceParams) -> Result<String> {
    let material = host_material(params)?;
    host_keys(session,params)?;
    Ok(material.super_password.clone())
}

fn host_keys(session: &Value, params: &HostSpaceParams) -> Result<crate::memory::SessionKeys> {
    let material = host_material(params)?;
    let salt = field(session,"kdf_salt")?;
    let wrapped = field(session,"wrapped_urk")?;
    let nonce = field(session,"urk_nonce")?;
    let keys = match session["vault_version"].as_i64() {
        Some(2) => crate::memory::SessionKeys::unlock_super(&material.super_password,salt,wrapped,nonce),
        Some(3) => crate::memory::SessionKeys::unlock_vault(&material.super_password,
            material.legacy_secret.as_deref().context("space host legacy factor missing")?,salt,wrapped,nonce),
        Some(4) => crate::memory::SessionKeys::unlock_v4(&material.super_password,salt,wrapped,nonce),
        _ => anyhow::bail!("space requires a supported super-password vault"),
    }?;
    if session["crypto_namespace"].as_str() == Some(crate::memory::crypto::RSRS_PREFIX) {
        crate::memory::SessionKeys::from_urk(keys.urk)
    } else { Ok(keys) }
}

fn selection(ownership: &AuthOwnership, id: &str, key: &str, params: &HostSpaceParams) -> Result<()> {
    crate::service::require_profile_change_host()?;
    crate::service::ensure_writable()?;
    canonical_operation_id(id)?;
    ensure!(!key.is_empty() && key.len() <= 128 && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)), "invalid space key");
    ensure!(params.schema_version == HOST_SPACE_SCHEMA_V && params.binding.installation_root == ownership.root
        && params.binding.profile == ownership.profile && ownership.root == crate::service::main_data_dir().canonicalize()?
        && ownership.profile == crate::service::data_dir().canonicalize()?, "space selection changed");
    if !matches!(params.operation,HostSpaceOperation::Kick { .. }) { host_material(params)?; }
    verify_locked_auth_identity(&ownership.root,&ownership.profile,&params.identity)
}

fn owned_index(journal: &Journal, ownership: &AuthOwnership, id: &str, key: &str, params: &HostSpaceParams) -> Result<Option<usize>> {
    let Some(index) = journal.operations.iter().position(|record| record.operation_id == id || record.idempotency_key == key)
        else { return Ok(None); };
    let record = &journal.operations[index];
    let operation = op(journal,index)?;
    ensure!(record.operation_id == id && record.idempotency_key == key && record.root == ownership.root
        && record.profile == ownership.profile && record.account.is_none() && record.identity.is_none() && record.key_operation.is_none()
        && record.fingerprint == fingerprint(&record.root,&record.profile,&params.source_identity,&params.operation)?
        && record.fingerprint == fingerprint(&record.root,&record.profile,&operation.source_identity,&operation.intent)?
        && read_original(&record.profile)?.as_deref().map(digest) == record.original_sha256, "space idempotency or source conflict");
    if !matches!(record.phase,AuthPhase::Committed | AuthPhase::RolledBack) {
        let active = journal.active.as_ref().context("space recovery missing")?;
        ensure!(active.operation_id.as_deref() == Some(id) && active.alias == record.alias && !active.owns_config
            && active.config.is_none() && active.directory == *operation.target.as_ref().unwrap_or(&record.profile)
            && if operation.target.is_some() { active.original.is_none() && !active.directory_existed }
                else { active.original.as_deref().map(digest) == record.original_sha256 && active.directory_existed }, "space recovery owner mismatch");
    }
    Ok(Some(index))
}

fn admit(ownership: &AuthOwnership, id: &str, key: &str, params: &HostSpaceParams) -> Result<(Journal,usize)> {
    let mut journal = load_journal()?;
    if let Some(index) = owned_index(&journal,ownership,id,key,params)? { return Ok((journal,index)); }
    ensure!(journal.active.is_none() && journal.operations.len() < 4096, "resolve the existing operation before space admission");
    ensure!(params.identity == params.source_identity, "space initial identity changed");
    let original = read_original(&ownership.profile)?;
    let members_original = read_optional(&ownership.profile.join(MEMBERS))?;
    let rows = members(members_original.as_deref())?;
    let mut kick = Vec::new();
    let target = match &params.operation {
        HostSpaceOperation::Join { space, .. } => {
            check_code(&params.operation,params.invite_code.as_deref().context("join invite input missing")?)?;
            let target = ownership.root.join("accounts").join(space);
            safe_path(&target)?;
            ensure!(target.starts_with(&ownership.root) && !target.try_exists()?, "space target already exists or is outside the installation");
            Some(target)
        }
        HostSpaceOperation::Invite { note, .. } => {
            let device = invite_projection::member_label(note.as_deref());
            ensure!(device.len() <= 128 && serde_json::to_vec_pretty(&rows)?.len() + device.len() + 256 <= 64 * 1024,
                "space member record or device-name limit exceeded");
            None
        }
        HostSpaceOperation::Kick { session_id, all } => {
            ensure!(!rows.is_empty(), "space has no issued member sessions");
            for row in &rows {
                if *all || session_id.as_deref() == Some(row.session_id.as_str()) {
                    kick.push(HostSpaceKickItem { session_id: row.session_id.clone(), state: HostSpaceKickState::Queued });
                }
            }
            ensure!(!kick.is_empty(), "space has no matching member session");
            None
        }
    };
    if target.is_none() {
        ensure!(matches!(params.identity,HostAuthIdentity::Bound { .. }), "space owner must have a bound identity");
        let session: Value = serde_json::from_slice(original.as_deref().context("space owner session missing")?)?;
        for field_name in ["addr","user","token"] { field(&session,field_name)?; }
        if matches!(params.operation,HostSpaceOperation::Invite { .. }) { source_super(&session,params)?; }
    }
    let material = if matches!(params.operation,HostSpaceOperation::Kick { .. }) { None } else { Some(host_material(params)?) };
    let alias = material.map(|value| value.keyring_account.clone())
        .unwrap_or_else(|| format!("login-{}",uuid::Uuid::new_v4().simple()));
    journal.active = Some(Recovery { directory: target.clone().unwrap_or_else(|| ownership.profile.clone()),
        original: if target.is_some() { None } else { original.clone() }, config: None, alias: alias.clone(),
        backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(), directory_existed: target.is_none(),
        operation_id: Some(id.to_owned()), owns_config: false, ..Recovery::default() });
    let index = journal.operations.len();
    journal.operations.push(AuthRecord { operation_id: id.to_owned(), idempotency_key: key.to_owned(),
        fingerprint: fingerprint(&ownership.root,&ownership.profile,&params.source_identity,&params.operation)?,
        root: ownership.root.clone(), profile: ownership.profile.clone(), original_sha256: original.as_deref().map(digest),
        candidate: None, candidate_backup: None, candidate_sha256: None, restored: None, alias,
        backend: material.map(|value| value.keyring_backend.clone()), phase: AuthPhase::Admitted,
        account: None, identity: None, key_operation: None,
        profile_remove: None,
        space_operation: Some(SpaceOperation { intent: params.operation.clone(), source_identity: params.source_identity.clone(),
            members_original, members_candidate: None, kick, session_id: None, code_backend: None,
            code_sha256: None, issued_at: None, target, credential_ready: false }) });
    save_journal(&mut journal)?;
    Ok((journal,index))
}

fn reject_unadmitted(ownership: &AuthOwnership, id: &str, key: &str, params: &HostSpaceParams) -> Result<(Journal,usize)> {
    let mut journal = load_journal()?;
    ensure!(journal.active.is_none() && journal.operations.len() < 4096
        && !journal.operations.iter().any(|record| record.operation_id == id || record.idempotency_key == key),
        "space admission may exist; retain the original operation");
    // No remote write can precede durable admission. Keep a terminal negative
    // receipt even when the helper was lost before admission or input was invalid.
    ensure!(params.identity == params.source_identity, "rejected admission source changed");
    let original = read_original(&ownership.profile)?;
    let index = journal.operations.len();
    journal.operations.push(AuthRecord { operation_id:id.to_owned(),idempotency_key:key.to_owned(),
        fingerprint:fingerprint(&ownership.root,&ownership.profile,&params.source_identity,&params.operation)?,
        root:ownership.root.clone(),profile:ownership.profile.clone(),original_sha256:original.as_deref().map(digest),
        candidate:None,candidate_backup: None, candidate_sha256:None,restored:None,alias:format!("login-{}",uuid::Uuid::new_v4().simple()),backend:None,
        phase:AuthPhase::RolledBack,account:None,identity:None,key_operation:None,profile_remove:None,
        space_operation:Some(SpaceOperation { intent:params.operation.clone(),source_identity:params.source_identity.clone(),
            members_original:None,members_candidate:None,kick:Vec::new(),session_id:None,code_backend:None,code_sha256:None,
            issued_at:None,target:None,credential_ready:false }) });
    save_journal(&mut journal)?;
    Ok((journal,index))
}

fn remote(addr: &str, token: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(10)).build();
    let url = format!("{}{path}",addr.trim_end_matches('/'));
    let request = if body.is_some() { agent.post(&url) } else { agent.get(&url) };
    let request = request.set("Authorization",&format!("Bearer {token}"));
    let response = match match body { Some(body) => request.send_json(body), None => request.call() } {
        Ok(response) => response,
        Err(ureq::Error::Status(400 | 401 | 403 | 404 | 405 | 422, _)) => return Err(RemoteRejected.into()),
        Err(_) => anyhow::bail!("space remote outcome unavailable"),
    };
    let mut bytes = Vec::new();
    response.into_reader().take(64 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| anyhow!("space response incomplete"))?;
    ensure!(bytes.len() <= 64 * 1024, "space response exceeds limit");
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid space response"))
}

fn invite(journal: &mut Journal, index: usize, params: &HostSpaceParams) -> Result<Option<String>> {
    let HostSpaceOperation::Invite { note, readonly } = op(journal,index)?.intent.clone() else { anyhow::bail!("not invite"); };
    let session = source_session(&journal.operations[index])?;
    let record = &journal.operations[index];
    let name = if record.profile == record.root {
        field(&session,"user")?.to_owned()
    } else {
        // Journal paths are canonical; resolve accounts under the admitted root before comparing.
        let accounts = record.root.join("accounts");
        safe_path(&accounts)?;
        let accounts = accounts.canonicalize().context("cannot resolve space accounts root")?;
        ensure!(record.profile.parent() == Some(accounts.as_path()), "space profile is not directly under its installation accounts root");
        record.profile.file_name().and_then(|name| name.to_str())
            .context("space profile name is not UTF-8")?.to_owned()
    };
    let secret = source_super(&session,params)?;
    let device = invite_projection::member_label(note.as_deref());
    op_mut(journal,index)?.issued_at = Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs,true));
    journal.operations[index].phase = AuthPhase::RemotePending;
    save_journal(journal)?;
    let reply = remote(field(&session,"addr")?,field(&session,"token")?,"/api/self/sessions",Some(json!({"device_name":device,"readonly":readonly})));
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) if error.is::<RemoteRejected>() => {
            journal.operations[index].phase = AuthPhase::RollingBack;
            save_journal(journal)?;
            reject_invite(journal,index)?;
            return Ok(None);
        }
        Err(_) => { journal.operations[index].phase = AuthPhase::RemoteUnknown; save_journal(journal)?; return Ok(None); }
    };
    let sid = field(&reply,"session_id")?.to_owned();
    ensure!(uuid::Uuid::parse_str(&sid)?.to_string() == sid, "invalid member session identity");
    let code = format!("{PREFIX}{}",hex::encode(serde_json::to_vec(&json!({"space":name,"addr":field(&session,"addr")?,
        "user":field(&session,"user")?,"token":field(&reply,"token")?,"session_id":sid,"super":secret,
        "secret_key":host_material(params)?.legacy_secret,"readonly":readonly,"control_operation_id":journal.operations[index].operation_id,
        "control_fingerprint":journal.operations[index].fingerprint}))?));
    ensure!(code.len() <= 32 * 1024, "invite result exceeds private control limit");
    // The host must persist this private response before it can authorize local commit.
    op_mut(journal,index)?.code_sha256 = Some(digest(code.as_bytes()));
    op_mut(journal,index)?.session_id = Some(sid);
    journal.operations[index].phase = AuthPhase::Authorized;
    save_journal(journal)?;
    Ok(Some(code))
}

fn stage_invite_code(journal: &mut Journal, index: usize, code: &str, params: &HostSpaceParams) -> Result<()> {
    let value = parse_code(code)?;
    let record = &journal.operations[index];
    let operation = op(journal,index)?;
    let HostSpaceOperation::Invite { note,readonly } = &operation.intent else { anyhow::bail!("not invite"); };
    let source = source_session(record)?;
    let material = host_material(params)?;
    ensure!(record.alias == material.keyring_account && record.backend.as_deref() == Some(material.keyring_backend.as_str())
        && operation.code_sha256.as_deref() == Some(digest(code.as_bytes()).as_str()), "invite host checkpoint changed");
    source_super(&source,params)?;
    ensure!(value["control_operation_id"].as_str() == Some(record.operation_id.as_str())
        && value["control_fingerprint"].as_str() == Some(record.fingerprint.as_str())
        && field(&value,"user")? == field(&source,"user")? && field(&value,"addr")? == field(&source,"addr")?
        && value["readonly"].as_bool() == Some(*readonly)
        && field(&value,"super")? == material.super_password, "invite checkpoint owner mismatch");
    let sid = field(&value,"session_id")?.to_owned();
    ensure!(uuid::Uuid::parse_str(&sid)?.to_string() == sid && operation.session_id.as_deref() == Some(sid.as_str()),
        "invalid invite checkpoint session");
    let mut rows = members(operation.members_original.as_deref())?;
    rows.push(Member { session_id:sid.clone(),device_name:invite_projection::member_label(note.as_deref()),
        readonly:*readonly,issued_at:operation.issued_at.clone().context("invite admission time missing")? });
    let operation = op_mut(journal,index)?;
    operation.session_id = Some(sid);
    operation.code_sha256 = Some(digest(code.as_bytes()));
    operation.members_candidate = Some(serde_json::to_vec_pretty(&rows)?);
    journal.operations[index].phase = AuthPhase::Prepared;
    save_journal(journal)
}

fn reject_invite(journal: &mut Journal, index: usize) -> Result<()> {
    ensure!(matches!(op(journal,index)?.intent,HostSpaceOperation::Invite { .. })
        && journal.operations[index].phase == AuthPhase::RollingBack, "space rejection is not an owned invite");
    journal.active.as_ref().context("invite recovery missing")?;
    // Credential retirement belongs to the host that owns the checkpoint.
    journal.operations[index].phase = AuthPhase::RolledBack;
    retire_active(journal)
}

fn join(journal: &mut Journal, index: usize, params: &HostSpaceParams) -> Result<()> {
    let code = params.invite_code.as_deref().context("join invite input missing")?;
    let value = check_code(&op(journal,index)?.intent,code)?;
    let HostSpaceOperation::Join { addr,user, .. } = op(journal,index)?.intent.clone() else { anyhow::bail!("not join"); };
    let target = op(journal,index)?.target.clone().context("join target missing")?;
    safe_path(&target)?;
    ensure!(read_original(&target)?.is_none(), "join target session already exists");
    let token = field(&value,"token")?;
    let who = remote(&addr,token,"/api/self",None)?;
    ensure!(field(&who,"user")? == user, "invite token belongs to a different account");
    let mut vault = remote(&addr,token,"/api/self/vault",None)?;
    if vault.get("version").is_none() { vault["version"] = json!(4); }
    let secret = field(&value,"super")?;
    let material = host_material(params)?;
    ensure!(material.super_password == secret && journal.operations[index].alias == material.keyring_account
        && journal.operations[index].backend.as_deref() == Some(material.keyring_backend.as_str()), "join host credential binding changed");
    let version = vault["version"].as_i64().context("join vault version missing")?;
    ensure!(version != 3 || value["secret_key"].as_str() == material.legacy_secret.as_deref(),
        "join legacy factor differs from the original invite");
    let mut session = json!({"user":user,"addr":addr,"token":token,"session_id":value["session_id"],
        "vault_version":version,"kdf_salt":vault["kdf_salt"],"wrapped_urk":vault["wrapped_urk"],
        "urk_nonce":vault["urk_nonce"],"keyring_account":material.keyring_account,
        "keyring_backend":material.keyring_backend});
    if version == 3 { session["legacy_secret_backend"] = json!(material.keyring_backend); }
    host_keys(&session,params)?;
    // No target existed at admission; verify that it has not acquired an unowned library.
    ensure!(!crate::service::database_path(&target)?.try_exists()?, "join target acquired an unowned library");
    checkpoint_account_candidate(journal,index,&session)?;
    op_mut(journal,index)?.credential_ready = true;
    journal.operations[index].phase = AuthPhase::Prepared;
    save_journal(journal)
}

fn kick(journal: &mut Journal, index: usize) -> Result<()> {
    let session = source_session(&journal.operations[index])?;
    for item in 0..op(journal,index)?.kick.len() {
        if op(journal,index)?.kick[item].state != HostSpaceKickState::Queued { continue; }
        let sid = op(journal,index)?.kick[item].session_id.clone();
        op_mut(journal,index)?.kick[item].state = HostSpaceKickState::RemotePending;
        journal.operations[index].phase = AuthPhase::RemotePending;
        save_journal(journal)?;
        let reply = remote(field(&session,"addr")?,field(&session,"token")?,&format!("/api/self/sessions/{sid}/revoke"),Some(json!({})));
        let state = match reply {
            Ok(reply) if reply["session_id"].as_str() == Some(sid.as_str()) => match reply["revoked"].as_bool() {
                Some(true) => HostSpaceKickState::Revoked,
                Some(false) => HostSpaceKickState::Absent,
                None => HostSpaceKickState::Unknown,
            },
            Err(error) if error.is::<RemoteRejected>() => HostSpaceKickState::Rejected,
            _ => HostSpaceKickState::Unknown,
        };
        op_mut(journal,index)?.kick[item].state = state;
        save_journal(journal)?;
    }
    prepare_kick(journal,index)
}

fn prepare_kick(journal: &mut Journal, index: usize) -> Result<()> {
    for item in &mut op_mut(journal,index)?.kick {
        if item.state == HostSpaceKickState::RemotePending { item.state = HostSpaceKickState::Unknown; }
    }
    let operation = op(journal,index)?;
    let rows: Vec<Member> = members(operation.members_original.as_deref())?.into_iter().filter(|row|
        !operation.kick.iter().any(|item| item.session_id == row.session_id && item.state == HostSpaceKickState::Revoked)).collect();
    op_mut(journal,index)?.members_candidate = Some(serde_json::to_vec_pretty(&rows)?);
    journal.operations[index].phase = AuthPhase::Prepared;
    save_journal(journal)
}

fn verify_join_candidate(journal: &Journal, index: usize, params: &HostSpaceParams) -> Result<()> {
    let record = &journal.operations[index];
    let operation = op(journal,index)?;
    let HostSpaceOperation::Join { addr,user, .. } = &operation.intent else { anyhow::bail!("not join"); };
    let target = operation.target.as_ref().context("join target missing")?;
    safe_path(target)?;
    ensure!(operation.credential_ready && record.candidate.as_deref().map(digest) == record.candidate_sha256,
        "join candidate credential checkpoint missing");
    let session: Value = serde_json::from_slice(record.candidate.as_deref().context("join candidate missing")?)?;
    ensure!(session["user"].as_str() == Some(user) && session["addr"].as_str() == Some(addr)
        && session["keyring_account"].as_str() == Some(record.alias.as_str())
        && session["keyring_backend"].as_str() == record.backend.as_deref(), "join candidate identity changed");
    let backend = record.backend.as_deref().context("join backend missing")?;
    require_persistent_auth_backend(backend)?;
    let material = host_material(params)?;
    ensure!(material.keyring_account == record.alias && material.keyring_backend == backend, "join host credential reference changed");
    host_keys(&session,params)?;
    // Admission required an absent profile. Never adopt a library created later.
    ensure!(!crate::service::database_path(target)?.try_exists()?, "join target acquired an unowned library");
    Ok(())
}

fn verify_join(journal: &Journal, index: usize, params: &HostSpaceParams) -> Result<()> {
    verify_join_candidate(journal,index,params)?;
    let record = &journal.operations[index];
    let operation = op(journal,index)?;
    let target = operation.target.as_ref().context("join target missing")?;
    ensure!(read_original(target)? == record.candidate, "join candidate not durably installed");
    let readonly = matches!(operation.intent,HostSpaceOperation::Join { readonly:true, .. });
    let agent = read_optional(&target.join("agent.json"))?;
    if readonly { ensure!(agent.as_deref() == Some(br#"{"readonly":true,"readonly_team":true}"#.as_slice()), "join readonly_team changed"); }
    else { ensure!(agent.is_none(), "join unexpectedly created agent configuration"); }
    Ok(())
}

include!("space/publication.rs");
include!("space/recovery_fixture.rs");
