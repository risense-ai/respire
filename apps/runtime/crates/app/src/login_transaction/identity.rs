//! Identity writes share the existing durable admission and recovery journal.
use super::*;
use std::io::Read;
use respire_protocol::identity_write_control::{
    HostIdentityWriteOperation, HostIdentityWriteOutcome, HostIdentityWriteParams,
    HostIdentityWriteResult, HostSessionRevokeResult, HOST_IDENTITY_WRITE_SCHEMA_V,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdentityOperation {
    operation: HostIdentityWriteOperation,
    source_identity: HostAuthIdentity,
    revoke: Option<HostSessionRevokeResult>,
}

fn fingerprint(root: &Path, profile: &Path, source: &HostAuthIdentity,
    operation: &HostIdentityWriteOperation) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&json!({"schema_version":HOST_IDENTITY_WRITE_SCHEMA_V,
        "method":"control.identity.write", "root":root,"profile":profile,
        "source_identity":source,"operation":operation}))?))
}

fn selection(ownership: &AuthOwnership, id: &str, key: &str, params: &HostIdentityWriteParams) -> Result<()> {
    crate::service::require_profile_change_host()?;
    canonical_operation_id(id)?;
    ensure!(!key.is_empty() && key.len() <= 128 && key.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)), "invalid identity idempotency key");
    ensure!(params.schema_version == HOST_IDENTITY_WRITE_SCHEMA_V
        && params.binding.installation_root == ownership.root && params.binding.profile == ownership.profile
        && ownership.root == crate::service::main_data_dir().canonicalize()?
        && ownership.profile == crate::service::data_dir().canonicalize()?, "identity write selection changed");
    verify_locked_auth_identity(&ownership.root, &ownership.profile, &params.identity)
}

fn owned_index(journal: &Journal, id: &str, key: &str,
    params: &HostIdentityWriteParams) -> Result<Option<usize>> {
    let Some(index) = journal.operations.iter().position(|record|
        record.operation_id == id || record.idempotency_key == key) else { return Ok(None); };
    let record = &journal.operations[index];
    let operation = record.identity.as_ref().context("operation is not an identity write")?;
    ensure!(record.operation_id == id && record.idempotency_key == key
        && record.root == params.binding.installation_root && record.profile == params.binding.profile && record.account.is_none()
        && record.fingerprint == fingerprint(&params.binding.installation_root, &params.binding.profile, &params.source_identity, &params.operation)?
        && record.fingerprint == fingerprint(&record.root, &record.profile, &operation.source_identity, &operation.operation)?,
        "identity idempotency conflict");
    if record.phase != AuthPhase::Committed {
        let active = journal.active.as_ref().context("identity recovery is missing")?;
        ensure!(active.operation_id.as_deref() == Some(id) && active.directory == record.profile
            && active.alias == record.alias && !active.owns_config && active.config.is_none()
            && active.backends.is_empty() && active.pass_backends.is_empty() && active.totp_backends.is_empty()
            && active.original.as_deref().map(digest) == record.original_sha256,
            "identity recovery binding mismatch");
    }
    verify_owned_session(journal, index)?;
    Ok(Some(index))
}

fn verify_owned_session(journal: &Journal, index: usize) -> Result<()> {
    let record = &journal.operations[index];
    let current = read_original(&record.profile)?;
    if record.phase == AuthPhase::Prepared || record.phase == AuthPhase::Committed {
        ensure!(record.candidate.as_deref().map(digest) == record.candidate_sha256, "identity candidate digest mismatch");
        let original = journal.active.as_ref().filter(|active|
            active.operation_id.as_deref() == Some(record.operation_id.as_str())).map(|active| &active.original);
        ensure!(current == record.candidate || (record.phase == AuthPhase::Prepared && original == Some(&current)),
            "identity candidate no longer owns the selected session");
    } else {
        ensure!(current.as_deref().map(digest) == record.original_sha256, "identity source session changed");
    }
    Ok(())
}

