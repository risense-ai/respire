fn checkpoint_account_candidate(journal: &mut Journal, index: usize, session: &Value) -> Result<()> {
    ensure!(["pass", "super", "secret", "secret_key"].iter().all(|field| session.get(*field).is_none()),
        "plaintext factors cannot enter recovery");
    let bytes = serde_json::to_vec_pretty(session)?;
    journal.operations[index].candidate_sha256 = Some(digest(&bytes));
    journal.operations[index].candidate = Some(bytes);
    journal.operations[index].candidate_backup = None;
    Ok(())
}

fn account_fingerprint(ownership: &AuthOwnership, identity: &HostAuthIdentity, register: bool, addr: &str, user: &str) -> Result<String> {
    // Replays never consume replacement credentials or repeat a remote request.
    // Do not put a password or an offline password verifier in the receipt.
    Ok(digest(&serde_json::to_vec(&json!({"schema_version":1,"register":register,
        "root":ownership.root,"source":ownership.profile,"identity":identity,"addr":addr,"user":user}))?))
}

fn admit_account(ownership: &AuthOwnership, identity: &HostAuthIdentity, operation_id: &str,
    key: &str, register: bool, addr: &str, user: &str) -> Result<(Journal, usize, bool)> {
    admit_account_with_intent(ownership, identity, operation_id, key, register, addr, user, None)
}

fn admit_account_with_intent(ownership: &AuthOwnership, identity: &HostAuthIdentity, operation_id: &str,
    key: &str, register: bool, addr: &str, user: &str,
    login_intent: Option<(bool, bool, bool)>) -> Result<(Journal, usize, bool)> {
    crate::service::require_profile_change_host()?;
    canonical_operation_id(operation_id)?;
    ensure!(!key.is_empty() && key.len() <= 128 && key.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)), "invalid auth idempotency key");
    ensure!(ownership.root == crate::service::main_data_dir().canonicalize()?
        && ownership.profile == crate::service::data_dir().canonicalize()?, "auth ownership changed");
    ensure!(!user.is_empty() && !addr.is_empty(), "account and server are required");
    if register {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    } else {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    }
    let base = account_fingerprint(ownership, identity, register, addr, user)?;
    let fingerprint = match login_intent {
        Some(intent) => digest(&serde_json::to_vec(&(&base, intent))?),
        None => base.clone(),
    };
    let mut journal = load_journal()?;
    if let Some(index) = journal.operations.iter().position(|record|
        record.operation_id == operation_id || record.idempotency_key == key) {
        let record = &journal.operations[index];
        let legacy_login = login_intent == Some((false, false, false)) && record.fingerprint == base
            && record.account.as_ref().is_some_and(|account| !account.register && account.login.is_none());
        ensure!(record.operation_id == operation_id && record.idempotency_key == key
            && record.root == ownership.root && record.profile == ownership.profile
            && (record.fingerprint == fingerprint || legacy_login) && record.account.is_some(), "auth idempotency conflict");
        return Ok((journal, index, false));
    }
    ensure!(journal.active.is_none(), "resolve the interrupted transaction before admitting another account operation");
    ensure!(journal.operations.len() < 4096, "auth receipt capacity reached");
    let original = read_original(&ownership.profile)?;
    let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
    journal.active = Some(Recovery { directory: ownership.profile.clone(), original: original.clone(),
        config: None, alias: alias.clone(), backends: Vec::new(), pass_backends: Vec::new(), totp_backends: Vec::new(),
        directory_existed: true, operation_id: Some(operation_id.to_owned()), owns_config: false, ..Recovery::default() });
    let index = journal.operations.len();
    journal.operations.push(AuthRecord { operation_id: operation_id.to_owned(), idempotency_key: key.to_owned(),
        fingerprint, root: ownership.root.clone(), profile: ownership.profile.clone(),
        original_sha256: original.as_deref().map(digest), candidate: None, candidate_backup: None, candidate_sha256: None, restored: None,
        alias, backend: None, phase: AuthPhase::Admitted, identity: None, key_operation: None, space_operation: None, profile_remove: None, account: Some(AccountOperation {
            register, addr: addr.to_owned(), user: user.to_owned(), source_identity: identity.clone(),
            target: None, authorization: None, vault: None, published_vault: false, vault_pending: false,
            login: login_intent.map(|(migrate_local, migrate_vault, offer_local_merge)|
                login::LoginProgress::new(migrate_local, migrate_vault, offer_local_merge)),
        }) });
    save_journal(&mut journal)?;
    Ok((journal, index, true))
}

