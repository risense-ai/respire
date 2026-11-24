/// Both guards are held by the helper, never by its waiting CLI parent.
pub struct AuthOwnership {
    root: PathBuf,
    profile: PathBuf,
    _root_lock: crate::lock::LibraryLock,
    _profile_lock: Option<crate::lock::LibraryLock>,
    _journal_lock: std::fs::File,
}

impl AuthOwnership {
    pub fn acquire(root: &Path, profile: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let profile = profile.canonicalize()?;
        ensure!(root == crate::service::main_data_dir().canonicalize()?
            && profile == crate::service::data_dir().canonicalize()?, "auth profile changed");
        let wait = std::time::Duration::from_secs(1);
        let root_lock = crate::lock::LibraryLock::acquire(&root, wait)?;
        let profile_lock = if root == profile { None }
            else { Some(crate::lock::LibraryLock::acquire(&profile, wait)?) };
        let journal_lock = auth_journal_lock()?;
        Ok(Self { root, profile, _root_lock: root_lock, _profile_lock: profile_lock, _journal_lock: journal_lock })
    }

    pub fn recover(&self, identity: &HostAuthIdentity) -> Result<()> {
        let mut journal = load_journal()?;
        if journal.active.as_ref().is_some_and(|active| active.retired) { return retire_active(&mut journal); }
        if journal.active.as_ref().is_some_and(|active| active.operation_id.is_some()) {
            // Identity writes resume only through their own fenced operation/result.
            if journal.operations.iter().any(|record| (record.identity.is_some() || record.key_operation.is_some() || record.space_operation.is_some() || record.profile_remove.is_some())
                && journal.active.as_ref().and_then(|active| active.operation_id.as_deref()) == Some(record.operation_id.as_str())) {
                verify_auth_identity(&self.root, &self.profile, identity)?;
                return Ok(());
            }
            verify_auth_identity_inner(&self.root, &self.profile, identity, true)?;
            if !journal.operations.iter().any(|record| record.account.is_some()
                && journal.active.as_ref().and_then(|active| active.operation_id.as_deref()) == Some(record.operation_id.as_str())) {
                recover_keygen(&mut journal, &self.root, &self.profile, None)?;
            }
        }
        Ok(())
    }

    /// Reconcile only the owned keygen receipt using the host's original material.
    pub fn recover_keygen(&self, identity: &HostAuthIdentity,
        material: Option<&respire_protocol::HostAuthKeygenMaterial>) -> Result<()> {
        let mut journal = load_journal()?;
        if journal.active.as_ref().is_some_and(|active| active.retired) {
            return retire_active(&mut journal);
        }
        if journal.active.is_some() {
            verify_auth_identity_inner(&self.root, &self.profile, identity, true)?;
            recover_keygen(&mut journal, &self.root, &self.profile, material)?;
        }
        Ok(())
    }

    /// Legacy rollback never admits or replays a journaled account operation.
    pub fn recover_legacy(&self, identity: &HostAuthIdentity) -> Result<bool> {
        verify_auth_identity(&self.root, &self.profile, identity)?;
        let mut journal = load_journal()?;
        if journal.active.as_ref().is_some_and(|active| active.retired) {
            retire_active(&mut journal)?;
            return Ok(true);
        }
        let Some(active) = journal.active.as_ref() else { return Ok(false); };
        ensure!(active.operation_id.is_none(),
            "query the original authentication operation; automatic recovery is not permitted");
        rollback_record(&mut journal)?;
        retire_active(&mut journal)?;
        Ok(true)
    }
}

fn canonical_operation_id(value: &str) -> Result<()> {
    let id = uuid::Uuid::parse_str(value).context("invalid auth operation ID")?;
    ensure!(id.get_version_num() == 4 && id.to_string() == value, "auth operation ID must be a canonical random UUID");
    Ok(())
}

fn validate_candidate(record: &AuthRecord) -> Result<Value> {
    let bytes = record.candidate.as_deref().context("auth candidate is missing")?;
    ensure!(record.candidate_sha256.as_deref() == Some(digest(bytes).as_str()), "auth candidate digest mismatch");
    let session: Value = serde_json::from_slice(bytes)?;
    let object = session.as_object().context("auth candidate must be an object")?;
    ensure!(object.len() == 7 && session.get("user").is_none()
        && session["vault_version"] == 4 && session["keyring_account"].as_str() == Some(record.alias.as_str())
        && session["auth_operation_id"].as_str() == Some(record.operation_id.as_str())
        && session["keyring_backend"].as_str() == record.backend.as_deref(), "invalid unbound auth candidate");
    let backend = record.backend.as_deref().context("auth credential backend is missing")?;
    require_persistent_auth_backend(backend)?;
    Ok(session)
}