fn result(journal: &Journal, index: usize) -> Result<HostIdentityWriteResult> {
    verify_owned_session(journal, index)?;
    let record = &journal.operations[index];
    let operation = record.identity.as_ref().context("identity receipt is missing")?;
    let current_sha256 = read_original(&record.profile)?.as_deref().map(digest);
    let identity = match (&current_sha256, &operation.source_identity) {
        (None, _) => HostAuthIdentity::Absent,
        (Some(session_sha256), HostAuthIdentity::Bound { account_id, .. }) =>
            HostAuthIdentity::Bound { account_id: account_id.clone(), session_sha256: session_sha256.clone() },
        (Some(session_sha256), HostAuthIdentity::Unbound { operation_id, .. }) =>
            HostAuthIdentity::Unbound { operation_id: operation_id.clone(), session_sha256: session_sha256.clone() },
        _ => anyhow::bail!("identity write created an unowned session"),
    };
    verify_locked_auth_identity(&record.root, &record.profile, &identity)?;
    Ok(HostIdentityWriteResult {
        operation_id: record.operation_id.clone(), fingerprint: record.fingerprint.clone(),
        operation: operation.operation.clone(), outcome: match record.phase {
            AuthPhase::Committed => HostIdentityWriteOutcome::Committed,
            AuthPhase::RemotePending | AuthPhase::RemoteUnknown => HostIdentityWriteOutcome::Unknown,
            AuthPhase::Admitted | AuthPhase::Prepared => HostIdentityWriteOutcome::Pending,
            _ => anyhow::bail!("invalid identity transaction phase"),
        }, path: record.profile.join("session.json"), session_found: record.original_sha256.is_some(),
        session_sha256: current_sha256, identity, revoke: operation.revoke.clone(),
    })
}

fn without_token(original: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    original.map(|bytes| -> Result<Vec<u8>> {
        let mut session: Value = serde_json::from_slice(bytes).context("invalid logout session")?;
        if let Some(fields) = session.as_object_mut() { fields.remove("token"); fields.remove("session_id"); }
        Ok(serde_json::to_vec_pretty(&session)?)
    }).transpose()
}

fn revoke_credentials(original: Option<&[u8]>) -> Result<(String, String)> {
    let session: Value = serde_json::from_slice(original.context("no local session")?)?;
    let addr = session["addr"].as_str().filter(|value| !value.is_empty()).context("cloud address is not configured")?;
    let token = session["token"].as_str().filter(|value| !value.is_empty()).context("no cloud token; log in first")?;
    Ok((addr.trim_end_matches('/').to_owned(), token.to_owned()))
}

fn admit(id: &str, key: &str, params: &HostIdentityWriteParams) -> Result<(Journal, usize)> {
    let mut journal = load_journal()?;
    if let Some(index) = owned_index(&journal, id, key, params)? { return Ok((journal, index)); }
    ensure!(journal.active.is_none(), "resolve the existing transaction before admitting an identity write");
    ensure!(journal.operations.len() < 4096, "auth receipt capacity reached");
    ensure!(serde_json::to_value(&params.source_identity)? == serde_json::to_value(&params.identity)?,
        "initial identity differs from the admitted source");
    let original = read_original(&params.binding.profile)?;
    match &params.operation {
        HostIdentityWriteOperation::Logout { full: false } => { without_token(original.as_deref())?; }
        HostIdentityWriteOperation::Logout { full: true } => {}
        HostIdentityWriteOperation::SessionRevoke { session_id } => {
            uuid::Uuid::parse_str(session_id).context("session id must be a full UUID")?;
            revoke_credentials(original.as_deref())?;
        }
    }
    let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
    journal.active = Some(Recovery { directory: params.binding.profile.clone(), original: original.clone(), config: None,
        alias: alias.clone(), backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(),
        directory_existed: true, operation_id: Some(id.to_owned()), owns_config: false, ..Recovery::default() });
    let index = journal.operations.len();
    journal.operations.push(AuthRecord { operation_id: id.to_owned(), idempotency_key: key.to_owned(),
        fingerprint: fingerprint(&params.binding.installation_root, &params.binding.profile, &params.source_identity, &params.operation)?,
        root: params.binding.installation_root.clone(), profile: params.binding.profile.clone(), original_sha256: original.as_deref().map(digest),
        candidate: None, candidate_backup: None, candidate_sha256: None, restored: None, alias, backend: None, phase: AuthPhase::Admitted, account: None, key_operation: None, space_operation: None, profile_remove: None,
        identity: Some(IdentityOperation { operation: params.operation.clone(), source_identity: params.source_identity.clone(), revoke: None }) });
    save_journal(&mut journal)?;
    Ok((journal, index))
}

