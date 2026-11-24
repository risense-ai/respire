    use super::tests::sample;
    use super::*;

    /// A tie must not overwrite a local dirty row: after attach unpushed (dirty=1), a same-stamp pull must not wipe the attach.
    #[test]
    fn put_synced_tie_keeps_local_dirty() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        let mut m = sample("11111111-2222-3333-4444-555555555555");
        m.local_title = "\u{65e7}".into();
        store.put(&m)?;

        // Local reparent (set_parent bumps updated_at and marks dirty)
        store.set_parent(&m.id, "p1")?;
        let local = store.all(false)?.remove(0);
        assert_eq!(local.local_parent_id, "p1");

        // Cloud pull of same id same updated_at with the old parent (reconcile tie)
        let mut remote = local.clone();
        remote.local_parent_id = String::new();
        let took = store.put_synced(&remote)?;
        assert!(!took, "tie + local dirty \u{2192} pull must be refused");
        assert_eq!(store.all(false)?[0].local_parent_id, "p1");
        Ok(())
    }
