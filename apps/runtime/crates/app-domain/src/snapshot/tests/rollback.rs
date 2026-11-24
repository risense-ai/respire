    #[test]
    fn injected_failures_restore_original_grants_outbox_and_state() -> Result<()> {
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        for (legacy, stage) in [false, true].into_iter().flat_map(|legacy| [
            "after_backups",
            "before_replace",
            "after_replace",
            "after_cache_clear",
            "after_installed",
        ].into_iter().map(move |stage| (legacy, stage))) {
            let (directory, db, snapshot) = if legacy {
                legacy_fixture(&["when_to_use", "superseded_by"], None)?
            } else {
                let (directory, db) = fixture()?;
                let snapshot = create(directory.path(), "account", "baseline", &keys)?;
                (directory, db, snapshot)
            };
            let root = directory.path();
            db.execute(
                "INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')",
                [],
            )?;
            db.execute(
                "INSERT INTO meta VALUES('synthetic_outbox','original-pending-write')",
                [],
            )?;
            drop(db);
            let ownership =
                crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
            let result =
                restore_with_hook(root, "account", &keys, &snapshot.id, &ownership, &|at| {
                    anyhow::ensure!(at != stage, "injected restore failure at {stage}");
                    Ok(())
                });
            assert!(result.is_err(), "fault did not run: {stage}");
            let original = Connection::open(root.join("rsrs.db"))?;
            assert_eq!(
                original.query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, u64>(0))?,
                2,
                "{stage}"
            );
            assert_eq!(
                original.query_row("SELECT count(*) FROM access_grants", [], |r| r
                    .get::<_, u64>(0))?,
                1,
                "{stage}"
            );
            assert_eq!(
                original.query_row(
                    "SELECT value FROM meta WHERE key='synthetic_outbox'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "original-pending-write",
                "{stage}"
            );
            assert!(
                ensure_sync_allowed(root).is_ok(),
                "rollback changed original quarantine: {stage}"
            );
            assert!(
                !root.join(RECOVERY_FILE).exists(),
                "recovery journal retained after successful rollback: {stage}"
            );
            assert!(
                list(root)?.len() >= 2,
                "portable prebackup missing: {stage}"
            );
        }
        for failure in [
            "CREATE TRIGGER reject_revision BEFORE UPDATE OF value ON meta
             WHEN NEW.key='memory_revision_v1'
             BEGIN SELECT RAISE(ABORT,'synthetic revision failure'); END;",
            "CREATE TRIGGER reject_revision BEFORE UPDATE OF value ON meta
             WHEN NEW.key='memory_revision_v1'
             BEGIN SELECT RAISE(IGNORE); END;",
            "CREATE TABLE synthetic_effect(id INTEGER);
             CREATE TRIGGER revision_side_effect AFTER UPDATE OF value ON meta
             WHEN NEW.key='memory_revision_v1'
             BEGIN INSERT INTO synthetic_effect VALUES(1); END;",
        ] {
            let (directory, db) = fixture()?;
            let root = directory.path();
            db.execute_batch("ALTER TABLE memories ADD COLUMN when_to_use TEXT")?;
            db.execute_batch(failure)?;
            let snapshot = create(root, "account", "legacy failure", &keys)?;
            let snapshot_db = root.join("snapshots").join(format!("{}.db", snapshot.id));
            let original_snapshot = fs::read(&snapshot_db)?;
            db.execute_batch(&legacy_revision_trigger_sql("when_to_use"))?;
            let revision: String = db.query_row(
                "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get(0),
            )?;
            drop(db);
            let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
            let error = match restore_locked(root, "account", &keys, &snapshot.id, &ownership) {
                Ok(_) => panic!("legacy trigger migration ignored a failure"),
                Err(error) => error,
            };
            let message = format!("{error:#}");
            assert!(message.contains("synthetic revision failure")
                || message.contains("memory revision could not be refreshed without other changes"));
            let live = Connection::open(root.join("rsrs.db"))?;
            assert_eq!(live.query_row("SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get::<_, String>(0))?, revision);
            assert_eq!(live.query_row("SELECT count(*) FROM memories", [], |row| row.get::<_, u64>(0))?, 1);
            assert_eq!(live.query_row("SELECT count(*) FROM access_grants", [], |row| row.get::<_, u64>(0))?, 1);
            if failure.contains("synthetic_effect") {
                assert_eq!(live.query_row("SELECT count(*) FROM synthetic_effect", [], |row| row.get::<_, u64>(0))?, 0);
            }
            assert_eq!(fs::read(&snapshot_db)?, original_snapshot);
            verify_backup(root, &snapshot.id, &keys)?;
            assert!(!root.join(RECOVERY_FILE).exists());
            for entry in fs::read_dir(root)? {
                assert!(!entry?.file_name().to_string_lossy().starts_with(".snapshot-restore-"));
            }
        }
        Ok(())
    }

    #[test]
    fn runtime_startup_rollback_uses_private_original_not_portable_backup() -> Result<()> {
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let selected = create(root, "account", "baseline", &keys)?;
        db.execute(
            "INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')",
            [],
        )?;
        drop(db);
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let receipt = restore_locked(root, "account", &keys, &selected.id, &ownership)?;
        rollback_locked(root, &receipt, &ownership)?;
        let original = Connection::open(root.join("rsrs.db"))?;
        assert_eq!(
            original.query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, u64>(0))?,
            2
        );
        assert_eq!(
            original.query_row("SELECT count(*) FROM access_grants", [], |r| r
                .get::<_, u64>(0))?,
            1
        );
        assert!(ensure_sync_allowed(root).is_ok());
        Ok(())
    }

    #[test]
    fn damaged_unrelated_snapshot_does_not_block_healthy_recovery() -> Result<()> {
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let healthy = create(root, "account", "healthy", &keys)?;
        let damaged = create(root, "account", "damaged", &keys)?;
        fs::write(
            root.join("snapshots").join(format!("{}.db", damaged.id)),
            "corrupt",
        )?;
        drop(db);
        let report = list_report(root)?;
        assert_eq!(report.snapshots.len(), 1);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].id, damaged.id);
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let receipt = restore_locked(root, "account", &keys, &healthy.id, &ownership)?;
        complete_restore(root, &receipt)?;
        assert_eq!(list_report(root)?.problems.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn live_database_symlinks_are_rejected_for_creation_restore_and_rollback() -> Result<()> {
        use std::os::unix::fs::symlink;
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let snapshot = create(root, "account", "baseline", &keys)?;
        drop(db);
        let live = root.join("rsrs.db");
        let external = root.join("original-target.db");
        fs::rename(&live, &external)?;
        symlink(&external, &live)?;
        let before = fs::read(&external)?;
        assert!(create(root, "account", "reject link", &keys).is_err());
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        assert!(restore_locked(root, "account", &keys, &snapshot.id, &ownership).is_err());
        assert_eq!(fs::read(&external)?, before);
        assert!(fs::symlink_metadata(&live)?.file_type().is_symlink());
        fs::remove_file(&live)?;
        fs::rename(&external, &live)?;
        let receipt = restore_locked(root, "account", &keys, &snapshot.id, &ownership)?;
        fs::rename(&live, &external)?;
        symlink(&external, &live)?;
        assert!(rollback_locked(root, &receipt, &ownership).is_err());
        assert!(root.join(&receipt.rollback_file).is_file());
        assert!(fs::symlink_metadata(&live)?.file_type().is_symlink());
        fs::remove_file(&live)?;
        fs::rename(&external, &live)?;
        rollback_locked(root, &receipt, &ownership)?;
        Ok(())
    }

    fn assert_normal_open_is_blocked_in_fresh_process(root: &Path) -> Result<()> {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "snapshot::tests::interrupted_restore_has_recoverable_original_image",
                "--test-threads=1",
            ])
            .env("RSRS_SNAPSHOT_FAULT_CHILD_ROOT", root)
            .env("RSRS_SNAPSHOT_FAULT_MODE", "probe")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "normal entry probe failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    #[test]
    fn corrupt_staged_database_cannot_be_marked_ready_and_original_is_recoverable() -> Result<()> {
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let snapshot = create(root, "account", "baseline", &keys)?;
        drop(db);
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let receipt = restore_locked(root, "account", &keys, &snapshot.id, &ownership)?;
        fs::write(root.join("rsrs.db"), b"damaged database after replacement")?;
        assert!(ensure_no_incomplete_restore(root).is_err());
        // SQLite itself rejects the staged file; no readiness can be based on it.
        assert!(Connection::open(root.join("rsrs.db"))?
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .is_err());
        rollback_locked(root, &receipt, &ownership)?;
        let original = Connection::open(root.join("rsrs.db"))?;
        assert_eq!(
            original.query_row("SELECT count(*) FROM access_grants", [], |r| r
                .get::<_, u64>(0))?,
            1
        );
        Ok(())
    }

    #[test]
    fn verified_write_failure_keeps_all_normal_entries_closed_until_rollback() -> Result<()> {
        for failure in ["before_verified_write", "after_verified_write"] {
            let (directory, db) = fixture()?;
            let root = directory.path();
            let keys = crate::SessionKeys::from_urk([7; 32])?;
            let snapshot = create(root, "account", "baseline", &keys)?;
            db.execute(
                "INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')",
                [],
            )?;
            drop(db);
            let ownership =
                crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
            let receipt = restore_locked(root, "account", &keys, &snapshot.id, &ownership)?;
            assert!(commit_with_hook(root, &receipt, &|phase| {
                anyhow::ensure!(
                    phase != failure,
                    "injected verified persistence I/O failure"
                );
                Ok(())
            })
            .is_err());
            assert_normal_open_is_blocked_in_fresh_process(root)?;
            rollback_locked(root, &receipt, &ownership)?;
            let original = Connection::open(root.join("rsrs.db"))?;
            assert_eq!(
                original.query_row("SELECT count(*) FROM memories", [], |row| row
                    .get::<_, u64>(0))?,
                2
            );
            assert_eq!(
                original.query_row("SELECT count(*) FROM access_grants", [], |row| row
                    .get::<_, u64>(0))?,
                1
            );
        }
        Ok(())
    }