fn prepare(journal: &mut Journal, index: usize, candidate: Option<Vec<u8>>) -> Result<()> {
    journal.operations[index].candidate_sha256 = candidate.as_deref().map(digest);
    journal.operations[index].candidate = candidate;
    journal.operations[index].phase = AuthPhase::Prepared;
    save_journal(journal)
}

fn commit(journal: &mut Journal, index: usize) -> Result<()> {
    crate::service::ensure_writable()?;
    verify_owned_session(journal, index)?;
    let record = &journal.operations[index];
    ensure!(record.phase == AuthPhase::Prepared, "identity operation is not prepared");
    // Full logout deletes only session.json. No keyring or configuration is owned here.
    restore(&record.profile.join("session.json"), record.candidate.as_deref())?;
    #[cfg(unix)] { std::fs::File::open(&record.profile)?.sync_all()?; }
    journal.operations[index].phase = AuthPhase::Committed;
    retire_active(journal)
}

fn send_revoke(addr: &str, token: &str, session_id: &str) -> Result<HostSessionRevokeResult> {
    let session_id = uuid::Uuid::parse_str(session_id)?;
    let response = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
        .post(&format!("{addr}/api/self/sessions/{session_id}/revoke"))
        .set("Authorization", &format!("Bearer {token}")).send_json(json!({}))
        .map_err(|_| anyhow!("session revoke outcome is unknown"))?;
    let mut bytes = Vec::new();
    let limit = respire_protocol::host_control::HOST_CONTROL_MAX_FRAME_BYTES;
    response.into_reader().take(limit as u64 + 1).read_to_end(&mut bytes)
        .map_err(|_| anyhow!("session revoke response is incomplete"))?;
    ensure!(bytes.len() <= limit, "session revoke response exceeds the control limit");
    let reply: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid session revoke response"))?;
    let revoked = reply["revoked"].as_bool().context("session revoke outcome is missing")?;
    let returned = reply["session_id"].as_str().context("session revoke identity is missing")?;
    ensure!(uuid::Uuid::parse_str(returned).ok() == Some(session_id), "session revoke response identity mismatch");
    // Do not retain arbitrary server fields (or credentials) in the durable receipt.
    Ok(HostSessionRevokeResult { revoked, session_id: returned.to_owned() })
}

/// The dispatcher must consume a Paused launch grant and bind ownership first.
/// A replay of RemotePending/Unknown never repeats the remote POST.
pub fn control_identity_write(ownership: &AuthOwnership, id: &str, key: &str,
    params: &HostIdentityWriteParams) -> Result<HostIdentityWriteResult> {
    selection(ownership, id, key, params)?;
    crate::service::ensure_writable()?;
    let (mut journal, index) = admit(id, key, params)?;
    if journal.operations[index].phase == AuthPhase::Admitted {
        let original = journal.active.as_ref().context("identity recovery is missing")?.original.clone();
        match &params.operation {
            HostIdentityWriteOperation::Logout { full } => {
                prepare(&mut journal, index, if *full { None } else { without_token(original.as_deref())? })?;
            }
            HostIdentityWriteOperation::SessionRevoke { session_id } => {
                let (addr, token) = revoke_credentials(original.as_deref())?;
                journal.operations[index].phase = AuthPhase::RemotePending;
                save_journal(&mut journal)?;
                let reply = match send_revoke(&addr, &token, session_id) {
                    Ok(reply) => reply,
                    Err(_) => {
                        journal.operations[index].phase = AuthPhase::RemoteUnknown;
                        save_journal(&mut journal)?;
                        return result(&journal, index);
                    }
                };
                let session: Value = serde_json::from_slice(original.as_deref().context("revoke source is missing")?)?;
                let candidate = if reply.revoked && session["session_id"].as_str() == Some(reply.session_id.as_str()) {
                    without_token(original.as_deref())?
                } else { original };
                journal.operations[index].identity.as_mut().context("identity receipt is missing")?.revoke = Some(reply);
                prepare(&mut journal, index, candidate)?;
            }
        }
    }
    if journal.operations[index].phase == AuthPhase::Prepared { commit(&mut journal, index)?; }
    result(&journal, index)
}

