    fn fixture() -> Result<(tempfile::TempDir, Connection)> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rsrs.db");
        drop(private_new(&path)?);
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE memories(id TEXT PRIMARY KEY,ciphertext TEXT,nonce TEXT,user TEXT NOT NULL DEFAULT 'account',created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z',updated_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'); CREATE TABLE access_grants(id TEXT PRIMARY KEY,label TEXT,root_id TEXT,token_hash TEXT,created_at TEXT,revoked INTEGER DEFAULT 0); CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES('memory_revision_v1','0123456789abcdef0123456789abcdef'); INSERT INTO access_grants(id,label,root_id,token_hash,created_at) VALUES('synthetic-grant','fixture','synthetic','synthetic-private-grant-marker','2026-01-01');")?;
        let (nonce, ciphertext) =
            crate::SessionKeys::from_urk([7; 32])?.encrypt_content("encrypted-fixture")?;
        db.execute(
            "INSERT INTO memories(id,ciphertext,nonce) VALUES('synthetic',?1,?2)",
            rusqlite::params![ciphertext, nonce],
        )?;
        Ok((directory, db))
    }

    fn legacy_fixture(
        columns: &[&str],
        preinstalled: Option<&str>,
    ) -> Result<(tempfile::TempDir, Connection, Manifest)> {
        let (directory, db) = fixture()?;
        drop(db);
        let path = directory.path().join("rsrs.db");
        crate::transport::local::validate_migration_snapshot(&path)?;
        let db = Connection::open(&path)?;
        crate::transport::local::register_witness_functions(&db)?;
        for column in columns {
            db.execute_batch(&format!("ALTER TABLE memories ADD COLUMN {column} TEXT"))?;
        }
        if let Some(column) = preinstalled {
            anyhow::ensure!(columns.contains(&column), "preinstalled trigger column missing");
            db.execute_batch(&legacy_revision_trigger_sql(column))?;
        }
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let snapshot = create(directory.path(), "account", "legacy", &keys)?;
        crate::transport::local::validate_migration_snapshot(&path)?;
        Ok((directory, db, snapshot))
    }
    #[test]
    fn snapshot_captures_committed_wal_excludes_grants_and_preserves_source() -> Result<()> {
        let (directory, db) = fixture()?;
        let manifest = create(
            directory.path(),
            "synthetic-account",
            "first",
            &crate::SessionKeys::from_urk([7; 32])?,
        )?;
        assert_eq!(manifest.entries, 1);
        assert_eq!(manifest.sdk_version, crate::core_sdk::CORE_VERSION);
        assert_eq!(list(directory.path())?.len(), 1);
        let copied = directory
            .path()
            .join("snapshots")
            .join(format!("{}.db", manifest.id));
        assert!(!fs::read(&copied)?
            .windows(b"synthetic-private-grant-marker".len())
            .any(|w| w == b"synthetic-private-grant-marker"));
        assert_eq!(
            db.query_row("SELECT count(*) FROM access_grants", [], |r| r
                .get::<_, u64>(0))?,
            1
        );
        let ciphertext: String =
            db.query_row("SELECT ciphertext FROM memories", [], |r| r.get(0))?;
        let nonce: String = db.query_row("SELECT nonce FROM memories", [], |r| r.get(0))?;
        assert_eq!(
            crate::SessionKeys::from_urk([7; 32])?.decrypt_content(&ciphertext, &nonce)?,
            "encrypted-fixture"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&copied)?.permissions().mode() & 0o777, 0o600);
            assert_eq!(
                fs::metadata(copied.with_extension("json"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let (nonce, ciphertext) =
            crate::SessionKeys::from_urk([7; 32])?.encrypt_content("second")?;
        db.execute(
            "INSERT INTO memories(id,ciphertext,nonce) VALUES('second',?1,?2)",
            rusqlite::params![ciphertext, nonce],
        )?;
        let second = create(
            directory.path(),
            "synthetic-account",
            "second",
            &crate::SessionKeys::from_urk([7; 32])?,
        )?;
        assert_ne!(second.id, manifest.id);
        assert_eq!(second.entries, 2);
        assert_eq!(list(directory.path())?.len(), 2);
        Ok(())
    }
    #[test]
    fn restore_checks_keys_accounts_schema_and_quarantines_sync() -> Result<()> {
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let manifest = create(root, "account", "baseline", &keys)?;
        db.execute("DELETE FROM memories", [])?;
        drop(db);
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        assert!(restore_locked(root, "wrong-account", &keys, &manifest.id, &ownership).is_err());
        assert!(restore_locked(
            root,
            "account",
            &crate::SessionKeys::from_urk([8; 32])?,
            &manifest.id,
            &ownership
        )
        .is_err());
        let receipt = restore_locked(root, "account", &keys, &manifest.id, &ownership)?;
        assert!(receipt.sync_quarantined);
        assert_ne!(receipt.before_restore, manifest.id);
        assert!(ensure_sync_allowed(root).is_err());
        let restored = Connection::open(root.join("rsrs.db"))?;
        assert_eq!(
            restored.query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, u64>(0))?,
            1
        );
        assert_eq!(
            restored.query_row("SELECT count(*) FROM access_grants", [], |r| r
                .get::<_, u64>(0))?,
            0
        );
        complete_restore(root, &receipt)?;
        assert!(resume_sync(root, "stale-review").is_err());
        assert!(ensure_sync_allowed(root).is_err());
        resume_sync(root, "0123456789abcdef0123456789abcdef")?;
        assert!(ensure_sync_allowed(root).is_ok());
        drop(restored);
        let mut completed = Vec::new();
        for _ in 0..2 {
            let operation = controlled_receipt().operation.context("controlled operation missing")?;
            let status = restore_operation_locked(root, "account", Some(&keys), &manifest.id, &operation, &ownership, true)?
                .context("initial controlled restore missing")?;
            assert!(status.receipt.sync_quarantined);
            complete_restore(root, &status.receipt)?;
            completed.push((operation, status.receipt));
        }
        let live = Connection::open(root.join("rsrs.db"))?;
        crate::transport::local::register_witness_functions(&live)?;
        live.execute("INSERT INTO memories(id,ciphertext,nonce) VALUES('after-o2','','')", [])?;
        let before_retry = fs::read(root.join(RECOVERY_FILE))?;
        let snapshot_count = list(root)?.len();
        let (operation, first_receipt) = &completed[0];
        for first_attempt in [false, true] {
            let status = restore_operation_locked(root, "account", None, &manifest.id, operation, &ownership, first_attempt)?
                .context("historical completed restore missing")?;
            assert_eq!(status.phase, "released");
            assert_eq!(status.receipt.before_restore, first_receipt.before_restore);
        }
        let mut conflict = operation.clone();
        conflict.request_fingerprint = "f".repeat(64);
        let error = restore_operation_locked(root, "account", None, &manifest.id, &conflict, &ownership, true)
            .err().context("historical restore accepted conflicting parameters")?;
        assert!(error.downcast_ref::<OperationConflict>().is_some());
        assert_eq!(live.query_row("SELECT count(*) FROM memories WHERE id='after-o2'", [], |row| row.get::<_, u64>(0))?, 1);
        assert_eq!(list(root)?.len(), snapshot_count);
        assert_eq!(fs::read(root.join(RECOVERY_FILE))?, before_retry);

        for (columns, preinstalled) in [
            (&["when_to_use"][..], None),
            (&["superseded_by"][..], None),
            (&["when_to_use", "superseded_by"][..], None),
            (&["when_to_use", "superseded_by"][..], Some("when_to_use")),
            (&["when_to_use", "superseded_by"][..], Some("superseded_by")),
        ] {
            let (directory, db, snapshot) = legacy_fixture(columns, preinstalled)?;
            let root = directory.path();
            let live = root.join("rsrs.db");
            let snapshot_db = root.join("snapshots").join(format!("{}.db", snapshot.id));
            let snapshot_json = root.join("snapshots").join(format!("{}.json", snapshot.id));
            let original_db = fs::read(&snapshot_db)?;
            let original_json = fs::read(&snapshot_json)?;
            let saved = Connection::open_with_flags(&snapshot_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let saved_revision: String = saved.query_row(
                "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
            )?;
            drop(saved);
            let expected_schema = schema_digest(&live)?;
            db.execute("INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')", [])?;
            drop(db);
            let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
            assert!(restore_locked(root, "wrong-account", &keys, &snapshot.id, &ownership).is_err());
            assert!(restore_locked(
                root, "account", &crate::SessionKeys::from_urk([8; 32])?, &snapshot.id, &ownership,
            ).is_err());
            assert_eq!(list(root)?.len(), 1);
            let receipt = restore_locked(root, "account", &keys, &snapshot.id, &ownership)?;
            assert!(receipt.sync_quarantined);
            assert_eq!(schema_digest(&live)?, expected_schema);
            assert_eq!(fs::read(&snapshot_db)?, original_db);
            assert_eq!(fs::read(&snapshot_json)?, original_json);
            verify_backup(root, &snapshot.id, &keys)?;
            let restored = Connection::open(&live)?;
            crate::transport::local::register_witness_functions(&restored)?;
            assert_eq!(restored.query_row("SELECT count(*) FROM memories", [], |row| row.get::<_, u64>(0))?, 1);
            let revision: String = restored.query_row(
                "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
            )?;
            assert_ne!(revision, saved_revision);
            crate::transport::local::validate_migration_snapshot(&live)?;
            let reopened_revision: String = restored.query_row(
                "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
            )?;
            assert_eq!(reopened_revision, revision);
            complete_restore(root, &receipt)?;
            for column in columns {
                let before: String = restored.query_row(
                    "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
                )?;
                restored.execute(&format!("UPDATE memories SET {column}='restored' WHERE id='synthetic'"), [])?;
                let after: String = restored.query_row(
                    "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
                )?;
                assert_ne!(after, before, "restored trigger did not track {column}");
            }
        }
        Ok(())
    }

    #[test]
    fn restore_rejects_schema_changes_without_touching_live_rows() -> Result<()> {
        for statement in [
            "ALTER TABLE memories ADD COLUMN future_schema TEXT",
            "PRAGMA user_version=42",
        ] {
            let (directory, db) = fixture()?;
            let keys = crate::SessionKeys::from_urk([7; 32])?;
            let manifest = create(directory.path(), "account", "baseline", &keys)?;
            db.execute_batch(statement)?;
            let ownership = crate::lock::LibraryLock::acquire(
                directory.path(),
                std::time::Duration::from_secs(1),
            )?;
            assert!(
                restore_locked(directory.path(), "account", &keys, &manifest.id, &ownership)
                    .is_err()
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM memories", [], |row| row
                    .get::<_, u64>(0))?,
                1
            );
            assert_eq!(
                list(directory.path())?.len(),
                1,
                "schema mismatch should not begin restore"
            );
        }
        let (directory, db, snapshot) = legacy_fixture(&["when_to_use"], None)?;
        db.execute_batch("ALTER TABLE memories ADD COLUMN future_schema TEXT")?;
        let live = directory.path().join("rsrs.db");
        let schema = schema_digest(&live)?;
        let ownership = crate::lock::LibraryLock::acquire(
            directory.path(), std::time::Duration::from_secs(1),
        )?;
        assert!(restore_locked(
            directory.path(), "account", &crate::SessionKeys::from_urk([7; 32])?,
            &snapshot.id, &ownership,
        ).is_err());
        assert_eq!(schema_digest(&live)?, schema);
        assert_eq!(list(directory.path())?.len(), 1);

        let (directory, db) = fixture()?;
        db.execute_batch("ALTER TABLE memories ADD COLUMN when_to_use TEXT")?;
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let snapshot = create(directory.path(), "account", "wrong trigger", &keys)?;
        db.execute_batch("CREATE TRIGGER memory_revision_when_to_use_update
            AFTER UPDATE OF when_to_use ON memories BEGIN SELECT 1; END;")?;
        let ownership = crate::lock::LibraryLock::acquire(
            directory.path(), std::time::Duration::from_secs(1),
        )?;
        assert!(restore_locked(directory.path(), "account", &keys, &snapshot.id, &ownership).is_err());
        assert_eq!(list(directory.path())?.len(), 1);
        Ok(())
    }
    #[test]
    fn repaired_v1_snapshot_staging_preserves_version_and_aborts_before_publish() -> Result<()> {
        let directory = tempfile::tempdir()?;
        #[cfg(windows)]
        let root = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(directory.path())?;
        #[cfg(not(windows))]
        let root = directory.path().canonicalize()?;
        let live = root.join("rsrs.db");
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        drop(crate::transport::local::LocalStore::open(&live)?);
        let sql = Connection::open(&live)?;
        crate::transport::local::register_witness_functions(&sql)?;
        let (nonce, ciphertext) = keys.encrypt_content("synthetic migrated snapshot content")?;
        sql.execute("INSERT INTO memories(id,ciphertext,nonce) VALUES('synthetic',?1,?2)",
            rusqlite::params![ciphertext, nonce])?;
        sql.execute_batch("DROP INDEX idx_memories_updated; PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(sql);
        // This is a real protected live 0→1 repair, not a test pragma stamp.
        drop(crate::transport::local::LocalStore::open(&live)?);
        let marker = |path: &Path| -> Result<i64> {
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            Ok(db.pragma_query_value(None, "user_version", |row| row.get(0))?)
        };
        assert_eq!(marker(&live)?, 1);
        let bundle = create(&root, "account", "repaired-v1", &keys)?;
        let portable = root.join("snapshots").join(format!("{}.db", bundle.id));
        let original_bundle = fs::read(&portable)?;
        let bundle_head = bundle.witness_head.clone().context("actual witness must be in v1 bundle")?;
        let sql = Connection::open(&live)?;
        crate::transport::local::register_witness_functions(&sql)?;
        sql.execute("INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')", [])?;
        sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(sql);
        let live_before = fs::read(&live)?;
        let ownership = crate::lock::LibraryLock::acquire(&root, std::time::Duration::from_secs(1))?;
        let fail = restore_with_hook(&root, "account", &keys, &bundle.id, &ownership, &|phase| {
            if phase == "before_replace" {
                anyhow::bail!("synthetic pre-publication failure");
            }
            Ok(())
        }).unwrap_err();
        assert!(format!("{fail:#}").contains("synthetic pre-publication failure"));
        assert_eq!(fs::read(&live)?, live_before);
        assert_eq!(fs::read(&portable)?, original_bundle);
        assert!(ensure_no_incomplete_restore(&root).is_ok());
        let receipt = restore_locked(&root, "account", &keys, &bundle.id, &ownership)?;
        complete_restore(&root, &receipt)?;
        assert_eq!(marker(&live)?, 1);
        assert_eq!(marker(&portable)?, 1);
        assert_eq!(fs::read(&portable)?, original_bundle);
        let head = crate::transport::local::witness_checkpoint_file(&live)?
            .context("restored witness missing")?;
        assert_eq!(head.head, bundle_head);
        let db = Connection::open_with_flags(&live, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let (encrypted, nonce): (String, String) = db.query_row("SELECT ciphertext,nonce FROM memories WHERE id='synthetic'",[],|row| Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(keys.decrypt_content(&encrypted, &nonce)?, "synthetic migrated snapshot content");
        assert!(!db.prepare("SELECT 1 FROM memories WHERE id='later'")?.exists([])?);
        Ok(())
    }

#[cfg(target_os = "linux")]
#[test]
fn confirmed_archive_uses_exact_custody_and_only_installs_once() -> Result<()> {
    use respire_spawn::owner_credential::ArchiveOperation;
    use crate::snapshot::backup_archive::{self, BackupArchiveHeader};
    let (directory, db) = fixture()?;
    let root = directory.path().canonicalize()?;
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let id = create(&root, "account", "fixture", &keys)?.id;
    let ownership = crate::lock::LibraryLock::acquire(&root, std::time::Duration::ZERO)?;
    let mut wire = Vec::new();
    backup_archive::seal_verified_snapshot(&root, &ownership, &keys, "account", &id, &mut wire, || Ok(()))?;
    let uuid = uuid::Uuid::new_v4();
    let op = ArchiveOperation::from_v4_bytes(*uuid.as_bytes())?;
    let custody = tempfile::tempdir()?;
    let custody = custody.path().canonicalize()?;
    let selected = respire_spawn::owner_credential::ArchiveTarget::pin(root.join("rsrs.db"))?;
    let mut reader = wire.as_slice();
    let pending = BackupArchiveHeader::read(&mut reader)?.authenticate(&keys, "account")?
        .stage_for_restore(reader, &custody, &root.join("rsrs.db"), op, || Ok(()))?;
    let intent = pending.handoff().intent_sha256();
    drop(pending.publish(|| Ok(()))?);
    let digest = Sha256::digest(&wire).into();
    use respire_spawn::owner_credential::{OwnerRecord, backup_grant::{self, Receiver, Scope}};
    let owner = OwnerRecord::from_bytes([12; 48]);
    let scope = Scope {
        operation: *uuid.as_bytes(), archive: digest, target: Sha256::digest(selected.path().as_os_str().as_encoded_bytes()).into(),
        target_identity: selected.identity_digest()?, account: Sha256::digest(b"account").into(),
        profile: Sha256::digest(custody.as_os_str().as_encoded_bytes()).into(), instance: [15; 16], generation: 1,
        supervisor_pid: std::process::id(), supervisor_birth: 1,
    };
    let issue = |intent, archive| -> Result<_> {
        let exact = Scope { archive, ..scope };
        let receiver = Receiver::arm(&owner, exact)?;
        let tag = backup_grant::issue(&owner, &receiver.challenge(), &exact, &intent, &[16;16])?;
        Ok(receiver.accept(intent, [16;16], &tag)?)
    };
    let vault = backup_archive::BackupVaultIdentity::fixture("account", &custody, uuid::Uuid::from_bytes([16;16]));
    for (name, epoch) in [("wrong", [16;16]), ("account", [17;16])] {
        let other = backup_archive::BackupVaultIdentity::fixture(name, &custody, uuid::Uuid::from_bytes(epoch));
        assert!(other.check(&issue(intent, digest)?).is_err());
    }
    let operation = SnapshotOperation { operation_id: uuid.to_string(), request_fingerprint: issue(intent, digest)?.fingerprint() };
    let mut other = operation.clone();
    other.operation_id = uuid::Uuid::new_v4().to_string();
    fs::remove_file(root.join("snapshots").join(format!("{id}.db")))?;
    fs::remove_file(root.join("snapshots").join(format!("{id}.json")))?;
    db.execute("DELETE FROM memories", [])?;
    drop(db);
    let live = root.join("rsrs.db");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&live, fs::Permissions::from_mode(0o640))?;
    assert_eq!(digest_file(&live).unwrap_err().to_string(), "snapshot file is not private");
    fs::set_permissions(&live, fs::Permissions::from_mode(0o600))?;
    let before = digest_file(&live)?;
    for (target, operation, intent, digest) in [
        (id.as_str(), &operation, [0; 32], digest),
        (id.as_str(), &operation, intent, [0; 32]),
        (id.as_str(), &other, intent, digest),
        ("11111111111111111111111111111111", &operation, intent, digest),
    ] {
        assert!(backup_archive::restore_approved_archive_locked(&root, &custody, &selected, &vault, &keys, target,
            operation, &ownership, issue(intent, digest)?, || Ok(())).is_err());
        assert_eq!(digest_file(&live)?, before);
        assert!(!root.join(RECOVERY_FILE).exists());
    }
    let apply = || backup_archive::restore_approved_archive_locked(&root, &custody, &selected, &vault, &keys, &id,
        &operation, &ownership, issue(intent, digest)?, || Ok(()));
    let status = apply()?.context("missing installed operation")?;
    assert_eq!(status.phase, "installed");
    assert!(apply().is_err());
    let restored = Connection::open_with_flags(&live, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    assert_eq!(restored.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get::<_, i64>(0))?, 1);
    Ok(())
}
