{
            let session = sync_phase(build_session)?;
            let local = sync_phase(build_local)?;
            if refresh {
                sync_tracked(&session, &local, &sync_phase(build_remote)?)?;
            }
            let mut after = 0;
            while let Some(next) =
                sync_phase(|| local.classify_conflicts_batch(&session, false, after))?
            {
                after = next;
            }
            let conflicts = sync_phase(|| local.list_conflicts(&session, id.as_deref(), all))?;
            let status = if conflicts.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            };
            let mut result = ResultEnvelope::new(
                "sync-conflicts",
                status,
                serde_json::json!({"count": conflicts.len(), "refresh": refresh}),
                vec![OutputItem::new(
                    "conflicts",
                    status,
                    conflicts.len().to_string(),
                )],
            );
            if !conflicts.is_empty() {
                result.actions.push("sync-conflicts --json".into());
            }
            result.details = serde_json::Value::Array(conflicts);
            emit_result(result)?;
}