/// Journal the local half of a soft logout under the serving dispatcher's library lock.
/// The caller owns native request admission and must quiesce store users before calling.
pub fn serving_soft_logout(held: &crate::lock::LibraryLock, id: &str, key: &str,
    params: &HostIdentityWriteParams) -> Result<HostIdentityWriteResult> {
    let root = crate::service::main_data_dir().canonicalize()?;
    let profile = crate::service::data_dir().canonicalize()?;
    ensure!(matches!(params.operation, HostIdentityWriteOperation::Logout { full: false })
        && params.schema_version == HOST_IDENTITY_WRITE_SCHEMA_V
        && params.binding.installation_root == root && params.binding.profile == profile
        && held.protects(&profile)?, "invalid serving logout ownership");
    ensure!(matches!((&params.identity, &params.source_identity),
        (HostAuthIdentity::Bound { account_id, .. }, HostAuthIdentity::Bound { account_id: source, .. })
            if account_id == source && params.binding.expected_account_id.as_ref() == Some(account_id)),
        "serving logout requires the selected bound account");
    crate::service::ensure_runtime_profile()?;
    crate::service::ensure_writable()?;
    canonical_operation_id(id)?;
    ensure!(!key.is_empty() && key.len() <= 128 && key.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)), "invalid identity idempotency key");
    let _root = if root != profile { Some(crate::lock::LibraryLock::try_acquire(&root)?) } else { None };
    let _journal = auth_journal_lock_with_wait(std::time::Duration::ZERO)?;
    verify_locked_auth_identity(&root, &profile, &params.identity)?;
    let (mut journal, index) = admit(id, key, params)?;
    if journal.operations[index].phase == AuthPhase::Admitted {
        let original = journal.active.as_ref().context("identity recovery is missing")?.original.as_deref();
        let candidate = without_token(original)?;
        prepare(&mut journal, index, candidate)?;
    }
    if journal.operations[index].phase == AuthPhase::Prepared { commit(&mut journal, index)?; }
    result(&load_journal()?, index)
}

/// Result queries can finish only a checkpointed local commit, never a remote write.
pub fn control_identity_write_result(ownership: &AuthOwnership, id: &str, key: &str,
    params: &HostIdentityWriteParams) -> Result<HostIdentityWriteResult> {
    selection(ownership, id, key, params)?;
    let mut journal = load_journal()?;
    let index = owned_index(&journal, id, key, params)?.context("identity admission receipt is missing")?;
    if journal.operations[index].phase == AuthPhase::Prepared { commit(&mut journal, index)?; }
    result(&journal, index)
}

