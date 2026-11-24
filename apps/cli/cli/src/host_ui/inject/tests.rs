use super::*;

#[test]
fn block_roundtrip_preserves_user_content() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("AGENTS.md");
    std::fs::write(&path, "# \u{7528}\u{6237}\u{81ea}\u{6709}\u{5185}\u{5bb9}\n\n\u{81ea}\u{5b9a}\u{4e49}\u{89c4}\u{5219}\u{3002}\n")?;
    assert!(write_block(path.clone())?);
    let text = std::fs::read_to_string(&path)?;
    assert!(text.starts_with("# \u{7528}\u{6237}\u{81ea}\u{6709}\u{5185}\u{5bb9}"));
    assert!(text.contains(BLOCK_BEGIN) && text.contains(BLOCK_END));
    assert!(text.contains("respire"));
    // Idempotent: injecting again must not duplicate
    assert!(!write_block(path.clone())?);
    // Strip the block and restore
    assert!(remove_block(path.clone())?);
    let after = std::fs::read_to_string(&path)?;
    assert_eq!(after.trim(), "# \u{7528}\u{6237}\u{81ea}\u{6709}\u{5185}\u{5bb9}\n\n\u{81ea}\u{5b9a}\u{4e49}\u{89c4}\u{5219}\u{3002}");
    assert!(!after.contains("respire"));
    Ok(())
}

#[test]
fn block_replace_updates_stale() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("CLAUDE.md");
    std::fs::write(&path, format!(" mine\n{BLOCK_BEGIN}\nold\n{BLOCK_END}\n"))?;
    assert!(write_block(path.clone())?);
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("mine"));
    assert!(text.contains("respire"));
    assert!(!text.contains("old\n#"));
    assert_eq!(text.matches(BLOCK_BEGIN).count(), 1);
    for old in [
        format!("mine\n{LEGACY_BLOCK_BEGIN}\nlegacy\n{LEGACY_BLOCK_END}\n"),
        format!("mine\n{LEGACY_BLOCK_BEGIN}\nlegacy\n{LEGACY_BLOCK_END}\nbetween\n{BLOCK_BEGIN}\nold\n{BLOCK_END}\ntail\n"),
    ] {
        std::fs::write(&path, old)?;
        assert_eq!(check_state(path.clone(), "block")?, "stale");
        assert!(write_block(path.clone())?);
        let migrated = std::fs::read_to_string(&path)?;
        assert!(migrated.starts_with("mine\n"));
        assert_eq!(migrated.matches(BLOCK_BEGIN).count(), 1);
        assert!(!migrated.contains(LEGACY_BLOCK_BEGIN));
        assert!(!migrated.contains("legacy\n"));
        assert!(!write_block(path.clone())?);
        assert!(remove_block(path.clone())?);
        assert!(!std::fs::read_to_string(&path)?.contains(BLOCK_BEGIN));
    }
    let incomplete = format!("mine\n{LEGACY_BLOCK_BEGIN}\nlegacy\n");
    std::fs::write(&path, &incomplete)?;
    assert!(write_block(path.clone()).is_err());
    assert_eq!(std::fs::read_to_string(&path)?, incomplete);
    Ok(())
}

#[test]
fn cover_remove_keeps_user_file() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("AGENTS.md");
    std::fs::write(&path, "\u{81ea}\u{6709}")?;
    // Not an inject-source file → strip the block only (no-op if none)
    assert!(!remove_cover(path.clone())?);
    assert_eq!(std::fs::read_to_string(&path)?, "\u{81ea}\u{6709}");
    // Pure inject file → delete the whole file
    std::fs::write(&path, INSTRUCTIONS_MD)?;
    assert!(remove_cover(path.clone())?);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn targets_include_all_thirteen() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
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

/// Reference mode (opencode): the target file is the full inject source — no block markers, still counts as fresh (fixed 2026-09-06).
#[test]
fn reference_mode_pure_source_is_fresh() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("respire.md");
    std::fs::write(&path, instructions_md()?)?;
    assert_eq!(check_state(path, "reference")?, "fresh");
    Ok(())
}

