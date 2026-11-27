    #[test]
    fn corrupted_or_mismatched_snapshot_fails_closed() -> Result<()> {
        let (directory, _db) = fixture()?;
        let manifest = create(
            directory.path(),
            "synthetic-account",
            "",
            &crate::SessionKeys::from_urk([7; 32])?,
        )?;
        let database = directory
            .path()
            .join("snapshots")
            .join(format!("{}.db", manifest.id));
        fs::write(database, b"damaged")?;
        assert!(list(directory.path()).is_err());

        let (directory, db, snapshot) = legacy_fixture(&["when_to_use"], None)?;
        assert!(snapshot.witness_head.is_some());
        let manifest_path = directory.path().join("snapshots").join(format!("{}.json", snapshot.id));
        let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        manifest["witness_head"] = serde_json::json!("tampered");
        fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
        drop(db);
        let ownership = crate::lock::LibraryLock::acquire(
            directory.path(), std::time::Duration::from_secs(1),
        )?;
        let error = match restore_locked(
            directory.path(), "account", &crate::SessionKeys::from_urk([7; 32])?,
            &snapshot.id, &ownership,
        ) {
            Ok(_) => panic!("restore accepted a mismatched witness"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("witness checkpoint mismatch"));
        let live = Connection::open(directory.path().join("rsrs.db"))?;
        assert_eq!(live.query_row("SELECT count(*) FROM memories", [], |row| row.get::<_, u64>(0))?, 1);
        assert!(!directory.path().join(RECOVERY_FILE).exists());

        let (directory, db, snapshot) = legacy_fixture(&["when_to_use"], None)?;
        let root = directory.path();
        let snapshot_db = root.join("snapshots").join(format!("{}.db", snapshot.id));
        db.execute("INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')", [])?;
        let revision: String = db.query_row(
            "SELECT value FROM meta WHERE key='memory_revision_v1'",
            [],
            |row| row.get(0),
        )?;
        drop(db);
        let ownership =
            crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let error = match restore_with_hook(
            root,
            "account",
            &crate::SessionKeys::from_urk([7; 32])?,
            &snapshot.id,
            &ownership,
            &|stage| {
                if stage == "after_backups" {
                    let mut changed = fs::read(&snapshot_db)?;
                    changed.push(0);
                    fs::write(&snapshot_db, changed)?;
                }
                Ok(())
            },
        ) {
            Ok(_) => panic!("restore accepted a snapshot changed after validation"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("snapshot changed during restore preparation"));
        let live = Connection::open(root.join("rsrs.db"))?;
        assert_eq!(
            live.query_row("SELECT count(*) FROM memories", [], |row| row.get::<_, u64>(0))?,
            2
        );
        assert_eq!(
            live.query_row(
                "SELECT value FROM meta WHERE key='memory_revision_v1'",
                [],
                |row| row.get::<_, String>(0),
            )?,
            revision
        );
        assert!(!root.join(RECOVERY_FILE).exists());
        for entry in fs::read_dir(root)? {
            assert!(!entry?.file_name().to_string_lossy().starts_with(".snapshot-restore-"));
        }
        Ok(())
    }
    #[test]
    fn invalid_source_and_destination_do_not_publish_partial_snapshot() -> Result<()> {
        let directory = tempfile::tempdir()?;
        assert!(create(
            directory.path(),
            "account",
            "",
            &crate::SessionKeys::from_urk([7; 32])?
        )
        .is_err());
        assert!(!directory.path().join("rsrs.db").exists());
        let (directory, _db) = fixture()?;
        fs::write(directory.path().join("snapshots"), "user-owned")?;
        assert!(create(
            directory.path(),
            "account",
            "",
            &crate::SessionKeys::from_urk([7; 32])?
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("snapshots"))?,
            "user-owned"
        );
        Ok(())
    }
    #[test]
    fn failed_integrity_cleans_only_its_own_files() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = Connection::open(directory.path().join("rsrs.db"))?;
        source.execute_batch("CREATE TABLE unrelated(id TEXT);")?;
        assert!(create(
            directory.path(),
            "account",
            "",
            &crate::SessionKeys::from_urk([7; 32])?
        )
        .is_err());
        assert_eq!(fs::read_dir(directory.path().join("snapshots"))?.count(), 0);
        assert_eq!(
            source.query_row("SELECT count(*) FROM unrelated", [], |r| r.get::<_, u64>(0))?,
            0
        );
        Ok(())
    }
