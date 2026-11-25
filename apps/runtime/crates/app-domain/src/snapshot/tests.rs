    use super::*;

    fn controlled_receipt() -> RestoreReceipt {
        RestoreReceipt {
            restored: "a".repeat(32),
            before_restore: "b".repeat(32),
            sync_quarantined: true,
            rollback_file: format!(".snapshot-rollback-{}.db", "c".repeat(32)),
            rollback_sha256: "d".repeat(64),
            account_fingerprint: format!("{:x}", Sha256::digest(b"account")),
            operation: Some(SnapshotOperation {
                operation_id: uuid::Uuid::new_v4().to_string(),
                request_fingerprint: "e".repeat(64),
            }),
            legacy_journal_sha256: None,
        }
    }

    #[test]
    fn legacy_receipts_remain_unbound_and_controlled_replay_never_restores() -> Result<()> {
        let receipt = controlled_receipt();
        let mut legacy = serde_json::to_value(RecoveryReceipt::from(&receipt))?;
        legacy.as_object_mut().context("receipt must be an object")?.remove("operation");
        let legacy: RecoveryReceipt = serde_json::from_value(legacy)?;
        assert!(legacy.operation.is_none());

        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let operation = receipt.operation.as_ref().context("controlled operation missing")?;
        assert!(restore_operation_locked(root, "account", None, &receipt.restored, operation, &ownership, false)?.is_none());
        write_record(root, &receipt, "installed", MAINTENANCE_FILE)?;
        write_journal(root, &receipt, "installed")?;
        for first_attempt in [false, true] {
            let status = restore_operation_locked(root, "account", None, &receipt.restored, operation, &ownership, first_attempt)?
                .context("original operation missing")?;
            assert_eq!(status.phase, "installed");
            assert_eq!(status.receipt.before_restore, receipt.before_restore);
        }
        assert!(!root.join("rsrs.db").exists());
        let mut changed = operation.clone();
        changed.request_fingerprint = "f".repeat(64);
        assert!(operation_status(root, "account", &changed).is_err());
        assert!(operation_status(root, "other-account", operation).is_err());
        write_journal(root, &receipt, "verified")?;
        release_restore(root, &receipt)?;
        let status = restore_operation_locked(root, "account", None, &receipt.restored, operation, &ownership, true)?
            .context("released operation missing")?;
        assert_eq!(status.phase, "released");
        assert!(!root.join("rsrs.db").exists());

        let mut second = controlled_receipt();
        second.restored = "b".repeat(32);
        second.rollback_file = format!(".snapshot-rollback-{}.db", "f".repeat(32));
        write_record(root, &second, "prepared", MAINTENANCE_FILE)?;
        write_journal(root, &second, "verified")?;
        release_restore(root, &second)?;
        let before_retry = fs::read(root.join(RECOVERY_FILE))?;
        for first_attempt in [false, true] {
            let status = restore_operation_locked(root, "account", None, &receipt.restored, operation, &ownership, first_attempt)?
                .context("historical operation missing")?;
            assert_eq!(status.phase, "released");
            assert_eq!(status.receipt.before_restore, receipt.before_restore);
            assert!(status.receipt.rollback_file.is_empty());
            assert!(status.receipt.rollback_sha256.is_empty());
            assert!(!status.cleanup_pending);
        }
        let error = restore_operation_locked(root, "account", None, &receipt.restored, &changed, &ownership, true)
            .err().context("historical fingerprint conflict was accepted")?;
        assert!(error.downcast_ref::<OperationConflict>().is_some());
        assert_eq!(fs::read(root.join(RECOVERY_FILE))?, before_retry);
        assert!(!root.join("rsrs.db").exists());

        let legacy_root = root.join("legacy");
        let ownership = crate::lock::LibraryLock::acquire(&legacy_root, std::time::Duration::from_secs(1))?;
        let mut legacy = controlled_receipt();
        legacy.operation = None;
        write_record(&legacy_root, &legacy, "prepared", MAINTENANCE_FILE)?;
        write_journal(&legacy_root, &legacy, "verified")?;
        let expected = describe_legacy_recovery(&legacy_root, "account")?;
        let operation = SnapshotOperation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            request_fingerprint: "a".repeat(64),
        };
        assert!(legacy_recovery_status(&legacy_root, "account", &operation, &expected.journal_sha256)?.is_none());
        let result = recover_legacy_locked(&legacy_root, "account", &expected, &operation, &ownership)?;
        assert_eq!(result.phase, "released");
        assert!(!result.cleanup_pending);
        let persisted = fs::read(legacy_root.join(RECOVERY_FILE))?;
        assert_eq!(legacy_recovery_status(&legacy_root, "account", &operation, &expected.journal_sha256)?
            .context("lost-response recovery result missing")?.phase, "released");
        assert_eq!(recover_legacy_locked(&legacy_root, "account", &expected, &operation, &ownership)?.phase, "released");
        assert_eq!(fs::read(legacy_root.join(RECOVERY_FILE))?, persisted);
        assert!(!legacy_root.join("rsrs.db").exists());
        assert!(legacy_recovery_status(&legacy_root, "other-account", &operation, &expected.journal_sha256).is_err());
        assert!(legacy_recovery_status(&legacy_root, "account", &operation, &"f".repeat(64)).is_err());
        let changed = SnapshotOperation { request_fingerprint: "b".repeat(64), ..operation };
        assert!(legacy_recovery_status(&legacy_root, "account", &changed, &expected.journal_sha256).is_err());
        Ok(())
    }

    #[test]
    fn controlled_cleanup_retains_terminal_identity_and_prefers_new_gate() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let mut receipt = controlled_receipt();
        receipt.legacy_journal_sha256 = Some("e".repeat(64));
        let operation = receipt.operation.as_ref().context("controlled operation missing")?;
        write_record(root, &receipt, "prepared", MAINTENANCE_FILE)?;
        write_journal(root, &receipt, "verified")?;
        fs::write(root.join(&receipt.rollback_file), b"private rollback placeholder")?;
        assert!(clear_with_hook(root, &receipt, &|_| anyhow::bail!("injected cleanup failure")).is_err());
        let status = operation_status(root, "account", operation)?.context("terminal operation missing")?;
        assert_eq!(status.phase, "released");
        assert!(status.cleanup_pending);
        let legacy_status = legacy_recovery_status(root, "account", operation, &"e".repeat(64))?
            .context("legacy cleanup result missing")?;
        assert_eq!(legacy_status.phase, "released");
        assert!(legacy_status.cleanup_pending && legacy_status.cleanup_warning.is_some());
        release_restore(root, &receipt)?;
        let status = operation_status(root, "account", operation)?.context("released operation missing")?;
        assert_eq!(status.phase, "released");
        assert!(!status.cleanup_pending);
        assert!(ensure_no_incomplete_restore(root).is_ok());
        assert!(runtime_bootstrap_target(root)?.is_none());
        let mut next = controlled_receipt();
        next.rollback_file = format!(".snapshot-rollback-{}.db", "f".repeat(32));
        write_record(root, &next, "prepared", MAINTENANCE_FILE)?;
        let journal = read_journal(root)?.context("new gate missing")?;
        assert_eq!(journal.phase, "prepared");
        assert_eq!(journal.receipt.operation, next.operation);
        assert_eq!(operation_status(root, "account", operation)?.context("historical operation missing during gate publication")?.phase, "released");
        write_journal(root, &next, "verified")?;
        release_restore(root, &next)?;
        for _ in 2..MAX_RECORDED_OPERATIONS {
            let mut following = controlled_receipt();
            following.rollback_file = format!(".snapshot-rollback-{}.db", uuid::Uuid::new_v4().simple());
            write_record(root, &following, "prepared", MAINTENANCE_FILE)?;
            write_journal(root, &following, "verified")?;
            release_restore(root, &following)?;
        }
        let journal_bytes = fs::read(root.join(RECOVERY_FILE))?;
        let document: serde_json::Value = serde_json::from_slice(&journal_bytes)?;
        let history = document["history"].as_array().context("history must be an array")?;
        assert_eq!(history.len(), MAX_RECORDED_OPERATIONS - 1);
        for entry in history {
            assert!(entry.get("rollback_file").is_none());
            assert!(entry.get("rollback_sha256").is_none());
        }
        let ownership = crate::lock::LibraryLock::acquire(root, std::time::Duration::from_secs(1))?;
        let keys = crate::SessionKeys::from_urk([7; 32])?;
        let overflow = controlled_receipt();
        let overflow_operation = overflow.operation.as_ref().context("controlled operation missing")?;
        let error = restore_operation_locked(root, "account", Some(&keys), &overflow.restored, overflow_operation, &ownership, true)
            .err().context("full history accepted a new restore")?;
        assert!(error.to_string().contains("history is full"));
        assert_eq!(fs::read(root.join(RECOVERY_FILE))?, journal_bytes);
        assert!(!root.join(MAINTENANCE_FILE).exists());
        assert!(!root.join("rsrs.db").exists());
        let original = restore_operation_locked(root, "account", None, &receipt.restored, operation, &ownership, true)?
            .context("full history lost the original operation")?;
        assert_eq!(original.phase, "released");
        let mut conflict = operation.clone();
        conflict.request_fingerprint = "f".repeat(64);
        assert!(operation_status(root, "account", &conflict).is_err());
        Ok(())
    }
include!("tests/backup_integrity.rs");
#[cfg(target_os = "linux")]
include!("tests/archive_offline_boundary.rs");
include!("tests/rollback.rs");
include!("tests/interruption.rs");
include!("tests/corruption.rs");
