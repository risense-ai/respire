fn clear_recovery(root: &Path, receipt: &RestoreReceipt) -> Result<()> {
    clear_with_hook(root, receipt, &|_| Ok(()))
}
fn clear_with_hook(
    root: &Path,
    receipt: &RestoreReceipt,
    hook: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    // Keep terminal history in the existing journal before removing the gate
    // or private rollback image. Legacy cleanup must preserve that history too.
    let journal = read_journal(root)?.context("snapshot recovery journal missing")?;
    let retain_journal = receipt.operation.is_some() || !journal.history.is_empty();
    if retain_journal {
        anyhow::ensure!(
            journal.receipt.operation == receipt.operation
                && journal.receipt.rollback_sha256 == receipt.rollback_sha256
                && journal.receipt.rollback_file == receipt.rollback_file,
            "snapshot cleanup receipt differs from durable journal"
        );
        let terminal = match journal.phase.as_str() {
            "verified" | "released" => "released",
            _ => "rolled_back",
        };
        write_journal(root, receipt, terminal)?;
    }
    // Once verified is durable, gate removal is a one-way commit release. Any
    // later cleanup failure must retain the selected state, never roll it back.
    remove_if_present(&root.join(MAINTENANCE_FILE))?;
    hook("after_gate_removed")?;
    respire_spawn::durable::sync_directory(root)?;
    if !retain_journal {
        remove_if_present(&root.join(RECOVERY_FILE))?;
        respire_spawn::durable::sync_directory(root)?;
    }
    remove_if_present(&root.join(&receipt.rollback_file))?;
    respire_spawn::durable::sync_directory(root)?;
    Ok(())
}

/// Persist commit while maintenance still blocks all normal requests. Failure
/// Legacy callers compensate failures under their lifecycle lock. Controlled
/// callers must query uncertain commits before choosing recovery or rollback.
pub fn commit_restore(root: &Path, receipt: &RestoreReceipt) -> Result<()> {
    commit_with_hook(root, receipt, &|_| Ok(()))
}
fn commit_with_hook(
    root: &Path,
    receipt: &RestoreReceipt,
    hook: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    anyhow::ensure!(
        maintenance_present(root)?,
        "snapshot commit requires active maintenance gate"
    );
    let journal = read_journal(root)?.context("snapshot completion journal missing")?;
    anyhow::ensure!(
        journal.receipt.rollback_file == receipt.rollback_file
            && journal.receipt.rollback_sha256 == receipt.rollback_sha256
            && journal.receipt.operation == receipt.operation
            && matches!(journal.phase.as_str(), "installed" | "verified"),
        "snapshot completion receipt differs from durable journal"
    );
    hook("before_verified_write")?;
    write_journal(root, receipt, "verified")?;
    hook("after_verified_write")?;
    Ok(())
}

/// Only call after commit_restore returned success. Cleanup errors are warnings
/// about an already committed state and must never trigger original-state rollback.
pub fn release_restore(root: &Path, receipt: &RestoreReceipt) -> Result<()> {
    let journal = read_journal(root)?.context("snapshot release journal missing")?;
    anyhow::ensure!(
        matches!(journal.phase.as_str(), "verified" | "released")
            && journal.receipt.rollback_file == receipt.rollback_file
            && journal.receipt.rollback_sha256 == receipt.rollback_sha256
            && journal.receipt.operation == receipt.operation,
        "snapshot release is not the committed operation"
    );
    clear_recovery(root, receipt)
}

pub fn complete_restore(root: &Path, receipt: &RestoreReceipt) -> Result<()> {
    commit_restore(root, receipt)?;
    release_restore(root, receipt)
}

pub fn recover_locked(
    root: &Path,
    account: &str,
    ownership: &crate::lock::LibraryLock,
) -> Result<()> {
    anyhow::ensure!(
        ownership.protects(root)?,
        "recovery requires this library's ownership lock"
    );
    let journal = read_journal(root)?.context("no interrupted snapshot operation exists")?;
    let verified = journal.phase == "verified" || terminal_phase(&journal.phase);
    let receipt: RestoreReceipt = journal.receipt.into();
    anyhow::ensure!(
        receipt.account_fingerprint == format!("{:x}", Sha256::digest(account.as_bytes())),
        "snapshot recovery belongs to another account"
    );
    if verified {
        clear_recovery(root, &receipt)
    } else {
        rollback_locked(root, &receipt, ownership)
    }
}

fn verify_key(path: &Path, manifest: &Manifest, keys: &crate::SessionKeys) -> Result<()> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (nonce, ciphertext): (String, String) = db.query_row(
        "SELECT nonce,ciphertext FROM snapshot_key_check WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let challenge = format!(
        "snapshot-key-check:v1:{}:{}:{}",
        manifest.id, manifest.source_library, manifest.account_fingerprint
    );
    anyhow::ensure!(
        keys.decrypt_content(&ciphertext, &nonce)
            .context("snapshot key validation failed")?
            == challenge,
        "snapshot key identity mismatch"
    );
    let mut statement = db.prepare("SELECT ciphertext,nonce FROM memories WHERE ciphertext<>''")?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (ciphertext, nonce) = row?;
        keys.decrypt_content(&ciphertext, &nonce)
            .context("snapshot contains content that cannot be opened by this account key")?;
    }
    Ok(())
}

pub fn ensure_sync_allowed(root: &Path) -> Result<()> {
    let path = crate::service::database_path(root)?;
    if !path.exists() {
        return Ok(());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_meta: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
        [],
        |r| r.get(0),
    )?;
    if has_meta {
        crate::metadata_gate::ensure_connection_sync_allowed(&connection)?;
        let quarantined: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key='snapshot_sync_quarantine_v1')",
            [],
            |r| r.get(0),
        )?;
        anyhow::ensure!(
            !quarantined,
            "snapshot restore quarantined sync; compare differences before snapshot resume-sync"
        );
    }
    Ok(())
}

pub fn resume_sync(root: &Path, reviewed_revision: &str) -> Result<()> {
    let database = crate::service::database_path(root)?;
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.execute_batch("BEGIN IMMEDIATE")?;
    ensure_no_incomplete_restore(root)?;
    crate::metadata_gate::ensure_connection_sync_allowed(&connection)?;
    let revision: String = connection.query_row(
        "SELECT value FROM meta WHERE key='memory_revision_v1'",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        revision == reviewed_revision,
        "library changed since the reviewed revision; sync remains quarantined"
    );
    connection.execute(
        "DELETE FROM meta WHERE key='snapshot_sync_quarantine_v1'",
        [],
    )?;
    connection.execute_batch("COMMIT")?;
    Ok(())
}

