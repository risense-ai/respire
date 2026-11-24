fn tool_argv(tool: &str, args: &serde_json::Value) -> Result<Vec<String>, String> {
    let str_field = |name: &str| {
        args.get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let number = |name: &str| match args.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a non-negative integer")),
    };
    let bool_field = |name: &str| match args.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a boolean")),
    };
    let bounded = |name: &str, min: u64, max: u64| match args.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| (min..=max).contains(value))
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer in {min}..={max}")),
    };
    let only_fields = |allowed: &[&str]| -> Result<(), String> {
        let fields = args
            .as_object()
            .ok_or("diary arguments must be an object")?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("unsupported diary tool argument".into());
        }
        Ok(())
    };
    let importance = |name: &str| -> Result<Option<String>, String> {
        if args.get(name).is_none() {
            return Ok(None);
        }
        let value = str_field(name).ok_or(format!("{name} must be important or trivial"))?;
        if !matches!(value.as_str(), "important" | "trivial") {
            return Err(format!("{name} must be important or trivial"));
        }
        Ok(Some(value))
    };

    let argv = match tool {
        "memory_status" => vec!["status".to_owned()],
        "memory_extract" => {
            let dry_run = bool_field("dry_run")? == Some(true);
            if !dry_run && bool_field("confirm")? != Some(true) {
                return Err(
                    "memory_extract writes memories: pass dry_run=true to preview or confirm=true to import"
                        .to_owned(),
                );
            }
            let mut cmd = vec!["extract".to_owned()];
            if let Some(id) = str_field("id") {
                cmd.push(format!("--id={id}"));
            }
            if dry_run {
                cmd.push("--dry-run".to_owned());
            }
            if let Some(limit) = number("limit")? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(since) = str_field("since") {
                cmd.push(format!("--since={since}"));
            }
            if let Some(value) = importance("importance")? {
                cmd.extend(["--importance".to_owned(), value]);
            }
            if let Some(parent) = str_field("parent") {
                cmd.push(format!("--parent={parent}"));
            }
            cmd
        }
        "memory_list" => {
            // Keep list metadata-only; memory_show is the explicit full-body read.
            let mut cmd = vec!["list".to_owned(), "--metadata".to_owned()];
            if let Some(limit) = number("limit")? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            cmd
        }
        "memory_recall" => {
            let query = str_field("query").ok_or("missing query")?;
            let mut cmd = vec!["recall".to_owned()];
            if let Some(limit) = number("limit")? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(kind) = str_field("type") {
                cmd.push(format!("--type={kind}"));
            }
            if bool_field("related")? == Some(false) {
                cmd.push("--no-related".to_owned());
            }
            cmd.extend(["--titles".to_owned(), "--".to_owned(), query]);
            cmd
        }
        "memory_remember" => {
            let content = str_field("content").ok_or("missing content")?;
            let mut cmd = vec!["remember".to_owned()];
            if let Some(title) = str_field("title") {
                cmd.push(format!("--title={title}"));
            }
            if let Some(value) = importance("importance")? {
                cmd.extend(["--importance".to_owned(), value]);
            }
            if let Some(parent) = str_field("parent") {
                cmd.push(format!("--parent={parent}"));
            }
            if let Some(kind) = str_field("type") {
                cmd.push(format!("--type={kind}"));
            }
            if let Some(id) = str_field("supersedes") {
                cmd.push(format!("--supersedes={id}"));
            }
            if let Some(value) = args.get("see_also") {
                let ids = value
                    .as_array()
                    .ok_or("see_also must be an array")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|id| !id.trim().is_empty())
                            .ok_or("see_also IDs must be non-empty strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if !ids.is_empty() {
                    cmd.push(format!("--see-also={}", ids.join(",")));
                }
            }
            if bool_field("force")? == Some(true) {
                cmd.push("--force".to_owned());
            }
            if args
                .get("merge_ids")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            {
                return Err("merge requires the dedicated CLI identity workflow".to_owned());
            }
            cmd.extend(["--".to_owned(), content]);
            cmd
        }
        "memory_show" => {
            let id = str_field("id").ok_or("missing id")?;
            vec!["show".to_owned(), "--".to_owned(), id]
        }
        "memory_forget" | "memory_restore" => {
            let id = str_field("id").ok_or("missing id")?;
            let command = if tool == "memory_forget" {
                "forget"
            } else {
                "restore"
            };
            vec![command.to_owned(), "--".to_owned(), id]
        }
        "memory_update" => {
            let id = str_field("id").ok_or("missing id")?;
            let mut cmd = vec!["update".to_owned()];
            if let Some(title) = str_field("title") {
                cmd.push(format!("--title={title}"));
            }
            if let Some(content) = str_field("content") {
                cmd.push(format!("--content={content}"));
            }
            if let Some(tags) = str_field("tags") {
                cmd.push(format!("--tags={tags}"));
            }
            if let Some(kind) = str_field("kind") {
                cmd.push(format!("--kind={kind}"));
            }
            if let Some(value) = importance("importance")? {
                cmd.extend(["--importance".to_owned(), value]);
            }
            cmd.extend(["--".to_owned(), id]);
            cmd
        }
        "memory_attach" => {
            let id = str_field("id").ok_or("missing id")?;
            let parent = str_field("parent").ok_or("missing parent")?;
            vec![
                "attach".to_owned(),
                format!("--parent={parent}"),
                "--".to_owned(),
                id,
            ]
        }
        "memory_tree" => {
            let mut cmd = vec!["tree".to_owned()];
            if let Some(from) = str_field("from") {
                cmd.push(format!("--from={from}"));
            }
            if bool_field("outline")? == Some(true) {
                cmd.push("--outline".to_owned());
            }
            if let Some(depth) = number("depth")? {
                cmd.extend(["--depth".to_owned(), depth.to_string()]);
            }
            cmd
        }
        "memory_history" => {
            let mut cmd = vec!["history".to_owned()];
            if let Some(limit) = number("limit")? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(id) = str_field("id") {
                cmd.extend(["--".to_owned(), id]);
            }
            cmd
        }
        "memory_diary_search" => {
            only_fields(&["query", "mode", "limit", "max_chars"])?;
            let query = str_field("query")
                .filter(|query| query.chars().count() <= 500)
                .ok_or("query must contain 1..=500 Unicode characters")?;
            let mut cmd = vec!["diary-search".to_owned()];
            if let Some(limit) = bounded("limit", 1, 20)? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(max_chars) = bounded("max_chars", 512, 12000)? {
                cmd.extend(["--max-chars".to_owned(), max_chars.to_string()]);
            }
            if let Some(mode) = str_field("mode") {
                if !matches!(mode.as_str(), "lexical" | "hybrid") {
                    return Err("mode must be lexical or hybrid".to_owned());
                }
                cmd.extend(["--mode".to_owned(), mode]);
            }
            cmd.extend(["--".to_owned(), query]);
            cmd
        }
        "memory_diary_fragment" => {
            only_fields(&["source_id", "fragment_id", "neighbors", "max_chars"])?;
            let source = str_field("source_id").ok_or("missing source_id")?;
            let fragment = str_field("fragment_id").ok_or("missing fragment_id")?;
            let mut cmd = vec!["diary-fragment".to_owned()];
            if let Some(neighbors) = bounded("neighbors", 0, 1)? {
                cmd.extend(["--neighbors".to_owned(), neighbors.to_string()]);
            }
            if let Some(max_chars) = bounded("max_chars", 512, 12000)? {
                cmd.extend(["--max-chars".to_owned(), max_chars.to_string()]);
            }
            cmd.extend(["--".to_owned(), source, fragment]);
            cmd
        }
        "memory_diary" => {
            let mut cmd = vec!["diary".to_owned()];
            if let Some(limit) = number("limit")? {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(date) = str_field("date") {
                cmd.push(format!("--date={date}"));
            }
            if let Some(from) = str_field("from") {
                cmd.push(format!("--from={from}"));
            }
            if let Some(to) = str_field("to") {
                cmd.push(format!("--to={to}"));
            }
            if let Some(contains) = str_field("contains") {
                cmd.push(format!("--contains={contains}"));
            }
            cmd
        }
        "memory_chain" => {
            let id = str_field("id").ok_or("missing id")?;
            let mut cmd = vec!["chain".to_owned()];
            if let Some(depth) = number("depth")? {
                cmd.extend(["--depth".to_owned(), depth.to_string()]);
            }
            cmd.extend(["--".to_owned(), id]);
            cmd
        }
        "memory_query_log_mark" => {
            let ids = str_field("ids").ok_or("missing ids")?;
            let mut cmd = vec!["query-log".to_owned(), "mark".to_owned()];
            match (bool_field("good")?, bool_field("bad")?) {
                (Some(true), Some(true)) => return Err("set only one of good or bad".to_owned()),
                (Some(true), _) => cmd.push("--good".to_owned()),
                (_, Some(true)) => cmd.push("--bad".to_owned()),
                _ => return Err("set good or bad".to_owned()),
            }
            cmd.extend(["--".to_owned(), ids]);
            cmd
        }
        "memory_taxonomy" => vec!["taxonomy".to_owned(), "--list".to_owned()],
        other => return Err(format!("unknown tool {other}")),
    };
    Ok(argv)
}