#[test]
fn cover_mode_stale_when_drifted() -> anyhow::Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("AGENTS.md");
    std::fs::write(
        &path,
        format!(
            "{INSTRUCTIONS_MD}\n<!-- \u{522b}\u{4eba}\u{52a0}\u{7684}\u{4e00}\u{884c} -->\n"
        ),
    )?;
    assert_eq!(check_state(path, "inject")?, "stale");
    Ok(())
}

/// opencode instructions: keep other entries, normalize `~` vs absolute, idempotent, uninstall drops only this item.
#[test]
fn opencode_instructions_preserve_entries_and_normalize() -> Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("opencode.json");
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "instructions": ["~/other.md", entity_display()], "model": "x"
        }))?,
    )?;
    let abs = entity_abs()?;
    assert!(patch_instructions(&path, true)?);
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let arr = v["instructions"]
        .as_array()
        .ok_or_else(|| anyhow!("instructions is not an array"))?;
    assert!(
        arr.iter().any(|x| x.as_str() == Some("~/other.md")),
        "other entries were deleted by mistake"
    );
    assert_eq!(
        arr.iter()
            .filter(|x| x.as_str() == Some(abs.as_str()))
            .count(),
        1,
        "normalized form has duplicates"
    );
    assert_eq!(v["model"], "x");
    assert!(!patch_instructions(&path, true)?, "not idempotent");
    assert!(patch_instructions(&path, false)?);
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(
        v["instructions"]
            .as_array()
            .ok_or_else(|| anyhow!("instructions is not an array"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn opencode_patch_errors_preserve_user_bytes_and_missing_remove_is_noop() -> Result<()> {
    let _isolation = crate::host_ui::test_lock::Isolate::new()?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("nested/opencode.json");
    assert!(!patch_instructions(&path, false)?);
    assert!(!path.exists());
    assert!(patch_instructions(&path, true)?);
    assert!(!patch_instructions(&path, true)?);
    for bytes in [b"{".as_slice(), b"[]".as_slice(), b"{\"instructions\":\"user.md\"}".as_slice()] {
        std::fs::write(&path, bytes)?;
        assert!(patch_instructions(&path, true).is_err());
        assert!(patch_instructions(&path, false).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    std::fs::write(&path, b"{\"model\":\"user-selected\"}")?;
    patch_instructions(&path, false)?;
    assert!(!patch_instructions(&path, false)?);
    assert!(patch_instructions(&path, true)?);
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(value["model"], "user-selected");
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    assert!(patch_instructions(&path, true).is_err());
    assert!(patch_prompt_append(&path, true).is_err());
    Ok(())
}

#[test]
fn opencode_lifecycle_respects_later_config_and_keeps_the_shared_entity() -> Result<()> {
    let _isolation = crate::host_ui::test_lock::Isolate::new()?;
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }
    let directory = tempfile::tempdir()?;
    let _home = RestoreHome(std::env::var_os("HOME"));
    std::env::set_var("HOME", directory.path());
    assert_eq!(opencode_state()?, "none");
    assert!(!remove_opencode()?);
    assert!(inject_opencode()?);
    assert_eq!(opencode_state()?, "fresh");
    assert!(!inject_opencode()?);
    let config = directory.path().join(".config/opencode");
    let later = config.join("opencode.jsonc");
    std::fs::write(&later, b"{\"instructions\":[\"user.md\"]}")?;
    assert_eq!(opencode_state()?, "none");
    std::fs::write(&later, b"{\"model\":\"user-model\"}")?;
    assert_eq!(opencode_state()?, "fresh");
    std::fs::write(&later, b"{")?;
    assert_eq!(opencode_state()?, "manual");
    assert!(inject_opencode().is_err());
    assert_eq!(std::fs::read(&later)?, b"{");
    std::fs::remove_file(&later)?;
    std::fs::create_dir(&later)?;
    assert_eq!(opencode_state()?, "manual");
    std::fs::remove_dir(&later)?;
    let append = config.join("oh-my-openagent.json");
    std::fs::write(&append, b"{\"agents\":{\"reviewer\":{\"prompt_append\":\"Keep this user rule\"}}}")?;
    assert!(inject_opencode()?);
    let installed: serde_json::Value = serde_json::from_slice(&std::fs::read(&append)?)?;
    assert_eq!(
        installed["agents"]["reviewer"]["prompt_append"],
        format!("Keep this user rule\n- Memory rules (respire): {}", entity_abs()?)
    );
    assert!(remove_opencode()?);
    assert!(entity_path()?.is_file());
    assert_eq!(opencode_state()?, "none");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&append)?)?;
    // Adding a managed line separates it from an unterminated user line.
    // Removal preserves that user line's newline and drops only our reference.
    assert_eq!(value["agents"]["reviewer"]["prompt_append"], "Keep this user rule\n");
    assert!(!remove_opencode()?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn cover_replacement_does_not_follow_a_symlink_into_user_source() -> Result<()> {
    let _isolation = crate::host_ui::test_lock::Isolate::new()?;
    let directory = tempfile::tempdir()?;
    let user = directory.path().join("user.md");
    let cover = directory.path().join("cover.md");
    std::fs::write(&user, b"user source must remain unchanged")?;
    std::os::unix::fs::symlink(&user, &cover)?;
    assert!(write_cover(cover.clone())?);
    assert!(!std::fs::symlink_metadata(&cover)?.file_type().is_symlink());
    assert_eq!(std::fs::read(&user)?, b"user source must remain unchanged");
    assert_eq!(check_state(cover.clone(), "reference")?, "fresh");
    assert!(remove_cover(cover.clone())?);
    assert!(!remove_cover(cover)?);
    Ok(())
}

#[test]
fn workbuddy_reference_respects_mode_and_does_not_require_full_policy_each_round() -> Result<()>
{
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    for mode in ["normal", "readonly", "off"] {
        std::fs::write(
            respire_client_host::paths::client_config_path(),
            serde_json::to_vec(&serde_json::json!({"service_mode": mode}))?,
        )?;
        let text = wb_ref_md("/isolated/respire.md")?;
        assert!(
            text.chars().count() < 4000,
            "{mode} reference exceeds host limit"
        );
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

#[test]
fn prompt_append_migrates_legacy_reference_preserves_user_config_and_removes_cleanly(
) -> Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.json");
    let abs = entity_abs()?;
    let current = format!("- Memory rules (respire): {abs}");
    let legacy =
        format!("- \u{8bb0}\u{5fc6}\u{94c1}\u{5f8b}\u{ff08}respire\u{ff09}\u{ff1a}{abs}");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "model":"owned-model", "agents":[{"prompt_append":format!("OWNED_KEEP\n{legacy}\n{current}\n")}]
        }))?,
    )?;
    assert!(patch_prompt_append(&path, true)?);
    let read =
        || -> Result<serde_json::Value> { Ok(serde_json::from_slice(&std::fs::read(&path)?)?) };
    let cfg = read()?;
    let prompt = cfg["agents"][0]["prompt_append"]
        .as_str()
        .ok_or_else(|| anyhow!("missing prompt"))?;
    assert!(prompt.contains("OWNED_KEEP"));
    assert!(!prompt.contains(&legacy));
    assert_eq!(prompt.matches(&current).count(), 1);
    assert_eq!(cfg["model"], "owned-model");
    assert!(!patch_prompt_append(&path, true)?);
    assert!(patch_prompt_append(&path, false)?);
    let cfg = read()?;
    assert_eq!(cfg["agents"][0]["prompt_append"], "OWNED_KEEP\n");
    assert_eq!(cfg["model"], "owned-model");
    Ok(())
}

#[test]
fn workbuddy_budget_counts_entire_document_and_refuses_without_mutation() -> Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("MEMORY.md");
    let content = wb_ref_md("/isolated/policy.md")?;
    let overhead = format!("\n\n{BLOCK_BEGIN}\n{}\n{BLOCK_END}\n", content.trim_end())
        .chars()
        .count();
    for total in [3999, 4000, 4001] {
        // Multibyte owned text verifies this is a character, not byte, budget.
        let owned = format!("{}\n\n", "\u{754c}".repeat(total - overhead));
        std::fs::write(&path, &owned)?;
        let preview = block_preview_with(&path, false, &content)?;
        assert_eq!(preview.after.chars().count(), total);
        if total <= WORKBUDDY_MAX_CHARS {
            assert!(write_block_with(&path, &content)?);
            assert!(std::fs::read_to_string(&path)?.starts_with(&owned));
            assert!(!write_block_with(&path, &content)?);
        } else {
            assert!(write_block_with(&path, &content)
                .unwrap_err()
                .to_string()
                .contains("maximum 4000"));
            assert_eq!(std::fs::read(&path)?, owned.as_bytes());
            // Removal remains possible for an already over-budget document.
            std::fs::write(&path, &preview.after)?;
            assert_eq!(check_state(path.clone(), "ref-block")?, "stale");
            assert!(remove_block(path.clone())?);
            assert!(std::fs::read_to_string(&path)?.starts_with(&owned));
        }
    }
    let new_path = dir.path().join("not-created/MEMORY.md");
    assert!(write_block_with(&new_path, &"x".repeat(4000)).is_err());
    assert!(!new_path.exists() && !new_path.parent().ok_or_else(|| anyhow!("fixture path has no parent"))?.exists());
    let owned = "OWNED\r\n".repeat(700);
    std::fs::write(&path, owned.as_bytes())?;
    assert!(write_block_with(&path, &content).is_err());
    assert_eq!(std::fs::read(&path)?, owned.as_bytes());
    Ok(())
}

