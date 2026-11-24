use clap::{CommandFactory, Parser};

#[test]
fn injected_chain_examples_follow_the_parser_contract() -> anyhow::Result<()> {
    for policy in [
        respire::inject::INSTRUCTIONS_MD,
        respire::inject::INSTRUCTIONS_LITE_MD,
    ] {
        let mut checked = 0;
        for example in policy
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|example| example.starts_with("rsrs chain ") && !example.contains('['))
        {
            let parsed = super::Cli::try_parse_from(example.split_whitespace())?;
            assert!(matches!(
                parsed.command,
                Some(super::Command::Chain { depth: 1..=3, .. })
            ));
            checked += 1;
        }
        assert!(
            checked > 0,
            "policy must retain an executable causal-chain example"
        );
    }
    assert!(super::Cli::try_parse_from([
        "rsrs",
        "chain",
        "synthetic-id",
        "--depth",
        "1-3",
        "--json"
    ])
    .is_err());
    Ok(())
}

#[test]
fn help_matches_command_contracts() -> anyhow::Result<()> {
    let mut command = super::Cli::command();
    let split = command
        .find_subcommand_mut("split")
        .ok_or_else(|| anyhow::anyhow!("split command missing"))?
        .render_long_help()
        .to_string();
    assert!(split.contains("Split a mixed node"));
    assert!(!split.contains("Tree hygiene"));
    assert!(!split.contains("--parent"));
    let cure = command
        .find_subcommand_mut("tree-cure")
        .ok_or_else(|| anyhow::anyhow!("tree-cure command missing"))?
        .render_long_help()
        .to_string();
    assert!(cure.contains("Tree hygiene"));
    let plugin = command
        .find_subcommand_mut("plugin")
        .ok_or_else(|| anyhow::anyhow!("plugin command missing"))?
        .render_long_help()
        .to_string();
    assert!(plugin.contains("list shows config"));
    assert!(!plugin.contains("--list"));
    assert!(super::Cli::try_parse_from(["rsrs", "plugin", "list"]).is_ok());
    assert!(super::Cli::try_parse_from([
        "rsrs",
        "plugin",
        "test",
        "post-sync",
        "--payload",
        "{}"
    ])
    .is_ok());
    assert!(super::Cli::try_parse_from(["rsrs", "plugin", "--list"]).is_err());
    assert!(super::Cli::try_parse_from(["rsrs", "plugin", "test"]).is_err());
    assert!(super::Cli::try_parse_from(["rsrs", "chain", "entry-id"]).is_ok());
    assert!(super::Cli::try_parse_from(["rsrs", "chain", "--from", "entry-id"]).is_err());
    assert!(super::Cli::try_parse_from(["rsrs", "config", "--data-dir", "isolated"]).is_ok());
    let revision = super::Cli::try_parse_from(["rsrs", "memory-revision", "--json"])?;
    assert!(matches!(
        revision.command,
        Some(super::Command::MemoryRevision)
    ));
    assert!(!super::is_write_command(&super::Command::MemoryRevision));
    for (args, writes) in [
        (&["rsrs", "extract", "--targets"][..], false),
        (&["rsrs", "extract", "--targets", "--all"][..], false),
        (&["rsrs", "extract", "--id", "codex"][..], true),
    ] {
        let parsed = super::Cli::try_parse_from(args.iter().copied())?;
        let extract = parsed
            .command
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("extract command missing"))?;
        assert_eq!(super::is_write_command(extract), writes);
        assert_eq!(super::is_off_allowed(extract), !writes);
    }
    assert!(!super::is_write_command(&super::Command::Taxonomy {
        list: true,
        ensure: None
    }));
    assert!(super::is_write_command(&super::Command::Taxonomy {
        list: false,
        ensure: None
    }));
    assert!(super::is_write_command(&super::Command::Taxonomy {
        list: true,
        ensure: Some("synthetic".into())
    }));
    assert!(!super::is_write_command_fine(
        &super::Command::MemoryRevision
    ));
    assert!(super::is_off_allowed(&super::Command::MemoryRevision));
    assert!(super::Cli::try_parse_from(["rsrs", "v"]).is_ok());
    assert!(super::Cli::try_parse_from(["rsrs", "version"]).is_ok());
    let v_help = command
        .find_subcommand_mut("v")
        .ok_or_else(|| anyhow::anyhow!("v command missing"))?
        .render_long_help()
        .to_string();
    assert!(v_help.to_ascii_lowercase().contains("version"));
    Ok(())
}

use super::capture_run;

