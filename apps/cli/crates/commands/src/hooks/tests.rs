use super::*;

#[test]
fn test_event_roundtrip() {
    for e in [
        HookEvent::PreRemember,
        HookEvent::PostRemember,
        HookEvent::PostRecall,
        HookEvent::PostForget,
        HookEvent::PostSync,
    ] {
        assert_eq!(HookEvent::parse(e.as_str()), Some(e));
    }
    assert_eq!(HookEvent::parse("nope"), None);
    assert!(HookEvent::PreRemember.is_pre());
    assert!(!HookEvent::PostSync.is_pre());
}

#[test]
fn test_config_missing_is_empty() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("missing/plugins.json");
    let cfg = read_config_at(&path)?;
    assert!(cfg.hooks.is_empty());
    let v = fire_at(&path, HookEvent::PreRemember, serde_json::json!({}))?;
    assert!(!v.blocked);
    Ok(())
}

#[test]
fn test_payload_summary_only() {
    let p = remember_payload("t", "normal", "context", "proj", "a very long body");
    assert_eq!(p["title"], "t");
    assert!(p["content_head"].is_string());
    assert!(p.get("id").is_none());
}

#[test]
fn fire_runs_shell_hook() -> anyhow::Result<()> {
    let _g = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("plugins.json");
    let cfg = serde_json::json!({
        "hooks": {
            "pre-remember": [{"cmd": "exit 0", "timeout_ms": 2000, "on_error": "warn"}]
        }
    });
    std::fs::write(&path, cfg.to_string())?;
    let v = fire_at(&path, HookEvent::PreRemember, serde_json::json!({"title":"t"}))?;
    assert!(!v.blocked);
    let cfg_block = serde_json::json!({
        "hooks": {
            "pre-remember": [{"cmd": "exit 1", "timeout_ms": 2000, "on_error": "block"}]
        }
    });
    std::fs::write(&path, cfg_block.to_string())?;
    let v = fire_at(&path, HookEvent::PreRemember, serde_json::json!({}))?;
    assert!(v.blocked);
    Ok(())
}

#[test]
fn payload_builders_and_event_names() -> anyhow::Result<()> {
    assert_eq!(EVENT_NAMES.len(), 5);
    let done = remember_done_payload("id", "t", "important");
    assert_eq!(done["id"], "id");
    let rec = recall_payload("q", "p", &[("a".into(), 0.9)]);
    assert_eq!(rec["query"], "q");
    assert_eq!(forget_payload("x")["id"], "x");
    assert_eq!(sync_payload(1, 2)["pulled"], 1);
    let dir = tempfile::tempdir()?;
    let v = fire_at(&dir.path().join("plugins.json"), HookEvent::PostSync, serde_json::json!({}))?;
    assert!(!v.blocked);
    Ok(())
}

