{
            let session = build_session()?;
            let (profile, store) = respire_app::service::open_store_with_profile()?;
            let mut candidates = store.all(false)?;
            let mut filter_summary = serde_json::Value::Null;
            // Time filter (§3.8 tidy intake): --since-resort reads resort_at, --since takes an explicit time;
            // both together take the later (stricter). Old entries with empty created_at are excluded.
            if since_resort || since.is_some() {
                let from = list_since_bound(since.as_deref(), since_resort)?;
                let before = candidates.len();
                candidates.retain(|s| s.local_created_at.as_str() > from.as_str());
                filter_summary =
                    serde_json::json!({"from": from, "before": before, "after": candidates.len()});
            }
            candidates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            candidates.truncate(limit);
            let out: Vec<(respire_app::MemoryEntry, i64)> = candidates
                .iter()
                .filter_map(|s| {
                    MemoryEngine::open(&session, s)
                        .ok()
                        .map(|e| (e, s.local_recall_count))
                })
                .collect();
            let items = out
                .iter()
                .map(|(e, _)| OutputItem::new(short_id(&e.id), OutputStatus::Ok, e.title.clone()))
                .collect();
            let mut result = ResultEnvelope::new(
                "list",
                OutputStatus::Ok,
                serde_json::json!({"count":out.len(),"limit":limit,"filter":filter_summary,"profile":profile.to_string_lossy()}),
                items,
            );
            result.details = serde_json::Value::Array(
                out.into_iter()
                    .filter_map(|(e, recall_count)| {
                        let mut row = serde_json::to_value(e).ok()?;
                        if let Some(obj) = row.as_object_mut() {
                            if metadata {obj.remove("content");}
                            obj.insert("recall_count".into(), serde_json::json!(recall_count));
                        }
                        Some(row)
                    })
                    .collect(),
            );
            emit_result(result)?;
}
