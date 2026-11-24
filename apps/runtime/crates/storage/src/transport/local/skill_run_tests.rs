    use super::*;

    fn store() -> (tempfile::TempDir, LocalStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::open(&dir.path().join("runs.db")).expect("open");
        (dir, store)
    }

    #[test]
    fn run_log_aggregates_ok_fail_and_latest() {
        let (_dir, store) = store();
        store.log_skill_run("skill-a", "ok", "").unwrap();
        store.log_skill_run("skill-a", "fail", "command 3 exit 2").unwrap();
        store.log_skill_run("skill-a", "ok", "").unwrap();
        let stats = store.skill_run_stats(&["skill-a".into(), "skill-b".into()]).unwrap();
        let a = stats.get("skill-a").expect("skill-a stats");
        assert_eq!((a.ok, a.fail), (2, 1));
        assert_eq!(a.last_status, "ok");
        assert!(a.last_ts > 0);
        assert!(!stats.contains_key("skill-b"), "unrun procedures stay absent");
    }

    #[test]
    fn three_consecutive_failures_are_reported_and_ok_resets() {
        let (_dir, store) = store();
        for _ in 0..3 {
            store.log_skill_run("broken", "fail", "").unwrap();
        }
        store.log_skill_run("healthy", "fail", "").unwrap();
        store.log_skill_run("healthy", "ok", "").unwrap();
        let failing = store.failing_skills(3).unwrap();
        assert_eq!(failing, vec![("broken".to_string(), 3)]);

        store.log_skill_run("broken", "ok", "").unwrap();
        assert!(store.failing_skills(3).unwrap().is_empty());
    }

    #[test]
    fn confounded_and_no_effect_are_verdicts_that_attribute_nothing() {
        let (_dir, store) = store();
        for status in ["confounded", "no_effect", "confounded"] {
            store
                .log_skill_run("a", status, "hook also changed")
                .unwrap();
        }
        let stats = store.skill_run_stats(&["a".into()]).unwrap();
        let a = stats.get("a").expect("stats exist even with no ok/fail");
        assert_eq!((a.ok, a.fail, a.confounded, a.no_effect), (0, 0, 2, 1));
        assert_eq!(a.last_status, "confounded");
        assert!(
            store.failing_skills(3).unwrap().is_empty(),
            "a non-verdict is not a failure"
        );

        // A confounded run between failures neither counts nor resets the streak.
        for _ in 0..3 {
            store.log_skill_run("b", "fail", "").unwrap();
        }
        store
            .log_skill_run("b", "confounded", "dep changed mid-run")
            .unwrap();
        assert_eq!(store.failing_skills(3).unwrap(), vec![("b".to_string(), 3)]);
        // ...and an ok still breaks it.
        store.log_skill_run("b", "ok", "").unwrap();
        assert!(store.failing_skills(3).unwrap().is_empty());
    }

    #[test]
    fn invalid_status_is_rejected() {
        let (_dir, store) = store();
        assert!(store.log_skill_run("skill-a", "maybe", "").is_err());
    }
