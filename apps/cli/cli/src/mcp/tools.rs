use super::*;

pub(super) fn tools() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "memory_status",
            "Local store status (data dir, session, counts). Use to check that rsrs is ready.",
            serde_json::json!({"type":"object","properties":{}}),
        ),
        (
            "memory_extract",
            "Import local coding-agent sessions (Codex, OpenCode, Claude Code, \u{2026}) into rsrs memories. Batch write: imports up to `limit` newest sessions per source, seals each, then auto-syncs; pass confirm=true to write, or dry_run=true to preview",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "id":{"type":"string","description":"One platform (codex/opencode/claude/\u{2026}); omit for all detected"},
                    "dry_run":{"type":"boolean","description":"Scan and summarize without writing"},
                    "limit":{"type":"integer","minimum":0,"description":"Maximum sessions to import per source (newest first)"},
                    "since":{"type":"string","description":"today|yesterday|7d|YYYY-MM-DD"},
                    "importance":{"type":"string","enum":["important","trivial"]},
                    "parent":{"type":"string","description":"Attach imported memories under this parent (id / tree / anchor title)"},
                    "confirm":{"type":"boolean","description":"Required to write (omit only with dry_run=true)"}
                }
            }),
        ),
        (
            "memory_remember",
            "Store a memory (judge-then-store; --force writes immediately; merge_ids deletes the listed ids)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "content":{"type":"string","description":"Memory body; prefer cause, action, and effect sections"},
                    "title":{"type":"string"},
                    "importance":{"type":"string","enum":["important","trivial"]},
                    "parent":{"type":"string","description":"Parent id or catalog title"},
                    "force":{"type":"boolean"},
                    "merge_ids":{"type":"string","description":"Comma-separated ids to merge into this content"},
                    "supersedes":{"type":"string","description":"Live important memory replaced by this conclusion"},
                    "see_also":{"type":"array","items":{"type":"string"},"description":"Live important memories to link bidirectionally"},
                    "type":{"type":"string","description":"Memory kind; default context"}
                },
                "required":["content"]
            }),
        ),
        (
            "memory_recall",
            "Semantic recall (local BGE + keyword fusion, no network)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "query":{"type":"string"},
                    "limit":{"type":"integer","description":"Default 20"},
                    "related":{"type":"boolean","default":true}
                },
                "required":["query"]
            }),
        ),
        (
            "memory_list",
            "List recent memories",
            serde_json::json!({
                "type":"object",
                "properties":{"limit":{"type":"integer","description":"Default 10"}}
            }),
        ),
        (
            "memory_show",
            "Show one memory in full (including causal-chain context)",
            serde_json::json!({
                "type":"object",
                "properties":{"id":{"type":"string"}},
                "required":["id"]
            }),
        ),
        (
            "memory_update",
            "Update title/body/tags/kind/importance of an existing memory",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "id":{"type":"string"},
                    "title":{"type":"string"},
                    "content":{"type":"string"},
                    "tags":{"type":"string"},
                    "kind":{"type":"string"},
                    "importance":{"type":"string","enum":["important","trivial"]}
                },
                "required":["id"]
            }),
        ),
        (
            "memory_attach",
            "Attach a child under a parent (explicit reparent)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "id":{"type":"string","description":"Child id"},
                    "parent":{"type":"string"}
                },
                "required":["id","parent"]
            }),
        ),
        (
            "memory_tree",
            "Causal tree: forest / subtree / outline",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "from":{"type":"string"},
                    "outline":{"type":"boolean"},
                    "depth":{"type":"integer"}
                }
            }),
        ),
        (
            "memory_history",
            "Entry change audit (create/update/delete/restore)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "id":{"type":"string"},
                    "limit":{"type":"integer"}
                }
            }),
        ),
        (
            "memory_diary_search",
            "Explicit cross-date diary fragments, bounded output; scores are relevance, not credibility",
            serde_json::json!({"type":"object","properties":{
                "query":{"type":"string","minLength":1,"maxLength":500},"limit":{"type":"integer","minimum":1,"maximum":20},
                "max_chars":{"type":"integer","minimum":512,"maximum":12000},
                "mode":{"type":"string","enum":["lexical","hybrid"]}},"required":["query"],"additionalProperties":false}),
        ),
        (
            "memory_diary_fragment",
            "Bounded read of a diary fragment; rejects changed or missing locators",
            serde_json::json!({"type":"object","properties":{
                "source_id":{"type":"string"},"fragment_id":{"type":"string"},
                "neighbors":{"type":"integer","minimum":0,"maximum":1},
                "max_chars":{"type":"integer","minimum":512,"maximum":12000}},"required":["source_id","fragment_id"],"additionalProperties":false}),
        ),
        (
            "memory_diary",
            "Diary time chain (trivial zone included)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "limit":{"type":"integer"},
                    "date":{"type":"string"},
                    "from":{"type":"string"},
                    "to":{"type":"string"},
                    "contains":{"type":"string"}
                }
            }),
        ),
        (
            "memory_chain",
            "Causal chain: ancestors + this entry + descendants",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "id":{"type":"string"},
                    "depth":{"type":"integer","description":"Default 3"}
                },
                "required":["id"]
            }),
        ),
        (
            "memory_query_log_mark",
            "Self-grade recall candidates: --good (chosen) or --bad (rejected)",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "ids":{"type":"string","description":"Comma-separated ids; 8-char prefixes ok"},
                    "good":{"type":"boolean"},
                    "bad":{"type":"boolean"}
                },
                "required":["ids"]
            }),
        ),
        (
            "memory_forget",
            "Tombstone a memory (propagates with sync)",
            serde_json::json!({
                "type":"object",
                "properties":{"id":{"type":"string"}},
                "required":["id"]
            }),
        ),
        (
            "memory_restore",
            "Restore a tombstoned memory by full id",
            serde_json::json!({
                "type":"object",
                "properties":{"id":{"type":"string"}},
                "required":["id"]
            }),
        ),
        (
            "memory_taxonomy",
            "List the 23 built-in catalog roots",
            serde_json::json!({"type":"object","properties":{}}),
        ),
    ]
}

