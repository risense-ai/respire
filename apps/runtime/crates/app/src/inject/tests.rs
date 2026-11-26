use super::*;

#[test]
fn targets_include_all_thirteen() -> anyhow::Result<()> {
    let iso = crate::test_lock::Isolate::new()?;
    let _paths = fixture_paths::Scoped::new(iso.path())?;
    let ts = targets()?;
    let ids: Vec<_> = ts.iter().map(|t| t.id).collect();
    for want in [
        "dsh",
        "opencode",
        "codex",
        "claude",
        "codebuddy",
        "workbuddy",
        "kylinbot",
        "pi",
        "zigcode",
        "deepseek",
        "qwen",
        "doubao",
        "generic",
    ] {
        assert!(ids.contains(&want), "missing target {want}");
    }
    Ok(())
}

#[test]
fn reference_mode_pure_source_is_fresh() -> anyhow::Result<()> {
    let _iso = crate::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("respire.md");
    std::fs::write(&path, instructions_md())?;
    assert_eq!(check_state(path, "reference"), "fresh");
    Ok(())
}

#[test]
fn cover_mode_stale_when_drifted() -> anyhow::Result<()> {
    let _iso = crate::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("AGENTS.md");
    std::fs::write(&path, format!("{INSTRUCTIONS_MD}\n<!-- \u{522b}\u{4eba}\u{52a0}\u{7684}\u{4e00}\u{884c} -->\n"))?;
    assert_eq!(check_state(path, "inject"), "stale");
    Ok(())
}

#[test]
fn workbuddy_reference_respects_mode_and_does_not_require_full_policy_each_round() -> Result<()> {
    let _iso = crate::test_lock::Isolate::new()?;
    for mode in ["normal", "readonly", "off"] {
        crate::service::set_workspace_mode(mode)?;
        let text = wb_ref_md("/isolated/respire.md");
        assert!(text.chars().count() < 4000, "{mode} reference exceeds host limit");
        assert!(!text.contains("Read the complete policy before each round"));
        match mode {
            "normal" => assert!(text.contains("one bounded archival pass")),
            "readonly" => {
                assert!(text.contains("There is no storage obligation in this mode"));
                assert!(!text.contains("Keep one bounded archival pass"));
            }
            "off" => {
                assert!(text.contains("Memory is off"));
                assert!(!text.contains("rsrs recall"));
                assert!(!text.contains("rsrs remember"));
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}
