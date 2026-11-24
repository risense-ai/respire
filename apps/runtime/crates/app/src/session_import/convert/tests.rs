    use super::*;

    fn sample() -> Session {
        Session {
            platform: "codex".into(),
            id: "0199abcd-1234".into(),
            path: "path".into(),
            cwd: Some("/home/u/respire".into()),
            created_at: Some("2026-10-01T00:00:00Z".into()),
            updated_at: Some("2026-10-01T02:00:00Z".into()),
            provider: Some("openai".into()),
            model: Some("gpt-5".into()),
            first_user_message: "add an extract command".into(),
            last_user_message: "ship it".into(),
            usage: [("total_tokens".to_owned(), serde_json::json!(1234))]
                .into_iter()
                .collect(),
            parent_id: None,
            agent: None,
        }
    }

    #[test]
    fn english_scaffold_preserves_chinese_user_data_and_input_version() {
        let mut session = sample();
        session.platform = "hermes".into();
        session.first_user_message = "\u{4fdd}\u{7559}\u{7528}\u{6237}\u{4e2d}\u{6587}\u{4e0e}\u{3010}\u{524d}\u{56e0}\u{3011}\u{539f}\u{59cb}\u{5f15}\u{7528}".into();
        session.last_user_message = "\u{4fdd}\u{7559}\u{6df7}\u{5408}\u{3010}Result\u{3011}\u{539f}\u{6587}".into();
        let fingerprint = session.content_fingerprint();
        let entry = session_entry(&session, &ConvertOptions::default());
        assert_eq!(fingerprint, session.content_fingerprint());
        assert_eq!(entry.id, session_id(&session.key()));
        assert_eq!(entry.title, "Hermes session \u{4fdd}\u{7559}\u{7528}\u{6237}\u{4e2d}\u{6587}\u{4e0e}\u{3010}\u{524d}\u{56e0}\u{3011}\u{539f}\u{59cb}\u{5f15}\u{7528}");
        assert!(entry.content.starts_with("\u{3010}Cause\u{3011}Hermes session"));
        assert!(entry.content.contains("\n\u{3010}Action\u{3011}\u{4fdd}\u{7559}\u{7528}\u{6237}\u{4e2d}\u{6587}\u{4e0e}\u{3010}\u{524d}\u{56e0}\u{3011}\u{539f}\u{59cb}\u{5f15}\u{7528}"));
        assert!(entry.content.contains("\nLast request: \u{4fdd}\u{7559}\u{6df7}\u{5408}\u{3010}Result\u{3011}\u{539f}\u{6587}"));
        assert!(entry.content.contains("\n\u{3010}Result\u{3011}Session updated at"));
        assert_eq!(entry.importance, "important");
        assert_eq!(scrub("\u{8bf4}\u{660e}\u{4e2d}\u{6587}\u{4fdd}\u{6301}\u{ff1b}\u{5bc6}\u{7801}\u{ff1a}fixture-secret"), "\u{8bf4}\u{660e}\u{4e2d}\u{6587}\u{4fdd}\u{6301}\u{ff1b}\u{5bc6}\u{7801}\u{ff1a} ***");
    }

    #[test]
    fn builds_entry_with_three_markers_and_metadata() {
        let entry = session_entry(&sample(), &ConvertOptions::default());
        assert!(entry.content.contains("\u{3010}Cause\u{3011}"));
        assert!(entry.content.contains("\u{3010}Action\u{3011}"));
        assert!(entry.content.contains("\u{3010}Result\u{3011}"));
        assert_eq!(entry.project, "respire");
        assert_eq!(entry.importance, "important");
        assert!(entry.title.starts_with("Codex session"));
        assert_eq!(
            entry.tags,
            vec!["session-import".to_owned(), "codex".to_owned()]
        );
    }

    #[test]
    fn scrub_redacts_secrets_and_pem() {
        let raw = "password=hunter2\nAuthorization: Bearer abcdef\ntoken=ghp_ABCDEFGHIJKLMNOP\nAKIAIOSFODNN7EXAMPLE\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----";
        let clean = scrub(raw);
        assert!(!clean.contains("hunter2"));
        assert!(!clean.contains("ghp_ABCDEFGHIJKLMNOP"));
        assert!(!clean.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!clean.contains("MIIabc"));
        assert!(clean.contains("[REDACTED PRIVATE KEY]"));
    }

    #[test]
    fn title_falls_back_to_id() {
        let mut s = sample();
        s.first_user_message = String::new();
        assert_eq!(derive_title(&s), format!("Codex session {}", "0199abcd"));
    }

    #[test]
    fn redact_only_when_keyword_adjoins_separator() {
        assert_eq!(
            scrub("\u{4f1a}\u{8bdd}\u{66f4}\u{65b0}\u{4e8e} 2026-10-01T02:03:00Z"),
            "\u{4f1a}\u{8bdd}\u{66f4}\u{65b0}\u{4e8e} 2026-10-01T02:03:00Z"
        );
        assert_eq!(
            scrub("fix the token refresh bug: now"),
            "fix the token refresh bug: now"
        );
        assert!(scrub("token=abcd1234").contains("***"));
        assert!(scrub("api_key: abcd1234").contains("***"));
        assert!(scrub("\u{5bc6}\u{7801}\u{ff1a}hunter2").contains("***"));
    }

    #[test]
    fn redact_masks_quoted_json_and_flag_values() {
        let object = scrub(r#"{"password": "hunter2"}"#);
        assert!(!object.contains("hunter2"));
        assert!(object.contains("***"));

        let pair = scrub(r#""secret":"abcdefgh123""#);
        assert!(!pair.contains("abcdefgh123"));
        assert!(pair.contains("***"));

        let flag = scrub("mysql -u root --password hunter2");
        assert!(!flag.contains("hunter2"));
        assert!(flag.contains("***"));
    }

    #[test]
    fn scrub_masks_bearer_without_separator() {
        let masked = scrub("curl -H 'Authorization: Bearer abcdefghijklmnop'");
        assert!(!masked.contains("abcdefghijklmnop"));
        let bare = scrub("Bearer abcdefghijklmnop");
        assert!(!bare.contains("abcdefghijklmnop"));
    }

    #[test]
    fn private_key_detection_is_strict() {
        let prose = "Let's begin by rotating the private key for the service";
        assert_eq!(scrub(prose), prose);
        let block = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert!(scrub(block).contains("[REDACTED PRIVATE KEY]"));
    }

    #[test]
    fn stable_id_is_deterministic_and_title_is_scrubbed() {
        let a = session_entry(&sample(), &ConvertOptions::default());
        let b = session_entry(&sample(), &ConvertOptions::default());
        assert_eq!(a.id, b.id);

        let mut s = sample();
        s.first_user_message = "password=hunter2 now continue".into();
        let entry = session_entry(&s, &ConvertOptions::default());
        assert!(!entry.title.contains("hunter2"));
    }

    #[test]
    fn mixed_and_repeated_credentials_are_scrubbed_in_every_projection() {
        let secrets = [
            "ghp_SYNTHETIC1234567890",
            "sk-SYNTHETIC1234567890",
            "SYNTHETIC_BEARER_A",
            "SYNTHETIC_BEARER_B",
            "fixture-password",
        ];
        let lines = [
            format!("{} password={}", secrets[0], secrets[4]),
            format!("{} {} password={}", secrets[0], secrets[1], secrets[4]),
            format!("Bearer {} Bearer {}", secrets[2], secrets[3]),
            format!(
                r#"{{"a":"Bearer {}","b":"Bearer {}"}}"#,
                secrets[2], secrets[3]
            ),
            format!("Bearer short; bearer {}; BEARER {}", secrets[2], secrets[3]),
            format!(
                r#"{{"note":"{}", "password":"{}", "other":"{}"}}"#,
                secrets[0], secrets[4], secrets[1]
            ),
            format!(
                "Bearer {} {} --password {}",
                secrets[2], secrets[0], secrets[4]
            ),
            format!(
                "{}\nBearer {}\npassword={}",
                secrets[0], secrets[2], secrets[4]
            ),
        ];
        for line in lines {
            let mut session = sample();
            session.first_user_message = line.clone();
            session.last_user_message = line.clone();
            let entry = session_entry(&session, &ConvertOptions::default());
            for output in [
                scrub(&line),
                entry.title,
                entry.content,
                session.to_json().to_string(),
            ] {
                for secret in secrets {
                    assert!(!output.contains(secret), "credential leaked from {line:?}");
                }
            }
        }
        let prose = "fix the token refresh bug: now; discuss bearer";
        assert_eq!(scrub(prose), prose);
        let jwt = "eyJSYNTHETIC_HEADER.SYNTHETIC_PAYLOAD.SYNTHETIC_SIGNATURE";
        let scrubbed = scrub(&format!("{jwt} {}", secrets[0]));
        assert!(!scrubbed.contains("SYNTHETIC_PAYLOAD"));
        assert!(!scrubbed.contains("SYNTHETIC_SIGNATURE"));
    }

    #[test]
    fn content_versioned_sqlite_import_gets_fresh_write_timestamp() -> anyhow::Result<()> {
        let mut session = sample();
        session.platform = "hermes".into();
        session.updated_at = Some("2020-01-01T00:00:00.000Z".into());
        let entry = session_entry(&session, &ConvertOptions::default());
        let imported = super::super::scan_util::parse_stamp(&entry.updated_at)
            .ok_or_else(|| anyhow::anyhow!("invalid import timestamp"))?;
        let source = super::super::scan_util::parse_stamp("2020-01-01T00:00:00.000Z")
            .ok_or_else(|| anyhow::anyhow!("invalid fixture timestamp"))?;
        assert!(imported > source);
        assert!(
            entry.content.contains("2020-01-01"),
            "source time remains in body"
        );
        Ok(())
    }
