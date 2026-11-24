    use super::*;

    fn sealed(keys: &SessionKeys, content: &str, stamp: &str) -> Result<StoredMemory> {
        let entry: crate::MemoryEntry = serde_json::from_value(
            serde_json::json!({"id":"entry","kind":"Context","tags":[],
            "title":"record","content":content,"user":"u","computer":"test","project":"sync",
            "created_at":"2026-09-19T00:00:00.000Z","updated_at":stamp,"emotion":-1.0,"parent_id":""}),
        )?;
        crate::MemoryEngine::seal(
            keys,
            &crate::memory::search::HashingEmbedder::default(),
            &entry,
            "u",
        )
    }

    fn fixture() -> Result<(tempfile::TempDir, LocalStore, SessionKeys)> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("local.db"))?;
        store.begin_sync_epoch("epoch")?;
        store.meta_set("sync_v2_resolution_support", "1")?;
        let keys = SessionKeys::from_urk([7; 32])?;
        let old = sealed(&keys, "current", "2026-09-19T00:00:00.001Z")?;
        let distinct = sealed(&keys, "unique old edit", "2026-09-19T00:00:00.002Z")?;
        let current = sealed(&keys, "current", "2026-09-19T00:00:00.003Z")?;
        store.apply_page(
            &keys,
            &Page {
                epoch: "epoch".into(),
                until: 3,
                cursor: 3,
                has_more: false,
                total: 1,
                alive: 1,
                changes: vec![
                    Change {
                        rev: 1,
                        status: "legacy_rejected".into(),
                        op_id: None,
                        blob: old,
                    },
                    Change {
                        rev: 2,
                        status: "legacy_rejected".into(),
                        op_id: None,
                        blob: distinct,
                    },
                    Change {
                        rev: 3,
                        status: "applied".into(),
                        op_id: None,
                        blob: current,
                    },
                ],
            },
            false,
        )?;
        Ok((dir, store, keys))
    }

    #[test]
    fn sync_only_equivalent_legacy_versions_are_automatically_processed() -> Result<()> {
        let (_dir, store, keys) = fixture()?;
        store.classify_conflicts(&keys, true)?;
        let outgoing = store.outgoing_resolutions()?;
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].conflict_rev, 1);
        assert_eq!(store.sync_counts()?.1, 2);
        let record = Resolution {
            seq: 1,
            conflict_rev: 1,
            id: "entry".into(),
            action: "equivalent".into(),
            head_rev: 3,
            restore_op_id: None,
            processed_at: "now".into(),
        };
        let reply = ResolveReply {
            results: vec![ResolutionResult {
                conflict_rev: 1,
                outcome: "processed".into(),
                resolution: Some(record.clone()),
            }],
        };
        store.acknowledge_resolutions("epoch", &outgoing, &reply)?;
        store.acknowledge_resolutions("epoch", &outgoing, &reply)?;
        store.apply_resolution_page(&ResolutionPage {
            epoch: "epoch".into(),
            cursor: 1,
            until: 1,
            has_more: false,
            resolutions: vec![record],
        })?;
        store.maintain_sync_storage()?;
        assert_eq!(store.sync_counts()?.1, 1);
        assert_eq!(store.conflict_metrics()?, (2, 1, 0, 0));
        let all = store.list_conflicts(&keys, None, true)?;
        assert!(all
            .iter()
            .any(|row| row["rev"] == 1 && row["payload_reclaimed"] == true));
        let pending = store.list_conflicts(&keys, None, false)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["kind"], "needs_review");
        assert_eq!(
            pending[0]["candidate"]["entry"]["content"],
            "unique old edit"
        );
        Ok(())
    }

    #[test]
    fn sync_merge_waits_for_content_acceptance_and_conflicted_restore_stays_open() -> Result<()> {
        let (_dir, store, keys) = fixture()?;
        store.queue_conflict_resolution(
            &keys,
            "epoch",
            2,
            3,
            "merge",
            Some("current plus unique old edit"),
        )?;
        assert!(store.outgoing_resolutions()?.is_empty());
        let operations = store.outgoing()?;
        assert_eq!(operations.len(), 1);
        assert_eq!(
            MemoryEngine::open(&keys, &operations[0].blob)?.content,
            "current plus unique old edit"
        );
        store.acknowledge(
            &operations,
            &[Receipt {
                op_id: operations[0].op_id.clone(),
                status: "conflict_saved".into(),
                stored_rev: 4,
                head_rev: 3,
            }],
            false,
        )?;
        store.maintain_sync_storage()?;
        let decisions = store.outgoing_resolutions()?;
        assert_eq!(decisions.len(), 1);
        store.acknowledge_resolutions(
            "epoch",
            &decisions,
            &ResolveReply {
                results: vec![ResolutionResult {
                    conflict_rev: 2,
                    outcome: "stale".into(),
                    resolution: None,
                }],
            },
        )?;
        assert_eq!(store.processed_conflict_action("epoch", 2)?, None);
        assert_eq!(store.sync_counts()?.1, 2);
        assert!(store.outgoing_resolutions()?.is_empty());
        Ok(())
    }

    #[test]
    fn sync_keep_current_stale_head_and_epoch_reset_do_not_fake_completion() -> Result<()> {
        let (_dir, store, keys) = fixture()?;
        store.queue_conflict_resolution(&keys, "epoch", 2, 3, "keep_current", None)?;
        let decisions = store.outgoing_resolutions()?;
        assert_eq!(decisions[0].expected_head_rev, 3);
        store.acknowledge_resolutions(
            "epoch",
            &decisions,
            &ResolveReply {
                results: vec![ResolutionResult {
                    conflict_rev: 2,
                    outcome: "stale".into(),
                    resolution: None,
                }],
            },
        )?;
        assert_eq!(store.sync_counts()?.1, 2);
        store.reset_sync_snapshot()?;
        store.begin_sync_epoch("new")?;
        assert_eq!(store.sync_counts()?.1, 0);
        assert_eq!(store.conflict_metrics()?, (2, 0, 2, 0));
        assert!(store
            .queue_conflict_resolution(&keys, "epoch", 2, 3, "keep_current", None)
            .is_err());
        Ok(())
    }

    #[test]
    fn sync_take_incoming_preserves_delete_intent() -> Result<()> {
        let (_dir, store, keys) = fixture()?;
        let mut deleted = sealed(&keys, "delete me", "2026-09-19T00:00:00.001Z")?;
        deleted.deleted = true;
        store.apply_page(
            &keys,
            &Page {
                epoch: "epoch".into(),
                until: 4,
                cursor: 4,
                has_more: false,
                total: 1,
                alive: 1,
                changes: vec![Change {
                    rev: 4,
                    status: "conflict_saved".into(),
                    op_id: None,
                    blob: deleted,
                }],
            },
            false,
        )?;
        store.queue_conflict_resolution(&keys, "epoch", 4, 3, "take_incoming", None)?;
        let outgoing = store.outgoing()?;
        assert_eq!(outgoing.len(), 1);
        assert!(outgoing[0].blob.deleted);
        assert_eq!(store.processed_conflict_action("epoch", 4)?, None);
        Ok(())
    }

    #[test]
    fn sync_reviewed_head_must_match_before_creating_a_merge() -> Result<()> {
        let (_dir, store, keys) = fixture()?;
        assert!(store
            .queue_conflict_resolution(
                &keys,
                "epoch",
                2,
                2,
                "merge",
                Some("based on an obsolete view")
            )
            .is_err());
        assert!(store.outgoing()?.is_empty());
        assert!(store.outgoing_resolutions()?.is_empty());
        assert_eq!(store.sync_counts()?.1, 2);
        Ok(())
    }
    include!("../conflict_recovery_tests.rs");
