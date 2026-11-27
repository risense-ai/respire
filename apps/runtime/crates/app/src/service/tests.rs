    use super::*;

    #[test]
    fn counter_bump_threshold_reset() -> anyhow::Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("om-mnt-{nonce}"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("maintenance.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({ "resort_threshold": 3 }))?,
        )?;
        assert_eq!(counter_bump(&path)?, (1, 3));
        assert_eq!(counter_bump(&path)?, (2, 3));
        assert_eq!(counter_bump(&path)?, (3, 3)); // at threshold (caller alerts)
        assert_eq!(counter_peek(&path), (3, 3));
        counter_reset(&path, "2026-09-10T00:00:00.000Z")?;
        assert_eq!(counter_peek(&path), (0, 3));
        counter_set_threshold(&path, 50)?;
        assert_eq!(counter_bump(&path)?, (1, 50));
        assert!(counter_set_threshold(&path, 0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn split_tags_basic() {
        assert_eq!(split_tags("a, b,,c"), vec!["a", "b", "c"]);
        assert!(split_tags("").is_empty());
    }

    /// Empty-value filter: env set to empty/whitespace must be dropped (old impl did not filter, empty hostname)
    #[test]
    fn first_non_empty_skips_blank() {
        assert_eq!(
            first_non_empty(["".to_owned(), "  ".to_owned(), "real".to_owned()]),
            Some("real".to_owned())
        );
        assert_eq!(
            first_non_empty(["  padded  ".to_owned()]),
            Some("padded".to_owned())
        );
        assert_eq!(first_non_empty([String::new()]), None);
        assert_eq!(first_non_empty(Vec::<String>::new()), None);
    }

    /// Syscall fallback: simulate mac — no /etc/hostname, no HOSTNAME/COMPUTERNAME,
    /// when earlier tiers are blank gethostname must supply the name (dropping the whoami tier fails)
    #[test]
    fn syscall_fallback_used_when_earlier_tiers_blank() -> anyhow::Result<()> {
        let syscall = whoami::hostname().map_err(|e| anyhow!("hostname: {e}"))?;
        let host = first_non_empty(["".to_owned(), "".to_owned(), "".to_owned(), syscall.clone()]);
        assert_eq!(
            host.as_deref(),
            Some(syscall.as_str()),
            "syscall tier unused when first three tiers are blank"
        );
        assert!(!host
            .ok_or_else(|| anyhow!("missing host"))?
            .trim()
            .is_empty());
        Ok(())
    }

    /// Syscall fallback: a real host always has a hostname (/etc/hostname or env or gethostname),
    /// so device_tag must not fall to unknown-host — regression for mac without /etc/hostname and without HOSTNAME
    #[test]
    fn device_tag_never_unknown_on_real_host() -> anyhow::Result<()> {
        let host = host_name();
        assert!(
            host.is_some(),
            "all four tiers blank: syscall fallback failed"
        );
        assert!(
            !host
                .ok_or_else(|| anyhow!("missing host"))?
                .trim()
                .is_empty(),
            "hostname must not be blank"
        );
        let tag = device_tag();
        assert!(
            !tag.starts_with("unknown-host"),
            "device_tag fell back to unknown-host: {tag}"
        );
        assert!(
            tag.ends_with("/linux") || tag.ends_with("/mac") || tag.ends_with("/win"),
            "platform suffix unexpected: {tag}"
        );
        Ok(())
    }

    /// Time lower bound: a date becomes that day's midnight, RFC3339 kept, empty string errors
    #[test]
    fn normalize_since_rules() -> anyhow::Result<()> {
        assert_eq!(normalize_since("2026-09-20")?, "2026-09-20T00:00:00Z");
        assert_eq!(normalize_since("  2026-09-20  ")?, "2026-09-20T00:00:00Z");
        assert_eq!(
            normalize_since("2026-09-20T04:54:18.486Z")?,
            "2026-09-20T04:54:18.486Z"
        );
        assert!(normalize_since("").is_err(), "empty string must error");
        assert!(
            normalize_since("   ").is_err(),
            "whitespace-only must error"
        );
        Ok(())
    }

    /// Later bound: both given → max; one given → that one; neither → 1970 epoch (no filter).
    /// This is the real §3.8 case: when resort_at is later than explicit --since, use resort_at.
    #[test]
    fn later_bound_picks_later() {
        let a = Some("2026-09-19T00:00:00Z".to_owned());
        let b = Some("2026-09-20T04:54:18.486Z".to_owned());
        assert_eq!(
            later_bound(a.clone(), b.clone()),
            "2026-09-20T04:54:18.486Z"
        );
        assert_eq!(
            later_bound(b.clone(), a.clone()),
            "2026-09-20T04:54:18.486Z"
        );
        assert_eq!(later_bound(a.clone(), None), "2026-09-19T00:00:00Z");
        assert_eq!(later_bound(None, b.clone()), "2026-09-20T04:54:18.486Z");
        assert_eq!(later_bound(None, None), "1970-01-01T00:00:00Z");
    }

    /// resort_at read/write loop: after counter_reset, resort_at reads back the same value;
    /// no file / no key → None (list --since-resort then falls back to no filter)
    #[test]
    fn resort_at_roundtrip() -> anyhow::Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("om-resort-at-{nonce}"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("maintenance.json");
        assert!(counter_reset(&path, "").is_ok());
        // Write empty stamp → resort_at treated as missing (filter drops empty)
        let v = std::fs::read_to_string(&path)?;
        assert!(serde_json::from_str::<serde_json::Value>(&v)?["resort_at"].is_string());
        counter_reset(&path, "2026-09-20T04:54:18.486Z")?;
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        assert_eq!(v["resort_at"], "2026-09-20T04:54:18.486Z");
        assert_eq!(v["writes_since_resort"], 0);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn mask_secret_shape() {
        assert_eq!(mask_secret("abcdefghijkl"), "abcd\u{2026}ijkl");
        assert_eq!(mask_secret("short"), "****");
    }

    /// Whole-library bound: `--since 1900` must be earlier than every real created_at (RFC3339 lexicographic compare),
    /// so it equals no filter — this is how "resort the whole library" takes its set; no extra mode (frozen 2026-09-20).
    #[test]
    fn since_1900_equals_whole_library() -> anyhow::Result<()> {
        let bound = normalize_since("1900")?;
        assert_eq!(bound, "1900");
        // Lexicographic order is time order: a 1900 prefix is less than any 20xx/21xx RFC3339 stamp
        for ts in [
            "2026-09-20T05:01:50.121Z",
            "2026-09-20T04:22:18.093Z",
            "2000-01-01T00:00:00Z",
            "2100-01-01T00:00:00Z",
        ] {
            assert!(
                ts > bound.as_str(),
                "{ts} should be later than the 1900 bound"
            );
        }
        // Combined with --since-resort, 1900 must not hide resort_at (take the later one)
        assert_eq!(
            later_bound(Some(bound), Some("2026-09-20T04:54:18.486Z".to_owned())),
            "2026-09-20T04:54:18.486Z"
        );
        Ok(())
    }
include!("tests/configuration_and_scope.rs");
include!("tests/workspace_and_split.rs");
include!("tests/memory_operations.rs");
