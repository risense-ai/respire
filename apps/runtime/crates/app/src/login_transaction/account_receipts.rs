fn owned_account_index(journal: &Journal, root: &Path, profile: &Path, id: &str, key: &str) -> Result<usize> {
    canonical_operation_id(id)?;
    let index = journal.operations.iter().position(|record| record.operation_id == id && record.idempotency_key == key)
        .context("account admission receipt is missing")?;
    let record = &journal.operations[index];
    let account = record.account.as_ref().context("operation is not account authentication")?;
    ensure!(record.root == root, "account receipt owner mismatch");
    let current = read_original(profile)?;
    let owns_source = record.profile == profile && (current.as_deref().map(digest) == record.original_sha256
        || recovery_credentials::matches_restored(record, profile, &current));
    // An untouched source may query a candidate whose directory does not exist yet.
    let owns_target = if owns_source { false } else {
        account.target.as_ref().map(|target| target.canonicalize()).transpose()?.as_deref() == Some(profile)
    };
    ensure!(record.profile == profile || owns_target, "account receipt owner mismatch");
    ensure!(owns_source || (owns_target && current.is_some()
        && (current == record.candidate || recovery_credentials::matches_restored(record, profile, &current))),
        "account receipt no longer owns the selected session");
    Ok(index)
}

fn verify_account_candidate(record: &AuthRecord) -> Result<()> {
    verify_account_material(record, true)
}

fn verify_account_material(record: &AuthRecord, require_published: bool) -> Result<()> {
    account_candidate_session(record, require_published).map(|_| ())
}

fn account_material_keys(record: &AuthRecord, require_published: bool,
    candidate: Option<&respire_protocol::host_control::HostAuthSessionMaterial>) -> Result<crate::memory::SessionKeys> {
    account_candidate_session(record, require_published)?;
    login::supplied_keys(record.candidate.as_deref().context("account candidate missing")?, candidate)
}

fn account_candidate_session(record: &AuthRecord, require_published: bool) -> Result<Value> {
    let account = record.account.as_ref().context("account receipt is missing")?;
    let target = account.target.as_ref().context("account target is missing")?;
    let bytes = record.candidate.as_deref().context("account candidate is missing")?;
    ensure!(record.candidate_sha256.as_deref() == Some(digest(bytes).as_str())
        && (!require_published || read_original(target)?.as_deref() == Some(bytes)), "account candidate changed");
    let session: Value = serde_json::from_slice(bytes)?;
    ensure!(session["user"].as_str() == Some(account.user.as_str())
        && session["addr"].as_str() == Some(account.addr.as_str())
        && session["keyring_account"].as_str() == Some(record.alias.as_str()), "account candidate identity mismatch");
    require_persistent_auth_backend(record.backend.as_deref().context("account credential checkpoint is missing")?)?;
    Ok(session)
}

fn session_material_keys(session: &Value, alias: &str, backend: &str) -> Result<crate::memory::SessionKeys> {
    require_persistent_auth_backend(backend)?;
    let secret = if session["vault_version"] == 1 {
        crate::keystore::read_imported_v1_password(alias, backend)?
    } else { crate::keystore::read_imported(alias, "super", backend)? };
    let salt = session["kdf_salt"].as_str().context("account salt is missing")?;
    let wrapped = session["wrapped_urk"].as_str().context("account wrap is missing")?;
    let nonce = session["urk_nonce"].as_str().context("account nonce is missing")?;
    let legacy_secret = session_legacy_secret(session, alias)?;
    let keys = match session["vault_version"].as_i64() {
        Some(1) => crate::memory::SessionKeys::unlock(&secret, legacy_secret.as_deref().context("original account secret is missing")?, salt, wrapped, nonce)?,
        Some(2) => crate::memory::SessionKeys::unlock_super(&secret, salt, wrapped, nonce)?,
        Some(3) => crate::memory::SessionKeys::unlock_vault(&secret, legacy_secret.as_deref().context("original secret key is missing")?, salt, wrapped, nonce)?,
        Some(4) => crate::memory::SessionKeys::unlock_v4(&secret, salt, wrapped, nonce)?,
        _ => anyhow::bail!("unsupported account vault"),
    };
    if session["crypto_namespace"].as_str() == Some(crate::memory::crypto::RSRS_PREFIX) {
        crate::memory::SessionKeys::from_urk(keys.urk)
    } else { Ok(keys) }
}