fn account_result(record: &AuthRecord) -> Result<HostAuthAccountResult> {
    let account = record.account.as_ref().context("operation is not account authentication")?;
    let applied = record.phase == AuthPhase::AwaitingVerification || record.phase == AuthPhase::Committed;
    if applied { verify_account_candidate(record)?; }
    Ok(HostAuthAccountResult { operation_id: record.operation_id.clone(),
        outcome: match record.phase {
            AuthPhase::Authorized | AuthPhase::LoginInput if account.login.as_ref()
                .is_some_and(|login| login.awaiting_input()) => HostAuthAccountOutcome::AwaitingInput,
            _ if account.vault_pending && !account.published_vault
                || record.phase != AuthPhase::RolledBack && account.login.as_ref().is_some_and(|login| login.pending()) => HostAuthAccountOutcome::Unknown,
            AuthPhase::AwaitingVerification => HostAuthAccountOutcome::AwaitingVerification,
            AuthPhase::Committed => HostAuthAccountOutcome::Committed,
            AuthPhase::RolledBack => HostAuthAccountOutcome::RolledBack,
            _ => HostAuthAccountOutcome::Unknown,
        }, source_profile: record.profile.clone(), target_profile: account.target.clone(),
        account_id: if applied { Some(digest(account.user.as_bytes())) } else { None },
        session_sha256: if applied { record.candidate_sha256.clone() } else { None },
        super_password: None,
        login: account.login.as_ref().map(|login| login.status(record.phase == AuthPhase::RolledBack, account)),
    })
}

