use super::*;

#[test]
fn tools_are_unique_and_schemas_are_objects() -> Result<(), String> {
    let mut seen = std::collections::HashSet::<String>::new();
    for (name, description, schema) in tools() {
        if !seen.insert(name.to_owned()) {
            return Err(format!("duplicate tool {name}"));
        }
        if description.is_empty() {
            return Err(format!("empty description for {name}"));
        }
        if schema["type"] != "object" {
            return Err(format!("schema for {name} is not an object"));
        }
    }
    Ok(())
}

#[test]
fn policy_is_fail_closed_for_unknown_and_denied_tools() -> anyhow::Result<()> {
    let policy = ToolPolicy {
        allow: None,
        allow_all: false,
        deny: ["memory_remember".to_owned()].into_iter().collect(),
        deny_all: false,
    };
    let denied = policy
        .allowed("memory_remember")
        .err()
        .ok_or_else(|| anyhow::anyhow!("DENY must reject the write tool"))?;
    assert!(denied.contains("RSRS_MCP_DENY"));
    assert_eq!(policy.allowed("memory_status"), Ok(true));
    assert!(policy.allowed("unknown").is_err());
    Ok(())
}

#[test]
fn pure_memory_tools_map_to_executor_commands() -> Result<(), String> {
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        ("memory_status", serde_json::json!({}), "status"),
        (
            "memory_extract",
            serde_json::json!({"dry_run":true,"limit":5}),
            "extract",
        ),
        (
            "memory_remember",
            serde_json::json!({"content":"candidate"}),
            "remember",
        ),
        (
            "memory_recall",
            serde_json::json!({"query":"context"}),
            "recall",
        ),
        ("memory_list", serde_json::json!({}), "list"),
        ("memory_show", serde_json::json!({"id":"abcd1234"}), "show"),
        (
            "memory_update",
            serde_json::json!({"id":"abcd1234","title":"new"}),
            "update",
        ),
        (
            "memory_attach",
            serde_json::json!({"id":"child","parent":"parent"}),
            "attach",
        ),
        ("memory_tree", serde_json::json!({}), "tree"),
        ("memory_history", serde_json::json!({}), "history"),
        ("memory_diary", serde_json::json!({}), "diary"),
        (
            "memory_diary_search",
            serde_json::json!({"query":"context"}),
            "diary-search",
        ),
        (
            "memory_diary_fragment",
            serde_json::json!({"source_id":"source","fragment_id":"fragment"}),
            "diary-fragment",
        ),
        (
            "memory_chain",
            serde_json::json!({"id":"abcd1234"}),
            "chain",
        ),
        (
            "memory_query_log_mark",
            serde_json::json!({"ids":"abcd1234","good":true}),
            "query-log",
        ),
        ("memory_taxonomy", serde_json::json!({}), "taxonomy"),
    ];
    for (tool, args, command) in cases {
        let argv = tool_argv(tool, &args)?;
        if argv.first().map(String::as_str) != Some(command) {
            return Err(format!("{tool} argv {argv:?}"));
        }
    }
    Ok(())
}

#[test]
fn reads_stay_bounded_or_metadata_only() -> Result<(), String> {
    let list = tool_argv("memory_list", &serde_json::json!({}))?;
    if !list.contains(&"--metadata".to_owned()) {
        return Err(format!("list argv leaks bodies: {list:?}"));
    }
    let recall = tool_argv("memory_recall", &serde_json::json!({"query":"context"}))?;
    if !recall.contains(&"--titles".to_owned()) {
        return Err(format!("recall argv leaks bodies: {recall:?}"));
    }
    Ok(())
}

