
/// Hold the matching library ownership guard throughout prepare and commit.
/// The private rollback image retains grants and is never a portable snapshot.
pub fn restore_locked(
    root: &Path,
    account: &str,
    keys: &crate::SessionKeys,
    id: &str,
    ownership: &crate::lock::LibraryLock,
) -> Result<RestoreReceipt> {
    restore_with_hook(root, account, keys, id, ownership, &|_| Ok(()))
}
fn restore_with_hook(
    root: &Path,
    account: &str,
    keys: &crate::SessionKeys,
    id: &str,
    ownership: &crate::lock::LibraryLock,
    hook: &dyn Fn(&str) -> Result<()>,
) -> Result<RestoreReceipt> {
    restore_operation_with_hook(root, account, keys, id, ownership, None, hook)
}

/// First-attempt dispatch may create an unknown operation; resume never does.
/// Retained history, including capacity refusal, prevents replay of older IDs.
pub fn restore_operation_locked(
    root: &Path,
    account: &str,
    keys: Option<&crate::SessionKeys>,
    id: &str,
    operation: &SnapshotOperation,
    ownership: &crate::lock::LibraryLock,
    first_attempt: bool,
) -> Result<Option<RestoreStatus>> {
    anyhow::ensure!(ownership.protects(root)?, "snapshot restore requires ownership of this library");
    if let Some(status) = operation_status(root, account, operation)? {
        anyhow::ensure!(status.receipt.restored == id, OperationConflict);
        return Ok(Some(status));
    }
    if !first_attempt {
        return Ok(None);
    }
    let keys = keys.context("initial snapshot restore requires runtime session keys")?;
    restore_operation_with_hook(root, account, keys, id, ownership, Some(operation), &|_| Ok(()))?;
    operation_status(root, account, operation)
}

