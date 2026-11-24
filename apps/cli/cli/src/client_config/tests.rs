use super::*;

fn edit(plan: ConfigPlan) -> Result<ConfigEdit> {
    match plan {
        ConfigPlan::HostWrite(edit)
        | ConfigPlan::ProfileMaintenance(edit)
        | ConfigPlan::RuntimeValidation(edit) => Ok(edit),
        ConfigPlan::ReadOnly(_) => anyhow::bail!("expected an edit plan"),
    }
}

#[test]
fn candidate_rollback_restores_exact_bytes_and_refuses_concurrent_writes() -> Result<()> {
    let _isolation = crate::host_ui::test_lock::Isolate::new()?;
    let path = paths::client_config_path();
    fs::create_dir_all(path.parent().context("config parent missing")?)?;
    let original = b"{ \"autosync\": false, \"cure_auto\": true }\n";
    fs::write(&path, original)?;
    let planned = edit(prepare_config(None, Some("https://example.invalid"), None, None, None)?)?;
    let staged = planned.stage_candidate()?;
    assert_eq!(staged.revision(), planned.assert_candidate()?.revision());
    planned.rollback_candidate()?;
    assert_eq!(fs::read(&path)?, original);
    planned.rollback_candidate()?;
    planned.stage_candidate()?;
    let external = b"{\"autosync\":false,\"cure_auto\":false}";
    fs::write(&path, external)?;
    for conflict in [
        planned.assert_candidate().err().context("candidate edit was not detected")?,
        planned.rollback_candidate().err().context("rollback overwrote an external edit")?,
        planned.stage_candidate().err().context("publication overwrote an external edit")?,
    ] {
        assert!(conflict.downcast_ref::<ConfigConflict>().is_some());
        assert!(conflict.to_string().contains("client config changed"));
    }
    assert_eq!(fs::read(&path)?, external);
    assert_eq!(fs::read(&path)?, external);
    use respire_protocol::config_control::{AgentConfigIntent, AgentConfigOutcome, AgentConfigReceipt};
    let root = paths::main_data_dir().canonicalize()?;
    let agent = root.join("agent.json");
    let review = review_agent_config(AgentConfigIntent { key: "diary_mode".into(), raw_value: "verbose".into() },
        root.clone(), root.clone())?;
    fs::write(&agent, b"{\"third_party\":true}")?;
    assert!(publish_agent_config(&review, || anyhow::bail!("receipt must not be called on conflict")).is_err());
    assert_eq!(fs::read(&agent)?, b"{\"third_party\":true}");
    fs::remove_file(&agent)?;
    let mut published = false;
    publish_agent_config(&review, || { published = true; Ok(()) })?;
    assert!(published);
    let receipt = AgentConfigReceipt { review: review.clone(), outcome: AgentConfigOutcome::Committed, result: Value::Null };
    verify_agent_config_receipt(&receipt)?;
    assert!(publish_agent_config(&review, || anyhow::bail!("original must not be replayed")).is_err());
    fs::write(&agent, b"{\"third_party\":true}")?;
    assert!(verify_agent_config_receipt(&receipt).is_err());
    for corrupt in [b"{".as_slice(), b"[]".as_slice(), b"null".as_slice()] {
        fs::write(&agent, corrupt)?;
        assert!(read_agent_config_for_write(&root).is_err());
        assert!(review_agent_config(review.intent.clone(), root.clone(), root.clone()).is_err());
        assert_eq!(fs::read(&agent)?, corrupt);
    }
    fs::remove_file(&agent)?;
    assert!(publish_agent_config(&review, || anyhow::bail!("synthetic receipt write failure")).is_err());
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&agent)?)?, json!({"diary_mode":"verbose"}));
    rollback_agent_config(&review, || Ok(()))?;
    assert!(!agent.exists());
    assert!(rollback_agent_config(&review, || anyhow::bail!("rollback must not replay")).is_err());
    let original = b"{ \"keep\": true, \"read_heat\": true }\n";
    fs::write(&agent, original)?;
    let captured = agent_config_bytes(&agent)?;
    fs::write(&agent, br#"{"read_heat":false}"#)?;
    assert_eq!(parse_agent_config_bytes(captured.as_deref())?["read_heat"], true);
    fs::write(&agent, original)?;
    assert_eq!(captured, Some(fs::read(&agent)?));
    assert_eq!(parse_agent_config_bytes(None)?, json!({}));
    assert!(parse_agent_config_bytes(Some(b"")).is_err());
    let special = review_agent_config(AgentConfigIntent { key: "read_heat".into(), raw_value: "false".into() },
        root.clone(), root.clone())?;
    publish_agent_config(&special, || Ok(()))?;
    let candidate = fs::read(&agent)?;
    fs::write(&agent, b"{\"third_party\":true}")?;
    assert!(rollback_agent_config(&special, || anyhow::bail!("rollback overwrote external edit")).is_err());
    assert_eq!(fs::read(&agent)?, b"{\"third_party\":true}");
    fs::write(&agent, candidate)?;
    rollback_agent_config(&special, || Ok(()))?;
    assert_eq!(fs::read(&agent)?, original);
    for corrupt in [b"{".as_slice(), b"[]".as_slice(), b"null".as_slice()] {
        fs::write(&agent, corrupt)?;
        assert!(review_agent_config(special.intent.clone(), root.clone(), root.clone()).is_err());
        assert_eq!(fs::read(&agent)?, corrupt);
    }
    Ok(())
}

