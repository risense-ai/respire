use super::*;

fn fake_ok(args: Vec<String>) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "command": args.first().cloned().unwrap_or_else(|| "cli".to_owned()),
        "status": "ok",
        "summary": {"ok": true},
        "items": [],
        "actions": [],
        "errors": [],
        "details": {}
    }))
}

#[test]
fn executable_hash_streams_all_bytes_and_preserves_refresh_detection() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let src = dir.path().join("source");
    let dest = dir.path().join("dest");
    for size in [0, 1, 65535, 65536, 65537, 3 * 65536 + 7] {
        let data: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
        std::fs::write(&src, &data).map_err(|error| error.to_string())?;
        let expected: [u8; 32] = Sha256::digest(&data).into();
        assert_eq!(file_sha256(&src)?, expected);
        std::fs::write(&dest, &data).map_err(|error| error.to_string())?;
        assert!(!bin_needs_refresh(&src, &dest)?);
        if size > 0 {
            let mut changed = data;
            changed[size - 1] ^= 1;
            std::fs::write(&dest, changed).map_err(|error| error.to_string())?;
            assert!(bin_needs_refresh(&src, &dest)?);
        }
    }
    std::fs::remove_file(&dest).map_err(|error| error.to_string())?;
    assert!(bin_needs_refresh(&src, &dest)?);
    assert!(file_sha256(&dest).is_err());
    assert!(file_sha256(dir.path()).is_err());
    Ok(())
}

#[test]
fn streamed_hash_retries_interruptions_and_reports_mid_read_errors() -> Result<(), String> {
    struct Reader {
        cursor: std::io::Cursor<Vec<u8>>,
        calls: usize,
        fail: bool,
        grow: bool,
    }
    impl Read for Reader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            if self.cursor.position() >= 65536 && self.fail {
                return Err(std::io::Error::other("injected mid-read failure"));
            }
            // Deterministically model a source growing/truncating while read.
            if self.cursor.position() == 65536 {
                if self.grow {
                    self.cursor.get_mut().resize(150000, 7);
                } else {
                    self.cursor.get_mut().truncate(70000);
                }
            }
            let until_boundary = if self.cursor.position() < 65536 {
                65536 - self.cursor.position() as usize
            } else {
                4096
            };
            let limit = out.len().min(4096).min(until_boundary);
            self.cursor.read(&mut out[..limit])
        }
    }
    let make = |fail, grow| Reader {
        cursor: std::io::Cursor::new(vec![3; 100000]),
        calls: 0,
        fail,
        grow,
    };
    for grow in [false, true] {
        let mut reference = Vec::new();
        make(false, grow)
            .read_to_end(&mut reference)
            .map_err(|error| error.to_string())?;
        let expected: [u8; 32] = Sha256::digest(&reference).into();
        assert_eq!(reader_sha256(make(false, grow))?, expected);
    }
    assert_eq!(
        reader_sha256(make(true, false)),
        Err("injected mid-read failure".into())
    );
    Ok(())
}

#[test]
fn tools_have_unique_names_and_schema() -> Result<(), String> {
    let mut seen = std::collections::HashSet::<String>::new();
    for (name, desc, schema) in tools() {
        if !seen.insert(name.to_string()) {
            return Err(format!("duplicate tool {name}"));
        }
        if desc.is_empty() {
            return Err(format!("empty description for {name}"));
        }
        if schema["type"] != "object" {
            return Err(format!("schema for {name} is not an object"));
        }
    }
    if tools().len() != TOOL_COUNT {
        return Err(format!(
            "expected {TOOL_COUNT} tools, got {}",
            tools().len()
        ));
    }
    Ok(())
}

#[test]
fn source_does_not_reinvoke_cli_binary() -> Result<(), String> {
    let prod = [include_str!("../mcp.rs"), include_str!("binary.rs"), include_str!("tools.rs")].concat();
    let http_fixture = include_str!("http_fixture.rs");
    if prod.contains("spawnSync") || prod.contains("std::process::Command") {
        return Err("production mcp.rs must not spawn a CLI process".to_owned());
    }
    let section = |source: &str, start: &str, end: &str| -> Result<String, String> {
        source.split_once(start)
            .and_then(|(_, rest)| rest.split_once(end).map(|(body, _)| body))
            .map(|body| body.split_whitespace().collect())
            .ok_or_else(|| format!("missing MCP source section: {start}"))
    };
    let stdio = section(&prod, "fn invoke_cli(", "fn call_tool(")?;
    let runtime = section(http_fixture, "fn invoke_runtime_cli(", "fn handle_rpc_live(")?;
    let http = section(http_fixture, "fn handle_rpc_live(", "fn encode_sse(")?;
    let serve = section(&prod, "pub fn serve(", "#[cfg(test)]")?;
    assert!(stdio.contains("rpc::call_method(\"cli.exec\",full,true)"));
    assert!(!stdio.contains("rpc::query_json(") && !stdio.contains("execute_json("));
    assert!(serve.contains(
        "handle_rpc_with(&req,&invoke_cli,&policy,ServiceAccess::current()?)"
    ));
    assert!(!serve.contains("&invoke_runtime_cli"));
    assert!(http.contains(
        "handle_rpc_with(req,&invoke_runtime_cli,&policy,access)"
    ));
    assert!(!http.contains("&invoke_cli,"));
    assert!(runtime.contains("rpc::query_json(args)"));
    assert!(
        !runtime.contains("rpc::call_method(")
            && !runtime.contains("net_rpc::")
            && !runtime.contains("invoke_cli(")
    );
    Ok(())
}

