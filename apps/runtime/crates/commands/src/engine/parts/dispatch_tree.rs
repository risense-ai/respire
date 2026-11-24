{
            let from = from.as_str();
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(true)?;
            let by_id: std::collections::HashMap<String, &respire_app::StoredMemory> =
                all.iter().map(|m| (m.id.clone(), m)).collect();
            let children_of = |pid: &str| -> Vec<String> {
                all.iter()
                    .filter(|m| !m.deleted && m.local_parent_id == pid)
                    .map(|m| m.id.clone())
                    .collect()
            };
            let label = |id: &str| -> String {
                by_id
                    .get(id)
                    .map(|m| {
                        if m.local_title.is_empty() {
                            id[..id.len().min(8)].to_owned()
                        } else {
                            m.local_title.clone()
                        }
                    })
                    .unwrap_or_else(|| id[..id.len().min(8)].to_owned())
            };
            fn render(
                store: &LocalStore,
                by_id: &std::collections::HashMap<String, &respire_app::StoredMemory>,
                children_of: &dyn Fn(&str) -> Vec<String>,
                label: &dyn Fn(&str) -> String,
                node: &str,
                depth: usize,
                prefix: &str,
                out: &mut Vec<String>,
            ) {
                let title = label(node);
                let kids = children_of(node);
                let meta = by_id.get(node).map(|m| m.local_kind.as_str()).unwrap_or("");
                let leaf = kids.is_empty();
                let level = prefix.chars().filter(|c| *c == '|').count();
                out.push(format!(
                    "id={} title={} kind={} level={} leaf={}",
                    node[..node.len().min(8)].to_owned(),
                    title,
                    meta,
                    level,
                    leaf
                ));
                if depth == 0 {
                    return;
                }
                let n = kids.len();
                for (i, kid) in kids.iter().enumerate() {
                    let last = i == n - 1;
                    let branch = if last { "`-- " } else { "|-- " };
                    // M1 (2026-09-20 audit): child-layer prefix must use next_prefix - non-last-child
                    // descendants inherit the vertical bar, else a 3-level tree renders `|-- |-- A1` instead of
                    // `|  |-- A1`. Code used to compute next_prefix then pass prefix, with
                    // `let _ = next_prefix;` silencing the unused warning - that line hid the bug.
                    let next_prefix = format!("{prefix}{}", if last { "   " } else { "|  " });
                    render(
                        store,
                        by_id,
                        children_of,
                        label,
                        kid,
                        depth - 1,
                        &format!("{next_prefix}{branch}"),
                        out,
                    );
                }
            }
            let _ = &session;
            let mut out = Vec::new();
            if from.is_empty() {
                let roots: Vec<String> = by_id
                    .iter()
                    .filter(|(_, m)| !m.deleted && m.local_parent_id.is_empty())
                    .map(|(id, _)| id.clone())
                    .collect();
                if roots.is_empty() {
                    emit_result(ResultEnvelope::new(
                        "tree",
                        OutputStatus::Skip,
                        serde_json::json!({"roots": 0, "depth": depth}),
                        vec![OutputItem::new("tree", OutputStatus::Skip, "no roots")],
                    ))?;
                    return Ok(());
                }
                for r in roots {
                    out.push(format!(
                        "id={} title={} kind=root level=0 leaf={}",
                        r[..r.len().min(8)].to_owned(),
                        label(&r),
                        children_of(&r).is_empty()
                    ));
                    let kids = children_of(&r);
                    let n = kids.len();
                    for (i, kid) in kids.iter().enumerate() {
                        let last = i == n - 1;
                        let branch = if last { "`-- " } else { "|-- " };
                        render(
                            &store,
                            &by_id,
                            &children_of,
                            &label,
                            kid,
                            depth - 1,
                            &branch,
                            &mut out,
                        );
                    }
                }
            } else {
                let full_from = respire_app::service::resolve_prefix(&all, from)?;
                out.push(format!(
                    "root={} depth={} mode=from",
                    short_id(&full_from),
                    depth
                ));
                render(
                    &store,
                    &by_id,
                    &children_of,
                    &label,
                    &full_from,
                    depth,
                    "",
                    &mut out,
                );
            }
            let items = out
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    OutputItem::new(format!("node-{}", i + 1), OutputStatus::Ok, row.clone())
                })
                .collect();
            let mut result = ResultEnvelope::new(
                "tree",
                OutputStatus::Ok,
                serde_json::json!({"count": out.len(), "depth": depth, "from": from}),
                items,
            );
            result.details = serde_json::json!({"rows": out});
            emit_result(result)?;
}
