
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
    let live_schema = schema_digest(&live)?;
    let schemas_match = live_schema == selected.database_schema_sha256;
    let legacy_columns = if schemas_match {
        Vec::new()
    } else {
        let columns = missing_legacy_revision_triggers(&live, &source)?;
        let omitted: Vec<String> = columns.iter()
            .map(|column| format!("memory_revision_{column}_update"))
            .collect();
        if !columns.is_empty()
            && schema_digest_without_triggers(&live, &omitted)? == selected.database_schema_sha256
        {
            columns
        } else {
            Vec::new()
        }
    };
    crate::metadata_gate::ensure_restore_allowed(&live, &source)?;
    verify_key(&source, &selected, keys)?;
    let mut temporary = prepare_restore_copy(root, &source, &selected, keys, &legacy_columns)?;
    // Historical portable v0 snapshots may predate the known additive
    // fields. Restore only if that exact detached normalization reaches
    // the existing live shape; arbitrary changes still fail before backups.
    if !schemas_match {
        anyhow::ensure!(
            schema_digest(&temporary.path)? == live_schema,
            "snapshot schema differs from the initialized library; explicit migration is required"
        );
    }
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
    let mut replacement_attempted = false;
    let result = (|| -> Result<()> {
        hook("after_backups")?;
        anyhow::ensure!(
            digest_file(&source)? == selected.database_sha256,
            "snapshot changed during restore preparation"
        );
        let stage_path = &temporary.path;
        let staged = Connection::open(stage_path).context("open verified detached snapshot for publication")?;
        staged.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON;")?;
        staged.execute("INSERT INTO meta(key,value) VALUES('snapshot_sync_quarantine_v1',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [serde_json::json!({"snapshot":id,"before_restore":receipt.before_restore}).to_string()])?;
        drop(staged);
        inventory(stage_path)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(stage_path)?
            .sync_all()?;
        hook("before_replace")?;
        // Read-only schema checks can recreate WAL sidecars; close them before replacing live.
        checkpoint_closed(&live)?;
        // A durable replacement can report an fsync error after rename, so mark
        // the attempt before calling it. Either outcome rolls back from image.
        replacement_attempted = true;
        temporary.publish(&live).context("publish verified detached snapshot")?;
        hook("after_replace")?;
        crate::resident::clear()?;
        hook("after_cache_clear")?;
        write_journal(root, &receipt, "installed")?;
        hook("after_installed")?;
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup_result = temporary.cleanup();
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


struct PreparedRestoreCopy {
    path: std::path::PathBuf,
    #[cfg(windows)]
    windows: Option<respire_spawn::owner_credential::PrivateRestoreStage>,
}

impl PreparedRestoreCopy {
    fn publish(&mut self, destination: &Path) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            self.windows.as_mut().ok_or_else(|| std::io::Error::other("restore stage missing"))?
                .publish_replacing(destination)
        }
        #[cfg(not(windows))]
        {
            respire_spawn::durable::replace_file(&self.path, destination)
        }
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            if let Some(mut stage) = self.windows.take() { stage.cleanup()?; }
            Ok(())
        }
        #[cfg(not(windows))]
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

impl Drop for PreparedRestoreCopy {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
fn prepare_restore_copy(
    root: &Path,
    source: &Path,
    selected: &Manifest,
    keys: &crate::SessionKeys,
    legacy_columns: &[&str],
) -> Result<PreparedRestoreCopy> {
    let name = format!(".snapshot-restore-{}", uuid::Uuid::new_v4().simple());
    let mut input = fs::File::open(source)?;
    #[cfg(windows)]
    let (owned, mut target) = {
        let windows = respire_spawn::owner_credential::PrivateRestoreStage::create(root, &name)?;
        let target = windows.writer()?;
        (PreparedRestoreCopy { path: windows.path().to_path_buf(), windows: Some(windows) }, target)
    };
    #[cfg(not(windows))]
    let (owned, mut target) = {
        let path = root.join(format!("{name}.db"));
        let target = private_new(&path)?;
        (PreparedRestoreCopy { path }, target)
    };
    std::io::copy(&mut input, &mut target)?;
    target.sync_all()?;
    drop(target);
    drop(input);
    let path = &owned.path;
    anyhow::ensure!(
        digest_file(path)? == selected.database_sha256,
        "snapshot changed during restore preparation"
    );
    verify_key(path, selected, keys)?;
    verify_witness(path, selected)?;
    if !legacy_columns.is_empty() {
        install_legacy_revision_triggers(path, legacy_columns)?;
    }
    crate::transport::local::validate_migration_snapshot(path)?;
    verify_key(path, selected, keys)?;
    if selected.witness_head.is_some() {
        verify_witness(path, selected)?;
    } else {
        // A valid pre-witness file never attested entries. Initialization
        // may create a fresh empty ledger, never fabricate an earlier chain.
        let report = crate::transport::local::witness_checkpoint_file(path)?
            .context("detached snapshot witness was not initialized")?;
        anyhow::ensure!(report.intact && report.count == 0 && report.head == "0".repeat(64),
            "unexpected legacy witness history after initialization");
    }
    let (entries, version) = inventory(path)?;
    anyhow::ensure!(
        entries == selected.entries && version == selected.sqlite_user_version,
        "snapshot inventory mismatch after detached schema initialization"
    );
    Ok(owned)
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