/// Extend the existing admission/recovery test without a network or new runner.
#[cfg(test)]
pub(super) fn assert_existing_recovery(root: &Path) -> Result<()> {
    use respire_protocol::ControlBinding;
    let ownership = AuthOwnership::acquire(root, root)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let original = serde_json::to_vec(&json!({"user":"identity-fixture", "addr":"https://never-contact.invalid",
        "token":"synthetic-unused-token", "session_id":session_id, "wrapped_urk":"preserved-wrap"}))?;
    let source = HostAuthIdentity::Bound { account_id: digest(b"identity-fixture"), session_sha256: digest(&original) };
    let mut params = HostIdentityWriteParams { schema_version: HOST_IDENTITY_WRITE_SCHEMA_V,
        binding: ControlBinding { installation_root: root.to_owned(), profile: root.to_owned(),
            launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: Some(digest(b"identity-fixture")) },
        identity: source.clone(), source_identity: source.clone(), operation: HostIdentityWriteOperation::Logout { full: false } };
    private_write(&root.join("session.json"), &original)?;
    let id = uuid::Uuid::new_v4().to_string();
    crate::service::set_workspace_mode("readonly")?;
    let before_readonly = std::fs::read(recovery_path())?;
    assert!(control_identity_write(&ownership, &id, &id, &params).is_err());
    assert_eq!(std::fs::read(recovery_path())?, before_readonly);
    crate::service::set_workspace_mode("off")?;
    let config = std::fs::read(crate::service::client_config_path())?;
    // Simulate loss after publishing a prepared local candidate, before its receipt.
    let (mut journal, index) = admit(&id, &id, &params)?;
    let candidate = without_token(Some(&original))?.context("logout candidate missing")?;
    prepare(&mut journal, index, Some(candidate.clone()))?;
    private_write(&root.join("session.json"), &candidate)?;
    params.identity = HostAuthIdentity::Bound { account_id: digest(b"identity-fixture"), session_sha256: digest(&candidate) };
    ownership.recover(&params.identity)?;
    assert!(load_journal()?.operations[index].identity.is_some());
    let completed = control_identity_write_result(&ownership, &id, &id, &params)?;
    assert!(completed.outcome == HostIdentityWriteOutcome::Committed);
    assert_eq!(std::fs::read(root.join("session.json"))?, candidate);
    assert_eq!(std::fs::read(crate::service::client_config_path())?, config);
    assert!(control_identity_write_result(&ownership, &id, "foreign-key", &params).is_err());

    // A remote checkpoint is unknown after interruption, never permission to POST again.
    private_write(&root.join("session.json"), &original)?;
    params.identity = source.clone();
    params.operation = HostIdentityWriteOperation::SessionRevoke { session_id: session_id.clone() };
    let revoke_id = uuid::Uuid::new_v4().to_string();
    let (mut journal, index) = admit(&revoke_id, &revoke_id, &params)?;
    journal.operations[index].phase = AuthPhase::RemotePending;
    save_journal(&mut journal)?;
    let pending = std::fs::read(recovery_path())?;
    assert!(control_identity_write_result(&ownership, &revoke_id, &revoke_id, &params)?.outcome == HostIdentityWriteOutcome::Unknown);
    assert!(control_identity_write(&ownership, &revoke_id, &revoke_id, &params)?.outcome == HostIdentityWriteOutcome::Unknown);
    assert_eq!(std::fs::read(recovery_path())?, pending);
    assert_eq!(std::fs::read(root.join("session.json"))?, original);
    params.operation = HostIdentityWriteOperation::SessionRevoke { session_id: uuid::Uuid::new_v4().to_string() };
    assert!(control_identity_write(&ownership, &revoke_id, &revoke_id, &params).is_err());
    params.operation = HostIdentityWriteOperation::SessionRevoke { session_id: session_id.clone() };
    params.source_identity = HostAuthIdentity::Absent;
    assert!(control_identity_write_result(&ownership, &revoke_id, &revoke_id, &params).is_err());
    params.source_identity = source;
    // Model the already durable successful reply; recovery applies only its local half.
    journal.operations[index].identity.as_mut().context("identity receipt missing")?.revoke =
        Some(HostSessionRevokeResult { revoked: true, session_id });
    prepare(&mut journal, index, Some(candidate.clone()))?;
    assert!(control_identity_write_result(&ownership, &revoke_id, &revoke_id, &params)?.outcome == HostIdentityWriteOutcome::Committed);
    assert_eq!(std::fs::read(root.join("session.json"))?, candidate);

    params.identity = HostAuthIdentity::Bound { account_id: digest(b"identity-fixture"), session_sha256: digest(&candidate) };
    params.source_identity = params.identity.clone();
    params.operation = HostIdentityWriteOperation::Logout { full: true };
    let full_id = uuid::Uuid::new_v4().to_string();
    let full = control_identity_write(&ownership, &full_id, &full_id, &params)?;
    assert!(full.outcome == HostIdentityWriteOutcome::Committed && full.session_found);
    assert!(full.identity == HostAuthIdentity::Absent && !root.join("session.json").exists());
    assert_eq!(std::fs::read(crate::service::client_config_path())?, config);
    params.identity = HostAuthIdentity::Absent;
    params.binding.expected_account_id = None;
    assert!(control_identity_write_result(&ownership, &full_id, &full_id, &params)?.outcome == HostIdentityWriteOutcome::Committed);
    params.operation = HostIdentityWriteOperation::Logout { full: false };
    assert!(control_identity_write(&ownership, &full_id, &full_id, &params).is_err());
    params.operation = HostIdentityWriteOperation::Logout { full: true };
    params.source_identity = HostAuthIdentity::Absent;
    let absent_id = uuid::Uuid::new_v4().to_string();
    let absent = control_identity_write(&ownership, &absent_id, &absent_id, &params)?;
    assert!(!absent.session_found && absent.outcome == HostIdentityWriteOutcome::Committed);
    assert!(!auth_recovery_pending()?);
    Ok(())
}