fn invoke(tool: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    crate::rpc_serve::execute_json(tool_argv(tool, args)?)
}

fn tools_list(policy: &ToolPolicy) -> serde_json::Value {
    serde_json::json!({
        "tools": tools()
            .into_iter()
            .filter(|(name, _, _)| policy.allowed(name).unwrap_or(false))
            .map(|(name, description, input_schema)| serde_json::json!({
                "name": name,
                "description": description,
                "inputSchema": input_schema,
            }))
            .collect::<Vec<_>>()
    })
}

fn handle_request(request: &serde_json::Value, policy: &ToolPolicy) -> Option<serde_json::Value> {
    if !request.is_object()
        || request.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || request
            .get("id")
            .is_some_and(|id| !(id.is_null() || id.is_string() || id.is_number()))
    {
        return Some(
            serde_json::json!({"jsonrpc":"2.0", "id":null, "error":{"code":-32600,"message":"invalid request"}}),
        );
    }
    let id = request.get("id")?;
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name":"respire-runtime","title":"Respire Runtime","version":crate::rpc_serve::runtime_identity().runtime_version},
        }),
        "ping" => serde_json::json!({}),
        "tools/list" => tools_list(policy),
        "tools/call" => {
            let params = request
                .get("params")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let name = params
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            match policy.allowed(name).and_then(|allowed| {
                if allowed {
                    invoke(name, &arguments)
                } else {
                    Err(policy
                        .allowed(name)
                        .err()
                        .unwrap_or_else(|| "unauthorized".to_owned()))
                }
            }) {
                Ok(mut envelope) => {
                    envelope["memory_context"] = serde_json::json!({
                        "memory_text_authority": "untrusted_historical_context",
                        "notice": NOTICE,
                    });
                    serde_json::json!({
                        "content": [{"type":"text","text":envelope.to_string()}],
                        "isError": false,
                    })
                }
                Err(error) => serde_json::json!({
                    "content": [{"type":"text","text":format!("tool failed: {error}")}],
                    "isError": true,
                }),
            }
        }
        other => {
            return Some(serde_json::json!({
                "jsonrpc":"2.0", "id":id,
                "error":{"code":-32601,"message":format!("unknown method {other}")},
            }));
        }
    };
    Some(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
}

