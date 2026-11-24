    use super::*;

    pub(super) fn sample(id: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            user: "alice".to_owned(),
            ciphertext: "aabbcc".to_owned(),
            nonce: "112233".to_owned(),
            embedding_enc: String::new(),
            updated_at: "2026-09-02T00:00:00.000Z".to_owned(),
            deleted: false,
            local_kind: "context".to_owned(),
            local_tags: "rust,\u{52a0}\u{5bc6}".to_owned(),
            local_title: "\u{6807}\u{9898}".to_owned(),
            local_project: "respire".to_owned(),
            local_computer: "pc1".to_owned(),
            local_embedding: Some(
                [0.1f32, 0.2, 0.3]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
            local_parent_id: String::new(),
            local_created_at: "2026-09-02T00:00:00.000Z".to_owned(),
            local_content_head: "\u{65e7}\u{5907}\u{4efd}\u{5e93}\u{5173}\u{7cfb}\u{8fb9} \u{6f14}\u{793a}\u{5185}\u{5bb9}".to_owned(),
            local_recall_count: 0,
            local_importance: "normal".to_owned(),
            local_device: "test-dev".to_owned(),
            local_chunks: Vec::new(),
            local_artifact: Vec::new(),
            local_modified_by: "test-dev".to_owned(),
            local_meta: Default::default(),
        }
    }

    #[test]
    fn put_all_forget_lww() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;

        // put a new row
        assert!(MemoryTransport::put(&store, &sample("id-1"))?);
        assert_eq!(MemoryTransport::count(&store)?, 1);

        store.meta_set("memory_metadata_v1_enabled", "1")?;
        store.connection.execute(
            "UPDATE memories SET memory_contract=1,summary='kept-summary',review_at='2026-12-01T00:00:00.000Z',
             artifact='{\"path\":\"artifact.txt\",\"repo\":null,\"commit\":null,\"purpose\":null}' WHERE id='id-1'", [],
        )?;
        store.meta_set("memory_metadata_v1_enabled", "0")?;
        let metadata_before: String = store.connection.query_row(
            "SELECT json_array(memory_contract,summary,source_refs,valid_until,review_at,artifact,verification)
             FROM memories WHERE id='id-1'", [], |row| row.get(0),
        )?;
        let outgoing_before = store.outgoing()?.len();
        assert!(store.connection.execute(
            "INSERT OR REPLACE INTO memories(id,ciphertext) VALUES('id-1','replacement')", [],
        ).is_err());
        assert_eq!(store.outgoing()?.len(), outgoing_before);
        store.connection.execute("UPDATE memories SET dirty=0 WHERE id='id-1'", [])?;

        // The actual put path, outbox trigger and review ledger share the outer transaction.
        let current_meta = store.all(true)?[0].local_meta.clone();
        let outgoing_before = store.outgoing()?.len();
        let failed: anyhow::Result<()> = store.write_transaction(|| {
            let mut changed = sample("id-1");
            changed.local_meta = current_meta.clone();
            changed.ciphertext = "changed".to_owned();
            changed.updated_at = "2026-09-04T00:00:00.000Z".to_owned();
            assert!(store.put_inner(&changed, true)?);
            store.connection.execute(
                "INSERT INTO review_history(entry_id,record_revision,action,actor,happened_at)
                 VALUES('id-1','target-revision','reschedule','test','now')", [],
            )?;
            anyhow::bail!("simulated review failure");
        });
        assert!(failed.is_err());
        assert_eq!(MemoryTransport::all(&store, false)?[0].ciphertext, "aabbcc");
        assert_eq!(store.outgoing()?.len(), outgoing_before);
        assert_eq!(store.connection.query_row(
            "SELECT COUNT(*) FROM review_history", [], |row| row.get::<_, i64>(0),
        )?, 0);

        // LWW: an older version does not overwrite
        let mut stale = sample("id-1");
        stale.updated_at = "2026-01-01T00:00:00.000Z".to_owned();
        stale.local_title = "\u{65e7}\u{6807}\u{9898}".to_owned();
        assert!(!MemoryTransport::put(&store, &stale)?);
        let all = MemoryTransport::all(&store, false)?;
        assert_eq!(all[0].local_title, "\u{6807}\u{9898}");

        // a newer version overwrites
        let mut fresh = sample("id-1");
        fresh.updated_at = "2026-09-03T00:00:00.000Z".to_owned();
        fresh.local_title = "\u{65b0}\u{6807}\u{9898}".to_owned();
        fresh.local_meta = current_meta.clone();
        assert!(MemoryTransport::put(&store, &fresh)?);
        let all = MemoryTransport::all(&store, false)?;
        assert_eq!(all[0].local_title, "\u{65b0}\u{6807}\u{9898}");
        assert_eq!(metadata_before, store.connection.query_row(
            "SELECT json_array(memory_contract,summary,source_refs,valid_until,review_at,artifact,verification)
             FROM memories WHERE id='id-1'", [], |row| row.get::<_, String>(0),
        )?);

        // tombstone: after forget, count=0, but all(true) still sees it
        assert!(MemoryTransport::forget(&store, "id-1")?);
        assert_eq!(MemoryTransport::count(&store)?, 0);
        assert!(MemoryTransport::all(&store, false)?.is_empty());
        assert_eq!(MemoryTransport::all(&store, true)?.len(), 1);

        // Legacy feature bytes are not returned through the public storage layer.
        let all = MemoryTransport::all(&store, true)?;
        assert!(all[0].local_embedding.is_none());
        let outgoing_before = store.outgoing()?.len();
        store.write_transaction(|| {
            assert!(store.put_inner(&sample("id-2"), true)?);
            store.connection.execute(
                "INSERT INTO review_history(entry_id,record_revision,action,actor,happened_at)
                 VALUES('id-2','target-revision','reschedule','test','now')", [],
            )?;
            Ok(())
        })?;
        assert_eq!(store.outgoing()?.len(), outgoing_before + 1);
        assert_eq!(store.connection.query_row(
            "SELECT COUNT(*) FROM review_history", [], |row| row.get::<_, i64>(0),
        )?, 1);
        let history = store.review_history("id-2", 1)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].entry_id, "id-2");
        assert_eq!(history[0].seq, Some(1));
        assert!(store.review_history("id-1", 20)?.is_empty());
        assert!(store.review_history("", 20).is_err());
        assert!(store.review_history("id-2", 0).is_err());
        assert!(store.review_history("id-2", 201).is_err());

        let mut reviewed = sample("id-2");
        reviewed.ciphertext = "newly sealed review".into();
        reviewed.updated_at = "2026-11-01T00:00:00.000Z".into();
        reviewed.local_meta.memory_contract = 1;
        reviewed.local_meta.review_at = Some("2026-12-01T00:00:00.000Z".into());
        let review = ReviewHistoryRow {
            seq: None, entry_id: reviewed.id.clone(),
            record_revision: format!("sha256:{}", "a".repeat(64)),
            action: "reschedule".into(), actor: r#"{"kind":"owner_client","id":"alice"}"#.into(),
            happened_at: "2026-11-01T00:00:00.000Z".into(),
            previous_verification: None, next_verification: None,
            previous_review_at: None, next_review_at: reviewed.local_meta.review_at.clone(), reason: None,
        };
        store.connection.execute(
            "INSERT INTO core_artifacts(memory_id,model,source,artifact)
             VALUES('id-2','review-rollback','aabbcc',x'0102')", [],
        )?;
        let before_review = store.memory_revision()?;
        let outgoing_before = store.outgoing()?.len();
        assert!(store.commit_review(&before_review, &reviewed, &review).is_err());
        assert_eq!(store.memory_revision()?, before_review);
        store.meta_set("memory_metadata_v1_enabled", "1")?;
        assert!(store.commit_review(&"0".repeat(32), &reviewed, &review).is_err());
        let mut invalid = review.clone();
        invalid.next_review_at = None;
        assert!(store.commit_review(&before_review, &reviewed, &invalid).is_err());
        invalid = review.clone();
        invalid.previous_review_at = Some("2026-10-01T00:00:00.000Z".into());
        assert!(store.commit_review(&before_review, &reviewed, &invalid).is_err());
        invalid = review.clone();
        invalid.action = "unsupported".into();
        assert!(store.commit_review(&before_review, &reviewed, &invalid).is_err());
        invalid.action = "revoke".into();
        invalid.reason = Some("  ".into());
        assert!(store.commit_review(&before_review, &reviewed, &invalid).is_err());
        store.connection.execute_batch(
            "CREATE TEMP TRIGGER fail_review_history BEFORE INSERT ON review_history
             BEGIN SELECT RAISE(ABORT,'simulated review history write failure'); END;",
        )?;
        assert!(store.commit_review(&before_review, &reviewed, &review).is_err());
        assert_eq!(store.memory_revision()?, before_review);
        assert_eq!(store.outgoing()?.len(), outgoing_before);
        assert_eq!(store.review_history("id-2", 20)?.len(), 1);
        let unchanged = store.all(false)?.into_iter().find(|row| row.id == "id-2")
            .ok_or_else(|| anyhow::anyhow!("missing review target"))?;
        assert_eq!(unchanged.ciphertext, "aabbcc");
        assert_eq!(unchanged.local_meta, Default::default());
        assert_eq!(store.connection.query_row(
            "SELECT artifact FROM core_artifacts WHERE memory_id='id-2' AND model='review-rollback'",
            [], |row| row.get::<_, Vec<u8>>(0),
        )?, vec![1, 2]);
        store.connection.execute_batch("DROP TRIGGER fail_review_history;")?;
        store.commit_review(&before_review, &reviewed, &review)?;
        assert_ne!(store.memory_revision()?, before_review);
        assert_eq!(store.outgoing()?.len(), outgoing_before + 1);
        let saved = store.all(false)?.into_iter().find(|row| row.id == "id-2")
            .ok_or_else(|| anyhow::anyhow!("missing saved review target"))?;
        assert_eq!(saved.ciphertext, reviewed.ciphertext);
        assert_eq!(saved.local_meta, reviewed.local_meta);
        let saved_history = store.review_history("id-2", 1)?;
        let mut expected_history = review.clone();
        expected_history.seq = Some(2);
        assert_eq!(saved_history, vec![expected_history]);
        let after_review = store.memory_revision()?;
        let outgoing_after = store.outgoing()?.len();
        assert!(store.commit_review(&before_review, &reviewed, &review).is_err());
        let mut older = reviewed.clone();
        older.updated_at = "2026-10-01T00:00:00.000Z".into();
        older.ciphertext = "stale review payload".into();
        let mut next_review = review.clone();
        next_review.previous_review_at = review.next_review_at.clone();
        assert!(store.commit_review(&after_review, &older, &next_review).is_err());
        assert_eq!(store.memory_revision()?, after_review);
        assert_eq!(store.outgoing()?.len(), outgoing_after);
        assert_eq!(store.review_history("id-2", 20)?.len(), 2);
        let (_, delta, _) = store.retrieval_delta(None)?;
        assert!(delta.iter().any(|row| row.id == "id-2" && row.local_meta == reviewed.local_meta));
        let snapshot = store.review_snapshot()?;
        assert_eq!(snapshot.memory_revision, store.memory_revision()?);
        assert!(snapshot.memories.iter().any(|row| row.id == "id-1" && row.deleted));
        assert!(snapshot.unavailable_ids.is_empty());
        let head = serde_json::to_string(&reviewed)?;
        store.connection.execute(
            "INSERT INTO sync_remote_heads(id,rev,wire) VALUES('id-2',1,?1)", [&head],
        )?;
        assert!(store.review_snapshot()?.unavailable_ids.is_empty());
        let mut pending = reviewed.clone();
        pending.ciphertext = "unmaterialized head".into();
        store.connection.execute(
            "UPDATE sync_remote_heads SET wire=?1 WHERE id='id-2'",
            [serde_json::to_string(&pending)?],
        )?;
        assert_eq!(store.review_snapshot()?.unavailable_ids, vec!["id-2"]);
        store.connection.execute("INSERT INTO sync_base(id,rev) VALUES('id-2',1)", [])?;
        assert!(store.review_snapshot()?.unavailable_ids.is_empty());
        store.connection.execute("UPDATE sync_remote_heads SET decode_error='unavailable key' WHERE id='id-2'", [])?;
        assert_eq!(store.review_snapshot()?.unavailable_ids, vec!["id-2"]);
        Ok(())
    }

    #[test]
    fn max_updated_at_cursor() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        assert_eq!(MemoryTransport::max_updated_at(&store)?, None);
        let mut a = sample("a");
        a.updated_at = "2026-09-02T01:00:00.000Z".to_owned();
        MemoryTransport::put(&store, &a)?;
        let mut b = sample("b");
        b.updated_at = "2026-09-02T02:00:00.000Z".to_owned();
        MemoryTransport::put(&store, &b)?;
        assert_eq!(
            MemoryTransport::max_updated_at(&store)?.as_deref(),
            Some("2026-09-02T02:00:00.000Z")
        );
        Ok(())
    }

    #[test]
    fn v1_schema_is_preserved_and_requires_legacy_export() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("old.db");
        // The historical demo has a different KDF and encrypts content directly.
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ciphertext TEXT NOT NULL,
                nonce TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'context',
                tags TEXT NOT NULL DEFAULT '',
                title TEXT NOT NULL DEFAULT '',
                tag_hashes TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO memories(ciphertext,nonce,kind,tags,title,created_at,updated_at)
            VALUES ('original-ciphertext','original-nonce','context','legacy','original-title','created','updated');",
        )?;
        drop(conn);
        let error = LocalStore::open(&path)
            .err()
            .ok_or_else(|| anyhow::anyhow!("legacy demo was unexpectedly opened"))?;
        assert!(error
            .to_string()
            .contains("unsupported legacy memory schema"));
        let preserved = Connection::open(&path)?;
        let original: (i64, String, String, String) = preserved.query_row(
            "SELECT id,ciphertext,nonce,title FROM memories",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            original,
            (
                1,
                "original-ciphertext".to_owned(),
                "original-nonce".to_owned(),
                "original-title".to_owned()
            )
        );
        assert_eq!(
            preserved.query_row("SELECT COUNT(*) FROM memories", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        assert!(preserved
            .prepare("SELECT tag_hashes FROM memories")?
            .exists([])?);
        Ok(())
    }

    #[test]
    fn text_schema_adds_missing_indexes_without_changing_encrypted_rows() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY, user TEXT NOT NULL, ciphertext TEXT NOT NULL,
                nonce TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                title TEXT NOT NULL
            );
            INSERT INTO memories VALUES ('kept-id','kept-user','original-ciphertext','original-nonce','created','updated','kept-title');",
        )?;
        migrate(&connection)?;
        migrate(&connection)?;
        let kept: (String, String, String, String, String, String, String) = connection.query_row(
            "SELECT id,user,ciphertext,nonce,created_at,updated_at,title FROM memories",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        assert_eq!(
            kept,
            (
                "kept-id".to_owned(),
                "kept-user".to_owned(),
                "original-ciphertext".to_owned(),
                "original-nonce".to_owned(),
                "created".to_owned(),
                "updated".to_owned(),
                "kept-title".to_owned()
            )
        );
        let derived: (String, Option<Vec<u8>>, i64) = connection.query_row(
            "SELECT embedding_enc,embedding,dirty FROM memories",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(derived, (String::new(), None, 1));
        let metadata: (i64, String, Option<String>) = connection.query_row(
            "SELECT memory_contract,source_refs,verification FROM memories",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(metadata, (0, "[]".to_owned(), None));
        let store = LocalStore { connection };
        assert!(!store.has_memory_metadata()?);
        assert!(!store.memory_metadata_v1_enabled()?);
        for assignment in [
            "memory_contract=1", "summary=''", "source_refs='[{\"id\":\"source\"}]'",
            "valid_until='later'", "review_at='later'", "artifact='{}'", "verification='{}'",
            "kind='hypothesis'", "kind='reflection'", "kind='self'",
        ] {
            store.meta_set("memory_metadata_v1_enabled", "1")?;
            let transaction = store.connection.unchecked_transaction()?;
            store.connection.execute(&format!("UPDATE memories SET {assignment}"), [])?;
            store.meta_set("memory_metadata_v1_enabled", "0")?;
            assert!(store.has_memory_metadata()?, "undetected metadata: {assignment}");
            transaction.rollback()?;
            assert!(!store.has_memory_metadata()?);
        }
        store.connection.execute(
            "INSERT INTO review_history(entry_id,record_revision,action,actor,happened_at)
             VALUES('deleted-entry','revision','reschedule','owner_client','now')", [],
        )?;
        assert!(store.has_memory_metadata()?);
        store.connection.execute("DELETE FROM review_history", [])?;
        assert!(!store.has_memory_metadata()?);
        store.meta_set("memory_metadata_v1_enabled", "0")?;
        assert!(!store.memory_metadata_v1_enabled()?);
        assert!(store.connection.execute(
            "UPDATE memories SET memory_contract=1 WHERE id='kept-id'", [],
        ).is_err());
        store.meta_set("memory_metadata_v1_enabled", "invalid")?;
        assert!(store.memory_metadata_v1_enabled().is_err());
        assert!(store.connection.execute(
            "UPDATE memories SET review_at='later' WHERE id='kept-id'", [],
        ).is_err());
        store.meta_set("memory_metadata_v1_enabled", "1")?;
        assert!(store.memory_metadata_v1_enabled()?);
        let before = store.connection.query_row(
            "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get::<_, String>(0),
        )?;
        store.connection.execute("UPDATE memories SET review_at='later' WHERE id='kept-id'", [])?;
        assert_ne!(before, store.connection.query_row(
            "SELECT value FROM meta WHERE key='memory_revision_v1'", [], |row| row.get::<_, String>(0),
        )?);
        store.meta_set("memory_metadata_v1_enabled", "0")?;
        assert!(!store.memory_metadata_v1_enabled()?);
        store.connection.execute("UPDATE memories SET dirty=0 WHERE id='kept-id'", [])?;
        assert!(store.connection.execute(
            "UPDATE memories SET review_at=NULL WHERE id='kept-id'", [],
        ).is_err());
        store.connection.execute("DROP TABLE review_history", [])?;
        assert!(store.has_memory_metadata().is_err());
        store.connection.execute("DROP TABLE meta", [])?;
        assert!(store.memory_metadata_v1_enabled().is_err());
        let connection = store.connection;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM memories", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    }

    #[test]
    fn schema_upgrade_rolls_back_if_existing_query_log_is_unsupported() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY, user TEXT NOT NULL, ciphertext TEXT NOT NULL,
                nonce TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            INSERT INTO memories VALUES ('kept-id','kept-user','original-ciphertext','original-nonce','created','updated');
            CREATE TABLE query_log (id INTEGER PRIMARY KEY, ts_unix INTEGER, hit_ids TEXT);",
        )?;
        assert!(migrate(&connection).is_err());
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(memories)")?
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(!columns.iter().any(|column| column == "embedding_enc"));
        assert!(!columns.iter().any(|column| column == "memory_contract"));
        assert!(!connection.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='review_history'",
        )?.exists([])?);
        let original: (String, String) =
            connection.query_row("SELECT ciphertext,nonce FROM memories", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(
            original,
            (
                "original-ciphertext".to_owned(),
                "original-nonce".to_owned()
            )
        );
        assert!(connection.prepare("SELECT hit_ids FROM query_log").is_ok());
        Ok(())
    }

    #[test]
    fn causal_tree_children_parent_chain() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        // root A + child B + grandchild C (causal: A cause → B effect-and-cause → C effect)
        let mut a = sample("a");
        a.local_parent_id = String::new();
        MemoryTransport::put(&store, &a)?;
        let mut b = sample("b");
        b.local_parent_id = "a".to_owned();
        MemoryTransport::put(&store, &b)?;
        let mut c = sample("c");
        c.local_parent_id = "b".to_owned();
        MemoryTransport::put(&store, &c)?;
        // root/child/grandchild queries
        let roots = store.roots()?;
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "a");
        let kids_b = store.children("b")?;
        assert_eq!(kids_b.len(), 1);
        assert_eq!(kids_b[0].id, "c");
        // cause-chain walk (root first)
        let chain = store.ancestor_chain("c")?;
        assert_eq!(chain, vec!["a".to_owned(), "b".to_owned()]);
        // set_parent (demote/promote bottom): rehang B under root A (already under A) → same parent, no change;
        // hanging B under C should be allowed here (cycle check is command-layer); this only checks set_parent takes
        assert!(store.set_parent("b", "a")?);
        let chain_b = store.ancestor_chain("b")?;
        assert_eq!(chain_b, vec!["a".to_owned()]);
        // cycle evidence: ancestor_chain can be used by the command layer to refuse a descendant attach
        let chain_self = store.ancestor_chain("a")?;
        assert!(chain_self.is_empty());
        Ok(())
    }

    #[test]
    fn grant_create_list_snapshot_revoke() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        let root = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let mut m = sample(root);
        m.id = root.to_owned();
        MemoryTransport::put(&store, &m)?;
        let mut child = sample("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff");
        child.local_parent_id = root.to_owned();
        MemoryTransport::put(&store, &child)?;
        assert!(store.create_grant(root, "").is_err());
        assert!(store.create_grant("not-a-uuid", "lab").is_err());
        let (grant, token) = store.create_grant(root, "lab")?;
        assert_eq!(grant.label, "lab");
        assert_eq!(store.list_grants()?.len(), 1);
        let snap = store
            .grant_snapshot(&token)?
            .ok_or_else(|| anyhow::anyhow!("snapshot"))?;
        assert_eq!(snap.len(), 2);
        assert!(store.revoke_grant(&grant.id[..8])?);
        assert!(store.grant_snapshot(&token)?.is_none());
        assert!(!store.revoke_grant(&grant.id)?);
        Ok(())
    }
