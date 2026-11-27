    use super::*;
    use crate::transport::MemoryTransport;

    #[test]
    fn encrypted_history_restore_uses_explicit_old_epoch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let s = LocalStore::open(&dir.path().join("a.db"))?;
        let keys = SessionKeys::from_urk([9; 32])?;
        let entry: crate::MemoryEntry = serde_json::from_value(serde_json::json!({
            "id":"entry","kind":"Context","tags":[],"title":"first","content":"retained original",
            "user":"u","computer":"test","project":"sync","created_at":"2026-09-19T00:00:00.001Z",
            "updated_at":"2026-09-19T00:00:00.001Z","emotion":-1.0,"parent_id":""
        }))?;
        let stored = crate::MemoryEngine::seal(
            &keys,
            &crate::memory::search::HashingEmbedder::default(),
            &entry,
            "u",
        )?;
        s.begin_sync_epoch("e")?;
        s.apply_page(&keys, &page(1, stored), false)?;
        s.maintain_sync_storage()?;
        s.reset_sync_snapshot()?;
        s.maintain_sync_storage()?;
        s.begin_sync_epoch("new")?;
        let mut new_page = page(1, tomb("entry", "2026-09-19T00:00:00.002Z"));
        new_page.epoch = "new".into();
        s.apply_page(&keys, &new_page, false)?;
        s.maintain_sync_storage()?;
        assert!(s.restore_sync_version(&keys, None, Some(1), None).is_err());
        assert_eq!(
            s.restore_sync_version(&keys, None, Some(1), Some("e"))?,
            "entry"
        );
        let outgoing = s.outgoing()?;
        assert_eq!(outgoing.len(), 1);
        assert_eq!(
            crate::MemoryEngine::open(&keys, &outgoing[0].blob)?.content,
            "retained original"
        );
        assert_eq!(outgoing[0].base_rev, Some(1));
        Ok(())
    }

    fn tomb(id: &str, ts: &str) -> StoredMemory {
        let mut b = StoredMemory::new_pending(id.to_owned(), "u".to_owned());
        b.updated_at = ts.to_owned();
        b.deleted = true;
        b
    }

    fn page(rev: i64, b: StoredMemory) -> Page {
        Page {
            epoch: "e".into(),
            until: rev,
            cursor: rev,
            has_more: false,
            changes: vec![Change {
                rev,
                status: "applied".into(),
                op_id: None,
                blob: b,
            }],
            total: 1,
            alive: 0,
        }
    }

    #[test]
    fn outbox_ack_cannot_clear_later_save_and_survives_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("local.db");
        let s = LocalStore::open(&path)?;
        s.put(&tomb("a", "2026-09-19T00:00:00.001Z"))?;
        let first = s.outgoing()?;
        s.put(&tomb("a", "2026-09-19T00:00:00.002Z"))?;
        let receipt = Receipt {
            op_id: first[0].op_id.clone(),
            status: "applied".into(),
            stored_rev: 1,
            head_rev: 1,
        };
        s.acknowledge(&first, &[receipt], false)?;
        assert_eq!(s.all_dirty()?.len(), 1);
        let remaining = s.outgoing()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].parent_op_id.as_deref(),
            Some(first[0].op_id.as_str())
        );
        s.maintain_sync_storage()?;
        assert_eq!(s.sync_history(None)?.len(), 2);
        drop(s);
        let reopened = LocalStore::open(&path)?;
        assert_eq!(reopened.outgoing()?[0].op_id, remaining[0].op_id);
        reopened.acknowledge(
            &remaining,
            &[Receipt {
                op_id: remaining[0].op_id.clone(),
                status: "applied".into(),
                stored_rev: 2,
                head_rev: 2,
            }],
            false,
        )?;
        reopened.maintain_sync_storage()?;
        assert!(reopened.outgoing()?.is_empty());
        assert!(reopened.sync_history(None)?.is_empty());
        Ok(())
    }

    #[test]
    fn pending_and_undecodable_heads_do_not_advance_editing_base() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let s = LocalStore::open(&dir.path().join("a.db"))?;
        let keys = SessionKeys::from_urk([7; 32])?;
        s.begin_sync_epoch("e")?;
        s.apply_page(
            &keys,
            &page(1, tomb("a", "2026-09-19T00:00:00.001Z")),
            false,
        )?;
        s.put(&tomb("a", "2026-09-19T00:00:00.003Z"))?;
        let outgoing = s.outgoing()?;
        s.apply_page(
            &keys,
            &page(2, tomb("a", "2026-09-19T00:00:00.002Z")),
            false,
        )?;
        let base: i64 =
            s.connection
                .query_row("SELECT rev FROM sync_base WHERE id='a'", [], |r| r.get(0))?;
        assert_eq!(base, 1);
        s.acknowledge(
            &outgoing,
            &[Receipt {
                op_id: outgoing[0].op_id.clone(),
                status: "conflict_saved".into(),
                stored_rev: 3,
                head_rev: 2,
            }],
            false,
        )?;
        // Re-entering with no pending operation must finish interrupted materialization.
        s.materialize_received(&keys)?;
        assert_eq!(
            s.updated_at_of("a")?.as_deref(),
            Some("2026-09-19T00:00:00.002Z")
        );
        let mut broken = tomb("a", "2026-09-19T00:00:00.004Z");
        broken.deleted = false;
        broken.ciphertext = "broken".into();
        broken.nonce = "broken".into();
        s.apply_page(&keys, &page(4, broken), false)?;
        let base: i64 =
            s.connection
                .query_row("SELECT rev FROM sync_base WHERE id='a'", [], |r| r.get(0))?;
        assert_eq!(base, 2);
        assert_eq!(s.sync_counts()?.2, 1);
        Ok(())
    }

    #[test]
    fn bad_page_rolls_back_versions_and_cursor() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let s = LocalStore::open(&dir.path().join("a.db"))?;
        let keys = SessionKeys::from_urk([7; 32])?;
        s.begin_sync_epoch("e")?;
        let mut p = page(2, tomb("a", "2026-09-19T00:00:00.002Z"));
        p.changes.push(p.changes[0].clone());
        assert!(s.apply_page(&keys, &p, false).is_err());
        assert_eq!(s.meta_get("sync_v2_cursor")?, None);
        assert!(s.all(true)?.is_empty());
        assert!(s.sync_history(None)?.is_empty());
        Ok(())
    }

    #[test]
    fn index_update_is_local_and_reset_keeps_pending_history() -> Result<()> {
        let dir = tempfile::tempdir()?;
        #[cfg(windows)]
        let profile = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(dir.path())?;
        #[cfg(not(windows))]
        let profile = dir.path().canonicalize()?;
        let path = profile.join("a.db");
        let s = LocalStore::open(&path)?;
        let mut b = tomb("a", "2026-09-19T00:00:00.001Z");
        b.deleted = false;
        b.ciphertext = "source".into();
        s.put(&b)?;
        let op = s.outgoing()?[0].op_id.clone();
        assert!(s.set_artifact("a", b"locator", "source")?);
        assert!(!s.set_artifact("a", b"locator", "stale")?);
        assert_eq!(s.outgoing()?.len(), 1);
        s.begin_sync_epoch("old")?;
        s.reset_sync_snapshot()?;
        s.begin_sync_epoch("new")?;
        let pending = s.outgoing()?;
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].op_id, op);
        assert!(s.sync_history(None)?.len() >= 2);

        let keys = SessionKeys::from_urk([9; 32])?;
        let mut remote = page(1, tomb("remote", "2026-09-19T00:00:00.001Z"));
        remote.epoch = "new".into();
        s.apply_page(&keys, &remote, false)?;
        let expected_memories = serde_json::to_string(&s.all(true)?)?;
        let expected_pending = serde_json::to_string(&pending)?;
        let expected_revision = s.meta_get("memory_revision_v1")?;
        s.connection.execute_batch(
            "PRAGMA user_version=0;
            ALTER TABLE sync_remote_heads RENAME TO previous_heads;
            CREATE TABLE sync_remote_heads(id TEXT PRIMARY KEY,rev INTEGER NOT NULL);
            INSERT INTO sync_remote_heads SELECT id,rev FROM previous_heads;
            DROP TABLE previous_heads;
            WITH RECURSIVE versions(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM versions WHERE n<1200)
            INSERT INTO sync_inbox(epoch,rev,id,status,wire)
                SELECT 'new',n+10,'history','applied',hex(zeroblob(8192)) FROM versions;
            WITH RECURSIVE versions(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM versions WHERE n<1200)
            INSERT INTO sync_outbox(op_id,id,user,ciphertext,nonce,updated_at,deleted,state)
                SELECT 'confirmed-'||n,'history','',hex(zeroblob(8192)),'','old',0,'applied' FROM versions;
            INSERT INTO sync_inbox(epoch,rev,id,status,wire)
                VALUES('new',2,'conflict','conflict_saved','{}'),
                      ('new',3,'processed','conflict_saved','{}');
            INSERT INTO sync_inbox(epoch,rev,id,status,wire,decode_error)
                VALUES('new',4,'undecodable','applied','{}','key unavailable');
            INSERT INTO sync_resolutions(epoch,conflict_rev,seq,id,action,head_rev,processed_at)
                VALUES('new',3,1,'processed','keep_current',1,'now');
            PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        let read_only = LocalStore::open_read_only(&path)?;
        assert_eq!(read_only.sync_history(Some("remote"))?.len(), 1);
        drop(read_only);
        let before = std::fs::metadata(&path)?.len();
        drop(s);
        let reader = rusqlite::Connection::open(&path)?;
        reader.execute_batch("BEGIN; SELECT count(*) FROM memories;")?;
        let reopened = LocalStore::open(&path)?;
        assert!(std::fs::metadata(&path)?.len() >= before / 2);
        reader.execute_batch("ROLLBACK")?;
        drop(reader);
        reopened.maintain_sync_storage()?;
        assert!(std::fs::metadata(&path)?.len() < before / 2);
        assert_eq!(
            serde_json::to_string(&reopened.all(true)?)?,
            expected_memories
        );
        assert_eq!(
            serde_json::to_string(&reopened.outgoing()?)?,
            expected_pending
        );
        // Backfilling the newly available remote wire changes review CAS inputs.
        let upgraded_revision = reopened.memory_revision()?;
        assert_ne!(Some(&upgraded_revision), expected_revision.as_ref());
        assert_eq!(reopened.sync_counts()?, (1, 1, 1));
        assert_eq!(reopened.conflict_metrics()?, (2, 1, 0, 0));
        assert_eq!(reopened.meta_get("sync_v2_cursor")?.as_deref(), Some("1"));
        let retained: i64 =
            reopened
                .connection
                .query_row("SELECT count(*) FROM sync_inbox", [], |r| r.get(0))?;
        assert_eq!(retained, 2);
        let head: String = reopened.connection.query_row(
            "SELECT wire FROM sync_remote_heads WHERE id='remote'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(head, serde_json::to_string(&remote.changes[0].blob)?);
        assert!(reopened
            .sync_history(Some("remote"))?
            .iter()
            .any(|row| row["rev"] == 1));
        reopened.maintain_sync_storage()?;
        assert_eq!(
            serde_json::to_string(&reopened.outgoing()?)?,
            expected_pending
        );
        assert_eq!(reopened.memory_revision()?, upgraded_revision);
        Ok(())
    }

    #[test]
    fn legacy_parent_is_reissued_before_first_v2_sync() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let s = LocalStore::open(&dir.path().join("a.db"))?;
        s.put(&tomb("a", "2026-09-19T00:00:00.001Z"))?;
        let first = s.outgoing()?;
        s.put(&tomb("a", "2026-09-19T00:00:00.002Z"))?;
        s.acknowledge(
            &first,
            &[Receipt {
                op_id: first[0].op_id.clone(),
                status: "applied".into(),
                stored_rev: 0,
                head_rev: 0,
            }],
            true,
        )?;
        let before = s.outgoing()?;
        assert!(before[0].parent_op_id.is_some());
        s.begin_sync_epoch("new")?;
        let after = s.outgoing()?;
        assert_eq!(after.len(), 1);
        assert!(after[0].parent_op_id.is_none());
        assert!(after[0].base_rev.is_none());
        assert_ne!(before[0].op_id, after[0].op_id);
        assert!(s.sync_history(None)?.len() >= 3);
        Ok(())
    }
