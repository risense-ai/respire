    fn test_app(dir: &std::path::Path) -> anyhow::Result<App> {
        let keys = crate::memory::SessionKeys::from_urk([7u8; 32])?;
        let store = LocalStore::open(&dir.join("t.db"))?;
        Ok(App {
            keys,
            store,
            embedder: Box::new(crate::memory::search::HashingEmbedder::default()),
        })
    }

    fn important(title: &str, content: &str) -> CreateReq {
        CreateReq {
            title: title.to_owned(),
            content: content.to_owned(),
            importance: Some("important".to_owned()),
            kind: "context".to_owned(),
            ..CreateReq::default()
        }
    }

    #[test]
    fn recall_keeps_indexed_memories_available_during_sync() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let app = test_app(dir.path())?;
        let (empty, pending) = recall_candidates(&app.store)?;
        assert!(empty.is_empty());
        assert_eq!(pending, 0);
        let indexed = app.create(&important("indexed", "rust memory"))?;
        let mut incoming = app.create(&important("incoming", "rust memory"))?;
        incoming.updated_at = "2099-01-01T00:00:00Z".into();
        incoming.content = "synchronized rust memory".into();
        let mut source = MemoryEngine::seal(&app.keys,
            &crate::memory::search::HashingEmbedder::default(), &incoming, "fixture")?;
        source.local_artifact.clear();
        app.store.put_synced(&source)?;
        let (ready, pending) = recall_candidates(&app.store)?;
        assert_eq!(pending, 1);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, indexed.id);
        let hits = app.search("rust memory", 10, None)?;
        assert!(hits.iter().any(|hit| hit.entry.id == indexed.id));
        assert!(hits.iter().all(|hit| hit.entry.id != incoming.id));
        assert_eq!(app.store.rebuild_index_with_progress(
            &app.keys, &crate::memory::search::HashingEmbedder::default(), "m3", |_, _| Ok(())
        )?, 1);
        let (ready, pending) = recall_candidates(&app.store)?;
        assert_eq!(ready.len(), 2);
        assert_eq!(pending, 0);
        Ok(())
    }

    #[test]
    fn recall_preserves_all_pending_and_corrupt_index_errors() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let app = test_app(dir.path())?;
        let mut memory = app.create(&important("source", "rust memory"))?;
        memory.updated_at = "2099-01-01T00:00:00Z".into();
        let mut source = MemoryEngine::seal(&app.keys,
            &crate::memory::search::HashingEmbedder::default(), &memory, "fixture")?;
        source.local_artifact.clear();
        app.store.put_synced(&source)?;
        let error = recall_candidates(&app.store).err()
            .ok_or_else(|| anyhow!("all-pending recall succeeded"))?;
        assert!(error.downcast_ref::<IndexPending>().is_some());
        source.local_artifact = b"invalid artifact".to_vec();
        app.store.put_inner(&source, false)?;
        assert!(recall_candidates(&app.store).is_err());
        Ok(())
    }

    #[test]
    fn app_crud_tree_attach_promote() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let app = test_app(dir.path())?;
        let parent = app.create(&important("parent-node", "parent body about rust memory"))?;
        let child = app.create(&CreateReq {
            parent: Some(parent.id.clone()),
            ..important("child-node", "child body about rust ownership")
        })?;
        let status = app.status()?;
        assert_eq!(status.local_alive, 2);
        let listed = app.list(10)?;
        assert!(!listed.is_empty());
        let found = app.search("rust memory", 5, None)?;
        assert!(!found.is_empty());
        let detail = app.show(&parent.id[..8])?;
        assert_eq!(detail.entry.id, parent.id);
        assert!(detail.children.iter().any(|c| c.id == child.id));
        let forest = app.tree("", 3)?;
        assert!(forest.iter().any(|n| n.id == parent.id));
        let updated = app.update(
            &child.id[..8],
            Some("child-renamed".to_owned()),
            Some("updated child body rust".to_owned()),
            Some("rust,mem".to_owned()),
            Some("decision".to_owned()),
        )?;
        assert_eq!(updated.title, "child-renamed");
        let (cid, pid) = app.attach(&child.id, &parent.id)?;
        assert_eq!(cid, child.id);
        assert_eq!(pid, parent.id);
        let (promoted, gp) = app.promote(&child.id)?;
        assert_eq!(promoted, child.id);
        assert!(gp.is_empty());
        assert!(app.delete(&child.id)?);
        let restored = app.restore(&child.id)?;
        assert_eq!(restored.id, child.id);
        let _ = app.candidates("rust memory ownership")?;
        let members = subtree_members(&app.store.all(true)?, &parent.id);
        assert!(members.contains(&parent.id));
        let _ = app.tree_cure(3)?;
        let _ = app.tree_float(false, 1)?;
        let _ = app.portrait_material(5)?;
        Ok(())
    }

    #[test]
    fn rejected_associations_restore_and_attach_are_input_errors_without_writes() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let app = test_app(dir.path())?;
        let diary = app.create(&CreateReq { title: "diary".into(), content: "diary body".into(), ..CreateReq::default() })?;
        let parent = app.create(&important("parent", "parent body"))?;
        let child = app.create(&CreateReq { parent: Some(parent.id.clone()), ..important("child", "child body") })?;
        let before = serde_json::to_value(app.store.all(true)?)?;
        let supersede = app.create(&CreateReq { supersedes: Some(diary.id.clone()), ..important("replacement", "replacement body") });
        let link = app.create(&CreateReq { see_also: vec![diary.id.clone()], ..important("linked", "linked body") });
        for error in [supersede.err(), link.err(), app.restore("deadbeef").err(), app.attach(&child.id, &child.id).err(), app.attach(&parent.id, &child.id).err()] {
            let error = error.ok_or_else(|| anyhow!("invalid input was accepted"))?;
            assert!(error.downcast_ref::<crate::input_error::InputError>().is_some(), "{error:#}");
        }
        assert_eq!(serde_json::to_value(app.store.all(true)?)?, before);
        // A cryptographic failure is still a runtime error, not malformed command input.
        let wrong = App { keys: crate::memory::SessionKeys::from_urk([9; 32])?, store: app.store, embedder: app.embedder };
        let error = wrong.restore(&child.id).err().ok_or_else(|| anyhow!("wrong key was accepted"))?;
        assert!(error.downcast_ref::<crate::input_error::InputError>().is_none());
        Ok(())
    }

    #[test]
    fn status_light_and_titles() -> anyhow::Result<()> {
        let _isolate = crate::test_lock::Isolate::new()?;
        let info = status_light()?;
        assert_eq!(info.local_total, 0);
        assert_eq!(derive_title("\u{3010}\u{7ecf}\u{9a8c}\u{3011}first line\nsecond"), "first line");
        assert_eq!(derive_title(""), "(untitled)");
        assert!(diary_mode() == "concise" || diary_mode() == "verbose");
        let saved_super = std::env::var_os("RSRS_SUPER");
        let code = crate::memory::crypto::generate_secret_key();
        std::env::set_var("RSRS_SUPER", &code);
        let result = (|| -> Result<()> {
            let (salt, wrapped, nonce) = crate::auth::wrap_with_rsrs(&code, &[7; 32])?;
            crate::auth::write_session_json(&serde_json::json!({
                "user":"fixture","vault_version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce
            }))?;
            let _deferred = respire_core_sdk::defer_indexing();
            let snapshot = r#"[{"id":"parent","content":"unique parent","title":"parent"},{"id":"child","content":"unique child","parent_id":"parent"},{"id":"parent-copy","content":"unique parent"},{"id":"new-child","content":"third body","parent_id":"parent-copy","see_also":["parent","parent-copy"]},{"id":"parent","content":"unique parent"}]"#;
            let first = import_json_text(snapshot)?;
            assert_eq!((first.imported, first.duplicates), (3, 2));
            let again = import_json_text(snapshot)?;
            assert_eq!((again.imported, again.duplicates), (0, 5));
            let keys = auth::load_local_session()?;
            let entries: Vec<_> = open_store()?.all(false)?.iter()
                .map(|stored| MemoryEngine::open(&keys, stored)).collect::<Result<_>>()?;
            let parent = entries.iter().find(|entry| entry.content == "unique parent").ok_or_else(|| anyhow!("parent missing"))?;
            let child = entries.iter().find(|entry| entry.content == "third body").ok_or_else(|| anyhow!("child missing"))?;
            assert_eq!(child.parent_id, parent.id);
            assert_eq!(child.see_also, vec![parent.id.clone()]);
            assert!(import_json_text(r#"[{"id":"a","content":"a","parent_id":"b"},{"id":"b","content":"b","parent_id":"a"}]"#).is_err());
            assert_eq!(open_store()?.all(false)?.len(), 3);
            let snapshot = local_snapshot(Some(&code))?;
            assert_eq!(serde_json::from_str::<Vec<MemoryEntry>>(&snapshot)?.len(), 3);
            let extra = r#"[{"id":"existing","content":"unique parent"},{"id":"new","content":"fourth body","supersedes":"existing","see_also":["existing"]}]"#;
            assert_eq!((import_json_text(extra)?.imported, import_json_text(extra)?.duplicates), (1, 2));
            let store = open_store()?;
            let all = store.all(false)?;
            let entries = all.iter().map(|stored| MemoryEngine::open(&keys, stored)).collect::<Result<Vec<_>>>()?;
            let parent = entries.iter().find(|entry| entry.content == "unique parent").ok_or_else(|| anyhow!("parent missing"))?;
            let replacement = entries.iter().find(|entry| entry.content == "fourth body").ok_or_else(|| anyhow!("replacement missing"))?;
            assert_eq!(parent.superseded_by, replacement.id);
            assert!(parent.see_also.contains(&replacement.id));
            Ok(())
        })();
        match saved_super {
            Some(value) => std::env::set_var("RSRS_SUPER", value),
            None => std::env::remove_var("RSRS_SUPER"),
        }
        result
    }
