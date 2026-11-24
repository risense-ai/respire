{
            let session = build_session()?;
            let store = build_local()?;
            let text = std::fs::read_to_string(&file)
                .map_err(|e| anyhow!("failed to read {file}: {e}"))?;
            let items: Vec<serde_json::Value> =
                serde_json::from_str(&text).map_err(|e| anyhow!("failed to parse {file}: {e}"))?;
            let all = store.all(true)?;
            let by_id: std::collections::HashMap<&str, &respire_app::StoredMemory> =
                all.iter().map(|m| (m.id.as_str(), m)).collect();
            for item in &items {
                let id = item["id"].as_str().unwrap_or_default();
                if let Some(stored) = by_id.get(id).copied().filter(|row| !row.deleted) {
                    respire_app::metadata_gate::ensure_write(&store, &MemoryEngine::open(&session, stored)?)?;
                }
            }
            let mut done = 0usize;
            let mut missing = 0usize;
            for it in &items {
                let id = it["id"].as_str().unwrap_or_default();
                let title = it["title"].as_str().unwrap_or_default().to_owned();
                if id.is_empty() {
                    continue;
                }
                let Some(stored) = by_id.get(id).copied().filter(|m| !m.deleted) else {
                    missing += 1;
                    continue;
                };
                let mut entry = MemoryEngine::open(&session, stored)?;
                entry.title = title;
                entry.updated_at = store.edit_stamp(&stored.id)?;
                entry.modified_by = respire_app::service::device_tag();
                let user = entry.user.clone();
                let new_stored = MemoryEngine::seal(&session, embedder!(), &entry, &user)?;
                store.put(&new_stored)?;
                done += 1;
            }
            let status = if missing == 0 {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            };
            emit_result(ResultEnvelope::new(
                "retitle-many",
                status,
                serde_json::json!({"updated": done, "missing": missing}),
                vec![
                    OutputItem::new("updated", OutputStatus::Ok, done.to_string()),
                    OutputItem::new(
                        "missing",
                        if missing == 0 {
                            OutputStatus::Ok
                        } else {
                            OutputStatus::Warn
                        },
                        missing.to_string(),
                    ),
                ],
            ))?;
            auto_sync(&session, &store);
}