pub fn http_response(request: McpHttpIn) -> HttpReply {
    let method = request.method.to_ascii_uppercase();
    let path = request
        .path
        .split('?')
        .next()
        .unwrap_or(request.path.as_str());
    if method == "GET" && (path == "/mcp" || path == "/sse") {
        let endpoint = format!("{}/mcp", request.origin.trim_end_matches('/'));
        return (
            200,
            "text/event-stream; charset=utf-8",
            format!("event: endpoint\ndata: {endpoint}\n\n").into_bytes(),
        );
    }
    if method != "POST" || (path != "/mcp" && path != "/sse") {
        return (
            405,
            "text/plain; charset=utf-8",
            b"method not allowed".to_vec(),
        );
    }
    let parsed = match serde_json::from_str::<serde_json::Value>(request.body.trim()) {
        Ok(value) => value,
        Err(error) => {
            return (
                400,
                "application/json; charset=utf-8",
                serde_json::json!({"error": format!("request body is not JSON: {error}")})
                    .to_string()
                    .into_bytes(),
            )
        }
    };
    let policy = match ToolPolicy::from_env() {
        Ok(policy) => policy,
        Err(error) => {
            return (
                400,
                "application/json; charset=utf-8",
                serde_json::json!({"error": error}).to_string().into_bytes(),
            )
        }
    };
    let payload = if let Some(requests) = parsed.as_array() {
        if requests.is_empty() {
            serde_json::json!({"jsonrpc":"2.0", "id":null, "error":{"code":-32600,"message":"empty batch"}})
        } else {
            let replies = requests
                .iter()
                .filter_map(|request| handle_request(request, &policy))
                .collect::<Vec<_>>();
            if replies.is_empty() {
                return (202, "application/json; charset=utf-8", Vec::new());
            }
            serde_json::Value::Array(replies)
        }
    } else {
        match handle_request(&parsed, &policy) {
            Some(reply) => reply,
            None => return (202, "application/json; charset=utf-8", Vec::new()),
        }
    };
    (
        200,
        "application/json; charset=utf-8",
        serde_json::to_vec(&payload)
            .unwrap_or_else(|_| br#"{"error":"cannot encode response"}"#.to_vec()),
    )
}

#[cfg(test)]
mod tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime_mcp/tests.rs")); }
