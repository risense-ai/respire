{
            let session = sync_phase(build_session)?;
            let local = sync_phase(build_local)?;
            let mut history = sync_phase(|| local.sync_history(id.as_deref()))?;
            if remote {
                let transport = sync_phase(build_remote)?;
                let cap = transport
                    .capabilities()?
                    .ok_or_else(|| anyhow!("server does not support revision history"))?;
                let changes =
                    read_remote_sync_history(&transport, &cap.epoch, id.as_deref(), None, None)?;
                let local_operations: Vec<_> = history
                    .into_iter()
                    .filter(|row| row.get("rev").is_none())
                    .collect();
                history = changes
                    .into_iter()
                    .map(|change| {
                        serde_json::json!({
                            "epoch":cap.epoch, "rev":change.rev, "status":change.status,
                            "op_id":change.op_id, "blob":change.blob
                        })
                    })
                    .collect();
                history.extend(local_operations);
            }
            for row in &mut history {
                let b: respire_app::StoredMemory = serde_json::from_value(row["blob"].clone())?;
                match MemoryEngine::open(&session, &b) {
                    Ok(entry) => row["entry"] = serde_json::to_value(entry)?,
                    Err(e) => row["decode_error"] = serde_json::json!(e.to_string()),
                }
                if let Some(obj) = row.as_object_mut() {
                    obj.remove("blob");
                }
            }
            let mut result = ResultEnvelope::new(
                "sync-history",
                OutputStatus::Ok,
                serde_json::json!({"count": history.len(), "remote": remote}),
                vec![OutputItem::new(
                    "versions",
                    OutputStatus::Ok,
                    history.len().to_string(),
                )],
            );
            result.details = serde_json::Value::Array(history);
            if !result.details.as_array().is_some_and(Vec::is_empty) {
                result.actions.push("sync-history --json".into());
            }
            emit_result(result)?;
}