/// Validate transaction identity without reading host credentials; vault authentication requires explicit material.
pub fn verify_auth_identity(root: &Path, profile: &Path, expected: &HostAuthIdentity) -> Result<()> {
    verify_auth_identity_inner(root, profile, expected, false)
}

/// Validate the locked serving profile's receipt without reading credentials.
/// Host-supplied material must still authenticate the vault before it can open.
pub fn verify_locked_auth_identity(root: &Path, profile: &Path, expected: &HostAuthIdentity) -> Result<()> {
    verify_auth_identity_inner(root, profile, expected, false)
}

fn verify_auth_identity_inner(root: &Path, profile: &Path, expected: &HostAuthIdentity, recovering: bool) -> Result<()> {
    let original = read_original(profile)?;
    match (original.as_deref(), expected) {
        (None, HostAuthIdentity::Absent) => Ok(()),
        (Some(bytes), HostAuthIdentity::Bound { account_id, session_sha256 }) => {
            let session: Value = serde_json::from_slice(bytes).context("invalid auth session JSON")?;
            let user = session["user"].as_str().filter(|user| !user.is_empty()).context("bound auth session needs a user")?;
            ensure!(digest(user.as_bytes()) == *account_id && digest(bytes) == *session_sha256,
                "bound auth session identity changed");
            Ok(())
        }
        (Some(bytes), HostAuthIdentity::Unbound { operation_id, session_sha256 }) => {
            canonical_operation_id(operation_id)?;
            ensure!(digest(bytes) == *session_sha256, "unbound auth session digest mismatch");
            let journal = load_journal_metadata()?;
            let record = journal.operations.iter().find(|record| record.operation_id == *operation_id)
                .context("unbound auth session has no transaction receipt")?;
            if record.key_operation.is_some() {
                ensure!(record.root == root && record.candidate.as_deref() == Some(bytes),
                    "offline recovery receipt does not own this session");
                return keys::verify_unbound_candidate(&journal, record, profile, false);
            }
            let prepared = recovering && record.phase == AuthPhase::Prepared
                && journal.active.as_ref().is_some_and(|active| active.operation_id.as_deref() == Some(operation_id.as_str()));
            ensure!(record.root == root && record.profile == profile && (record.phase == AuthPhase::Committed || prepared)
                && record.candidate.as_deref() == Some(bytes), "unbound auth receipt does not own this session");
            validate_candidate(record)?;
            Ok(())
        }
        _ => anyhow::bail!("auth session identity mismatch; malformed sessions are not anonymous"),
    }
}

fn verify_host_keygen_candidate(record: &AuthRecord,
    material: Option<&respire_protocol::HostAuthKeygenMaterial>) -> Result<String> {
    let material = material.context("host_preparation_required: keygen verification requires the original host material")?;
    let session = validate_candidate(record)?;
    ensure!(session["kdf_salt"].as_str() == Some(material.kdf_salt.as_str())
        && session["wrapped_urk"].as_str() == Some(material.wrapped_urk.as_str())
        && session["urk_nonce"].as_str() == Some(material.urk_nonce.as_str())
        && record.alias == material.keyring_account
        && record.backend.as_deref() == Some(material.keyring_backend.as_str()),
        "host keygen material differs from the original operation");
    crate::memory::SessionKeys::unlock_v4(&material.super_password,
        &material.kdf_salt, &material.wrapped_urk, &material.urk_nonce)
        .context("host keygen material does not unlock the original candidate")?;
    Ok(material.super_password.clone())
}

fn recover_keygen(journal: &mut Journal, root: &Path, profile: &Path,
    material: Option<&respire_protocol::HostAuthKeygenMaterial>) -> Result<()> {
    let active = journal.active.as_ref().context("auth recovery is missing")?;
    let id = active.operation_id.as_deref().context("active recovery is not an auth transaction")?;
    let index = journal.operations.iter().position(|record| record.operation_id == id)
        .context("auth recovery has no admission receipt")?;
    let record = &journal.operations[index];
    ensure!(record.account.is_none() && record.identity.is_none() && record.key_operation.is_none()
        && record.space_operation.is_none() && record.profile_remove.is_none(), "operation is not a keygen transaction");
    ensure!(record.root == root && record.profile == profile && active.directory == profile
        && record.alias == active.alias && record.original_sha256 == active.original.as_deref().map(digest),
        "auth recovery owner mismatch");
    ensure!(active.backends.is_empty() && active.pass_backends.is_empty()
        && active.totp_backends.is_empty() && active.legacy_secret_backends.is_empty(),
        "host_recovery_required: retain the historical Runtime-owned keygen credential record");
    let current = read_original(profile)?;
    if record.phase == AuthPhase::Prepared && current.is_some() && current == record.candidate {
        verify_host_keygen_candidate(record, material)?;
        journal.operations[index].phase = AuthPhase::Committed;
        return retire_active(journal);
    }
    ensure!(record.phase == AuthPhase::Admitted || record.phase == AuthPhase::Prepared,
        "terminal auth operation retained an active transaction");
    ensure!(recovery_credentials::matches_original(active, &current), "auth session changed outside its transaction; retain recovery");
    rollback_record(journal)?;
    journal.operations[index].phase = AuthPhase::RolledBack;
    retire_active(journal)
}