fn session_legacy_secret(session: &Value, alias: &str) -> Result<Option<String>> {
    let field = match session["vault_version"].as_i64().unwrap_or(1) { 1 => "secret", 3 => "secret_key", _ => return Ok(None) };
    match session.get("legacy_secret_backend") {
        Some(value) => {
            let backend = value.as_str().context("invalid legacy secret backend")?;
            require_persistent_auth_backend(backend)?;
            Ok(Some(crate::keystore::read_imported(alias, "legacy-secret", backend)?))
        }
        None => {
            // Historical whole-stack sessions predate host factor references.
            Ok(session[field].as_str().filter(|value| !value.is_empty()).map(str::to_owned))
        }
    }
}

fn checkpoint_legacy_secret(journal: &mut Journal, session: &mut Value, alias: &str, secret: Option<&str>) -> Result<()> {
    if !matches!(session["vault_version"].as_i64(), Some(1 | 3)) {
        session.as_object_mut().context("session must be an object")?.remove("legacy_secret_backend");
        return Ok(());
    }
    let secret = secret.context("original legacy secret is required")?;
    let backend = crate::keystore::import_credential_checkpoint(alias, "legacy-secret", secret, |backend| {
        require_persistent_auth_backend(backend)?;
        let active = journal.active.as_mut().context("legacy secret recovery is missing")?;
        ensure!(active.alias == alias, "legacy secret checkpoint owner mismatch");
        if !active.legacy_secret_backends.iter().any(|existing| existing == backend) {
            active.legacy_secret_backends.push(backend.to_owned());
        }
        save_journal(journal)
    })?;
    require_persistent_auth_backend(backend)?;
    session["legacy_secret_backend"] = json!(backend);
    Ok(())
}

