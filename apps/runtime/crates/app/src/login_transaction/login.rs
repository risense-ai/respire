//! Resume only the admitted login; never journal the supplemental request object.
use super::*;
use crate::MemoryTransport;
use respire_protocol::host_control::{HostAuthLoginContinuation, HostAuthLoginStatus};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportSnapshot {
    target: PathBuf,
    sha256: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeCheckpoint {
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundChallenge {
    binding: String,
    challenge: crate::auth::PasswordAuthorizationChallenge,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginProgress {
    migrate_local: bool,
    migrate_vault: bool,
    offer_local_merge: bool,
    merge_local: Option<bool>,
    merge_target: Option<PathBuf>,
    required_inputs: Vec<String>,
    #[serde(default)]
    password_slot: bool,
    #[serde(default)]
    pub(super) original_target_key: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    challenge: Option<ChallengeCheckpoint>,
    snapshot: Option<ImportSnapshot>,
    import_started: bool,
    #[serde(default)]
    sync_started: bool,
    import: Option<Value>,
    sync: Option<Value>,
    migration_cloud: Option<Value>,
    migration_salt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migration_auth_sha256: Option<String>,
    publication_pending: bool,
    publication_done: bool,
    publication_step: Option<String>,
    vault_done: bool,
    password_done: bool,
}

impl LoginProgress {
    pub(super) fn new(migrate_local: bool, migrate_vault: bool, offer_local_merge: bool) -> Self {
        Self { migrate_local, migrate_vault, offer_local_merge, merge_local: None, merge_target: None,
            required_inputs: Vec::new(), password_slot: false, original_target_key: None, challenge: None, snapshot: None,
            import_started: false, sync_started: false, import: None, sync: None, migration_cloud: None,
            migration_salt: None, migration_auth_sha256: None, publication_pending: false, publication_done: false,
            publication_step: None, vault_done: false, password_done: false }
    }

    pub(super) fn status(&self, rolled_back: bool, account: &AccountOperation) -> HostAuthLoginStatus {
        HostAuthLoginStatus { required_inputs: if rolled_back { Vec::new() } else { self.required_inputs.clone() },
            user: account.user.clone(), addr: account.addr.clone(),
            dir: account.target.clone().or_else(|| self.merge_target.clone()), super_issued: None,
            import: self.import.clone(), sync: self.sync.clone() }
    }

    pub(super) fn awaiting_input(&self) -> bool { !self.required_inputs.is_empty() }

    fn imports_local(&self) -> bool { self.migrate_local || self.merge_local == Some(true) }

    pub(super) fn offer_merge(&mut self, target: &Path) -> Result<()> {
        ensure!(self.offer_local_merge && !self.migrate_local, "local merge was not admitted");
        ensure!(self.merge_target.as_deref().is_none_or(|original| original == target),
            "local merge offer destination changed");
        self.merge_target = Some(target.to_owned());
        Ok(())
    }

    pub(super) fn confirm_merge(&mut self, target: &Path, confirmed: bool) -> Result<()> {
        ensure!(self.offer_local_merge && !self.migrate_local && self.merge_target.as_deref() == Some(target),
            "local merge confirmation has no admitted offer for this destination");
        ensure!(self.merge_local.is_none_or(|original| original == confirmed), "local merge confirmation cannot change");
        self.merge_local = Some(confirmed);
        Ok(())
    }

    pub(super) fn pending(&self) -> bool {
        !self.required_inputs.is_empty() || self.imports_local() && self.import.is_none()
            || self.publication_pending
    }

    pub(super) fn publication_started(&self) -> bool {
        self.publication_pending || self.publication_done || self.vault_done || self.password_done
    }

    pub(super) fn can_compensate_import(&self) -> bool {
        self.imports_local() && !self.migrate_vault && !self.import_started
            && self.import.is_none() && !self.publication_started()
    }
}

fn progress(journal: &mut Journal, index: usize) -> Result<&mut LoginProgress> {
    journal.operations[index].account.as_mut().and_then(|account| account.login.as_mut())
        .context("login continuation receipt is missing")
}

fn migration_auth_digest(record: &AuthRecord, candidate_sha256: &str, authentication: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(&record.operation_id, &record.fingerprint, candidate_sha256, authentication))?))
}

/// Bind the original authentication request before the host confirms credential custody.
pub(super) fn bind_migration_auth(record: &mut AuthRecord, candidate_sha256: &str,
    authentication: Option<&Value>, acknowledging: bool) -> Result<()> {
    let proof = authentication.map(|authentication| migration_auth_digest(record, candidate_sha256, authentication)).transpose()?;
    let candidate_exists = record.candidate_sha256.is_some();
    let login = record.account.as_mut().and_then(|account| account.login.as_mut())
        .context("login continuation receipt is missing")?;
    ensure!(login.migrate_vault == proof.is_some(), "migration authentication evidence is missing or unexpected");
    if acknowledging || candidate_exists {
        ensure!(login.migration_auth_sha256 == proof,
            "migration authentication differs from the original login candidate; original operation retained");
    } else {
        login.migration_auth_sha256 = proof;
    }
    Ok(())
}