#[test]
fn workbuddy_midflight_user_changes_are_preserved_and_partial_state_is_reported() -> Result<()>
{
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("MEMORY.md");
    let entity = dir.path().join("policy.md");
    let content = wb_ref_md("/isolated/policy.md")?;
    for concurrent in ["NEW USER CONTENT\r\n".to_owned(), "\u{754c}".repeat(4001)] {
        std::fs::write(&path, "ORIGINAL USER CONTENT\r\n")?;
        let preview = block_preview_with(&path, false, &content)?;
        check_document_budget(&preview, WORKBUDDY_MAX_CHARS)?;
        std::fs::write(&entity, "OLD POLICY")?;
        assert!(write_cover(entity.clone())?);
        // The user edits after the first target has been updated.
        std::fs::write(&path, concurrent.as_bytes())?;
        let error =
            finish_workbuddy_injection(&path, &entity, &content, &preview.revision, true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("partially completed: shared policy was updated"));
        assert!(error.contains("no automatic rollback"));
        assert!(error.contains("rerun rsrs inject --id workbuddy only after review"));
        assert_eq!(std::fs::read(&path)?, concurrent.as_bytes());
        assert_eq!(std::fs::read_to_string(&entity)?, instructions_md()?);
    }
    Ok(())
}

#[test]
fn workbuddy_second_target_io_failure_reports_partial_completion_without_rollback() -> Result<()>
{
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("MEMORY.md");
    let moved = dir.path().join("user-moved-memory.md");
    let entity = dir.path().join("policy.md");
    let content = wb_ref_md("/isolated/policy.md")?;
    let owned = b"USER FILE\r\n";
    std::fs::write(&path, owned)?;
    let preview = block_preview_with(&path, false, &content)?;
    check_document_budget(&preview, WORKBUDDY_MAX_CHARS)?;
    assert!(write_cover(entity.clone())?);
    // An actual filesystem read failure in the second target, after cover.
    std::fs::rename(&path, &moved)?;
    std::fs::create_dir(&path)?;
    let error = finish_workbuddy_injection(&path, &entity, &content, &preview.revision, true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("partially completed: shared policy was updated"));
    assert!(error.contains("failed to read"));
    assert!(error.contains("Inspect both current files"));
    assert!(path.is_dir());
    assert_eq!(std::fs::read(&moved)?, owned);
    assert_eq!(std::fs::read_to_string(entity)?, instructions_md()?);
    Ok(())
}