#[test]
fn hook_failure_policies_and_explicit_veto() -> anyhow::Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("plugins.json");
    for (event, policy, blocked, warned) in [
        (HookEvent::PreRemember, "", false, true),
        (HookEvent::PreRemember, "warn", false, true),
        (HookEvent::PreRemember, "skip", false, false),
        (HookEvent::PreRemember, "block", true, false),
        (HookEvent::PostSync, "", false, false),
        (HookEvent::PostSync, "block", false, false),
        (HookEvent::PostSync, "warn", false, true),
    ] {
        let cfg = serde_json::json!({"hooks": {event.as_str(): [
            {"cmd": "exit 1", "on_error": policy, "timeout_ms": 2000}
        ]}});
        std::fs::write(&path, cfg.to_string())?;
        let verdict = fire_at(&path, event, serde_json::json!({}))?;
        assert_eq!(
            verdict.blocked,
            blocked,
            "{} policy={policy}",
            event.as_str()
        );
        assert_eq!(!verdict.warnings.is_empty(), warned);
        assert_eq!(verdict.failures.len(), 1, "execution errors must reach host acknowledgements, including skip");
        assert!(!verdict.reason.contains("failed to start"));
        assert!(verdict
            .warnings
            .iter()
            .all(|warning| !warning.contains("failed to start")));
    }
    let veto_command = if cfg!(windows) {
        "echo {\"verdict\":\"block\",\"reason\":\"private data\"}"
    } else {
        "echo '{\"verdict\":\"block\",\"reason\":\"private data\"}'"
    };
    let cfg = serde_json::json!({"hooks": {"pre-remember": [
        {"cmd": veto_command, "on_error": "skip"},
        {"cmd": "exit 1", "on_error": "warn"}
    ]}});
    std::fs::write(&path, cfg.to_string())?;
    for payload in [
        serde_json::json!({}),
        serde_json::json!({"data": "x".repeat(2 * 1024 * 1024)}),
    ] {
        // The echo hook exits without reading stdin, including a payload larger than the pipe.
        let verdict = fire_at(&path, HookEvent::PreRemember, payload)?;
        assert!(verdict.blocked, "{} {:?}", verdict.reason, verdict.warnings);
        assert!(verdict.reason.contains("private data"));
        assert!(verdict.warnings.is_empty(), "a veto must stop later hooks");
    }
    let outcome = run_hook(
        &HookSpec {
            cmd: "exit 0".to_owned(),
            timeout_ms: 2000,
            on_error: "skip".to_owned(),
        },
        &serde_json::json!({"data": "x".repeat(2 * 1024 * 1024)}),
    );
    match outcome {
        HookOutcome::Failed(error) => assert!(error.contains("stdin write failed"), "{error}"),
        _ => panic!("a broken pipe without an explicit veto must still fail"),
    }
    std::fs::write(&path, "invalid json")?;
    assert!(read_config_at(&path).is_err());
    let post = fire_config(&serde_json::from_value(serde_json::json!({"hooks": {"post-sync": [
        {"cmd": veto_command}, {"cmd": "exit 1", "on_error": "warn"}
    ]}}))?, HookEvent::PostSync, serde_json::json!({}));
    assert!(!post.blocked, "post events cannot veto committed work");
    assert_eq!(post.warnings.len(), 2, "post veto must not suppress later hooks");
    Ok(())
}

#[test]
fn timeout_hook_child() -> anyhow::Result<()> {
    let event = std::env::var("RSRS_HOOK_EVENT").unwrap_or_default();
    if let Some(path) = event.strip_prefix("timeout-test:") {
        std::fs::write(path, "running")?;
        std::thread::sleep(Duration::from_secs(4));
        std::fs::write(path, "survived")?;
    }
    Ok(())
}

#[test]
fn timeout_terminates_hook_tree_and_unblocks_writer() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("hook-state");
    let executable = std::env::current_exe()?;
    #[cfg(windows)]
    let cmd = format!(
        "\"\"{}\" --exact hooks::tests::timeout_hook_child --nocapture\"",
        executable.display()
    );
    #[cfg(not(windows))]
    let cmd = format!(
        "'{}' --exact hooks::tests::timeout_hook_child --nocapture",
        executable.display().to_string().replace('\'', "'\\''")
    );
    let spec = HookSpec {
        cmd,
        timeout_ms: 2000,
        on_error: "block".to_owned(),
    };
    let envelope = serde_json::json!({
        "event": format!("timeout-test:{}", marker.display()),
        "data": "x".repeat(2 * 1024 * 1024),
    });
    let started = Instant::now();
    let outcome = run_hook(&spec, &envelope);
    let failure = match outcome {
        HookOutcome::Failed(failure) => failure,
        _ => panic!("expected timeout failure"),
    };
    assert!(started.elapsed() < Duration::from_secs(6), "{failure}");
    assert!(failure.contains("timeout"), "{failure}");
    assert!(failure.contains("stdin write failed"), "{failure}");
    assert!(!failure.contains("did not stop"), "{failure}");
    assert!(!failure.contains("not reaped"), "{failure}");
    assert!(!failure.contains("could not be reaped"), "{failure}");
    assert!(!failure.contains("termination"), "{failure}");
    assert!(!failure.contains("stdout read"), "{failure}");
    assert!(!failure.contains("stderr read"), "{failure}");
    assert_eq!(std::fs::read_to_string(&marker)?, "running");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(std::fs::read_to_string(&marker)?, "running");
    Ok(())
}
