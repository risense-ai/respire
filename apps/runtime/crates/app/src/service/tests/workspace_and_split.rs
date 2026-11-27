    /// Split contract: attach children to the outline, reseal it, and preserve the body when no summary is supplied.
    /// Isolation: split_exec used to auto-sync at the end via remote_configured(), which reads the **real**
    /// ~/.rsrs/session.json — if the test machine is logged in, cloud data is pulled into the temp library,
    /// and the "two children" assert is polluted (2026-09-16: got 7 not 2). This test forces auto-sync off.
    #[test]
    fn split_exec_lands_children_and_summary() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let saved = crate::env::var("RSRS_NO_AUTOSYNC").ok();
        std::env::set_var("RSRS_NO_AUTOSYNC", "1");
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        let keys = crate::memory::SessionKeys::from_urk([7u8; 32])?;
        let embedder = crate::memory::search::HashingEmbedder::default();
        let mut entry = test_entry("\u{4e00}\u{5927}\u{5768}\u{6df7}\u{88c5}\u{5185}\u{5bb9}");
        entry.id = "11111111-parent".into();
        let stored0 = MemoryEngine::seal(&keys, &embedder, &entry, "u")?;
        store.put(&stored0)?;
        let spec = SplitSpec {
            summary: Some("\u{672c}\u{56de}\u{5408}\u{5ba1}\u{8ba1}\u{4e86} backend\u{ff08}\u{7eb2}\u{ff1a}\u{8be6}\u{60c5}\u{62c6}\u{89c1}\u{5b50}\u{679c}\u{ff09}".into()),
            items: vec![
                SplitOp {
                    title: "\u{2460}classes.py \u{5047} import".into(),
                    content: "\u{3010}\u{524d}\u{56e0}\u{3011}\u{5ba1}\u{8ba1}\u{53d1}\u{73b0}\u{3010}\u{884c}\u{4e3a}\u{3011}classes.py \u{91cd} import \u{4e0d}\u{5b58}\u{5728}\u{6a21}\u{5757}\u{3010}\u{540e}\u{679c}\u{3011}POST \u{5fc5}\u{4e94}\u{767e}\u{ff0c}\u{5220}\u{9664}\u{8be5}\u{6bb5}\u{5373}\u{6108}\u{3002}".into(),
                    kind: "skill".into(),
                    tags: vec!["audit".into()],
                    parent: "\u{7eb2}".into(),
                },
                SplitOp {
                    title: "\u{2461}admin \u{8c41}\u{514d}\u{7f3a}\u{5931}".into(),
                    content: "\u{3010}\u{884c}\u{4e3a}\u{3011}assignments.py update/archive \u{65e0}\u{8c41}\u{514d}\u{3010}\u{540e}\u{679c}\u{3011}\u{7ba1}\u{7406}\u{5458}\u{80fd}\u{770b}\u{4e0d}\u{80fd}\u{6539}\u{ff0c}\u{8865}\u{9f50}\u{4e00}\u{81f4}\u{6027}\u{3002}".into(),
                    kind: "decision".into(),
                    tags: vec![],
                    parent: "\u{7eb2}".into(),
                },
            ],
        };
        let n = split_exec(&keys, &embedder, &store, &entry, &spec)?;
        assert_eq!(n, 2);
        let kids = store.children("11111111-parent")?;
        assert_eq!(kids.len(), 2, "both children hang under the outline");
        assert!(kids.iter().all(|k| k.local_parent_id == "11111111-parent"));
        // Original becomes an outline: body replaced
        let upd = store
            .all(true)?
            .into_iter()
            .find(|m| m.id == "11111111-parent")
            .ok_or_else(|| anyhow!("not found"))?;
        assert!(
            upd.local_content_head.contains("\u{7eb2}\u{ff1a}\u{8be6}\u{60c5}\u{62c6}\u{89c1}"),
            "original should become an outline"
        );
        // summary=original body: become an outline without changing content (keep-original path)
        let spec2 = SplitSpec {
            summary: Some("\u{4e00}\u{5927}\u{5768}\u{6df7}\u{88c5}\u{5185}\u{5bb9}".into()),
            items: vec![SplitOp {
                title: "\u{2462}\u{7b2c}\u{4e09}\u{679c}".into(),
                content: "\u{3010}\u{6559}\u{8bad}\u{3011}\u{518d}\u{8865}\u{4e00}\u{679c}\u{9a8c}\u{8bc1}\u{539f}\u{6761}\u{4fdd}\u{7559}\u{8def}\u{5f84}\u{4e0d}\u{88ab}\u{8bef}\u{8f6c}\u{3002}".into(),
                kind: "skill".into(),
                tags: vec![],
                parent: "\u{7eb2}".into(),
            }],
        };
        assert!(split_exec(&keys, &embedder, &store, &entry, &spec2).is_ok());
        match saved {
            Some(v) => std::env::set_var("RSRS_NO_AUTOSYNC", v),
            None => std::env::remove_var("RSRS_NO_AUTOSYNC"),
        }
        Ok(())
    }

    /// Split refuse: a single child and no outline kept = a rewrite, not a split.
    #[test]
    fn split_exec_refuses_single_child_no_summary() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = LocalStore::open(&dir.path().join("t.db"))?;
        let keys = crate::memory::SessionKeys::from_urk([7u8; 32])?;
        let embedder = crate::memory::search::HashingEmbedder::default();
        let mut entry = test_entry("\u{5185}\u{5bb9}");
        entry.id = "22222222-single".into();
        let stored0 = MemoryEngine::seal(&keys, &embedder, &entry, "u")?;
        store.put(&stored0)?;
        let spec = SplitSpec {
            summary: None,
            items: vec![SplitOp {
                title: "\u{4e00}\u{679c}".into(),
                content: "\u{552f}\u{4e00}\u{4e00}\u{679c}\u{7b49}\u{540c}\u{6539}\u{5199}\u{3002}".into(),
                kind: "context".into(),
                tags: vec![],
                parent: "\u{7eb2}".into(),
            }],
        };
        assert!(split_exec(&keys, &embedder, &store, &entry, &spec).is_err());
        Ok(())
    }

    #[test]
    fn expand_tilde_and_activity_append() {
        let p = expand_tilde("plain");
        assert_eq!(p, PathBuf::from("plain"));
        let home = expand_tilde("~/x");
        assert!(home.ends_with("x"));
        assert_eq!(activity_append("", "a"), "a");
        assert_eq!(activity_append("hi", "line"), "hi\nline");
        assert_eq!(activity_append("hi\n", "line"), "hi\nline");
    }

    #[test]
    fn account_dir_rejects_bad_names() {
        assert!(account_dir("").is_err());
        assert!(account_dir("a/b").is_err());
        assert!(account_dir("ok-name_1").is_ok());
    }

    #[test]
    fn workspace_and_agent_config_roundtrip() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        assert_eq!(workspace_mode(), "normal");
        assert_eq!(diary_mode(), "concise");
        write_client_config(&serde_json::json!({"api_base":"https://fixture.invalid", "custom":"preserved"}))?;
        set_workspace_mode("readonly")?;
        assert_eq!(workspace_mode(), "readonly");
        assert!(service_paused());
        assert!(readonly_mode());
        assert!(ensure_writable().is_err());
        assert!(import_json_text("[]").is_err());
        account_use("work")?;
        assert_eq!(workspace_mode(), "readonly", "device mode survives account switching");
        assert_eq!(read_client_config().ok_or_else(|| anyhow::anyhow!("missing client config"))?["custom"], "preserved");
        set_workspace_mode("off")?;
        assert_eq!(workspace_mode(), "off");
        assert!(off_mode());
        assert!(import_json_text("[]").is_err());
        account_use("main")?;
        assert_eq!(workspace_mode(), "off");
        let before = std::fs::read(client_config_path())?;
        assert!(set_workspace_mode("nope").is_err());
        assert_eq!(std::fs::read(client_config_path())?, before);
        set_workspace_mode("normal")?;
        assert_eq!(workspace_mode(), "normal");
        assert!(ensure_writable().is_ok());
        write_agent_config_key("memory_off", &serde_json::json!(true))?;
        assert_eq!(workspace_mode(), "off");
        write_agent_config_key("memory_off", &serde_json::json!(false))?;
        write_agent_config_key("readonly", &serde_json::json!(true))?;
        assert_eq!(workspace_mode(), "readonly");
        write_agent_config_key("readonly", &serde_json::json!(false))?;
        assert_eq!(workspace_mode(), "normal");
        write_agent_config_key("readonly_team", &serde_json::json!(true))?;
        set_workspace_mode("normal")?;
        assert_eq!(workspace_mode(), "readonly", "normal mode cannot lift shared-space permissions");
        assert!(!service_paused(), "team read-only access must still pull and index shared memory");
        assert!(ensure_writable().is_err());
        set_workspace_mode("readonly")?;
        assert!(service_paused(), "device read-only mode must pause even a shared-space replica");
        std::fs::write(agent_config_path(), serde_json::to_vec(&serde_json::json!({"readonly":true,"readonly_team":true}))?)?;
        write_client_config(&serde_json::json!({"custom":"preserved"}))?;
        assert!(!service_paused(), "legacy shared-space flags are account permissions, not device mode");
        set_workspace_mode("normal")?;
        write_agent_config_key("readonly_team", &serde_json::json!(false))?;
        write_agent_config_key("diary_mode", &serde_json::json!("verbose"))?;
        assert_eq!(diary_mode(), "verbose");
        assert!(set_workspace_mode("nope").is_err());
        match saved {
            Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        Ok(())
    }

    #[test]
    fn accounts_list_use_remove() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let listed = account_list()?;
        assert_eq!(listed["accounts"][0]["name"], "main");
        let used = account_use("work")?;
        assert_eq!(used["name"], "work");
        std::fs::write(account_dir("work")?.join("session.json"), r#"{"user":"w"}"#)?;
        let listed = account_list()?;
        assert!(listed["accounts"].as_array().map(|a| a.len()).unwrap_or(0) >= 2);
        account_use("main")?;
        account_remove("work")?;
        assert!(account_remove("missing").is_err());
        match saved {
            Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        Ok(())
    }