pub(super) fn factor_keys(session: &Value, factors: &respire_protocol::host_control::HostAuthVaultFactors)
    -> Result<crate::memory::SessionKeys> {
    use respire_protocol::host_control::HostAuthVaultFactors as Factors;
    let salt = session["kdf_salt"].as_str().context("vault salt missing")?;
    let wrapped = session["wrapped_urk"].as_str().context("vault wrap missing")?;
    let nonce = session["urk_nonce"].as_str().context("vault nonce missing")?;
    let version = session["vault_version"].as_i64().context("vault version missing")?;
    match (version, factors) {
        (1, Factors::V1 { password, account_secret }) => crate::memory::SessionKeys::unlock(password, account_secret, salt, wrapped, nonce),
        (2, Factors::V2 { super_password }) => crate::memory::SessionKeys::unlock_super(super_password, salt, wrapped, nonce),
        (3, Factors::V3 { super_password, secret_key }) => crate::memory::SessionKeys::unlock_vault(super_password, secret_key, salt, wrapped, nonce),
        (4, Factors::V4 { super_password }) => crate::memory::SessionKeys::unlock_v4(super_password, salt, wrapped, nonce),
        _ => anyhow::bail!("host factors do not match the original vault version"),
    }.context("explicit host factors do not unlock the original vault")
}

pub(super) fn raw_factor_keys(session: &Value, password: Option<&str>, super_password: Option<&str>, secret: Option<&str>)
    -> Result<crate::memory::SessionKeys> {
    use respire_protocol::host_control::HostAuthVaultFactors as Factors;
    let factors = match session["vault_version"].as_i64() {
        Some(1) => Factors::V1 { password: password.context("explicit v1 password required")?.to_owned(),
            account_secret: secret.context("explicit v1 Account Secret required")?.to_owned() },
        Some(2) => Factors::V2 { super_password: super_password.context("explicit super key required")?.to_owned() },
        Some(3) => Factors::V3 { super_password: super_password.context("explicit super key required")?.to_owned(),
            secret_key: secret.context("explicit v3 Secret Key required")?.to_owned() },
        Some(4) => Factors::V4 { super_password: super_password.context("explicit super key required")?.to_owned() },
        _ => anyhow::bail!("unsupported host vault version"),
    };
    factor_keys(session, &factors)
}

pub(super) fn supplied_keys(bytes: &[u8], material: Option<&respire_protocol::host_control::HostAuthSessionMaterial>)
    -> Result<crate::memory::SessionKeys> {
    let material = material.context("explicit host session material is required")?;
    ensure!(digest(bytes) == material.session_sha256, "host session material digest changed");
    factor_keys(&serde_json::from_slice(bytes)?, &material.factors)
}

fn request_candidate(params: &HostAuthLoginParams, record: &AuthRecord)
    -> Result<Option<respire_protocol::host_control::HostAuthSessionMaterial>> {
    use respire_protocol::host_control::{HostAuthSessionMaterial, HostAuthVaultFactors as Factors};
    let Some(bytes) = record.candidate.as_deref() else { return Ok(None); };
    let session: Value = serde_json::from_slice(bytes)?;
    let input = params.continuation.as_ref();
    let password = input.and_then(|input| input.legacy_password.as_ref()).or_else(|| match &params.authorization {
        HostAuthAuthorization::Password { password, .. } => Some(password), _ => None });
    let secret = input.and_then(|input| input.account_secret.as_ref());
    let factors = match session["vault_version"].as_i64() {
        Some(1) => Factors::V1 { password: password.context("explicit v1 password required")?.clone(),
            account_secret: secret.context("explicit v1 Account Secret required")?.clone() },
        Some(2) => Factors::V2 { super_password: params.super_password.clone().context("explicit super key required")? },
        Some(3) => Factors::V3 { super_password: params.super_password.clone().context("explicit super key required")?,
            secret_key: input.and_then(|input| input.secret_key.as_ref()).context("explicit v3 Secret Key required")?.clone() },
        Some(4) => Factors::V4 { super_password: params.super_password.clone().context("explicit super key required")? },
        _ => anyhow::bail!("unsupported login candidate vault version"),
    };
    Ok(Some(HostAuthSessionMaterial { session_sha256: digest(bytes), factors }))
}

fn reply(receipt: HostAuthAccountResult) -> respire_protocol::host_control::HostAuthLoginReply {
    respire_protocol::host_control::HostAuthLoginReply { receipt, handoff: None }
}