#[test]
fn config_plans_enforce_coordinator_boundaries_and_report_observed_workers() -> Result<()> {
    let _isolation = crate::host_ui::test_lock::Isolate::new()?;
    assert!(matches!(prepare_config(None, None, None, None, None)?, ConfigPlan::ReadOnly(_)));
    let planned = edit(prepare_config(None, None, Some(false), Some(false), Some(3))?)?;
    let saved = planned.publish_host()?;
    let envelope = serde_json::to_value(saved.envelope(None, false))?;
    assert_eq!(envelope["summary"]["rpc_parallelism"], 3);
    assert!(envelope["summary"]["rpc_workers"].is_null());
    let envelope = serde_json::to_value(saved.envelope(Some(2), true))?;
    assert_eq!(envelope["summary"]["rpc_workers"], 2);
    assert!(envelope["items"].as_array().context("config items missing")?
        .iter().any(|item| item["value"].as_str().is_some_and(|value| value.contains("after runtime restart"))));
    assert!(edit(prepare_config(None, Some("https://example.invalid"), None, None, None)?)?
        .publish_host().is_err());
    assert!(edit(prepare_config(None, None, Some(true), None, None)?)?
        .publish_host().is_err());
    assert!(edit(prepare_config(None, None, Some(false), None, None)?)?
        .publish_profile_switch().is_err());
    let profile = paths::main_data_dir().join("accounts/next");
    let profile_text = profile.to_str().context("fixture profile is not UTF-8")?;
    let plan = prepare_config(Some(profile_text), None, None, None, None)?;
    assert!(matches!(&plan, ConfigPlan::ProfileMaintenance(_)));
    assert!(edit(plan)?.publish_host().is_err());
    let path = paths::client_config_path();
    fs::remove_file(&path)?;
    let absent = edit(prepare_config(None, Some("https://example.invalid"), None, None, None)?)?;
    absent.stage_candidate()?;
    absent.rollback_candidate()?;
    assert!(!path.exists());
    Ok(())
}

#[test]
fn malformed_config_and_failed_publication_leave_original_files_untouched() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("client.json");
    for bytes in [b"{".as_slice(), b"{\"service_mode\":\"unexpected\"}".as_slice(), b"[]".as_slice()] {
        fs::write(&path, bytes)?;
        assert!(read_at(path.clone()).is_err());
        assert_eq!(fs::read(&path)?, bytes);
    }
    fs::remove_file(&path)?;
    fs::create_dir(&path)?;
    assert!(read_at(path.clone()).is_err());
    assert!(replace_config(&path, Some(b"{}" )).is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    let missing_parent = directory.path().join("missing/client.json");
    assert!(replace_config(&missing_parent, Some(b"{}")).is_err());
    assert!(!missing_parent.exists());
    Ok(())
}