#[cfg(test)]
#[test]
fn serving_soft_logout_requires_selected_lock_and_preserves_local_session() -> Result<()> {
    use crate::lock::LibraryLock;
    use respire_protocol::ControlBinding;
    let isolated = crate::test_lock::Isolate::new()?;
    let root = isolated.path().canonicalize()?;
    let original = br#"{"user":"local","token":"synthetic","session_id":"old","wrapped_urk":"retain"}"#;
    private_write(&root.join("session.json"), original)?;
    let account = digest(b"local");
    let source = HostAuthIdentity::Bound { account_id: account.clone(), session_sha256: digest(original) };
    let id = uuid::Uuid::new_v4().to_string();
    let mut params = HostIdentityWriteParams { schema_version: HOST_IDENTITY_WRITE_SCHEMA_V,
        binding: ControlBinding { installation_root: root.clone(), profile: root.clone(),
            launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: Some(account) },
        identity: source.clone(), source_identity: source, operation: HostIdentityWriteOperation::Logout { full: false } };
    let foreign = LibraryLock::try_acquire(&root.join("foreign"))?;
    assert!(serving_soft_logout(&foreign, &id, &id, &params).is_err());
    assert_eq!(read_original(&root)?.as_deref(), Some(original.as_slice()));
    let held = LibraryLock::try_acquire(&root)?;
    let receipt = serving_soft_logout(&held, &id, &id, &params)?;
    assert!(receipt.outcome == HostIdentityWriteOutcome::Committed);
    let session: Value = serde_json::from_slice(&read_original(&root)?.unwrap())?;
    assert_eq!(session, json!({"user":"local","wrapped_urk":"retain"}));
    let bytes = std::fs::read(recovery_path())?;
    params.identity = receipt.identity;
    assert!(serving_soft_logout(&held, &id, &id, &params)?.outcome == HostIdentityWriteOutcome::Committed);
    assert!(serving_soft_logout(&held, &id, "foreign-key", &params).is_err());
    assert_eq!(std::fs::read(recovery_path())?, bytes);
    Ok(())
}

