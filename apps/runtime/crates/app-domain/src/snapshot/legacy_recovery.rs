#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRecoveryDescription {
    pub journal_sha256: String,
    pub phase: String,
    pub account_fingerprint: String,
}

/// Describe existing legacy bytes only; this does not authorize recovery or open the database.
pub fn describe_legacy_recovery(root: &Path, account: &str) -> Result<LegacyRecoveryDescription> {
    let recovery = read_record(root, RECOVERY_FILE)?;
    let maintenance = read_record(root, MAINTENANCE_FILE)?;
    let account_fingerprint = format!("{:x}", Sha256::digest(account.as_bytes()));
    for (journal, _) in recovery.iter().chain(maintenance.iter()) {
        anyhow::ensure!(
            journal.receipt.operation.is_none() && journal.history.is_empty(),
            "snapshot has an operation identity; legacy recovery is forbidden"
        );
        anyhow::ensure!(
            journal.receipt.account_fingerprint == account_fingerprint,
            "snapshot recovery belongs to another account"
        );
    }
    if let (Some((recovery, _)), Some((maintenance, _))) = (&recovery, &maintenance) {
        anyhow::ensure!(
            maintenance.phase == "prepared" && recovery.receipt == maintenance.receipt,
            "legacy snapshot recovery and maintenance journals disagree"
        );
    }
    let (selected, bytes) = recovery
        .as_ref()
        .or(maintenance.as_ref())
        .context("no interrupted legacy snapshot operation exists")?;
    let description = LegacyRecoveryDescription {
        journal_sha256: format!("{:x}", Sha256::digest(bytes)),
        phase: selected.phase.clone(),
        account_fingerprint,
    };
    resolve_journal(
        recovery.map(|(journal, _)| journal),
        maintenance.map(|(journal, _)| journal),
    )?
    .context("legacy snapshot journal disappeared")?;
    Ok(description)
}

