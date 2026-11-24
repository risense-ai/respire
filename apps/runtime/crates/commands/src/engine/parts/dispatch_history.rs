{
            let store = build_local()?;
            let full = match id.as_deref() {
                Some(i) => {
                    let all = store.all(true)?;
                    Some(respire_app::service::resolve_prefix(&all, i)?)
                }
                None => None,
            };
            let rows = store.entry_audit(full.as_deref(), limit)?;
            if json_mode() {
                let mut result = ResultEnvelope::new(
                    "history",
                    OutputStatus::Ok,
                    serde_json::json!({"count": rows.len()}),
                    Vec::new(),
                );
                result.details = serde_json::Value::Array(rows);
                emit_result(result)?;
                return Ok(());
            }
            let status = if rows.is_empty() {
                OutputStatus::Skip
            } else {
                OutputStatus::Ok
            };
            let items = rows
                .iter()
                .map(|r| {
                    let ts = r["ts"].as_str().unwrap_or("");
                    let action = r["action"].as_str().unwrap_or("");
                    let id = r["entry_id"].as_str().unwrap_or("").get(..8).unwrap_or("");
                    let title = r["title"].as_str().unwrap_or("");
                    OutputItem::new("change", status, format!("{ts} {action} {id} {title}"))
                })
                .collect::<Vec<_>>();
            let mut result = ResultEnvelope::new(
                "history",
                status,
                serde_json::json!({"count": rows.len()}),
                if items.is_empty() {
                    vec![OutputItem::new(
                        "history",
                        OutputStatus::Skip,
                        "no change records",
                    )]
                } else {
                    items
                },
            );
            result.details = serde_json::Value::Array(rows);
            emit_result(result)?;
}