fn auth_result(record: &AuthRecord, material: Option<&respire_protocol::HostAuthKeygenMaterial>) -> Result<HostAuthResult> {
    let committed = record.phase == AuthPhase::Committed;
    let current = read_original(&record.profile)?;
    ensure!(if committed { current == record.candidate }
        else { current.as_deref().map(digest) == record.original_sha256
            || recovery_credentials::matches_restored(record, &record.profile, &current) }, "auth result owner session changed");
    Ok(HostAuthResult {
        operation_id: record.operation_id.clone(),
        outcome: match record.phase {
            AuthPhase::Committed => HostAuthOutcome::Committed,
            AuthPhase::RolledBack => HostAuthOutcome::RolledBack,
            _ => HostAuthOutcome::Unknown,
        },
        session_sha256: if committed { record.candidate_sha256.clone() } else { None },
        path: record.profile.join("session.json"),
        super_password: if committed { Some(verify_host_keygen_candidate(record, material)?) } else { None },
    })
}

fn keygen_overwrite_guard(profile: &Path, original: Option<&[u8]>, force: bool) -> Result<()> {
    let session: Value = match original {
        Some(bytes) => serde_json::from_slice(bytes).context("invalid keygen session; original files preserved")?,
        None => json!({}),
    };
    ensure!(session.is_object(), "keygen session must be an object");
    let has_wrap = session["wrapped_urk"].as_str().is_some_and(|wrap| !wrap.is_empty());
    if force { return Ok(()); }
    ensure!(!has_wrap || (!session["user"].as_str().is_some_and(|user| !user.is_empty())
        && !session["token"].as_str().is_some_and(|token| !token.is_empty())),
        "keygen would replace cloud key material; back up first and use force explicitly");
    let path = crate::service::database_path(profile)?;
    if path.exists() {
        let database = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let memories: bool = database.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')", [], |row| row.get(0))?;
        if memories {
            let count: i64 = database.query_row("SELECT COUNT(*) FROM memories WHERE deleted = 0", [], |row| row.get(0))?;
            ensure!(count == 0, "keygen would replace keys for existing memories; back up first and use force explicitly");
        }
    }
    Ok(())
}

fn keygen_fingerprint(root: &Path, profile: &Path, force: bool) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&json!({"schema_version":respire_protocol::HOST_AUTH_SCHEMA_V,
        "method":"control.auth.keygen", "root":root,"profile":profile,"force":force}))?))
}

fn require_persistent_auth_backend(backend: &str) -> Result<()> {
    ensure!(backend != "kernel", "offline auth requires a persistent credential backend; the volatile kernel keyring cannot recover this transaction");
    Ok(())
}