pub(super) fn candidate_reply(record: &AuthRecord) -> Result<respire_protocol::host_control::HostAuthLoginReply> {
    use respire_protocol::host_control::{HostAuthLoginHandoff, HostAuthLoginSlot};
    let mut result = reply(account_result(record)?);
    if record.phase == AuthPhase::Authorized && record.account.as_ref().and_then(|account| account.login.as_ref())
        .is_some_and(|login| login.required_inputs.as_slice() == ["host_credentials"]) {
        let session: Value = serde_json::from_slice(record.candidate.as_deref().context("login custody candidate missing")?)?;
        let mut slots = if session["vault_version"] == 1 { vec![HostAuthLoginSlot::Pass] }
            else { vec![HostAuthLoginSlot::Super] };
        if matches!(session["vault_version"].as_i64(), Some(1 | 3)) { slots.push(HostAuthLoginSlot::LegacySecret); }
        if record.account.as_ref().and_then(|account| account.login.as_ref()).is_some_and(|login| login.password_slot) {
            if session["vault_version"] != 1 { slots.push(HostAuthLoginSlot::Pass); }
        }
        result.handoff = Some(HostAuthLoginHandoff::Credentials {
            candidate_sha256: record.candidate_sha256.clone().context("login custody digest missing")?, session, slots });
    }
    Ok(result)
}

fn require_inputs(journal: &mut Journal, index: usize, inputs: &[&str]) -> Result<respire_protocol::host_control::HostAuthLoginReply> {
    progress(journal, index)?.required_inputs = inputs.iter().map(|input| (*input).to_owned()).collect();
    save_journal(journal)?;
    Ok(reply(account_result(&journal.operations[index])?))
}

fn challenge_binding(record: &AuthRecord) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(&record.operation_id, &record.idempotency_key,
        &record.fingerprint, &record.root, &record.profile, &record.alias))?))
}

pub(super) fn encode_challenge(record: &AuthRecord, challenge: crate::auth::PasswordAuthorizationChallenge) -> Result<String> {
    serde_json::to_string(&BoundChallenge { binding: challenge_binding(record)?, challenge }).map_err(Into::into)
}

pub(super) fn verify_challenge(record: &AuthRecord, sha256: &str, payload: &str) -> Result<crate::auth::PasswordAuthorizationChallenge> {
    ensure!(digest(payload.as_bytes()) == sha256, "challenge credential changed");
    let bound: BoundChallenge = serde_json::from_str(payload).context("invalid challenge credential")?;
    ensure!(bound.binding == challenge_binding(record)?, "challenge operation binding changed");
    Ok(bound.challenge)
}

fn checkpoint_challenge(journal: &mut Journal, index: usize,
    challenge: crate::auth::PasswordAuthorizationChallenge) -> Result<respire_protocol::host_control::HostAuthLoginHandoff> {
    ensure!(journal.operations[index].phase == AuthPhase::RemotePending,
        "challenge does not own a pending password step");
    let payload = encode_challenge(&journal.operations[index], challenge)?;
    let sha256 = digest(payload.as_bytes());
    progress(journal, index)?.challenge = Some(ChallengeCheckpoint { sha256: sha256.clone() });
    journal.operations[index].phase = AuthPhase::LoginInput;
    progress(journal, index)?.required_inputs = vec!["totp".to_owned()];
    save_journal(journal)?;
    Ok(respire_protocol::host_control::HostAuthLoginHandoff::Challenge { payload, sha256 })
}

fn read_challenge(journal: &Journal, index: usize,
    supplied: Option<&respire_protocol::host_control::HostAuthLoginChallenge>) -> Result<crate::auth::PasswordAuthorizationChallenge> {
    let record = &journal.operations[index];
    ensure!(record.phase == AuthPhase::LoginInput, "challenge is not awaiting input");
    let checkpoint = record.account.as_ref().and_then(|account| account.login.as_ref())
        .and_then(|login| login.challenge.as_ref()).context("challenge checkpoint is missing")?;
    let supplied = supplied.context("host challenge material is required; original operation retained")?;
    ensure!(supplied.sha256 == checkpoint.sha256, "host challenge digest changed");
    verify_challenge(record, &checkpoint.sha256, &supplied.payload)
}

fn save_authorization(journal: &mut Journal, index: usize, mut authorization: Value) -> Result<()> {
    let object = authorization.as_object_mut().context("authorization must be an object")?;
    for field in ["pass", "super", "secret", "secret_key"] { object.remove(field); }
    journal.operations[index].account.as_mut().context("login receipt missing")?.authorization = Some(authorization);
    journal.operations[index].phase = AuthPhase::Authorized;
    let login = progress(journal, index)?;
    login.required_inputs.clear();
    login.challenge = None;
    save_journal(journal)
}

include!("login/publication.rs");