#[test]
fn prompt_reference_migration_matches_whole_lines_and_preserves_examples_crlf() {
    let current = "- Memory rules (respire): /policy.md";
    let legacy = "- \u{8bb0}\u{5fc6}\u{94c1}\u{5f8b}\u{ff08}respire\u{ff09}\u{ff1a}/policy.md";
    let owned = format!("USER\r\nExample: {legacy}\r\n> {legacy}\r\n`{legacy}`\r\n{legacy} suffix\r\n```text\r\n{legacy}\r\n{current}\r\n```\r\n~~~\r\n{legacy}\r\n~~~\r\n");
    let source = format!("{owned}{legacy}\r\n{current}\r\nTAIL\r\n");
    let migrated = rewrite_prompt_reference(&source, true, current, legacy);
    assert_eq!(migrated, format!("{owned}{current}\r\nTAIL\r\n"));
    assert_eq!(
        rewrite_prompt_reference(&migrated, true, current, legacy),
        migrated
    );
    assert_eq!(
        rewrite_prompt_reference(&migrated, false, current, legacy),
        format!("{owned}TAIL\r\n")
    );
    assert_eq!(
        rewrite_prompt_reference(&owned, false, current, legacy),
        owned
    );
    let quoted = format!("quoted \"{legacy}\"\r\n");
    assert_eq!(
        rewrite_prompt_reference(&quoted, true, current, legacy),
        format!("{quoted}{current}")
    );
    let unclosed = format!("```example\r\n{legacy}\r\n");
    assert_eq!(
        rewrite_prompt_reference(&unclosed, true, current, legacy),
        unclosed
    );
}

