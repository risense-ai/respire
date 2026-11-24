{
            let session = sync_phase(build_session)?;
            let local = sync_phase(build_local)?;
            let id = match sync_phase(|| {
                local.restore_sync_version(&session, op_id.as_deref(), rev, epoch.as_deref())
            }) {
                Ok(id) => id,
                Err(error) if error.is::<respire_app::transport::local::SyncVersionNotFound>() => {
                    let transport = sync_phase(build_remote)?;
                    let cap = transport
                        .capabilities()?
                        .ok_or_else(|| anyhow!("server does not support revision history"))?;
                    if epoch.as_deref().is_some_and(|value| value != cap.epoch) {
                        anyhow::bail!("the server no longer retains the requested sync epoch");
                    }
                    let change = read_remote_sync_history(
                        &transport,
                        &cap.epoch,
                        None,
                        op_id.as_deref(),
                        if op_id.is_some() { None } else { rev },
                    )?
                    .pop()
                    .ok_or_else(|| anyhow!("version not found on the server"))?;
                    let page = respire_app::transport::protocol::Page {
                        epoch: cap.epoch,
                        until: change.rev,
                        cursor: change.rev,
                        has_more: false,
                        changes: vec![change],
                        total: 0,
                        alive: 0,
                    };
                    sync_phase(|| {
                        local.cache_history(&page)?;
                        local.restore_sync_version(
                            &session,
                            op_id.as_deref(),
                            rev,
                            epoch.as_deref(),
                        )
                    })?
                }
                Err(error) => return Err(error),
            };
            emit_result(ResultEnvelope::new(
                "sync-restore",
                OutputStatus::Pending,
                serde_json::json!({"restored":id,"pending_sync":true}),
                vec![OutputItem::new("restored", OutputStatus::Pending, &id).action("sync")],
            ))?;
            auto_sync(&session, &local);
}
