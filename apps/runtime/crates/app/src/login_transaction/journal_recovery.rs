
fn recovery_path() -> PathBuf {
    crate::service::client_config_path().with_file_name(".login-recovery.json")
}

fn auth_journal_lock() -> Result<std::fs::File> {
    auth_journal_lock_with_wait(std::time::Duration::from_secs(1))
}

fn auth_journal_lock_with_wait(wait: std::time::Duration) -> Result<std::fs::File> {
    let path = recovery_path().with_extension("json.lock");
    std::fs::create_dir_all(path.parent().context("auth recovery root is missing")?)?;
    ensure!(!std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "auth recovery lock must not be a symbolic link");
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                ensure!(std::time::Instant::now() < deadline, "auth recovery journal is busy");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.file_name().is_some_and(|name| name == "session.json") {
        let session: Value = serde_json::from_slice(bytes).context("session write must contain valid JSON")?;
        ensure!(["pass", "super", "secret", "secret_key"].iter().all(|field| session.get(*field).is_none()),
            "plaintext factors cannot enter a session write");
    }
    let temporary = path.with_file_name(format!(".login-write-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        respire_spawn::durable::replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() { std::fs::remove_file(&temporary)?; }
    result
}

fn restore(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => private_write(path, bytes),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

pub fn recovery_pending() -> bool {
    match load_journal_metadata() { Ok(journal) => journal.active.is_some(), Err(_) => true }
}

pub fn recovery_state() -> Result<respire_protocol::host_control::HostLoginRecoveryState> {
    use respire_protocol::host_control::HostLoginRecoveryState;
    let journal = load_journal_metadata()?;
    Ok(match journal.active.as_ref() {
        None => HostLoginRecoveryState::Clear,
        Some(active) if active.operation_id.is_none() => HostLoginRecoveryState::LegacyPending,
        Some(_) => HostLoginRecoveryState::OperationPending,
    })
}

/// Public fields from the selected local session and the durable transaction journal.
pub fn host_account_status(profile: &Path) -> Result<Value> {
    let session: Option<Value> = read_original(profile)?.as_deref().map(serde_json::from_slice).transpose()?;
    let journal = load_journal_metadata()?;
    let state = match journal.active.as_ref() {
        None => "none",
        Some(active) => {
            let login_pending = active.operation_id.as_ref().is_some_and(|id| {
                journal.operations.iter().any(|record| {
                    &record.operation_id == id
                        && record.account.as_ref().is_some_and(|account| !account.register)
                        && matches!(record.phase, AuthPhase::LoginInput | AuthPhase::RemotePending
                            | AuthPhase::Authorized | AuthPhase::Prepared | AuthPhase::AwaitingVerification)
                })
            });
            if login_pending { "login_pending" } else { "recovery_pending" }
        }
    };
    let account = session.as_ref().and_then(|session| session["user"].as_str()).filter(|user| !user.trim().is_empty());
    let signed_in = account.is_some() && session.as_ref().is_some_and(|session| {
        session["token"].as_str().is_some_and(|token| !token.trim().is_empty())
    });
    Ok(json!({"signed_in": signed_in, "account_id": account, "transaction_state": state}))
}

pub fn auth_recovery_pending() -> Result<bool> {
    let journal = load_journal_metadata()?;
    let Some(active) = journal.active.as_ref() else { return Ok(false); };
    let Some(id) = active.operation_id.as_deref() else { return Ok(false); };
    let record = journal.operations.iter().find(|record| record.operation_id == id)
        .context("active auth recovery has no admission receipt")?;
    if let Some(account) = &record.account {
        if record.phase == AuthPhase::AwaitingVerification {
            ensure!(record.root == crate::service::main_data_dir().canonicalize()?
                && account.target.as_deref() == Some(active.directory.as_path())
                && active.directory.canonicalize()? == crate::service::data_dir().canonicalize()?
                && active.alias == record.alias, "runtime verification does not own the active account candidate");
            // Serving starts locked; the host supplies keys before store verification and commit.
            account_candidate_session(record, true)?;
            return Ok(false);
        }
    }
    if record.key_operation.is_some() && record.phase == AuthPhase::AwaitingVerification {
        keys::verify_boot_candidate(record, active)?;
        return Ok(false);
    }
    Ok(true)
}

/// Called by the host under its takeover lock, after stopping the interrupted runtime.
/// Roll back local state only. A published cloud v4 wrap is resumed by the existing
/// migration path, which proves that the old and new wraps contain the same URK.
pub fn recover_interrupted() -> Result<()> {
    crate::service::require_profile_change_host()?;
    let _journal_lock = auth_journal_lock()?;
    let mut journal = load_journal()?;
    let Some(record) = &journal.active else { return Ok(()); };
    if record.retired { return retire_active(&mut journal); }
    if record.operation_id.is_some() {
        ensure!(!journal.operations.iter().any(|operation|
            Some(operation.operation_id.as_str()) == record.operation_id.as_deref()
                && (operation.account.is_some() || operation.identity.is_some() || operation.key_operation.is_some() || operation.space_operation.is_some() || operation.profile_remove.is_some())),
            "account operation requires its original receipt; remote effects must not be automatically retried or rolled back");
        let root = crate::service::main_data_dir().canonicalize()?;
        let profile = crate::service::data_dir().canonicalize()?;
        return recover_keygen(&mut journal, &root, &profile, None);
    }
    rollback_record(&mut journal)?;
    retire_active(&mut journal)
}

fn rollback_record(journal: &mut Journal) -> Result<()> {
    rollback_record_inner(journal, true)
}

fn quarantine_new_profile(record: &Recovery) -> Result<()> {
    if !record.directory_existed && record.directory.exists() {
        let parent = record.directory.parent().context("recovery directory has no parent")?;
        std::fs::rename(&record.directory, parent.join(format!(".failed-{}", record.alias)))?;
    }
    Ok(())
}

fn rollback_record_inner(journal: &mut Journal, quarantine: bool) -> Result<()> {
    let record = journal.active.as_ref().context("rollback recovery is missing")?.clone();
    ensure!(!record.retired, "retired recovery cannot restore an old session");
    ensure!(record.backends.is_empty() && record.pass_backends.is_empty()
        && record.totp_backends.is_empty() && record.legacy_secret_backends.is_empty(),
        "host_recovery_required: historical credential cleanup metadata retained");
    ensure!(record.alias.starts_with("login-") && record.alias.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'), "invalid recovery credential alias");
    ensure!(record.directory.exists() || record.original.is_none(), "original account directory is missing; retain the recovery record");
    // Cancelling an untouched source needs neither its missing keys nor a session rewrite.
    let unchanged = read_original(&record.directory)? == record.original;
    let restored = if unchanged { record.original.clone() }
        else { recovery_credentials::rollback_snapshot(journal, &record)? };
    if restored != record.original {
        journal.active.as_mut().context("rollback recovery is missing")?.restored = restored.clone();
        if let Some(operation_id) = record.operation_id.as_deref() {
            let operation = journal.operations.iter_mut().find(|operation| operation.operation_id == operation_id)
                .context("rollback operation is missing")?;
            operation.restored = restored.as_ref().map(|bytes| RestoredSession { profile: record.directory.clone(), sha256: digest(bytes) });
        }
        save_journal(journal)?;
    }
    if !unchanged && record.directory.exists() {
        read_original(&record.directory)?;
        restore(&record.directory.join("session.json"), restored.as_deref())?;
    }
    if record.operation_id.is_none() || record.owns_config {
        restore(&crate::service::client_config_path(), record.config.as_deref())?;
    }
    if quarantine { quarantine_new_profile(&record)?; }
    Ok(())
}

/// Read the selected source as one committed snapshot, without creating plaintext files.
/// The domain reader resolves both factors through the private host bridge.
pub fn local_snapshot(source_secret: Option<&str>) -> Result<String> {
    crate::service::local_snapshot(source_secret)
}

#[derive(Debug)]
struct LocalKeyMaterialRejected {
    directory: PathBuf,
    original_sha256: String,
}

impl std::fmt::Display for LocalKeyMaterialRejected {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("local library uses different key material; original data was preserved")
    }
}

impl std::error::Error for LocalKeyMaterialRejected {}

// A sessionless library can identify itself only by decrypting with the authorized data key.
fn library_accepts_keys(directory: &Path, keys: &crate::memory::SessionKeys) -> Result<bool> {
    let path = crate::service::database_path(directory)?;
    if !path.is_file() { return Ok(false); }
    let database = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_memories: bool = database.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')", [], |row| row.get(0))?;
    if !has_memories { return Ok(false); }
    let mut statement = database.prepare("SELECT ciphertext,nonce FROM memories WHERE ciphertext <> '' AND COALESCE(deleted,0)=0")?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let mut stored = crate::memory::model::StoredMemory::new_pending(String::new(), String::new());
        stored.ciphertext = row.get(0)?;
        stored.nonce = row.get(1)?;
        if crate::memory::MemoryEngine::open(keys, &stored).is_err() { return Ok(false); }
        count += 1;
    }
    Ok(count > 0)
}

/// A remote identity includes its server; filesystem case folding is not account identity.
pub fn login_directory(addr: &str, user: &str) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let base = addr.trim().trim_end_matches('/');
    let matches = |directory: &Path| -> Result<bool> {
        let Some(bytes) = read_original(directory)? else { return Ok(false); };
        let session: Value = serde_json::from_slice(&bytes).context("account session is invalid")?;
        let stored_addr = session["addr"].as_str().unwrap_or("").trim().trim_end_matches('/');
        // Older recovery-only profiles carry v1/v3 factors before the first login.
        let recovery_only = stored_addr.is_empty() && !crate::service::database_path(directory)?.is_file();
        Ok(session["user"].as_str() == Some(user) && (stored_addr == base || recovery_only))
    };
    for directory in [crate::service::data_dir(), crate::service::main_data_dir()] {
        if matches(&directory)? { return Ok(directory); }
    }
    let accounts = crate::service::accounts_root();
    if accounts.is_dir() {
        for item in std::fs::read_dir(&accounts)? {
            let item = item?;
            if item.file_type()?.is_dir() && matches(&item.path())? { return Ok(item.path()); }
        }
    }
    let plain = crate::service::account_dir(user)?;
    if !plain.exists() { return Ok(plain); }
    let suffix = hex::encode(Sha256::digest(format!("{base}\n{user}").as_bytes()));
    let isolated = crate::service::account_dir(&format!("{user}-{}", &suffix[..16]))?;
    ensure!(!isolated.exists() || matches(&isolated)? || read_original(&isolated)?.is_none(),
        "isolated account destination is occupied; original libraries were preserved");
    Ok(isolated)
}

pub struct PreparedLogin {
    pub user: String,
    pub directory: PathBuf,
    original: Option<Vec<u8>>,
    session: Value,
    super_password: String,
    login_password: Option<String>,
    legacy_secret: Option<String>,
    alias: String,
    recovery_written: bool,
    migration_cloud: Option<Value>,
    migration_auth: Option<Value>,
}

fn read_original(directory: &Path) -> Result<Option<Vec<u8>>> {
    let path = directory.join("session.json");
    if std::fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_symlink())
        || std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!("login destination must not be a symbolic link");
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