#[test]
fn tool_argv_covers_required_fields() -> Result<(), String> {
    if tool_argv("memory_remember", &serde_json::json!({})).is_ok() {
        return Err("remember without content should fail".to_owned());
    }
    if tool_argv("memory_recall", &serde_json::json!({})).is_ok() {
        return Err("recall without query should fail".to_owned());
    }
    if tool_argv("memory_show", &serde_json::json!({})).is_ok() {
        return Err("show without id should fail".to_owned());
    }
    if tool_argv("nope", &serde_json::json!({})).is_ok() {
        return Err("unknown tool should fail".to_owned());
    }
    let remember = tool_argv(
        "memory_remember",
        &serde_json::json!({"content":"x","title":"t","force":true,"parent":"p"}),
    )?;
    if remember[0] != "remember" || !remember.contains(&"--force".to_owned()) {
        return Err(format!("remember argv {remember:?}"));
    }
    let mark = tool_argv(
        "memory_query_log_mark",
        &serde_json::json!({"ids":"abc","good":true}),
    )?;
    if !mark.contains(&"--good".to_owned()) {
        return Err(format!("mark argv {mark:?}"));
    }
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        ("memory_status", serde_json::json!({}), "status"),
        (
            "memory_extract",
            serde_json::json!({"id":"codex","dry_run":true,"limit":5}),
            "extract",
        ),
        (
            "memory_remember",
            serde_json::json!({"content":"c","importance":"important","type":"decision","merge_ids":"a,b"}),
            "remember",
        ),
        (
            "memory_recall",
            serde_json::json!({"query":"q","limit":5}),
            "recall",
        ),
        ("memory_list", serde_json::json!({"limit":2}), "list"),
        ("memory_show", serde_json::json!({"id":"ab"}), "show"),
        (
            "memory_update",
            serde_json::json!({"id":"ab","title":"t","content":"c","tags":"a","kind":"context","importance":"trivial"}),
            "update",
        ),
        (
            "memory_attach",
            serde_json::json!({"id":"c","parent":"p"}),
            "attach",
        ),
        (
            "memory_tree",
            serde_json::json!({"from":"r","outline":true,"depth":2}),
            "tree",
        ),
        (
            "memory_history",
            serde_json::json!({"id":"ab","limit":4}),
            "history",
        ),
        (
            "memory_diary",
            serde_json::json!({"limit":3,"date":"today","from":"a","to":"b","contains":"x"}),
            "diary",
        ),
        (
            "memory_chain",
            serde_json::json!({"id":"ab","depth":1}),
            "chain",
        ),
        (
            "memory_query_log_mark",
            serde_json::json!({"ids":"ab","bad":true}),
            "query-log",
        ),
        ("memory_forget", serde_json::json!({"id":"ab"}), "forget"),
        ("memory_restore", serde_json::json!({"id":"ab"}), "restore"),
        ("memory_taxonomy", serde_json::json!({}), "taxonomy"),
    ];
    for (name, args, first) in cases {
        let argv = tool_argv(name, &args)?;
        if argv.first().map(String::as_str) != Some(first) {
            return Err(format!("{name} argv {argv:?}"));
        }
    }
    if tool_argv(
        "memory_query_log_mark",
        &serde_json::json!({"ids":"ab","good":true,"bad":true}),
    )
    .is_ok()
    {
        return Err("good and bad together should fail".to_owned());
    }
    if tool_argv("memory_query_log_mark", &serde_json::json!({"ids":"ab"})).is_ok() {
        return Err("mark without good/bad should fail".to_owned());
    }
    if tool_argv("memory_attach", &serde_json::json!({"id":"c"})).is_ok() {
        return Err("attach without parent should fail".to_owned());
    }
    Ok(())
}