fn commit_account(ownership: &AuthOwnership, journal: &mut Journal, index: usize, prepared: &mut PreparedLogin) -> Result<HostAuthAccountResult> {
    ensure!(read_original(&ownership.profile)?.as_deref().map(digest) == journal.operations[index].original_sha256,
        "source profile changed before account commit");
    ensure!(read_original(&prepared.directory)? == prepared.original, "target profile changed before account commit");
    prepared.alias = journal.operations[index].alias.clone();
    prepared.session["keyring_account"] = json!(prepared.alias);
    let active = journal.active.as_mut().context("account recovery is missing")?;
    active.directory = prepared.directory.clone();
    active.original = prepared.original.clone();
    active.directory_existed = prepared.directory.exists();
    active.config = match std::fs::read(crate::service::client_config_path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    active.owns_config = true;
    journal.operations[index].account.as_mut().context("account receipt is missing")?.target = Some(prepared.directory.clone());
    checkpoint_account_candidate(journal, index, &prepared.session)?;
    save_journal(journal)?;
    // Lock creation can create a new target directory, so its prior absence was journaled above.
    let target = if prepared.directory.exists() { prepared.directory.canonicalize()? } else { prepared.directory.clone() };
    let _target_lock = if target == ownership.root || target == ownership.profile { None }
        else { Some(crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?) };
    ensure!(read_original(&prepared.directory)? == prepared.original, "target changed while acquiring ownership");
    prepared.commit_inner_for(Some(&journal.operations[index].operation_id))?;
    *journal = load_journal()?;
    account_result(&journal.operations[index])
}

/// A durable pending receipt is written before POST; an interrupted POST is never replayed.
pub fn control_register(ownership: &AuthOwnership, operation_id: &str, key: &str,
    params: &HostAuthRegisterParams) -> Result<HostAuthAccountResult> {
    ensure!(params.schema_version == respire_protocol::HOST_AUTH_SCHEMA_V
        && params.binding.installation_root == ownership.root && params.binding.profile == ownership.profile,
        "registration binding mismatch");
    let source = read_original(&ownership.profile)?;
    let local: Value = match source.as_deref() { Some(bytes) => serde_json::from_slice(bytes)?, None => json!({}) };
    ensure!(["super", "pass", "secret", "secret_key"].iter().all(|field| local.get(*field).is_none()),
        "host must normalize registration source credentials before admission");
    let addr = params.addr.trim().trim_end_matches('/');
    let (mut journal, index, fresh) = admit_account(ownership, &params.identity, operation_id, key, true, addr, &params.user)?;
    if !fresh { return account_result(&journal.operations[index]); }
    let material = params.material.as_ref().context("registration requires host-prepared vault material")?;
    let slot = material.keyring_account.strip_prefix("login-").context("invalid host registration credential slot")?;
    ensure!(slot.len() == 32 && slot.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid host registration credential slot");
    require_persistent_auth_backend(&material.keyring_backend)?;
    let keys = crate::memory::SessionKeys::unlock_v4(&material.super_password,
        &material.kdf_salt, &material.wrapped_urk, &material.urk_nonce)
        .context("host registration material does not unlock its vault")?;
    if let Some(expected) = &params.super_password {
        ensure!(*expected == material.super_password, "recovery code does not match host registration material");
    }
    let use_source = matches!(&params.identity, HostAuthIdentity::Absent | HostAuthIdentity::Unbound { .. });
    if matches!(&params.identity, HostAuthIdentity::Unbound { .. }) {
        ensure!(local["vault_version"] == 4 && local["kdf_salt"] == material.kdf_salt
            && local["wrapped_urk"] == material.wrapped_urk && local["urk_nonce"] == material.urk_nonce
            && local["keyring_account"] == material.keyring_account && local["keyring_backend"] == material.keyring_backend,
            "host registration material changed the receipt-backed unbound vault");
    }
    let vault = json!({"version":4,"kdf_salt":material.kdf_salt,
        "wrapped_urk":material.wrapped_urk,"urk_nonce":material.urk_nonce});
    journal.operations[index].alias = material.keyring_account.clone();
    journal.operations[index].backend = Some(material.keyring_backend.clone());
    // Host owns both credential slots and their compensation; Runtime journals only their identity.
    journal.active.as_mut().context("account recovery is missing")?.alias = material.keyring_account.clone();
    journal.operations[index].account.as_mut().context("account receipt is missing")?.vault = Some(vault);
    journal.operations[index].phase = AuthPhase::RemotePending;
    save_journal(&mut journal)?;
    let authorized = match crate::auth::registration_authorization(addr, &params.user, &params.password) {
        Ok(authorization) => authorization,
        Err(error) => {
            journal.operations[index].phase = AuthPhase::RemoteUnknown;
            save_journal(&mut journal)?;
            return Err(error.context("registration outcome unknown; query the original operation, do not resend"));
        }
    };
    journal.operations[index].account.as_mut().context("account receipt is missing")?.authorization = Some(authorized.clone());
    journal.operations[index].phase = AuthPhase::Authorized;
    save_journal(&mut journal)?;
    let user = authorized["user"].as_str().filter(|user| !user.is_empty()).context("registration authorization is missing its user")?;
    let token = authorized["token"].as_str().filter(|token| !token.is_empty()).context("registration authorization is missing its token")?;
    let directory = if use_source { ownership.profile.clone() } else { login_directory(addr, user)? };
    let original = read_original(&directory)?;
    let mut session: Value = match original.as_deref() { Some(bytes) => serde_json::from_slice(bytes)?, None => json!({}) };
    ensure!(session.is_object() && ["super", "pass", "secret", "secret_key"].iter().all(|field| session.get(*field).is_none()),
        "host must normalize registration target credentials before admission");
    ensure!(session["user"].as_str().is_none_or(|owner| owner.is_empty() || owner == user),
        "registration destination belongs to another account");
    let database = crate::service::database_path(&directory)?;
    if database.is_file() {
        let database = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let has_memories: bool = database.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')", [], |row| row.get(0))?;
        if has_memories {
            let encrypted: i64 = database.query_row("SELECT count(*) FROM memories WHERE ciphertext <> ''", [], |row| row.get(0))?;
            let same_wrap = session["kdf_salt"] == material.kdf_salt && session["wrapped_urk"] == material.wrapped_urk
                && session["urk_nonce"] == material.urk_nonce;
            ensure!(encrypted == 0 || same_wrap || library_accepts_keys(&directory, &keys)?,
                "host registration vault does not unlock the destination library");
        }
    }
    session["user"] = json!(user);
    session["addr"] = json!(addr);
    session["token"] = json!(token);
    session["session_id"] = authorized["session_id"].clone();
    session["vault_version"] = json!(4);
    session["kdf_salt"] = json!(material.kdf_salt);
    session["wrapped_urk"] = json!(material.wrapped_urk);
    session["urk_nonce"] = json!(material.urk_nonce);
    session["keyring_account"] = json!(material.keyring_account);
    session["keyring_backend"] = json!(material.keyring_backend);
    ensure!(read_original(&ownership.profile)? == source, "source profile changed before registration commit");
    let active = journal.active.as_mut().context("account recovery is missing")?;
    active.directory = directory.clone();
    active.original = original.clone();
    active.directory_existed = directory.exists();
    active.config = match std::fs::read(crate::service::client_config_path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    active.owns_config = true;
    journal.operations[index].account.as_mut().context("account receipt is missing")?.target = Some(directory.clone());
    checkpoint_account_candidate(&mut journal, index, &session)?;
    save_journal(&mut journal)?;
    let target = if directory.exists() { directory.canonicalize()? } else { directory.clone() };
    let _target_lock = if target == ownership.root || target == ownership.profile { None }
        else { Some(crate::lock::LibraryLock::acquire(&target, std::time::Duration::from_secs(1))?) };
    ensure!(read_original(&directory)? == original, "registration target changed while acquiring ownership");
    journal.operations[index].phase = AuthPhase::Prepared;
    save_journal(&mut journal)?;
    if !directory.exists() {
        std::fs::create_dir_all(directory.parent().context("account directory has no parent")?)?;
        std::fs::create_dir(&directory)?;
    }
    private_write(&directory.join("session.json"), &serde_json::to_vec_pretty(&session)?)?;
    crate::service::set_data_dir(&directory.to_string_lossy())?;
    journal.operations[index].phase = AuthPhase::AwaitingVerification;
    save_journal(&mut journal)?;
    account_result(&journal.operations[index])
}

pub fn control_login(ownership: &AuthOwnership, operation_id: &str, key: &str,
    params: &HostAuthLoginParams) -> Result<respire_protocol::host_control::HostAuthLoginReply> {
    login::control_login(ownership, operation_id, key, params)
}

/// Wrap only private login responses; the host consumes any custody handoff.
pub fn login_reply(receipt: HostAuthAccountResult) -> Result<respire_protocol::host_control::HostAuthLoginReply> {
    let journal = load_journal()?;
    let record = journal.operations.iter().find(|record| record.operation_id == receipt.operation_id)
        .context("login reply operation missing")?;
    let mut reply = login::candidate_reply(record)?;
    reply.receipt = receipt;
    Ok(reply)
}

/// Result recovery never authenticates again and never resends a registration.
pub fn control_account_result(ownership: &AuthOwnership, identity: &HostAuthIdentity,
    operation_id: &str, key: &str,
    candidate: Option<&respire_protocol::host_control::HostAuthSessionMaterial>) -> Result<HostAuthAccountResult> {
    if is_registration_receipt(&ownership.root, &ownership.profile, operation_id, key)? {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    } else {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    }
    let mut journal = load_journal()?;
    let index = owned_account_index(&journal, &ownership.root, &ownership.profile, operation_id, key)?;
    let record = &journal.operations[index];
    if record.phase == AuthPhase::Prepared {
        let target = record.account.as_ref().and_then(|account| account.target.as_ref()).context("account target is missing")?;
        let active = journal.active.as_ref().context("account recovery is missing")?;
        ensure!(active.operation_id.as_deref() == Some(operation_id) && active.directory == *target
            && active.alias == record.alias && active.owns_config, "account recovery binding mismatch");
        let lock_target = if target.exists() { target.canonicalize()? } else { target.clone() };
        let _target_lock = if lock_target == ownership.root || lock_target == ownership.profile { None }
            else { Some(crate::lock::LibraryLock::acquire(target, std::time::Duration::from_secs(1))?) };
        let current = read_original(target)?;
        ensure!(current == record.candidate || current == active.original, "interrupted account session changed");
        verify_account_material(record, false)?;
        if record.account.as_ref().is_some_and(|account| !account.register) {
            account_material_keys(record, false, candidate)?;
        }
        verify_account_config(active)?;
        if current != record.candidate {
            private_write(&target.join("session.json"), record.candidate.as_deref().context("account candidate is missing")?)?;
        }
        crate::service::set_data_dir(&target.to_string_lossy())?;
        journal.operations[index].phase = AuthPhase::AwaitingVerification;
        save_journal(&mut journal)?;
    }
    if journal.operations[index].phase == AuthPhase::AwaitingVerification || journal.operations[index].phase == AuthPhase::Committed {
        verify_account_candidate(&journal.operations[index])?;
        if journal.operations[index].account.as_ref().is_some_and(|account| !account.register) {
            account_material_keys(&journal.operations[index], true, candidate)?;
        }
    }
    let account = journal.operations[index].account.as_ref().context("account receipt is missing")?;
    if journal.operations[index].phase == AuthPhase::AwaitingVerification
        && account.register && account.vault_pending && !account.published_vault {
        let authorized = account.authorization.as_ref().context("registration authorization is missing")?;
        let vault = account.vault.as_ref().filter(|vault| vault.is_object())
            .context("registration vault is missing")?;
        // Observe the original publication only. Failed reads and mismatches retain Unknown.
        if PreparedLogin::fetch_vault(&account.addr, authorized).is_ok_and(|current| current == *vault) {
            journal.operations[index].account.as_mut().context("account receipt is missing")?.published_vault = true;
            save_journal(&mut journal)?;
        }
    }
    login::resume_import(ownership, &mut journal, index, candidate)?;
    login::reconcile_publication(&mut journal, index)?;
    account_result(&journal.operations[index])
}

/// Classify only an authenticated, receipt-owned account operation without loading factors.
pub fn is_registration_receipt(root: &Path, profile: &Path, id: &str, key: &str) -> Result<bool> {
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    ensure!(root == crate::service::main_data_dir().canonicalize()?
        && profile == crate::service::data_dir().canonicalize()?, "account receipt selection changed");
    let journal = load_journal_metadata()?;
    let index = owned_account_index(&journal, &root, &profile, id, key)?;
    Ok(journal.operations[index].account.as_ref().context("account receipt is missing")?.register)
}