fn verify_account_config(active: &Recovery) -> Result<()> {
    let current = match std::fs::read(crate::service::client_config_path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if current != active.config {
        let mut expected: Value = match &active.config { Some(bytes) => serde_json::from_slice(bytes)?, None => json!({}) };
        expected["data_dir"] = json!(active.directory.to_string_lossy());
        ensure!(current.as_ref().map(|bytes| serde_json::from_slice::<Value>(bytes)).transpose()? == Some(expected),
            "client configuration changed outside the account transaction; retain recovery");
    }
    Ok(())
}

/// Completion owns the journal, not the running store's library lock. The callback
/// must independently verify the live target Runtime; a client boolean is not proof.
pub fn control_account_complete(root: &Path, profile: &Path, identity: &HostAuthIdentity,
    operation_id: &str, key: &str,
    candidate: Option<&respire_protocol::host_control::HostAuthSessionMaterial>, migration_password: Option<&str>,
    mut verify_runtime: impl FnMut(&HostAuthAccountResult) -> Result<()>) -> Result<HostAuthAccountResult> {
    crate::service::require_profile_change_host()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    ensure!(root == crate::service::main_data_dir().canonicalize()?
        && profile == crate::service::data_dir().canonicalize()?, "completion selection changed");
    let _journal_lock = auth_journal_lock()?;
    if is_registration_receipt(&root, &profile, operation_id, key)? {
        verify_locked_auth_identity(&root, &profile, identity)?;
    } else {
        verify_locked_auth_identity(&root, &profile, identity)?;
    }
    let mut journal = load_journal()?;
    let index = owned_account_index(&journal, &root, &profile, operation_id, key)?;
    let record = &journal.operations[index];
    verify_account_candidate(record)?;
    if record.account.as_ref().is_some_and(|account| !account.register) {
        account_material_keys(record, true, candidate)?;
    }
    let target = record.account.as_ref().and_then(|account| account.target.as_ref())
        .context("account target is missing")?.canonicalize()?;
    ensure!(target == profile, "completion requires the receipt's selected target");
    if record.phase == AuthPhase::Committed { return account_result(record); }
    ensure!(record.phase == AuthPhase::AwaitingVerification, "account is not ready for runtime verification");
    ensure!(journal.active.as_ref().and_then(|active| active.operation_id.as_deref()) == Some(operation_id),
        "completion does not own active recovery");
    verify_runtime(&account_result(record)?)?;
    login::finish_migration(&mut journal, index, migration_password)?;
    if journal.operations[index].account.as_ref().and_then(|account| account.login.as_ref())
        .is_some_and(|login| login.pending()) {
        return account_result(&journal.operations[index]);
    }
    let record = &journal.operations[index];
    let account = record.account.as_ref().context("account receipt is missing")?;
    if account.register && !account.published_vault {
        let authorized = account.authorization.clone().context("registration authorization is missing")?;
        let addr = account.addr.clone();
        let vault = account.vault.clone().context("registration vault is missing")?;
        let pending = account.vault_pending;
        let current = PreparedLogin::fetch_vault(&addr, &authorized)?;
        if current != vault {
            ensure!(current.is_null(), "server vault differs; preserve local credentials and receipt");
            if pending { return account_result(&journal.operations[index]); }
            journal.operations[index].account.as_mut().context("account receipt is missing")?.vault_pending = true;
            save_journal(&mut journal)?;
            let token = authorized["token"].as_str().context("registration token is missing")?;
            let sent = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
                .post(&format!("{addr}/api/self/vault"))
                .set("Authorization", &format!("Bearer {token}")).set("If-None-Match", "*").send_json(vault.clone());
            if sent.is_err() && PreparedLogin::fetch_vault(&addr, &authorized)? != vault {
                return account_result(&journal.operations[index]);
            }
        }
        journal.operations[index].account.as_mut().context("account receipt is missing")?.published_vault = true;
        save_journal(&mut journal)?;
    }
    verify_account_candidate(&journal.operations[index])?;
    verify_runtime(&account_result(&journal.operations[index])?)?;
    journal.operations[index].phase = AuthPhase::Committed;
    retire_active(&mut journal)?;
    account_result(&journal.operations[index])
}

pub fn control_account_rollback(mut ownership: AuthOwnership, identity: &HostAuthIdentity,
    operation_id: &str, key: &str) -> Result<HostAuthAccountResult> {
    crate::service::require_profile_change_host()?;
    if is_registration_receipt(&ownership.root, &ownership.profile, operation_id, key)? {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    } else {
        verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    }
    let mut journal = load_journal()?;
    let index = owned_account_index(&journal, &ownership.root, &ownership.profile, operation_id, key)?;
    let record = &journal.operations[index];
    if record.phase == AuthPhase::RolledBack { return account_result(record); }
    let active = journal.active.as_ref().context("account recovery is missing")?;
    let target = if active.directory.exists() { Some(active.directory.canonicalize()?) } else { None };
    let target_lock = if target.as_ref().is_none_or(|target| *target == ownership.root || *target == ownership.profile) { None }
        else { Some(crate::lock::LibraryLock::acquire(&active.directory, std::time::Duration::from_secs(1))?) };
    rollback_account_material(&mut journal, index)?;
    // Windows cannot quarantine a directory while SQLite still owns its lock file.
    // The root/source guards and the durable RollingBack record remain held.
    drop(target_lock);
    if journal.active.as_ref().is_some_and(|active| !active.directory_existed && target.as_deref() == Some(ownership.profile.as_path())) {
        drop(ownership._profile_lock.take());
    }
    finish_account_rollback(&mut journal, index)?;
    account_result(&journal.operations[index])
}

fn rollback_account_material(journal: &mut Journal, index: usize) -> Result<()> {
    let record = &journal.operations[index];
    ensure!(record.phase != AuthPhase::Committed && record.phase != AuthPhase::RemotePending
        && record.phase != AuthPhase::RemoteUnknown, "remote outcome must be resolved before compensation");
    let account = record.account.as_ref().context("account receipt is missing")?;
    ensure!(!account.login.as_ref().is_some_and(|login| login.publication_started()),
        "legacy publication is pending or complete; retain the original account receipt");
    ensure!(!account.vault_pending && !account.published_vault, "published or unknown remote vault cannot be compensated by deleting local keys");
    let active = journal.active.as_ref().context("account recovery is missing")?;
    ensure!(active.operation_id.as_deref() == Some(record.operation_id.as_str()), "rollback does not own recovery");
    let current = read_original(&active.directory)?;
    ensure!(recovery_credentials::matches_original(active, &current) || current == record.candidate,
        "target session changed outside the transaction");
    if active.owns_config {
        verify_account_config(active)?;
    }
    journal.operations[index].phase = AuthPhase::RollingBack;
    save_journal(journal)?;
    rollback_record_inner(journal, false)?;
    Ok(())
}

fn finish_account_rollback(journal: &mut Journal, index: usize) -> Result<()> {
    quarantine_new_profile(journal.active.as_ref().context("account recovery is missing")?)?;
    journal.operations[index].phase = AuthPhase::RolledBack;
    retire_active(journal)
}

fn rollback_import_before_copy(ownership: &AuthOwnership, journal: &mut Journal, index: usize,
    target_lock: Option<crate::lock::LibraryLock>, original_material: Option<&str>,
    candidate: Option<&respire_protocol::host_control::HostAuthSessionMaterial>) -> Result<()> {
    let record = &journal.operations[index];
    let account = record.account.as_ref().context("import account missing")?;
    let login = account.login.as_ref().context("import progress missing")?;
    let active = journal.active.as_ref().context("import recovery missing")?;
    let target_identity = active.directory.canonicalize()?;
    ensure!(record.phase == AuthPhase::AwaitingVerification && !account.register
        && login.can_compensate_import()
        && record.root == ownership.root && record.profile == ownership.profile
        && active.owns_config
        && (!active.directory_existed || original_material.is_some())
        && active.alias == record.alias
        && account.target.as_ref() == Some(&active.directory)
        && target_identity != ownership.root && target_identity != ownership.profile,
        "import cannot prove an unpublished owned target; retain the original operation");
    ensure!(crate::service::main_data_dir().canonicalize()? == ownership.root
        && crate::service::data_dir().canonicalize()? == target_identity
        && read_original(&ownership.profile)?.as_deref().map(digest) == record.original_sha256,
        "import source or selection changed before compensation");
    verify_locked_auth_identity(&ownership.root, &ownership.profile, &account.source_identity)?;
    ensure!(target_lock.as_ref().context("import target ownership missing")?.protects(&active.directory)?,
        "import target ownership changed before compensation");
    ensure!(original_import_material(record, active)?.as_deref() == original_material,
        "original target credentials changed before compensation");
    let candidate_keys = account_material_keys(record, true, candidate)?;
    ensure!(original_material.is_none_or(|original| original == digest(&candidate_keys.urk)),
        "original and candidate target keys differ; retain the candidate and original operation");
    rollback_account_material(journal, index)?;
    let record = &journal.operations[index];
    let active = journal.active.as_ref().context("import recovery missing")?;
    ensure!(recovery_credentials::matches_original(active, &read_original(&active.directory)?)
        && original_import_material(record, active)?.as_deref() == original_material
        && crate::service::data_dir().canonicalize()? == ownership.profile,
        "import compensation could not verify restored target or source selection");
    verify_locked_auth_identity(&ownership.root, &ownership.profile,
        &record.account.as_ref().context("import account missing")?.source_identity)?;
    drop(target_lock);
    finish_account_rollback(journal, index)
}

fn original_import_material(record: &AuthRecord, active: &Recovery) -> Result<Option<String>> {
    let Some(bytes) = active.original.as_deref() else { return Ok(None); };
    let session: Value = serde_json::from_slice(bytes)?;
    let account = record.account.as_ref().context("import account missing")?;
    let alias = session["keyring_account"].as_str().context("original target credential alias missing")?;
    let backend = session["keyring_backend"].as_str().context("original target credential backend missing")?;
    ensure!(alias != record.alias && session["user"].as_str() == Some(account.user.as_str())
        && session["addr"].as_str() == Some(account.addr.as_str()), "original target credential identity mismatch");
    require_persistent_auth_backend(backend)?;
    ensure!(["pass", "super", "secret", "secret_key"].iter().all(|field| session.get(*field).is_none()),
        "host must normalize original import credentials");
    let original = account.login.as_ref().and_then(|login| login.original_target_key.as_ref())
        .context("explicit original target key proof missing")?;
    ensure!(original.0 == digest(bytes), "original target session changed after host validation");
    Ok(Some(original.1.clone()))
}