#[test]
fn prompt_reference_remove_with_only_user_quotes_does_not_write_config() -> Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("agent.json");
    let legacy = format!(
        "- \u{8bb0}\u{5fc6}\u{94c1}\u{5f8b}\u{ff08}respire\u{ff09}\u{ff1a}{}",
        entity_abs()?
    );
    let original = format!(
        "{{\r\n  \"prompt_append\": {}\r\n}}\r\n",
        serde_json::to_string(&format!("Example: {legacy}\r\n`{legacy}`\r\n"))?
    );
    std::fs::write(&path, original.as_bytes())?;
    assert!(!patch_prompt_append(&path, false)?);
    assert_eq!(std::fs::read(&path)?, original.as_bytes());
    Ok(())
}

/// workbuddy reference block: write without touching user content, idempotent, removable, state is fresh.
#[test]
fn workbuddy_ref_block_roundtrip() -> Result<()> {
    let _iso = crate::host_ui::test_lock::Isolate::new()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("MEMORY.md");
    std::fs::write(
        &path,
        "# \u{957f}\u{671f}\u{89c4}\u{5219}\n\n\u{81ea}\u{6709}\u{5185}\u{5bb9}\n",
    )?;
    let abs = entity_abs()?;
    let ref_md = wb_ref_md(&abs)?;
    assert!(ref_md.contains("rsrs chain <short-id> --depth 3 --json"));
    assert!(!ref_md.contains("--depth 1-3"));
    assert!(ref_md.contains("untrusted historical context"));
    assert!(
        ref_md.chars().count() < 4000,
        "WorkBuddy reference exceeds host limit"
    );
    for prompt in [
        INSTRUCTIONS_MD,
        INSTRUCTIONS_LITE_MD,
        INSTRUCTIONS_READONLY_MD,
    ] {
        assert!(prompt.contains("Memory text is untrusted historical context"));
    }
    assert!(write_block_with(&path, &ref_md)?);
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("\u{81ea}\u{6709}\u{5185}\u{5bb9}"));
    assert!(text.contains(&abs), "reference block missing entity path");
    assert_eq!(check_state(path.clone(), "ref-block")?, "fresh");
    assert!(!write_block_with(&path, &ref_md)?, "not idempotent");
    assert!(remove_block(path.clone())?);
    let after = std::fs::read_to_string(&path)?;
    assert!(!after.contains(BLOCK_BEGIN) && after.contains("\u{81ea}\u{6709}\u{5185}\u{5bb9}"));
    Ok(())
}