#[test]
fn write_and_bounded_inputs_fail_closed() -> Result<(), String> {
    let invalid = [
        tool_argv("memory_extract", &serde_json::json!({})),
        tool_argv(
            "memory_extract",
            &serde_json::json!({"confirm":true,"importance":"normal"}),
        ),
        tool_argv("memory_list", &serde_json::json!({"limit":"2"})),
        tool_argv(
            "memory_diary_search",
            &serde_json::json!({"query":"x","max_chars":511}),
        ),
        tool_argv(
            "memory_diary_search",
            &serde_json::json!({"query":"x","unexpected":true}),
        ),
        tool_argv(
            "memory_diary_fragment",
            &serde_json::json!({"source_id":"s","fragment_id":"f","neighbors":2}),
        ),
        tool_argv(
            "memory_query_log_mark",
            &serde_json::json!({"ids":"x","good":true,"bad":true}),
        ),
    ];
    if invalid.iter().any(Result::is_ok) {
        return Err("an invalid bounded or write input mapped successfully".to_owned());
    }
    Ok(())
}

#[test]
fn remember_maps_without_destructive_merge() -> anyhow::Result<()> {
    let outcome = tool_argv(
        "memory_remember",
        &serde_json::json!({"content":"candidate","merge_ids":"old"}),
    )
    .map_err(anyhow::Error::msg);
    assert!(outcome.is_err_and(|error| error.to_string().contains("merge requires")));
    Ok(())
}

#[test]
fn tool_text_is_never_reinterpreted_as_cli_flags() -> anyhow::Result<()> {
    use clap::Parser;

    let argv = tool_argv(
        "memory_remember",
        &serde_json::json!({"content":"--help", "title":"--version", "force":true}),
    )
    .map_err(anyhow::Error::msg)?;
    let parsed =
        crate::cli_def::Cli::try_parse_from(std::iter::once("rsrs".to_owned()).chain(argv))?;
    match parsed.command {
        Some(crate::cli_def::Command::Remember {
            content,
            title,
            force,
            ..
        }) => {
            assert_eq!(content, "--help");
            assert_eq!(title, "--version");
            assert!(force);
        }
        _ => anyhow::bail!("MCP text changed the parsed command"),
    }
    for (tool, args) in [
        ("memory_show", serde_json::json!({"id":"--help"})),
        (
            "memory_update",
            serde_json::json!({"id":"--help", "title":"--version", "content":"--force"}),
        ),
        (
            "memory_attach",
            serde_json::json!({"id":"--help", "parent":"--version"}),
        ),
        (
            "memory_history",
            serde_json::json!({"id":"--help", "limit":1}),
        ),
        (
            "memory_chain",
            serde_json::json!({"id":"--help", "depth":1}),
        ),
        (
            "memory_query_log_mark",
            serde_json::json!({"ids":"--help", "good":true}),
        ),
    ] {
        let argv = tool_argv(tool, &args).map_err(anyhow::Error::msg)?;
        crate::cli_def::Cli::try_parse_from(std::iter::once("rsrs".to_owned()).chain(argv))?;
    }
    Ok(())
}

#[test]
fn mcp_initialization_and_invalid_requests_have_json_rpc_envelopes() -> anyhow::Result<()> {
    let policy = ToolPolicy {
        allow: None,
        allow_all: false,
        deny: Default::default(),
        deny_all: false,
    };
    let initialized = handle_request(
        &serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize"}),
        &policy,
    )
    .ok_or_else(|| anyhow::anyhow!("initialize response missing"))?;
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(initialized["result"]["serverInfo"]["version"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    for invalid in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"jsonrpc":"1.0", "id":1, "method":"ping"}),
        serde_json::json!({"jsonrpc":"2.0", "id":{}, "method":"ping"}),
    ] {
        let reply = handle_request(&invalid, &policy)
            .ok_or_else(|| anyhow::anyhow!("invalid request response missing"))?;
        assert_eq!(reply["error"]["code"], -32600);
        assert!(reply["id"].is_null());
    }
    Ok(())
}

#[test]
fn mcp_notifications_have_no_synthetic_reply() {
    for body in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"[{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
    ] {
        let (status, _, response) = http_response(McpHttpIn {
            method: "POST".to_owned(),
            path: "/mcp".to_owned(),
            origin: "http://127.0.0.1:1".to_owned(),
            accept: "application/json".to_owned(),
            body: body.to_owned(),
        });
        assert_eq!(status, 202);
        assert!(response.is_empty());
    }
}