pub(super) fn tool_argv(name: &str, args: &serde_json::Value) -> Result<Vec<String>, String> {
    let opt_str = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let opt_u64 = |key: &str| args.get(key).and_then(|v| v.as_u64());
    let opt_bool = |key: &str| args.get(key).and_then(|v| v.as_bool());
    let bounded = |key: &str, min: u64, max: u64| -> Result<Option<u64>, String> {
        args.get(key)
            .map(|v| {
                v.as_u64()
                    .filter(|v| (min..=max).contains(v))
                    .ok_or_else(|| format!("{key} must be an integer in {min}..={max}"))
            })
            .transpose()
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
    match name {
        "memory_status" => Ok(vec!["status".to_owned()]),
        "memory_extract" => {
            let dry_run = opt_bool("dry_run") == Some(true);
            if !dry_run && opt_bool("confirm") != Some(true) {
                return Err(
                    "memory_extract writes memories: pass dry_run=true to preview or confirm=true to import"
                        .to_owned(),
                );
            }
            let mut cmd = vec!["extract".to_owned()];
            if let Some(id) = opt_str("id") {
                cmd.extend(["--id".to_owned(), id.to_owned()]);
            }
            if opt_bool("dry_run") == Some(true) {
                cmd.push("--dry-run".to_owned());
            }
            if args.get("limit").is_some() {
                let limit = opt_u64("limit").ok_or("limit must be a non-negative integer")?;
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(since) = opt_str("since") {
                cmd.extend(["--since".to_owned(), since.to_owned()]);
            }
            if let Some(importance) = opt_str("importance") {
                cmd.extend(["--importance".to_owned(), importance.to_owned()]);
            }
            if let Some(parent) = opt_str("parent") {
                cmd.extend(["--parent".to_owned(), parent.to_owned()]);
            }
            Ok(cmd)
        }
        "memory_remember" => {
            let content = opt_str("content").ok_or("missing content")?;
            let mut cmd = vec!["remember".to_owned(), content.to_owned()];
            if let Some(title) = opt_str("title") {
                cmd.extend(["--title".to_owned(), title.to_owned()]);
            }
            if let Some(importance) = opt_str("importance") {
                cmd.extend(["--importance".to_owned(), importance.to_owned()]);
            }
            if let Some(parent) = opt_str("parent") {
                cmd.extend(["--parent".to_owned(), parent.to_owned()]);
            }
            if let Some(kind) = opt_str("type") {
                cmd.extend(["--type".to_owned(), kind.to_owned()]);
            }
            if let Some(merge_ids) = opt_str("merge_ids") {
                cmd.extend(["--merge-ids".to_owned(), merge_ids.to_owned()]);
            }
            if let Some(id) = opt_str("supersedes") {
                cmd.extend(["--supersedes".to_owned(), id.to_owned()]);
            }
            if let Some(value) = args.get("see_also") {
                let ids = value
                    .as_array()
                    .ok_or("see_also must be an array")?
                    .iter()
                    .map(|v| v.as_str().ok_or("see_also IDs must be strings"))
                    .collect::<Result<Vec<_>, _>>()?;
                if !ids.is_empty() {
                    cmd.extend(["--see-also".to_owned(), ids.join(",")]);
                }
            }
            if opt_bool("force") == Some(true) {
                cmd.push("--force".to_owned());
            }
            Ok(cmd)
        }
        "memory_recall" => {
            let query = opt_str("query").ok_or("missing query")?;
            let mut cmd = vec!["recall".to_owned(), query.to_owned()];
            if opt_bool("related") == Some(false) {
                cmd.push("--no-related".to_owned());
            }
            if let Some(limit) = opt_u64("limit") {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            Ok(cmd)
        }
        "memory_list" => {
            let mut cmd = vec!["list".to_owned()];
            cmd.extend([
                "--limit".to_owned(),
                opt_u64("limit").unwrap_or(10).to_string(),
            ]);
            Ok(cmd)
        }
        "memory_show" => {
            let id = opt_str("id").ok_or("missing id")?;
            Ok(vec!["show".to_owned(), id.to_owned()])
        }
        "memory_update" => {
            let id = opt_str("id").ok_or("missing id")?;
            let mut cmd = vec!["update".to_owned(), id.to_owned()];
            if let Some(title) = opt_str("title") {
                cmd.extend(["--title".to_owned(), title.to_owned()]);
            }
            if let Some(content) = opt_str("content") {
                cmd.extend(["--content".to_owned(), content.to_owned()]);
            }
            if let Some(tags) = opt_str("tags") {
                cmd.extend(["--tags".to_owned(), tags.to_owned()]);
            }
            if let Some(kind) = opt_str("kind") {
                cmd.extend(["--kind".to_owned(), kind.to_owned()]);
            }
            if let Some(importance) = opt_str("importance") {
                cmd.extend(["--importance".to_owned(), importance.to_owned()]);
            }
            Ok(cmd)
        }
        "memory_attach" => {
            let id = opt_str("id").ok_or("missing id")?;
            let parent = opt_str("parent").ok_or("missing parent")?;
            Ok(vec![
                "attach".to_owned(),
                id.to_owned(),
                "--parent".to_owned(),
                parent.to_owned(),
            ])
        }
        "memory_tree" => {
            let mut cmd = vec!["tree".to_owned()];
            if let Some(from) = opt_str("from") {
                cmd.extend(["--from".to_owned(), from.to_owned()]);
            }
            if opt_bool("outline") == Some(true) {
                cmd.push("--outline".to_owned());
            }
            if let Some(depth) = opt_u64("depth") {
                cmd.extend(["--depth".to_owned(), depth.to_string()]);
            }
            Ok(cmd)
        }
        "memory_history" => {
            let mut cmd = vec!["history".to_owned()];
            if let Some(id) = opt_str("id") {
                cmd.push(id.to_owned());
            }
            if let Some(limit) = opt_u64("limit") {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            Ok(cmd)
        }
        "memory_diary_search" => {
            only_fields(&["query", "mode", "limit", "max_chars"])?;
            let query = opt_str("query")
                .filter(|q| !q.trim().is_empty() && q.chars().count() <= 500)
                .ok_or("query must contain 1..=500 Unicode characters")?;
            let mut cmd = vec!["diary-search".to_owned()];
            for (key, flag, min, max) in [
                ("limit", "--limit", 1, 20),
                ("max_chars", "--max-chars", 512, 12000),
            ] {
                if let Some(value) = bounded(key, min, max)? {
                    cmd.extend([flag.to_owned(), value.to_string()]);
                }
            }
            if args.get("mode").is_some() {
                let mode = opt_str("mode")
                    .filter(|m| matches!(*m, "lexical" | "hybrid"))
                    .ok_or("mode must be lexical or hybrid")?;
                cmd.extend(["--mode".to_owned(), mode.to_owned()]);
            }
            cmd.extend(["--".to_owned(), query.to_owned()]);
            Ok(cmd)
        }
        "memory_diary_fragment" => {
            only_fields(&["source_id", "fragment_id", "neighbors", "max_chars"])?;
            let source = opt_str("source_id").ok_or("missing source_id")?;
            let fragment = opt_str("fragment_id").ok_or("missing fragment_id")?;
            let mut cmd = vec!["diary-fragment".to_owned()];
            for (key, flag, min, max) in [
                ("neighbors", "--neighbors", 0, 1),
                ("max_chars", "--max-chars", 512, 12000),
            ] {
                if let Some(value) = bounded(key, min, max)? {
                    cmd.extend([flag.to_owned(), value.to_string()]);
                }
            }
            cmd.extend(["--".to_owned(), source.to_owned(), fragment.to_owned()]);
            Ok(cmd)
        }
        "memory_diary" => {
            let mut cmd = vec!["diary".to_owned()];
            if let Some(limit) = opt_u64("limit") {
                cmd.extend(["--limit".to_owned(), limit.to_string()]);
            }
            if let Some(date) = opt_str("date") {
                cmd.extend(["--date".to_owned(), date.to_owned()]);
            }
            if let Some(from) = opt_str("from") {
                cmd.extend(["--from".to_owned(), from.to_owned()]);
            }
            if let Some(to) = opt_str("to") {
                cmd.extend(["--to".to_owned(), to.to_owned()]);
            }
            if let Some(contains) = opt_str("contains") {
                cmd.extend(["--contains".to_owned(), contains.to_owned()]);
            }
            Ok(cmd)
        }
        "memory_chain" => {
            let id = opt_str("id").ok_or("missing id")?;
            let mut cmd = vec!["chain".to_owned(), id.to_owned()];
            if let Some(depth) = opt_u64("depth") {
                cmd.extend(["--depth".to_owned(), depth.to_string()]);
            }
            Ok(cmd)
        }
        "memory_query_log_mark" => {
            let ids = opt_str("ids").ok_or("missing ids")?;
            let mut cmd = vec!["query-log".to_owned(), "mark".to_owned(), ids.to_owned()];
            match (opt_bool("good"), opt_bool("bad")) {
                (Some(true), Some(true)) => return Err("set only one of good or bad".to_owned()),
                (Some(true), _) => cmd.push("--good".to_owned()),
                (_, Some(true)) => cmd.push("--bad".to_owned()),
                _ => return Err("set good or bad".to_owned()),
            }
            Ok(cmd)
        }
        "memory_forget" => {
            let id = opt_str("id").ok_or("missing id")?;
            Ok(vec!["forget".to_owned(), id.to_owned()])
        }
        "memory_restore" => {
            let id = opt_str("id").ok_or("missing id")?;
            Ok(vec!["restore".to_owned(), id.to_owned()])
        }
        "memory_taxonomy" => Ok(vec!["taxonomy".to_owned(), "--list".to_owned()]),
        other => Err(format!("unknown tool {other}")),
    }
}