#[test]
fn input_failures_remain_distinct_from_runtime_failures() {
    let input: anyhow::Error =
        respire_client_host::input_error::InputError("no such entry: fixture".into()).into();
    let (envelope, code) = super::command_failure(&input.context("resort validation"));
    assert_eq!(code, 2);
    assert_eq!(envelope.summary["reason"], "invalid_input");
    assert_eq!(envelope.details["error_type"], "user");
    let runtime: anyhow::Error = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "owned fixture I/O failure",
    )
    .into();
    let (envelope, code) = super::command_failure(&runtime);
    assert_eq!(code, 1);
    assert_eq!(envelope.summary["reason"], "runtime_error");
    assert_eq!(envelope.details["error_type"], "runtime");
    let original = "7c633a08-9077-4710-882e-ac3d2998ab8b";
    let unknown: anyhow::Error = super::autostart::AuthOutcomeUnknown {
        operation_id: original.into(),
        force: true,
    }
    .into();
    let (envelope, code) = super::command_failure(&unknown.context("private exchange"));
    assert_eq!(code, 1);
    assert_eq!(envelope.summary["reason"], "auth_outcome_unknown");
    assert_eq!(envelope.details["operation_id"], original);
    assert_eq!(envelope.details["force"], true);
    assert_eq!(envelope.details["replayed"], false);
    let unknown: anyhow::Error = super::autostart::AccountOutcomeUnknown {
        operation_id: original.into(),
        source_profile: std::path::PathBuf::from("original-profile"),
        register: true,
    }
    .into();
    let (envelope, _) = super::command_failure(&unknown);
    assert_eq!(envelope.summary["reason"], "account_outcome_unknown");
    assert_eq!(envelope.details["source_profile"], "original-profile");
    assert_eq!(envelope.details["register"], true);
    assert_eq!(envelope.details["result_command"], "runtime account-result");
    let scope = serde_json::json!({"account_id":"synthetic", "instance_id":"synthetic-instance", "generation":1});
    let unknown: anyhow::Error = super::autostart::CoreConfigUnknown {
        details: serde_json::json!({"operation_id":original, "idempotency_key":original,
            "source_profile":"synthetic-profile", "scope":scope, "kind":"core_config",
            "query_only":true, "replayed":false, "result_command":"runtime core-config-result"}),
    }.into();
    let (envelope, code) = super::command_failure(&unknown.context("private candidate must not escape"));
    assert_eq!(code, 1);
    assert_eq!(envelope.summary["core_application"]["state"], "unknown");
    assert_eq!(envelope.details["scope"], scope);
    assert_eq!(envelope.details["operation_id"], original);
    assert_eq!(envelope.details["query_only"], true);
    assert_eq!(envelope.details["replayed"], false);
    assert!(envelope.errors.iter().all(|error| !error.contains("private candidate")));
    let original_details = envelope.details.clone();
    for cleanup_stage in ["refresh_agent_config", "mark(Paused)"] {
        let Err(error) = super::autostart::CoreConfigUnknown::after_cleanup(
            original_details.clone(), Err(anyhow::anyhow!("original apply response lost")),
            Err(anyhow::anyhow!("maintenance I/O failed").context(cleanup_stage))) else {
            panic!("cleanup failure was discarded");
        };
        let chain = format!("{error:#}");
        assert!(chain.contains("original apply response lost"));
        assert!(chain.contains(cleanup_stage) && chain.contains("maintenance I/O failed"));
        let (envelope, code) = super::command_failure(&error);
        assert_eq!(code, 1);
        let mut expected = original_details.clone();
        expected["cleanup_failed"] = true.into();
        assert_eq!(envelope.details, expected);
        assert_eq!(envelope.summary["core_application"]["state"], "unknown");
        assert!(envelope.errors.iter().all(|error| !error.contains("maintenance I/O failed")));
    }
    let parsed = capture_run(vec!["--json".into(), "--not-a-command".into()]);
    assert_eq!(parsed.exit, 2);
    assert_eq!(parsed.envelope.summary["reason"], "invalid_input");
    assert_eq!(parsed.envelope.details["error_type"], "user");
}

#[test]
fn account_handoff_pending_preserves_committed_query_only_context() {
    let operation = "7c633a08-9077-4710-882e-ac3d2998ab8b";
    let source = std::path::PathBuf::from(include_str!("../../test-cases/fixtures/i18n/synthetic-path.txt"));
    for (register, expected_args) in [
        (
            false,
            serde_json::json!(["--operation-id", operation, "--source-profile", source,]),
        ),
        (
            true,
            serde_json::json!([
                "--operation-id",
                operation,
                "--source-profile",
                source,
                "--register",
            ]),
        ),
    ] {
        let pending: anyhow::Error = respire_host_control::AccountHandoffPending {
            operation_id: operation.into(),
            source_profile: source.clone(),
            register,
        }
        .into();
        let (envelope, code) =
            super::command_failure(&pending.context("synthetic supervisor handoff failure"));
        assert_eq!(code, 1);
        assert!(matches!(envelope.status, super::OutputStatus::Fail));
        assert_eq!(
            envelope.summary,
            serde_json::json!({
                "reason": "account_handoff_pending",
            })
        );
        assert_eq!(
            envelope.details,
            serde_json::json!({
                "error_type": "handoff_pending",
                "operation_id": operation,
                "source_profile": source,
                "register": register,
                "committed": true,
                "result_command": "runtime account-result",
                "result_args": expected_args,
                "query_only": true,
                "replayed": false,
            })
        );
    }
}
