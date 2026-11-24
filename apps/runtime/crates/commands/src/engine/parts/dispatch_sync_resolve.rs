{
            let session = sync_phase(build_session)?;
            let local = sync_phase(build_local)?;
            let remote = sync_phase(build_remote)?;
            sync_tracked(&session, &local, &remote)?;
            sync_phase(|| {
                local.queue_conflict_resolution(
                    &session,
                    &epoch,
                    rev,
                    head_rev,
                    &action.replace('-', "_"),
                    content.as_deref(),
                )
            })?;
            let stats = sync_tracked(&session, &local, &remote)?;
            let processed = sync_phase(|| local.processed_conflict_action(&epoch, rev))?;
            emit_result(ResultEnvelope::new(
                "sync-resolve",
                if processed.is_some() {
                    OutputStatus::Ok
                } else {
                    OutputStatus::Pending
                },
                serde_json::json!({"epoch":epoch,"rev":rev,"processed":processed.is_some(),"action":processed,"pending_conflicts":stats.conflicts}),
                vec![OutputItem::new(
                    "revision",
                    if processed.is_some() {
                        OutputStatus::Ok
                    } else {
                        OutputStatus::Pending
                    },
                    rev.to_string(),
                )],
            ))?;
            if processed.is_none() {
                anyhow::bail!("current version changed or the adopt conflicted; the record is still pending - re-run sync-conflicts");
            }
}
