#[test]
fn anonymous_owner_agent_setting_is_persisted_without_changing_other_fields() -> Result<()> {
    let mut runtime = OwnedRuntime::new()?;
    let agent = runtime.fixture.profile.join("agent.json");
    std::fs::write(
        &agent,
        br#"{"keep":"synthetic unrelated setting","diary_mode":"concise"}"#,
    )?;
    let owner = runtime.start()?;
    let (output, result) =
        runtime
            .fixture
            .command(&["agent-config", "--set", "diary_mode=verbose"])?;
    anyhow::ensure!(
        output.status.success(),
        "anonymous agent setter failed: {result}"
    );
    assert_eq!(result["command"], "agent-config");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["summary"]["action"], "updated");
    assert_eq!(result["summary"]["key"], "diary_mode");
    assert_eq!(result["summary"]["value"], "verbose");
    assert_eq!(result["errors"], json!([]));
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&agent)?)?,
        json!({"keep":"synthetic unrelated setting","diary_mode":"verbose"})
    );
    let current = runtime.health()?;
    assert_eq!(current.pid, owner.pid);
    assert_eq!(current.process_start_id, owner.process_start_id);
    assert_eq!(current.instance_id, owner.instance_id);
    runtime.assert_anonymous_selection_unchanged()?;
    let committed = std::fs::read(&agent)?;
    // Host input rejection neither publishes bytes nor restarts the verified owner.
    let (output, rejected) = runtime.fixture.command(
        &["agent-config", "--set", "read_heat=not-a-bool"])?;
    assert_eq!(output.status.code(), Some(1), "{rejected}");
    assert_eq!(rejected["status"], "fail");
    assert!(rejected["errors"].as_array().context("rejection errors missing")?.iter()
        .any(|error| error.as_str().is_some_and(|error| error.contains("read_heat must be true or false"))));
    assert_eq!(std::fs::read(&agent)?, committed);
    let unchanged = runtime.health()?;
    assert_eq!(unchanged.instance_id, owner.instance_id);
    assert_eq!(unchanged.pid, owner.pid);
    assert!(!runtime.fixture.marker().exists());
    runtime.assert_anonymous_selection_unchanged()?;
    runtime.stop()?;
    Ok(())
}
