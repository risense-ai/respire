    #[test]
    fn interrupted_restore_has_recoverable_original_image() -> Result<()> {
        const CHILD: &str = "RSRS_SNAPSHOT_FAULT_CHILD_ROOT";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = std::path::PathBuf::from(root);
            std::env::set_var("RSRS_DATA_DIR", &root);
            let mode = std::env::var("RSRS_SNAPSHOT_FAULT_MODE")?;
            if mode == "probe" {
                for result in [
                    crate::service::open_store().map(|_| ()),
                    crate::service::open_store_with_profile().map(|_| ()),
                    crate::service::initialize_runtime_store().map(|_| ()),
                ] {
                    let error = result
                        .err()
                        .context("normal library entry accepted pending restore")?;
                    anyhow::ensure!(
                        error.to_string().contains("snapshot"),
                        "unexpected open failure: {error}"
                    );
                }
                return Ok(());
            }
            if mode == "write" {
                use crate::transport::MemoryTransport;
                let store = crate::service::open_store()?;
                let pending =
                    crate::StoredMemory::new_pending("new-after-commit".into(), "account".into());
                anyhow::ensure!(store.put(&pending)?, "post-commit write was not accepted");
                return Ok(());
            }
            let id = std::env::var("RSRS_SNAPSHOT_FAULT_ID")?;
            let ownership =
                crate::lock::LibraryLock::acquire(&root, std::time::Duration::from_secs(1))?;
            let receipt = restore_with_hook(
                &root,
                "account",
                &crate::SessionKeys::from_urk([7; 32])?,
                &id,
                &ownership,
                &|stage| {
                    if stage == mode {
                        std::process::exit(77);
                    }
                    Ok(())
                },
            )?;
            commit_restore(&root, &receipt)?;
            if mode == "after_commit" {
                std::process::exit(77);
            }
            clear_with_hook(&root, &receipt, &|stage| {
                if stage == mode {
                    std::process::exit(77);
                }
                Ok(())
            })?;
            anyhow::bail!("interruption hook was not reached");
        }
        for phase in [
            "after_replace",
            "after_installed",
            "after_commit",
            "after_gate_removed",
        ] {
            let (directory, db) = fixture()?;
            let root = directory.path();
            let keys = crate::SessionKeys::from_urk([7; 32])?;
            let snapshot = create(root, "account", "baseline", &keys)?;
            db.execute(
                "INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')",
                [],
            )?;
            drop(db);
            let output = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "snapshot::tests::interrupted_restore_has_recoverable_original_image",
                    "--test-threads=1",
                ])
                .env(CHILD, root)
                .env("RSRS_SNAPSHOT_FAULT_ID", &snapshot.id)
                .env("RSRS_SNAPSHOT_FAULT_MODE", phase)
                .output()?;
            assert_eq!(
                output.status.code(),
                Some(77),
                "child did not reach {phase}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if phase != "after_gate_removed" {
                assert_normal_open_is_blocked_in_fresh_process(root)?;
            } else {
                assert!(ensure_no_incomplete_restore(root).is_ok());
                let output = std::process::Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "snapshot::tests::interrupted_restore_has_recoverable_original_image",
                        "--test-threads=1",
                    ])
                    .env(CHILD, root)
                    .env("RSRS_SNAPSHOT_FAULT_MODE", "write")
                    .output()?;
                anyhow::ensure!(
                    output.status.success(),
                    "normal post-commit write failed: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let ownership =
                crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
            recover_locked(root, "account", &ownership)?;
            let current = Connection::open(root.join("rsrs.db"))?;
            let rolled_back = matches!(phase, "after_replace" | "after_installed");
            assert_eq!(
                current.query_row("SELECT count(*) FROM access_grants", [], |row| row
                    .get::<_, u64>(0))?,
                if rolled_back { 1 } else { 0 },
                "{phase}"
            );
            assert_eq!(
                current.query_row(
                    "SELECT count(*) FROM memories WHERE id='later'",
                    [],
                    |row| row.get::<_, u64>(0)
                )?,
                if rolled_back { 1 } else { 0 },
                "{phase}"
            );
            if phase == "after_gate_removed" {
                assert_eq!(
                    current.query_row(
                        "SELECT count(*) FROM memories WHERE id='new-after-commit'",
                        [],
                        |row| row.get::<_, u64>(0)
                    )?,
                    1,
                    "recovery discarded accepted post-commit write"
                );
            }
            assert!(ensure_no_incomplete_restore(root).is_ok());
        }
        Ok(())
    }

    #[test]
    fn busy_restore_preserves_live_database_and_prebackup() -> Result<()> {
        let (directory, db) = fixture()?;
        let root = directory.path();
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let manifest = create(root, "account", "baseline", &keys)?;
        db.execute(
            "INSERT INTO memories(id,ciphertext,nonce) VALUES('pending','','')",
            [],
        )?;
        let reader = Connection::open(root.join("rsrs.db"))?;
        reader.execute_batch("BEGIN; SELECT * FROM memories;")?;
        db.execute(
            "INSERT INTO memories(id,ciphertext,nonce) VALUES('later','','')",
            [],
        )?;
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        assert!(restore_locked(root, "account", &keys, &manifest.id, &ownership).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, u64>(0))?,
            3
        );
        assert!(
            list(root)?.len() >= 2,
            "mandatory pre-restore backup missing"
        );
        assert!(root.join("rsrs.db-wal").exists(), "live WAL removed");
        reader.execute_batch("ROLLBACK")?;
        Ok(())
    }

    #[test]
    fn legacy_manifest_without_witness_fields_remains_valid_for_pre_witness_database() -> Result<()>
    {
        let (directory, _db) = fixture()?;
        let snapshot = create(
            directory.path(),
            "account",
            "legacy",
            &crate::SessionKeys::from_urk([7; 32])?,
        )?;
        assert!(snapshot.witness_head.is_none());
        let path = directory
            .path()
            .join("snapshots")
            .join(format!("{}.json", snapshot.id));
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let fields = old.as_object_mut().context("manifest object")?;
        for name in ["witness_head", "witness_count", "witness_intact"] {
            fields.remove(name);
        }
        fs::write(&path, serde_json::to_vec(&old)?)?;
        assert_eq!(list(directory.path())?.len(), 1);
        Ok(())
    }