#[cfg(all(test, target_os="linux"))]
#[test]
#[ignore = "requires locked Runtime binary and private local tmpfs"]
fn serving_binary_authenticates_and_finishes_soft_logout() -> Result<()> {
    use std::{net::TcpListener, process::{Command, Stdio}, time::{Duration, Instant}};
    let _guard=crate::test_lock::guard();
    let _custody=crate::migration::memory_keyring_for_test();
    let dir=tempfile::tempdir_in(std::env::var("CARD010_NATIVE_TMPFS")?)?;
    let root=dir.path().canonicalize()?;
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore { fn drop(&mut self){match self.0.take(){Some(v)=>std::env::set_var("RSRS_DATA_DIR",v),None=>std::env::remove_var("RSRS_DATA_DIR")}} }
    let _restore=Restore(std::env::var_os("RSRS_DATA_DIR"));
    std::env::set_var("RSRS_DATA_DIR",&root);
    let (pass,_,salt,wrap,nonce)=crate::auth::new_vault_v4()?;
    let session=serde_json::to_vec(&json!({"user":"source","token":"synthetic","session_id":"synthetic","vault_version":4,"kdf_salt":salt,"wrapped_urk":wrap,"urk_nonce":nonce,"addr":"http://127.0.0.1:1"}))?;
    private_write(&root.join("session.json"),&session)?;
    let listener=TcpListener::bind(("127.0.0.1",0))?;
    let addr=listener.local_addr()?;drop(listener);
    let base=format!("http://{addr}");
    let token=uuid::Uuid::new_v4().to_string();
    struct Owned(std::process::Child);
    impl Drop for Owned { fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();} }
    let mut child=Owned(Command::new(std::env::var("CARD010_RUNTIME")?).args(["serve","--port",&addr.port().to_string(),"--host","127.0.0.1"])
        .env("HOME",&root).env("RSRS_RPC_TOKEN",&token).env("RSRS_NO_AUTOSYNC","1").stdout(Stdio::null()).spawn()?);
    let wait=|f:&mut dyn FnMut()->bool| ->Result<()> {
        let end=Instant::now()+Duration::from_secs(8);while !f(){ensure!(Instant::now()<end,"fixture timeout");std::thread::sleep(Duration::from_millis(20));}Ok(())
    };
    let ep=root.join("runtime/endpoint.json");wait(&mut ||ep.is_file())?;
    let endpoint:Value=serde_json::from_slice(&std::fs::read(ep)?)?;
    let http=ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(10)).build();
    let unlock=json!({"schema_version":2,"profile":root,"session_sha256":digest(&session),"instance_id":endpoint["instance_id"],
        "generation":endpoint["generation"],"super_password":pass,"provider_update":{"kind":"replace","credentials":[]}});
    let receipt:Value=http.post(&format!("{base}/api/runtime/unlock")).set("Authorization",&format!("Bearer {token}")).send_json(unlock)?.into_json()?;
    ensure!(receipt["store_ready"]==true);
    let runtime = root.join("runtime");
    let instance = endpoint["instance_id"].as_str().context("missing instance")?;
    let read_owner = || respire_spawn::owner_credential::read_owner_credential(&runtime, instance);
    wait(&mut || read_owner().is_ok())?;
    let live:Value=http.get(&format!("{base}/api/health")).set("Authorization",&format!("Bearer {token}")).call()?.into_json()?;
    let generation = live["generation"].as_u64().context("missing generation")?;
    let id = uuid::Uuid::new_v4();
    let request = || http.post(&format!("{base}/v1/host/transactions/logout"))
        .set("Authorization",&format!("Bearer {token}")).set("x-respire-operation-id",&id.to_string())
        .set("x-respire-session-revision",&digest(&session));
    ensure!(matches!(request().set("x-respire-host-proof","wrong").call(),Err(ureq::Error::Status(401,_))));
    let mut native = Command::new(std::env::var("CARD010_RUNTIME")?).arg("--host-soft-logout-internal")
        .env("HOME",&root).env("RSRS_RPC_PORT",addr.port().to_string()).env("RSRS_RPC_TOKEN",&token)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    std::io::Write::write_all(&mut native.stdin.take().context("native input")?, id.to_string().as_bytes())?;
    let output = native.wait_with_output()?;
    ensure!(output.status.success() && serde_json::from_slice::<Value>(&output.stdout)? == json!({"status":"completed"}));
    wait(&mut || child.0.try_wait().is_ok_and(|s|s.is_some_and(|s|s.success())))?;
    ensure!(read_owner().is_err());
    let saved:Value=serde_json::from_slice(&std::fs::read(root.join("session.json"))?)?;
    ensure!(saved.get("token").is_none() && saved.get("session_id").is_none()
        && saved["wrapped_urk"] == wrap && saved["user"] == "source");
    Ok(())
}
