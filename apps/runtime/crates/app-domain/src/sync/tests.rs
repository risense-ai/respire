    use super::*;
    use crate::memory::model::Kind;
    use crate::memory::search::HashingEmbedder;
    use crate::memory::{MemoryEngine, MemoryEntry, MemoryQuery};

    fn entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id: id.to_owned(),
            kind: Kind::Context,
            tags: vec!["t".to_owned()],
            title: id.to_owned(),
            content: content.to_owned(),
            user: "u".to_owned(),
            computer: "c".to_owned(),
            project: "p".to_owned(),
            created_at: "2026-09-02T00:00:00.000Z".to_owned(),
            updated_at: "2026-09-02T00:00:00.000Z".to_owned(),
            emotion: -1.0,
            parent_id: String::new(),
            importance: "normal".to_owned(),
            device: "test-dev".to_owned(),
            modified_by: "test-dev".to_owned(),
        }
    }

    #[test]
    fn put_synced_keeps_local_artifact_when_ciphertext_unchanged() -> anyhow::Result<()> {
        // A same-content pull must preserve the local Core index locator.
        let keys = SessionKeys::from_urk([42u8; 32])?;
        let dir = tempfile::tempdir()?;
        let local = LocalStore::open(&dir.path().join("local.db"))?;
        let embedder = HashingEmbedder::default();
        let e = entry("keep-1", "\u{4fdd}\u{7559}\u{5411}\u{91cf}");
        let stored = MemoryEngine::seal(&keys, &embedder, &e, "local")?;
        assert!(!stored.local_artifact.is_empty());
        local.put(&stored)?;

        // Simulate a remote pull: same ciphertext, no vector, updated_at newer
        let mut from_remote = stored.clone();
        from_remote.local_embedding = None;
        from_remote.local_artifact.clear();
        from_remote.embedding_enc = String::new();
        from_remote.updated_at = "2027-01-01T00:00:00.000Z".to_owned();
        assert!(local.put_synced(&from_remote)?);

        let got = local
            .all(true)?
            .into_iter()
            .find(|m| m.id == stored.id)
            .ok_or_else(|| anyhow!("not found"))?;
        assert!(
            got.local_artifact == stored.local_artifact,
            "an unchanged source must retain its local Core index locator"
        );
        Ok(())
    }

    #[test]
    fn put_synced_drops_local_artifact_when_content_changed() -> anyhow::Result<()> {
        // A changed source invalidates the old local Core index locator.
        let keys = SessionKeys::from_urk([43u8; 32])?;
        let dir = tempfile::tempdir()?;
        let local = LocalStore::open(&dir.path().join("local.db"))?;
        let embedder = HashingEmbedder::default();
        let e = entry("chg-1", "\u{539f}\u{5185}\u{5bb9}");
        let stored = MemoryEngine::seal(&keys, &embedder, &e, "local")?;
        local.put(&stored)?;

        let mut changed = entry("chg-1", "\u{88ab}\u{4ed6}\u{7aef}\u{6539}\u{8fc7}\u{7684}\u{65b0}\u{5185}\u{5bb9}");
        changed.updated_at = "2027-01-01T00:00:00.000Z".to_owned();
        let mut blob = MemoryEngine::seal(&keys, &embedder, &changed, "local")?;
        blob.local_embedding = None; // Remote records carry no local feature copy.
        blob.local_artifact.clear();
        blob.embedding_enc = String::new();
        assert!(local.put_synced(&blob)?);

        let got = local
            .all(true)?
            .into_iter()
            .find(|m| m.id == stored.id)
            .ok_or_else(|| anyhow!("not found"))?;
        assert!(
            got.local_artifact.is_empty(),
            "a changed source must be indexed again from its decrypted entry"
        );
        Ok(())
    }

    #[test]
    fn merge_blobs_lww_skips_stale_remote() -> anyhow::Result<()> {
        let keys = SessionKeys::from_urk([42u8; 32])?;
        let dir = tempfile::tempdir()?;
        let local = LocalStore::open(&dir.path().join("local.db"))?;
        let embedder = HashingEmbedder::default();
        let e = entry("lww-1", "LWW \u{5185}\u{5bb9}");
        let stored = MemoryEngine::seal(&keys, &embedder, &e, "local")?;
        local.put(&stored)?;
        // Local updated_at=T; remote blob older → skip
        let mut stale = stored.clone();
        stale.updated_at = "2026-01-01T00:00:00.000Z".to_owned();
        let pulled = merge_blobs(&keys, &local, vec![stale])?;
        assert_eq!(pulled, 0, "\u{65e7}\u{526f}\u{672c}\u{4e0d}\u{5f97}\u{8986}\u{76d6}\u{672c}\u{5730}\u{65b0}\u{72b6}\u{6001}");
        assert_eq!(local.all(false)?.len(), 1);
        // Remote blob newer (same id, new payload: new title, updated_at newer) → store
        let mut fresh = e.clone();
        fresh.title = "LWW \u{65b0}\u{6807}\u{9898}".to_owned();
        fresh.updated_at = "2027-01-01T00:00:00.000Z".to_owned();
        let fresh_blob = MemoryEngine::seal(&keys, &embedder, &fresh, "local")?;
        let pulled = merge_blobs(&keys, &local, vec![fresh_blob])?;
        assert_eq!(pulled, 1);
        assert_eq!(local.all(false)?[0].local_title, "LWW \u{65b0}\u{6807}\u{9898}");
        Ok(())
    }

    fn remote_configured_flag() -> anyhow::Result<()> {
        // Isolate: temp HOME (no session.json), only test the env-var branch
        let saved_home = crate::env::var("HOME").ok();
        let dir = tempfile::tempdir()?;
        std::env::set_var("HOME", dir.path());
        let saved = crate::env::var("RSRS_ADDR").ok();
        std::env::remove_var("RSRS_ADDR");
        assert!(!remote_configured());
        std::env::set_var("RSRS_ADDR", "http://x");
        assert!(remote_configured());
        // restore
        match saved {
            Some(v) => std::env::set_var("RSRS_ADDR", v),
            None => std::env::remove_var("RSRS_ADDR"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        Ok(())
    }

    /// End-to-end: two local stores as two devices, via a fake remote, verify sync merge and tombstone.
    #[test]
    fn sync_merges_two_devices() -> anyhow::Result<()> {
        // Local store as "cloud": device A writes → sync to hub → device B sync pulls
        let dir = tempfile::tempdir()?;
        let keys = SessionKeys::from_urk([7u8; 32])?;
        let embedder = HashingEmbedder::default();

        // Hub store (stands in for the cloud)
        let cloud = LocalStore::open(&dir.path().join("cloud.db"))?;

        // Device A: write two rows
        let a = LocalStore::open(&dir.path().join("a.db"))?;
        let e1 = entry("a-1", "Rust \u{6240}\u{6709}\u{6743} \u{5185}\u{5b58}\u{5b89}\u{5168}");
        let s1 = MemoryEngine::seal(&keys, &embedder, &e1, "u")?;
        a.put(&s1)?;
        let e2 = entry("a-2", "Python \u{6570}\u{636e}\u{5206}\u{6790}");
        let s2 = MemoryEngine::seal(&keys, &embedder, &e2, "u")?;
        a.put(&s2)?;

        // Device A sync → hub
        let cloud_transport = fake_remote(&cloud);
        let stats = sync_all(&keys, &a, &cloud_transport)?;
        assert_eq!(stats.pushed, 2);

        // Device B: empty store, sync ← hub
        let b = LocalStore::open(&dir.path().join("b.db"))?;
        let cloud_transport2 = fake_remote(&cloud);
        let stats = sync_all(&keys, &b, &cloud_transport2)?;
        assert_eq!(stats.pulled, 2);

        // B can semantically recall what A wrote (HashingEmbedder vectors differ from BGE semantics,
        // Core may omit test fixtures; this checks successful recall after sync, not retrieval quality.
        let all = b.all(false)?;
        let q = MemoryQuery::new("\u{5185}\u{5b58}\u{5b89}\u{5168} \u{501f}\u{7528}").limit(5);
        let ranked = MemoryEngine::recall_local(&keys, &embedder, &all, &q)?;
        if !ranked.is_empty() {
            assert_eq!(ranked[0].id, "a-1");
        }

        // Device B deletes one → sync → device A pulls the tombstone and the row is gone
        b.forget("a-2")?;
        let cloud_transport3 = fake_remote(&cloud);
        sync_all(&keys, &b, &cloud_transport3)?;
        let cloud_transport4 = fake_remote(&cloud);
        sync_all(&keys, &a, &cloud_transport4)?;
        let a_all = a.all(false)?;
        assert!(a_all.iter().all(|m| m.id != "a-2"));
        Ok(())
    }

    #[test]
    fn snapshot_quarantine_refuses_sync_before_any_remote_callback() -> Result<()> {
        struct UnreachableRemote;
        impl MemoryTransport for UnreachableRemote {
            fn capabilities(&self) -> Result<Option<crate::transport::protocol::Capabilities>> {
                panic!("quarantined remote capabilities")
            }
            fn put(&self, _: &StoredMemory) -> Result<bool> {
                panic!("quarantined push")
            }
            fn all(&self, _: bool) -> Result<Vec<StoredMemory>> {
                panic!("quarantined pull")
            }
            fn max_updated_at(&self) -> Result<Option<String>> {
                panic!("quarantined cursor")
            }
            fn forget(&self, _: &str) -> Result<bool> {
                panic!("quarantined delete")
            }
            fn count(&self) -> Result<i64> {
                panic!("quarantined count")
            }
        }
        let root = tempfile::tempdir()?;
        let local = LocalStore::open(&root.path().join("rsrs.db"))?;
        local.meta_set("snapshot_sync_quarantine_v1", "synthetic")?;
        let error = sync_all(&SessionKeys::from_urk([7; 32])?, &local, &UnreachableRemote)
            .err()
            .ok_or_else(|| anyhow::anyhow!("quarantined sync succeeded"))?;
        anyhow::ensure!(
            error.to_string().contains("quarantined"),
            "unexpected sync error: {error}"
        );
        Ok(())
    }

    /// Wrap a local store as a "remote" (fake cloud for tests).
    fn fake_remote(store: &LocalStore) -> FakeRemote<'_> {
        FakeRemote(store)
    }

    struct FakeRemote<'a>(&'a LocalStore);

    impl MemoryTransport for FakeRemote<'_> {
        fn put(&self, memory: &StoredMemory) -> Result<bool> {
            self.0.put(memory)
        }
        fn all(&self, include_deleted: bool) -> Result<Vec<StoredMemory>> {
            self.0.all(include_deleted)
        }
        fn max_updated_at(&self) -> Result<Option<String>> {
            self.0.max_updated_at()
        }
        fn forget(&self, id: &str) -> Result<bool> {
            self.0.forget(id)
        }
        fn count(&self) -> Result<i64> {
            self.0.count()
        }
    }

    /// Batch-push failure path: first batch ok, second fails — succeeded batch is undirtied, leftover stays dirty for a retry.
    #[test]
    fn sync_push_batch_failure_keeps_uncleaned_dirty() -> anyhow::Result<()> {
        let keys = SessionKeys::from_urk([42u8; 32])?;
        let dir = tempfile::tempdir()?;
        let embedder = HashingEmbedder::default();
        let cloud = LocalStore::open(&dir.path().join("cloud.db"))?;
        let a = LocalStore::open(&dir.path().join("a.db"))?;
        for i in 0..101 {
            let e = entry(&format!("bulk-{i}"), &format!("\u{6279}\u{91cf}\u{6761}\u{76ee} {i}"));
            let s = MemoryEngine::seal(&keys, &embedder, &e, "u")?;
            a.put(&s)?;
        }
        let remote = FlakyBatchRemote {
            cloud: &cloud,
            calls: std::cell::Cell::new(0),
        };
        let err = sync_all(&keys, &a, &remote).unwrap_err();
        assert!(err.to_string().contains("batch 2 failed"));
        // First batch (100 rows) pushed and undirtied; leftover 1 stays dirty for the next push
        assert_eq!(a.all_dirty()?.len(), 1);
        assert_eq!(cloud.count()?, 100);
        // Retry: a healthy remote finishes and takes the leftover
        let stats = sync_all(&keys, &a, &fake_remote(&cloud))?;
        assert_eq!(stats.pushed, 1);
        assert_eq!(cloud.count()?, 101);
        Ok(())
    }

    struct FlakyBatchRemote<'a> {
        cloud: &'a LocalStore,
        calls: std::cell::Cell<usize>,
    }

    impl MemoryTransport for FlakyBatchRemote<'_> {
        fn put(&self, memory: &StoredMemory) -> Result<bool> {
            self.cloud.put(memory)
        }
        fn put_batch(&self, memories: &[StoredMemory]) -> Result<Vec<bool>> {
            let n = self.calls.get();
            self.calls.set(n + 1);
            if n >= 1 {
                anyhow::bail!("batch 2 failed");
            }
            let mut out = Vec::with_capacity(memories.len());
            for m in memories {
                out.push(self.cloud.put(m)?);
            }
            Ok(out)
        }
        fn all(&self, include_deleted: bool) -> Result<Vec<StoredMemory>> {
            self.cloud.all(include_deleted)
        }
        fn max_updated_at(&self) -> Result<Option<String>> {
            self.cloud.max_updated_at()
        }
        fn forget(&self, id: &str) -> Result<bool> {
            self.cloud.forget(id)
        }
        fn count(&self) -> Result<i64> {
            self.cloud.count()
        }
    }