/// The caller supplies ownership only after checking the inherited launch capability.
/// Replays and result queries never generate replacement key material.
pub fn control_keygen(
    ownership: &AuthOwnership, identity: &HostAuthIdentity, operation_id: &str,
    idempotency_key: &str, force: bool, query_only: bool,
    material: Option<&respire_protocol::HostAuthKeygenMaterial>,
) -> Result<HostAuthResult> {
    crate::service::require_profile_change_host()?;
    ensure!(query_only || material.is_some(), "keygen mutation requires host-provided key material");
    canonical_operation_id(operation_id)?;
    ensure!(!idempotency_key.is_empty() && idempotency_key.len() <= 128
        && idempotency_key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
        "invalid auth idempotency key");
    ensure!(ownership.root == crate::service::main_data_dir().canonicalize()?
        && ownership.profile == crate::service::data_dir().canonicalize()?, "auth ownership changed");
    let mut journal = load_journal()?;
    ensure!(!journal.active.as_ref().is_some_and(|active| active.operation_id.is_some()),
        "recover the owned auth transaction before executing or querying it");
    ensure!(journal.active.is_none(), "interrupted login must be recovered before auth control");
    verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    let fingerprint = keygen_fingerprint(&ownership.root, &ownership.profile, force)?;
    if let Some(record) = journal.operations.iter().find(|record|
        record.operation_id == operation_id || (record.root == ownership.root && record.idempotency_key == idempotency_key)) {
        ensure!(record.operation_id == operation_id && record.idempotency_key == idempotency_key
            && record.fingerprint == fingerprint && record.root == ownership.root && record.profile == ownership.profile,
            "auth idempotency conflict");
        if let (Some(material), Some(_)) = (material, record.candidate.as_ref()) {
            verify_host_keygen_candidate(record, Some(material))?;
        }
        return auth_result(record, material);
    }
    ensure!(!query_only, "auth operation has no admission receipt");
    let material = material.context("keygen mutation requires host-provided key material")?;
    ensure!(material.keyring_account.starts_with("login-") && material.keyring_account.len() <= 128
        && material.keyring_account.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid host keygen credential account");
    require_persistent_auth_backend(&material.keyring_backend)?;
    #[cfg(all(target_os = "linux", any(feature = "native-credential-fixtures", not(any(test, feature = "test-support")))))]
    ensure!(material.keyring_backend == "secret_service", "unsupported host keygen credential backend");
    #[cfg(any(not(target_os = "linux"), all(any(test, feature = "test-support"), not(feature = "native-credential-fixtures"))))]
    ensure!(material.keyring_backend == "native", "unsupported host keygen credential backend");
    ensure!(!material.keyring_backend.is_empty() && !material.super_password.is_empty(),
        "host keygen credential metadata or key is missing");
    crate::memory::SessionKeys::unlock_v4(&material.super_password,
        &material.kdf_salt, &material.wrapped_urk, &material.urk_nonce)
        .context("host keygen material does not unlock the candidate")?;
    // Retaining receipts is necessary to prevent an old operation ID from rotating keys again.
    ensure!(journal.operations.len() < 4096, "auth receipt capacity reached; existing results remain queryable");
    let original = read_original(&ownership.profile)?;
    let alias = material.keyring_account.clone();
    ensure!(!journal.operations.iter().any(|record| record.alias == alias),
        "host keygen credential slot already belongs to an operation");
    if let Some(bytes) = original.as_deref() {
        let session: Value = serde_json::from_slice(bytes).context("invalid keygen source session")?;
        ensure!(session["keyring_account"].as_str() != Some(alias.as_str()),
            "host keygen credential slot belongs to the original session");
    }
    let config = match std::fs::read(crate::service::client_config_path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    journal.active = Some(Recovery { directory: ownership.profile.clone(), original: original.clone(), config,
        alias: alias.clone(), backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(), directory_existed: true,
        operation_id: Some(operation_id.to_owned()), owns_config: false, ..Recovery::default() });
    let index = journal.operations.len();
    journal.operations.push(AuthRecord { operation_id: operation_id.to_owned(), idempotency_key: idempotency_key.to_owned(),
        fingerprint, root: ownership.root.clone(), profile: ownership.profile.clone(), original_sha256: original.as_deref().map(digest),
        candidate: None, candidate_backup: None, candidate_sha256: None, restored: None, alias: alias.clone(), backend: None, phase: AuthPhase::Admitted, account: None, identity: None, key_operation: None, space_operation: None, profile_remove: None });
    save_journal(&mut journal)?;
    if keygen_overwrite_guard(&ownership.profile, original.as_deref(), force).is_err() {
        recover_keygen(&mut journal, &ownership.root, &ownership.profile, Some(material))?;
        return auth_result(&journal.operations[index], Some(material));
    }
    let mutation = (|| -> Result<()> {
        let session = json!({"vault_version":4,"kdf_salt":material.kdf_salt,
            "wrapped_urk":material.wrapped_urk,"urk_nonce":material.urk_nonce,
            "keyring_account":alias,"keyring_backend":material.keyring_backend,
            "auth_operation_id":operation_id});
        // Custody is checkpointed by the host before this request; no plaintext enters recovery.
        let candidate = serde_json::to_vec_pretty(&session)?;
        journal.operations[index].backend = Some(material.keyring_backend.clone());
        journal.operations[index].candidate_sha256 = Some(digest(&candidate));
        journal.operations[index].candidate = Some(candidate);
        validate_candidate(&journal.operations[index])?;
        journal.operations[index].phase = AuthPhase::Prepared;
        save_journal(&mut journal)?;
        ensure!(read_original(&ownership.profile)? == original, "auth session changed before commit");
        private_write(&ownership.profile.join("session.json"),
            journal.operations[index].candidate.as_deref().context("auth candidate is missing")?)?;
        journal.operations[index].phase = AuthPhase::Committed;
        retire_active(&mut journal)
    })();
    if let Err(error) = mutation {
        // Re-read durable state: a failed terminal fsync may follow a committed session.
        journal = load_journal()?;
        if journal.active.is_some() { recover_keygen(&mut journal, &ownership.root, &ownership.profile, Some(material))?; }
        if journal.operations.get(index).is_some_and(|record| record.phase == AuthPhase::Committed) {
            save_journal(&mut journal)?;
            return auth_result(&journal.operations[index], Some(material));
        }
        return Err(error.context("auth keygen did not commit; query its retained operation receipt"));
    }
    auth_result(&journal.operations[index], Some(material))
}

