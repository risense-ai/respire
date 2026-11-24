{
            if json_mode() {
                // Lightweight status (no session unlock, no embedder) - the client calls this often at start; must be fast
                let app_status = respire_app::service::status_light()?;
                let si = respire_app::service::session_info();
                // "local store is readable" = this machine has a key wrap (wrapped_urk) and can unwrap it - unrelated to "logged into the cloud".
                // Old code used has_token: offline local mode has no token, always showed locked, the client kept popping login
                // asking to "join the memory store" (user report 2026-09-16). Rule: a key wrap means the local store is joined;
                // an empty nominal user (offline keygen with no name) still counts as joined.
                let unlocked = si.has_local_keys;
                let mut v = serde_json::json!({
                    "unlocked": unlocked,
                    "offline": !remote_configured() && si.has_local_keys,
                    "data_dir": app_status.data_dir,
                    "session": si,
                    "server_addr": respire_app::service::server_addr(),
                    "autosync": respire_app::service::autosync_enabled(),
                    "remote_configured": remote_configured(),
                });
                v["local_total"] = serde_json::json!(app_status.local_total);
                v["local_alive"] = serde_json::json!(app_status.local_alive);
                v["max_updated_at"] = serde_json::json!(app_status.max_updated_at);
                v["workspace"] = serde_json::json!(respire_app::service::workspace_mode());
                let live = sync_live();
                v["sync_scheduler"] = crate::rpc_serve::sync_scheduler_status();
                v["recall_statistics"] = rpc::recall_stats_status();
                v["retrieval_index"] = rpc::index_status();
                v["model_operation"] = crate::model_progress::status()?;
                v["sync_live"] = serde_json::json!({
                    "phase": if live.phase.is_empty() { "idle" } else { live.phase.as_str() },
                    "pulled": live.pulled,
                    "pushed": live.pushed,
                    "remote_alive": live.remote_alive,
                    "pending": live.pending,
                    "conflicts": live.conflicts,
                    "error": live.error,
                });
                emit_result(ResultEnvelope::new(
                    "status",
                    OutputStatus::Ok,
                    v,
                    Vec::new(),
                ))?;
                return Ok(());
            }
            let app_status = respire_app::service::status_light()?;
            let count = app_status.local_alive;
            let user = read_session_json()
                .ok()
                .and_then(|d| d["user"].as_str().map(|s| s.to_owned()))
                .unwrap_or_else(|| "local".to_owned());
            let (mode, server) = match remote_config_from_session_or_env()? {
                Some((addr, _)) => {
                    let mode = if respire_app::service::autosync_enabled() {
                        "cloud sync (auto)"
                    } else {
                        "cloud sync (manual)"
                    };
                    (mode, Some(addr))
                }
                None => ("local authority store (offline)", None),
            };
            let mut items = vec![
                OutputItem::new("mode", OutputStatus::Ok, mode),
                OutputItem::new("user", OutputStatus::Ok, user),
                OutputItem::new("memories", OutputStatus::Ok, count.to_string()),
                OutputItem::new(
                    "local_total",
                    OutputStatus::Ok,
                    app_status.local_total.to_string(),
                ),
                OutputItem::new(
                    "max_updated_at",
                    OutputStatus::Ok,
                    app_status.max_updated_at.clone().unwrap_or_default(),
                ),
            ];
            if let Some(addr) = server.as_ref() {
                items.insert(2, OutputItem::new("server", OutputStatus::Ok, addr.clone()));
            }
            emit_result(ResultEnvelope::new(
                "status",
                OutputStatus::Ok,
                serde_json::json!({
                    "remote_configured": server.is_some(),
                    "local_alive": count,
                    "local_total": app_status.local_total,
                    "retrieval_index": rpc::index_status(),
                    "model_operation": crate::model_progress::status()?,
                    "max_updated_at": app_status.max_updated_at,
                }),
                items,
            ))?;
}