fn restore_operation_with_hook(
    root: &Path,
    account: &str,
    keys: &crate::SessionKeys,
    id: &str,
    ownership: &crate::lock::LibraryLock,
    operation: Option<&SnapshotOperation>,
    hook: &dyn Fn(&str) -> Result<()>,
) -> Result<RestoreReceipt> {
    anyhow::ensure!(
        ownership.protects(root)?,
        "snapshot restore requires ownership of this library"
    );
    if let Some(previous) = read_journal(root)? {
        anyhow::ensure!(
            terminal_phase(&previous.phase)
                && !maintenance_present(root)?
                && !root.join(&previous.receipt.rollback_file).try_exists()?,
            "an earlier snapshot recovery is pending; run snapshot recover --confirm"
        );
        // Check capacity before backups, journals, or any DB replacement.
        next_history(previous)?;
    }
    let live = crate::service::database_path(root)?;
    reject_live_symlink(&live)?;
    let selected = read_manifest(root, id)?;
    let source_library = format!(
        "{:x}",
        Sha256::digest(fs::canonicalize(&live)?.to_string_lossy().as_bytes())
    );
    let account_fingerprint = format!("{:x}", Sha256::digest(account.as_bytes()));
    anyhow::ensure!(
        selected.source_library == source_library
            && selected.account_fingerprint == account_fingerprint,
        "snapshot belongs to another account or library"
    );
    let source = root.join("snapshots").join(format!("{id}.db"));
    let legacy_columns = if schema_digest(&live)? == selected.database_schema_sha256 {
        Vec::new()
    } else {
        // This preflight permits only known additions; the staged schema is checked in full below.
        let columns = missing_legacy_revision_triggers(&live, &source)?;
        let omitted: Vec<String> = columns.iter()
            .map(|column| format!("memory_revision_{column}_update"))
            .collect();
        anyhow::ensure!(
            !columns.is_empty()
                && schema_digest_without_triggers(&live, &omitted)? == selected.database_schema_sha256,
            "snapshot schema differs from the initialized library; explicit migration is required"
        );
        columns
    };
    crate::metadata_gate::ensure_restore_allowed(&live, &source)?;
    verify_key(&source, &selected, keys)?;
    let before = create(root, account, &format!("before restore {id}"), keys)?;
    let rollback_file = format!(".snapshot-rollback-{}.db", uuid::Uuid::new_v4().simple());
    let rollback = root.join(&rollback_file);
    let rollback_build = root.join(format!(
        ".snapshot-rollback-build-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    crate::service::backup_database_policy(&live, &rollback_build, false)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&rollback_build)?
        .sync_all()?;
    respire_spawn::durable::publish_new_file(&rollback_build, &rollback)?;
    let receipt = RestoreReceipt {
        restored: id.to_owned(),
        before_restore: before.id,
        sync_quarantined: true,
        rollback_sha256: digest_file(&rollback)?,
        rollback_file,
        account_fingerprint,
        operation: operation.cloned(),
        legacy_journal_sha256: None,
    };
    // Durable receipt exists before the first operation that can replace live.
    write_record(root, &receipt, "prepared", MAINTENANCE_FILE)?;
    write_journal(root, &receipt, "prepared")?;
    let temporary = root.join(format!(
        ".snapshot-restore-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let mut replacement_attempted = false;
    let mut temporary_owned = false;
    let result = (|| -> Result<()> {
        hook("after_backups")?;
        let mut input = fs::File::open(&source)?;
        let mut target = private_new(&temporary)?;
        temporary_owned = true;
        std::io::copy(&mut input, &mut target)?;
        target.sync_all()?;
        drop(target);
        drop(input);
        anyhow::ensure!(
            digest_file(&temporary)? == selected.database_sha256,
            "snapshot changed during restore preparation"
        );
        verify_key(&temporary, &selected, keys)?;
        if !legacy_columns.is_empty() {
            install_legacy_revision_triggers(&temporary, &legacy_columns)?;
            verify_key(&temporary, &selected, keys)?;
            verify_witness(&temporary, &selected)?;
            let (entries, version) = inventory(&temporary)?;
            anyhow::ensure!(
                entries == selected.entries && version == selected.sqlite_user_version,
                "snapshot inventory mismatch after legacy trigger migration"
            );
        }
        anyhow::ensure!(
            schema_digest(&temporary)? == schema_digest(&live)?,
            "snapshot schema differs from the initialized library; explicit migration is required"
        );
        let staged = Connection::open(&temporary)?;
        staged.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON;")?;
        staged.execute("INSERT INTO meta(key,value) VALUES('snapshot_sync_quarantine_v1',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [serde_json::json!({"snapshot":id,"before_restore":receipt.before_restore}).to_string()])?;
        drop(staged);
        inventory(&temporary)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        hook("before_replace")?;
        // Read-only schema checks can recreate WAL sidecars; close them before replacing live.
        checkpoint_closed(&live)?;
        // A durable replacement can report an fsync error after rename, so mark
        // the attempt before calling it. Either outcome rolls back from image.
        replacement_attempted = true;
        respire_spawn::durable::replace_file(&temporary, &live)?;
        hook("after_replace")?;
        crate::resident::clear()?;
        hook("after_cache_clear")?;
        write_journal(root, &receipt, "installed")?;
        hook("after_installed")?;
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup_result = if temporary_owned {
            match fs::remove_file(&temporary) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        } else {
            Ok(())
        };
        let rollback_result = if replacement_attempted {
            rollback_locked(root, &receipt, ownership)
        } else {
            clear_recovery(root, &receipt)
        };
        return match (rollback_result, cleanup_result) {
            (Ok(()), Ok(())) => Err(error.context(format!("restore aborted; original state retained/restored; pre-restore snapshot {}", receipt.before_restore))),
            (Ok(()), Err(cleanup_error)) => Err(error.context(format!("restore aborted; original state retained/restored; temporary copy cleanup failed: {cleanup_error}; pre-restore snapshot {}", receipt.before_restore))),
            (Err(rollback_error), Ok(())) => Err(error.context(format!("restore recovery required: {rollback_error:#}; journal {RECOVERY_FILE}; pre-restore snapshot {}", receipt.before_restore))),
            (Err(rollback_error), Err(cleanup_error)) => Err(error.context(format!("restore recovery required: {rollback_error:#}; temporary copy cleanup failed: {cleanup_error}; journal {RECOVERY_FILE}; pre-restore snapshot {}", receipt.before_restore))),
        };
    }
    Ok(receipt)
}

/// Restore only this operation's original local image, including grants/outbox.
pub fn rollback_locked(
    root: &Path,
    receipt: &RestoreReceipt,
    ownership: &crate::lock::LibraryLock,
) -> Result<()> {
    anyhow::ensure!(
        ownership.protects(root)?,
        "rollback requires this library's ownership lock"
    );
    let journal = read_journal(root)?.context("snapshot recovery journal missing")?;
    anyhow::ensure!(
        !terminal_phase(&journal.phase)
            && (receipt.operation.is_none() || journal.phase != "verified"),
        "snapshot restore is committed or rolled back; query before compensation"
    );
    anyhow::ensure!(
        journal.phase != "verified" || maintenance_present(root)?,
        "snapshot restore is committed; rollback would discard accepted writes"
    );
    anyhow::ensure!(
        journal.receipt.rollback_file == receipt.rollback_file
            && journal.receipt.rollback_sha256 == receipt.rollback_sha256
            && journal.receipt.operation == receipt.operation,
        "rollback receipt differs from durable journal"
    );
    let original = root.join(&receipt.rollback_file);
    anyhow::ensure!(
        digest_file(&original)? == receipt.rollback_sha256,
        "local rollback image digest mismatch"
    );
    let live = crate::service::database_path(root)?;
    if let Err(error) = checkpoint_closed(&live) {
        let corrupt = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<rusqlite::Error>())
            .any(|error| {
                matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
                )
            });
        let has_sidecars = ["-wal", "-shm"].iter().any(|suffix| {
            std::path::PathBuf::from(format!("{}{suffix}", live.to_string_lossy())).exists()
        });
        anyhow::ensure!(
            corrupt && !has_sidecars,
            "rollback checkpoint failed; original recovery image retained: {error}"
        );
    }
    let temporary = root.join(format!(
        ".snapshot-rollback-install-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    copy_private(&original, &temporary)?;
    if let Err(error) = respire_spawn::durable::replace_file(&temporary, &live) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    crate::resident::clear()?;
    clear_recovery(root, receipt)
}
fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
