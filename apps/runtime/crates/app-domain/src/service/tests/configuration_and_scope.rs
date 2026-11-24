
    /// v4 session contract: when session.json only has wrapped_urk, has_local_keys must be true —
    /// the old impl looked at secret_key/secret, always false on v4, so offline local mode was judged "not connected"
    /// and kept prompting login (reported 2026-09-16). Offline without a token still counts as "local library unlocked".
    #[test]
    fn session_info_v4_recognizes_wrapped_urk_as_local_keys() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved_dir = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        // v4 shape: wrap material only, no user/token/plaintext keys (keygen output looks like this)
        std::fs::write(
            dir.path().join("session.json"),
            serde_json::to_vec(&serde_json::json!({
                "kdf_salt": "aabb",
                "wrapped_urk": "deadbeef",
                "urk_nonce": "ccdd",
                "vault_version": 4,
            }))?,
        )?;
        let si = session_info();
        assert!(si.has_session, "session.json present means has_session");
        assert!(
            si.has_local_keys,
            "v4 with only wrapped_urk still counts as local keys"
        );
        assert!(!si.has_token, "offline mode has no token");
        match saved_dir {
            Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        Ok(())
    }

    #[test]
    fn resume_session_skips_the_form_when_the_wrap_exists() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved_dir = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        std::fs::write(
            dir.path().join("session.json"),
            serde_json::to_vec(&serde_json::json!({
                "user": "alice",
                "addr": "https://example.invalid",
                "kdf_salt": "aabb",
                "wrapped_urk": "deadbeef",
                "urk_nonce": "ccdd",
                "vault_version": 4,
            }))?,
        )?;
        let got = resume_session()?;
        match saved_dir {
            Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        if got["resumed"] != true {
            anyhow::bail!("wrapped key should resume without a form");
        }
        if got["user"] != "alice" || got["addr"] != "https://example.invalid" {
            anyhow::bail!("resume lost user or addr: {got}");
        }
        if got.get("pass").is_some() || got.get("super").is_some() {
            anyhow::bail!("resume must not return secrets");
        }
        Ok(())
    }

    #[test]
    fn resume_session_fills_the_public_server_when_nothing_is_stored() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved_dir = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let got = resume_session()?;
        match saved_dir {
            Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        if got["need_login"] != true {
            anyhow::bail!("empty profile should still need a login password");
        }
        if got["addr"] != DEFAULT_SERVER_ADDR {
            anyhow::bail!("empty profile addr was {}", got["addr"]);
        }
        Ok(())
    }

    #[test]
    fn malformed_client_config_is_rejected_without_overwrite() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved_dir = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let path = dir.path().join("client.json");
        std::fs::write(&path, b"[]")?;
        let result = set_cure_auto(true);
        match saved_dir {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        let error = result.err().context("array client config must be rejected")?;
        assert!(error.to_string().contains("must contain a JSON object"), "unexpected error: {error:#}");
        assert_eq!(std::fs::read(&path)?, b"[]");
        Ok(())
    }

    #[test]
    fn client_config_wrong_data_dir_type_is_rejected_without_overwrite() -> anyhow::Result<()> {
        let _guard = crate::test_lock::guard();
        let dir = tempfile::tempdir()?;
        let saved_dir = crate::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let path = dir.path().join("client.json");
        std::fs::write(&path, br#"{"custom":"preserved","data_dir":123}"#)?;
        let result = set_cure_auto(true);
        match saved_dir {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        let error = result.err().context("wrong data_dir type must be rejected")?;
        assert_eq!(std::fs::read(&path)?, br#"{"custom":"preserved","data_dir":123}"#);
        assert!(error.to_string().contains("data_dir"), "unexpected error: {error:#}");
        Ok(())
    }

    #[test]
    fn subtree_members_collects_root_and_descendants() {
        let mk = |id: &str, parent: &str, deleted: bool| {
            let mut m = crate::memory::model::StoredMemory::new_pending(id.into(), "u".into());
            m.local_parent_id = parent.into();
            m.deleted = deleted;
            m
        };
        let all = vec![
            mk("aaaaaaaa-root", "", false),
            mk("bbbbbbbb-child", "aaaaaaaa-root", false),
            mk("cccccccc-grand", "bbbbbbbb-child", false),
            mk("dddddddd-other", "", false),
            mk("eeeeeeee-dead", "aaaaaaaa-root", true),
        ];
        let members = subtree_members(&all, "aaaaaaaa-root");
        assert!(members.contains("aaaaaaaa-root"));
        assert!(members.contains("bbbbbbbb-child"));
        assert!(members.contains("cccccccc-grand"));
        assert!(!members.contains("dddddddd-other"));
        // Tombstones stay out (search uses store.all(false) live rows); missing root → empty
        assert!(!members.contains("eeeeeeee-dead"));
        assert!(subtree_members(&all, "ffffffff-none").is_empty());
        // Cycles do not explode: root and child point at each other, stop at dedup
        let cyclic = vec![
            mk("aaaaaaaa-root", "bbbbbbbb-child", false),
            mk("bbbbbbbb-child", "aaaaaaaa-root", false),
        ];
        let m2 = subtree_members(&cyclic, "aaaaaaaa-root");
        assert!(m2.contains("aaaaaaaa-root") && m2.contains("bbbbbbbb-child"));
    }

    #[test]
    fn data_dir_config_roundtrip() -> anyhow::Result<()> {
        let real_client = dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir missing"))?
            .join(".rsrs")
            .join("client.json");
        let before = std::fs::read(&real_client).ok();
        let iso = crate::test_lock::Isolate::new()?;
        let custom = iso.path().join("mem-test");
        std::fs::create_dir_all(&custom)?;
        let absolute = custom.to_string_lossy().to_string();
        set_data_dir(&absolute)?;
        assert_eq!(data_dir(), custom);
        assert!(
            iso.path().join("client.json").is_file(),
            "client.json was not written inside the isolated root"
        );
        set_data_dir("")?;
        assert_eq!(data_dir(), iso.path());
        {
            struct ModelRootEnv([(&'static str, Option<std::ffi::OsString>); 4]);
            impl Drop for ModelRootEnv {
                fn drop(&mut self) {
                    for (name, value) in &self.0 {
                        match value {
                            Some(value) => std::env::set_var(name, value),
                            None => std::env::remove_var(name),
                        }
                    }
                }
            }
            let _restore = ModelRootEnv([
                "RSRS_DATA_DIR", "RSRS_M3_DIR", "ONEMEMORY_M3_DIR", "RESPIRE_M3_DIR",
            ].map(|name| (name, std::env::var_os(name))));
            for name in ["RSRS_M3_DIR", "ONEMEMORY_M3_DIR", "RESPIRE_M3_DIR"] {
                std::env::remove_var(name);
            }
            let canonical_root = iso.path().canonicalize()?;
            #[cfg(windows)]
            let ordinary_root = {
                let spelling = canonical_root.to_str().context("isolated root is not UTF-8")?;
                if let Some(rest) = spelling.strip_prefix(r"\\?\UNC\") {
                    PathBuf::from(format!(r"\\{rest}"))
                } else {
                    PathBuf::from(spelling.strip_prefix(r"\\?\")
                        .context("canonical Windows root lacks a verbatim prefix")?)
                }
            };
            #[cfg(not(windows))]
            let ordinary_root = iso.path().to_path_buf();
            let ordinary_profile = ordinary_root.join("accounts/second");
            std::fs::create_dir_all(&ordinary_profile)?;
            let canonical_profile = ordinary_profile.canonicalize()?;
            std::env::set_var("RSRS_DATA_DIR", &ordinary_root);
            set_data_dir(&canonical_profile.to_string_lossy())?;
            assert_eq!(data_dir(), canonical_profile);
            set_data_dir(&ordinary_profile.to_string_lossy())?;
            for root in [&ordinary_root, &canonical_root] {
                std::env::set_var("RSRS_DATA_DIR", root);
                assert_eq!(data_dir(), ordinary_profile);
                for profile in [ordinary_profile.clone(), canonical_profile.clone(),
                    ordinary_profile.join("absent/child"), canonical_profile.join("absent/child")] {
                    let config = serde_json::json!({"data_dir":profile.to_string_lossy()});
                    assert_eq!(data_dir_from_config(Some(&config)), profile);
                }
                for profile in [ordinary_root.with_extension("outside-profile"),
                    ordinary_profile.join("../second")] {
                    let config = serde_json::json!({"data_dir":profile.to_string_lossy()});
                    assert_eq!(data_dir_from_config(Some(&config)), *root);
                }
                #[cfg(windows)]
                for name in ["absent.", "absent "] {
                    let config = serde_json::json!({"data_dir":ordinary_profile.join(name)});
                    assert_eq!(data_dir_from_config(Some(&config)), *root);
                }
            }
            #[cfg(unix)]
            {
                let link = ordinary_profile.join("dangling");
                std::os::unix::fs::symlink(ordinary_root.with_extension("missing-target"), &link)?;
                let config = serde_json::json!({"data_dir":link.join("child")});
                assert_eq!(data_dir_from_config(Some(&config)), canonical_root);
            }
            std::env::set_var("RSRS_DATA_DIR", iso.path());
            set_data_dir("")?;
            let models = iso.path().join("models/bge-m3");
            let roots = resolved_roots()?;
            assert!(roots.legacy_whole_isolation);
            assert_eq!(roots.asset_root, models);
            assert_eq!(crate::memory::bge::discovered_model_dir()?, models);
            let config_before = std::fs::read(iso.path().join("client.json"))?;
            for name in ["RSRS_DATA_DIR", "RSRS_M3_DIR"] {
                for invalid in ["", " ", "\t\r\n"] {
                    std::env::set_var("RSRS_DATA_DIR", iso.path());
                    std::env::set_var("RSRS_M3_DIR", &models);
                    std::env::set_var(name, invalid);
                    assert!(crate::memory::bge::default_user_model_dir().is_err(), "{name}");
                    assert!(crate::memory::bge::cpu_ort_cache_path().is_err(), "{name}");
                    assert!(crate::memory::bge::m3_model_dir().is_err(), "{name}");
                    assert!(crate::memory::bge::discovered_model_dir().is_err(), "{name}");
                    assert!(validate_client_config().is_err(), "{name}");
                    assert!(resolved_roots().is_err(), "{name}");
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                for name in ["RSRS_DATA_DIR", "RSRS_M3_DIR"] {
                    std::env::set_var("RSRS_DATA_DIR", iso.path());
                    std::env::set_var("RSRS_M3_DIR", &models);
                    std::env::set_var(name, std::ffi::OsString::from_vec(vec![0xff]));
                    assert!(crate::memory::bge::default_user_model_dir().is_err(), "{name}");
                    assert!(crate::memory::bge::cpu_ort_cache_path().is_err(), "{name}");
                    assert!(crate::memory::bge::m3_model_dir().is_err(), "{name}");
                    assert!(crate::memory::bge::discovered_model_dir().is_err(), "{name}");
                    assert!(validate_client_config().is_err(), "{name}");
                    assert!(resolved_roots().is_err(), "{name}");
                }
            }
            assert_eq!(std::fs::read(iso.path().join("client.json"))?, config_before);
        }
        assert_eq!(
            std::fs::read(&real_client).ok(),
            before,
            "test rewrote the real client.json"
        );
        Ok(())
    }

    #[test]
    fn resolve_prefix_rules() -> anyhow::Result<()> {
        let mut a =
            crate::memory::model::StoredMemory::new_pending("11111111-aaaa".into(), "u".into());
        a.local_title = "\u{7532}".into();
        let mut b =
            crate::memory::model::StoredMemory::new_pending("11111111-bbbb".into(), "u".into());
        b.local_title = "\u{4e59}".into();
        let mut c =
            crate::memory::model::StoredMemory::new_pending("22222222-cccc".into(), "u".into());
        c.local_title = "\u{4e19}".into();
        c.deleted = true;
        let all = vec![a, b, c];
        assert!(resolve_prefix(&all, "11111111").is_err()); // many hits
        assert!(resolve_prefix(&all, "22222222").is_err()); // deleted
        assert_eq!(resolve_prefix(&all, "11111111-aaaa")?, "11111111-aaaa");
        Ok(())
    }

    /// Regression lock (reported 2026-09-06): --parent with a short prefix must resolve to a full id,
    /// and a miss must really empty — no "warned standalone store but printed attach success" contradiction.
    #[test]
    fn parent_prefix_resolution_contract() -> anyhow::Result<()> {
        let mut p = crate::memory::model::StoredMemory::new_pending(
            "04a7be06-5ebd-4ba5-9da3-eb9a066ed2d5".into(),
            "u".into(),
        );
        p.local_title = "\u{56e0}".into();
        let all = vec![p];
        // Unique prefix hit → full id
        assert_eq!(
            resolve_prefix(&all, "04a7be06")?,
            "04a7be06-5ebd-4ba5-9da3-eb9a066ed2d5"
        );
        // Missing → Err (caller unwrap_or_default empties it; must not refill the original prefix)
        assert!(resolve_prefix(&all, "deadbeef").is_err());
        let dangling: String = resolve_prefix(&all, "deadbeef").unwrap_or_default();
        assert!(dangling.is_empty());
        Ok(())
    }

    fn test_entry(content: &str) -> MemoryEntry {
        MemoryEntry {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id: "test-1".into(),
            kind: Kind::Context,
            tags: vec![],
            title: "\u{6d4b}\u{6761}".into(),
            content: content.into(),
            user: "u".into(),
            computer: String::new(),
            project: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            emotion: -1.0,
            parent_id: String::new(),
            importance: "normal".to_owned(),
            device: "test-dev".to_owned(),
            modified_by: "test-dev".to_owned(),
        }
    }

    /// Auto-sync switch contract: RSRS_NO_AUTOSYNC non-empty and not 0 means off (every write path must use this).
    #[test]
    fn autosync_disabled_by_env_is_off() {
        let _guard = crate::test_lock::guard();
        let saved = crate::env::var("RSRS_NO_AUTOSYNC").ok();
        std::env::set_var("RSRS_NO_AUTOSYNC", "1");
        assert!(!autosync_enabled(), "auto-sync must be off when set to 1");
        std::env::set_var("RSRS_NO_AUTOSYNC", "yes");
        assert!(!autosync_enabled(), "any non-zero value must turn it off");
        match saved {
            Some(v) => std::env::set_var("RSRS_NO_AUTOSYNC", v),
            None => std::env::remove_var("RSRS_NO_AUTOSYNC"),
        }
    }

