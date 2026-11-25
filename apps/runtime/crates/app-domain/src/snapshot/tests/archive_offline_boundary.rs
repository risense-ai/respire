// This copies verified bytes into a disposable fixture's existing snapshot
// layout. It is not a production upload, publication, or control transport.
fn place_authenticated_archive(
    root: &Path,
    expected_id: &str,
    keys: &crate::SessionKeys,
    bytes: &[u8],
) -> Result<()> {
    let directory = root.join("snapshots");
    private_dir(&directory)?;
    let mut database = private_new(&directory.join(format!("{expected_id}.db")))?;
    let manifest = backup_archive::open_to_stage(keys, "account", bytes, &mut database, || Ok(()))?;
    assert_eq!(manifest.id, expected_id);
    database.sync_all()?;
    let mut manifest_file = private_new(&directory.join(format!("{expected_id}.json")))?;
    manifest_file.write_all(&serde_json::to_vec(&manifest)?)?;
    manifest_file.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn authenticated_archive_reuses_offline_restore_and_preserves_source_binding() -> Result<()> {
    let (source, database) = fixture()?;
    let source_root = source.path();
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let snapshot_id = create(source_root, "account", "archive fixture", &keys)?.id;
    let source_lock = crate::lock::LibraryLock::acquire(source_root, std::time::Duration::ZERO)?;
    let operation = SnapshotOperation {
        operation_id: uuid::Uuid::new_v4().to_string(),
        request_fingerprint: "a".repeat(64),
    };
    let mut archive = Vec::new();
    backup_archive::seal_verified_snapshot(
        source_root, &source_lock, &keys, "account", &snapshot_id, &mut archive, || Ok(()),
    )?;
    let snapshot_base = source_root.join("snapshots").join(&snapshot_id);
    fs::remove_file(snapshot_base.with_extension("db"))?;
    fs::remove_file(snapshot_base.with_extension("json"))?;

    let (different, different_db) = fixture()?;
    let different_root = different.path();
    different_db.execute("UPDATE access_grants SET label='different fixture'", [])?;
    drop(different_db);
    let different_path = crate::service::database_path(different_root)?;
    place_authenticated_archive(different_root, &snapshot_id, &keys, &archive)?;
    // A correct Vault and structurally valid authenticated archive cannot
    // silently override the physical library binding of native restore.
    let different_lock = crate::lock::LibraryLock::acquire(different_root, std::time::Duration::ZERO)?;
    let before = fs::read(&different_path)?;
    let error = restore_operation_locked(
        different_root, "account", Some(&keys), &snapshot_id, &operation, &different_lock, true,
    ).err().context("another physical library unexpectedly accepted the archive")?;
    assert!(error.to_string().contains("another account or library"));
    assert_eq!(fs::read(&different_path)?, before);
    assert!(!different_root.join(RECOVERY_FILE).try_exists()?);
    assert!(!different_root.join(MAINTENANCE_FILE).try_exists()?);
    assert_eq!(list(different_root)?.len(), 1, "no pre-restore backup was written");

    database.execute("DELETE FROM memories", [])?;
    drop(database);
    place_authenticated_archive(source_root, &snapshot_id, &keys, &archive)?;
    let old = crate::service::database_path(source_root)?;
    let before = fs::read(&old)?;
    let wrong_lock = restore_operation_locked(
        source_root, "account", Some(&keys), &snapshot_id, &operation, &different_lock, true,
    );
    assert!(wrong_lock.is_err());
    assert_eq!(fs::read(&old)?, before);
    assert!(!source_root.join(RECOVERY_FILE).try_exists()?);
    assert!(!source_root.join(MAINTENANCE_FILE).try_exists()?);

    let installed = restore_operation_locked(
        source_root, "account", Some(&keys), &snapshot_id, &operation, &source_lock, true,
    )?.context("offline install did not record the operation")?;
    assert_eq!(installed.phase, "installed");
    let observed = restore_operation_locked(
        source_root, "account", None, &snapshot_id, &operation, &source_lock, false,
    )?.context("offline resume lost the original receipt")?;
    assert_eq!(observed.phase, installed.phase);
    assert_eq!(observed.receipt.before_restore, installed.receipt.before_restore);
    let restored = installed.receipt;
    assert_eq!(restored.restored, snapshot_id);
    assert!(restored.sync_quarantined);
    assert!(ensure_sync_allowed(source_root).is_err());
    let reader = Connection::open_with_flags(&old, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (ciphertext, nonce): (String, String) = reader.query_row(
        "SELECT ciphertext,nonce FROM memories WHERE id='synthetic'", [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(keys.decrypt_content(&ciphertext, &nonce)?, "encrypted-fixture");
    assert_eq!(reader.query_row("SELECT count(*) FROM access_grants", [], |row| row.get::<_, u64>(0))?, 0);
    drop(reader);
    complete_restore(source_root, &restored)?;
    Ok(())
}