#[derive(Debug, Clone)]
pub struct LegacyRecoveryStatus {
    pub phase: String,
    pub cleanup_pending: bool,
    pub cleanup_warning: Option<String>,
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Query only the existing journal/history; absence is never evidence of recovery.
pub fn legacy_recovery_status(
    root: &Path,
    account: &str,
    operation: &SnapshotOperation,
    journal_sha256: &str,
) -> Result<Option<LegacyRecoveryStatus>> {
    let Some(journal) = read_journal(root)? else { return Ok(None); };
    let digest = if let Some(entry) = journal.history.iter()
        .find(|entry| entry.operation.operation_id == operation.operation_id) {
        entry.legacy_journal_sha256.as_deref()
    } else if journal.receipt.operation.as_ref()
        .is_some_and(|recorded| recorded.operation_id == operation.operation_id) {
        journal.receipt.legacy_journal_sha256.as_deref()
    } else { return Ok(None); };
    anyhow::ensure!(valid_digest(journal_sha256) && digest == Some(journal_sha256), OperationConflict);
    let status = operation_status(root, account, operation)?
        .context("legacy recovery journal changed during query")?;
    let cleanup_warning = (terminal_phase(&status.phase) && status.cleanup_pending)
        .then(|| "legacy snapshot recovery completed; local cleanup is pending".to_owned());
    Ok(Some(LegacyRecoveryStatus {
        phase: status.phase,
        cleanup_pending: status.cleanup_pending,
        cleanup_warning,
    }))
}

/// Persist the recovery identity before one recovery attempt; retries only query it.
pub fn recover_legacy_locked(
    root: &Path,
    account: &str,
    expected: &LegacyRecoveryDescription,
    operation: &SnapshotOperation,
    ownership: &crate::lock::LibraryLock,
) -> Result<LegacyRecoveryStatus> {
    anyhow::ensure!(ownership.protects(root)?, "legacy recovery requires this library's ownership lock");
    if let Some(status) = legacy_recovery_status(root, account, operation, &expected.journal_sha256)? {
        return Ok(status);
    }
    anyhow::ensure!(uuid::Uuid::parse_str(&operation.operation_id).is_ok_and(|id|
        id.get_version_num() == 4 && id.to_string() == operation.operation_id)
        && valid_digest(&operation.request_fingerprint), "invalid legacy recovery operation identity");
    anyhow::ensure!(
        describe_legacy_recovery(root, account)? == *expected,
        "legacy snapshot description changed before recovery"
    );
    let journal = read_journal(root)?.context("legacy snapshot journal disappeared before binding")?;
    let phase = journal.phase;
    let mut receipt: RestoreReceipt = journal.receipt.into();
    receipt.operation = Some(operation.clone());
    receipt.legacy_journal_sha256 = Some(expected.journal_sha256.clone());
    write_journal(root, &receipt, &phase)?;
    let outcome = recover_locked(root, account, ownership);
    anyhow::ensure!(ownership.protects(root)?, "legacy recovery ownership changed");
    let mut status = legacy_recovery_status(root, account, operation, &expected.journal_sha256)?
        .context("legacy recovery has no durable result; outcome is unknown")?;
    if let Err(error) = outcome {
        if !terminal_phase(&status.phase) { return Err(error); }
        status.cleanup_pending = true;
        status.cleanup_warning = Some("legacy snapshot recovery completed; local cleanup is pending".to_owned());
    }
    Ok(status)
}

/// Read under the host's installation takeover lock. This also supports commit
/// while the restarted runtime owns the library lock; it never opens the DB.
pub fn operation_status(
    root: &Path,
    account: &str,
    operation: &SnapshotOperation,
) -> Result<Option<RestoreStatus>> {
    let Some(journal) = read_journal(root)? else {
        return Ok(None);
    };
    if let Some(entry) = journal.history.iter().find(|entry| entry.operation.operation_id == operation.operation_id) {
        anyhow::ensure!(entry.account_fingerprint == format!("{:x}", Sha256::digest(account.as_bytes())), "snapshot recovery belongs to another account");
        anyhow::ensure!(entry.operation.request_fingerprint == operation.request_fingerprint, OperationConflict);
        return Ok(Some(RestoreStatus {
            phase: entry.phase.clone(),
            receipt: RestoreReceipt {
                restored: entry.restored.clone(),
                before_restore: entry.before_restore.clone(),
                sync_quarantined: entry.sync_quarantined,
                rollback_file: String::new(),
                rollback_sha256: String::new(),
                account_fingerprint: entry.account_fingerprint.clone(),
                operation: Some(entry.operation.clone()),
                legacy_journal_sha256: entry.legacy_journal_sha256.clone(),
            },
            cleanup_pending: false,
        }));
    }
    anyhow::ensure!(
        journal.receipt.account_fingerprint == format!("{:x}", Sha256::digest(account.as_bytes())),
        "snapshot recovery belongs to another account"
    );
    let Some(recorded) = &journal.receipt.operation else {
        if terminal_phase(&journal.phase) {
            return Ok(None);
        }
        anyhow::bail!("legacy snapshot recovery has no request identity; explicit legacy recovery is required");
    };
    if recorded.operation_id != operation.operation_id {
        return Ok(None);
    }
    anyhow::ensure!(
        recorded.request_fingerprint == operation.request_fingerprint,
        OperationConflict
    );
    let cleanup_pending = maintenance_present(root)?
        || root.join(&journal.receipt.rollback_file).try_exists()?;
    Ok(Some(RestoreStatus {
        phase: journal.phase,
        receipt: journal.receipt.into(),
        cleanup_pending,
    }))
}

fn maintenance_present(root: &Path) -> Result<bool> {
    match fs::symlink_metadata(root.join(MAINTENANCE_FILE)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn ensure_no_incomplete_restore(root: &Path) -> Result<()> {
    anyhow::ensure!(
        !maintenance_present(root)?,
        "snapshot maintenance blocks normal library access; host recovery/commit is required"
    );
    if let Some(journal) = read_journal(root)? {
        anyhow::ensure!(journal.phase=="verified" || terminal_phase(&journal.phase),"interrupted snapshot restore; host must run snapshot recover --confirm before opening the library");
    }
    Ok(())
}

/// Only the runtime holding library ownership can bootstrap the staged store.
/// This does not release the maintenance gate for any business request.
pub fn runtime_bootstrap_target(root: &Path) -> Result<Option<String>> {
    let Some(journal) = read_journal(root)? else {
        return Ok(None);
    };
    if terminal_phase(&journal.phase) {
        return Ok(None);
    }
    anyhow::ensure!(
        matches!(journal.phase.as_str(), "installed" | "verified"),
        "snapshot restore preparation is incomplete; recover before startup"
    );
    Ok(Some(journal.receipt.restored))
}

fn checkpoint_closed(live: &Path) -> Result<()> {
    reject_live_symlink(live)?;
    let connection = Connection::open_with_flags(live, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_millis(300))?;
    let (busy, log, checkpointed): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    anyhow::ensure!(
        busy == 0 && (log == checkpointed || log < 0),
        "snapshot restore checkpoint is busy; original library retained"
    );
    connection.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
    drop(connection);
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{suffix}", live.to_string_lossy()));
        anyhow::ensure!(
            !sidecar.exists(),
            "snapshot restore found an active SQLite sidecar; library was not replaced"
        );
    }
    Ok(())
}
fn copy_private(source: &Path, destination: &Path) -> Result<()> {
    let mut target = private_new(destination)?;
    let mut input = fs::File::open(source)?;
    std::io::copy(&mut input, &mut target)?;
    target.sync_all()?;
    Ok(())
}