#[test]
fn memory_extract_requires_confirm_and_valid_limit() -> Result<(), String> {
    if tool_argv("memory_extract", &serde_json::json!({})).is_ok() {
        return Err("extract without confirm or dry_run should fail".to_owned());
    }
    if tool_argv(
        "memory_extract",
        &serde_json::json!({"confirm":true,"limit":-1}),
    )
    .is_ok()
    {
        return Err("a negative extract limit should fail".to_owned());
    }
    let write = tool_argv(
        "memory_extract",
        &serde_json::json!({"confirm":true,"limit":5}),
    )?;
    if !write.contains(&"--limit".to_owned()) || write.contains(&"--dry-run".to_owned()) {
        return Err(format!("extract write argv {write:?}"));
    }
    let preview = tool_argv("memory_extract", &serde_json::json!({"dry_run":true}))?;
    if !preview.contains(&"--dry-run".to_owned()) {
        return Err(format!("extract preview argv {preview:?}"));
    }
    Ok(())
}

#[test]
fn refusal_diagnostics_explain_configuration_without_overriding_modes() -> Result<(), String> {
    let policy = ToolPolicy::parse(None, None)?;
    assert!(policy
        .denial_reason("memory_forget", ServiceAccess::default())
        .contains("RSRS_MCP_ALLOW"));
    assert!(policy
        .denial_reason(
            "memory_forget",
            ServiceAccess {
                readonly: true,
                off: false
            }
        )
        .contains("read-only"));
    assert!(policy
        .denial_reason(
            "memory_recall",
            ServiceAccess {
                readonly: false,
                off: true
            }
        )
        .contains("paused"));
    assert!(ToolPolicy::parse(Some("all"), Some("memory_recall"))?
        .denial_reason("memory_recall", ServiceAccess::default())
        .contains("RSRS_MCP_DENY"));
    assert!(!policy
        .denial_reason("untrusted-secret-like-tool-name", ServiceAccess::default())
        .contains("untrusted-secret"));
    Ok(())
}

#[test]
fn readonly_taxonomy_dispatch_preserves_read_only_argv() -> anyhow::Result<()> {
    let policy = ToolPolicy::parse(None, None).map_err(anyhow::Error::msg)?;
    let access = ServiceAccess { readonly: true, off: false };
    assert!(access.readonly);
    assert!(policy.allows("memory_taxonomy", access));
    let observed = std::cell::RefCell::new(Vec::new());
    let invoke = |args| {
        *observed.borrow_mut() = args;
        Err("independent Runtime callback reached".to_owned())
    };
    let error = call_tool(
        &serde_json::json!({"name":"memory_taxonomy"}),
        &invoke,
        &policy,
        access,
    )
    .err().ok_or_else(|| anyhow::anyhow!("callback failure was swallowed"))?;
    assert!(error.contains("independent Runtime callback reached"));
    assert_eq!(*observed.borrow(), vec!["taxonomy".to_owned(), "--list".to_owned()]);
    Ok(())
}

#[test]
fn http_and_sse_batch_enforce_policy_before_callbacks() -> Result<(), String> {
    let policy = ToolPolicy::parse(None, None)?;
    for path in ["/mcp", "/sse"] {
        for accept in ["application/json", "text/event-stream"] {
            let dispatch = |req: &serde_json::Value| {
                handle_rpc_with(
                    req,
                    &|_| panic!("disabled HTTP tool invoked"),
                    &policy,
                    ServiceAccess::default(),
                )
            };
            let (status,content_type,body)=http_response_with(HttpIn{method:"POST".into(),path:path.into(),origin:"http://127.0.0.1".into(),accept:accept.into(),body:serde_json::json!([
                {"jsonrpc":"2.0","id":1,"method":"tools/list"},
                {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_restore","arguments":{"id":"synthetic"}}},
                {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_remember","arguments":{"content":"x","merge_ids":"synthetic"}}}
            ]).to_string()},&dispatch);
            assert_eq!(status, 200);
            let text = String::from_utf8(body).map_err(|e| e.to_string())?;
            let json = if content_type.starts_with("text/event-stream") {
                text.lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .ok_or("SSE data")?
            } else {
                &text
            };
            let frames: serde_json::Value =
                serde_json::from_str(json).map_err(|e| e.to_string())?;
            assert_eq!(
                frames[0]["result"]["tools"]
                    .as_array()
                    .ok_or("tools")?
                    .len(),
                TOOL_COUNT - 2
            );
            assert_eq!(frames[1]["result"]["isError"], true);
            assert_eq!(frames[2]["result"]["isError"], true);
        }
    }
    Ok(())
}

mod permissions;
