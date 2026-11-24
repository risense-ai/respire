{
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(false)?;
            let full = match respire_app::service::resolve_prefix(&all, id.as_str()) {
                Ok(v) => v,
                Err(e) => {
                    emit_result(ResultEnvelope::new(
                        "chain",
                        OutputStatus::Fail,
                        serde_json::json!({"id": id, "depth": depth}),
                        vec![OutputItem::new(
                            "entry",
                            OutputStatus::Fail,
                            format!("not found ({e})"),
                        )],
                    ))?;
                    return Ok(());
                }
            };
            let by_id: std::collections::HashMap<String, &respire_app::StoredMemory> =
                all.iter().map(|m| (m.id.clone(), m)).collect();
            let title_of = |pid: &str| -> String {
                by_id
                    .get(pid)
                    .map(|m| {
                        if m.local_title.is_empty() {
                            short_id(pid)
                        } else {
                            m.local_title.clone()
                        }
                    })
                    .unwrap_or_else(|| short_id(pid))
            };
            // 1. Cause chain: root -> direct parent, full body per layer (no truncate - chain is an explicit "I want the whole picture")
            let anc_ids = store.ancestor_chain(&full).unwrap_or_default();
            let mut ancestors: Vec<respire_app::MemoryEntry> = Vec::new();
            for pid in &anc_ids {
                if let Some(s) = by_id.get(pid) {
                    if let Ok(e) = MemoryEngine::open(&session, s) {
                        ancestors.push(e);
                    }
                }
            }
            let entry = by_id
                .get(&full)
                .and_then(|s| MemoryEngine::open(&session, s).ok());
            // 2. Effect chain: BFS down `depth` layers
            let mut descendants: Vec<(usize, respire_app::MemoryEntry)> = Vec::new();
            let mut frontier: Vec<(String, usize)> = vec![(full.clone(), 1)];
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            while let Some((cur, lv)) = frontier.pop() {
                if lv > depth {
                    continue;
                }
                for c in store.children(&cur).unwrap_or_default() {
                    if !seen.insert(c.id.clone()) {
                        continue;
                    }
                    if let Ok(e) = MemoryEngine::open(&session, &c) {
                        let next = e.id.clone();
                        descendants.push((lv, e));
                        frontier.push((next, lv + 1));
                    }
                }
            }
            let anc_json: Vec<serde_json::Value> = ancestors
                .iter()
                .map(|a| serde_json::json!({ "id": a.id, "title": a.title, "content": a.content }))
                .collect();
            let des_json: Vec<serde_json::Value> = descendants
                .iter()
                .map(|(lv, d)| serde_json::json!({ "level": lv, "id": d.id, "title": d.title, "content": d.content }))
                .collect();
            let mut items = Vec::with_capacity(ancestors.len() + descendants.len() + 1);
            for a in &ancestors {
                items.push(OutputItem::new(
                    format!("cause-{}", short_id(&a.id)),
                    OutputStatus::Ok,
                    title_of(&a.id),
                ));
            }
            if let Some(e) = &entry {
                items.push(OutputItem::new(
                    "entry",
                    OutputStatus::Ok,
                    format!("{} {}", short_id(&e.id), e.title),
                ));
            }
            for (lv, d) in &descendants {
                items.push(OutputItem::new(
                    format!("effect-{lv}-{}", short_id(&d.id)),
                    OutputStatus::Ok,
                    d.title.clone(),
                ));
            }
            let mut result = ResultEnvelope::new(
                "chain",
                OutputStatus::Ok,
                serde_json::json!({"id": full, "depth": depth, "ancestors": ancestors.len(), "descendants": descendants.len()}),
                items,
            );
            result.details =
                serde_json::json!({"entry": entry, "ancestors": anc_json, "descendants": des_json});
            emit_result(result)?;
}
