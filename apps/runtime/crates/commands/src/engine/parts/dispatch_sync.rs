{
            progress::phase("\u{8bfb}\u{53d6}\u{8d26}\u{6237}\u{548c}\u{5f85}\u{540c}\u{6b65}\u{6570}\u{636e}", "Reading account and pending sync data");
            let session = sync_phase(build_session)?;
            let local = sync_phase(build_local)?;
            let remote = sync_phase(build_remote)?;
            let mut stats = sync_with_retry(&session, &local, &remote)?;
            let mut purged = 0usize;
            // Auto-purge old tombstones (purge_days from agent.json; 0=immediate, default 30)
            let purge_days = respire_app::service::read_agent_config()
                .get("purge_days")
                .and_then(|v| v.as_i64())
                .unwrap_or(30);
            if purge_days >= 0 && !respire_app::service::readonly_mode() {
                let n = sync_phase(|| local.auto_purge_old(purge_days))?;
                purged = n;
                if n > 0 {
                    // Maintenance creates later operations; never extend the caller's upload boundary.
                    schedule_autosync(&session, &local);
                    stats.pending = sync_phase(|| local.sync_counts())?.0;
                }
            }
            // One hook after all rounds, with the same totals as the final report.
            crate::hooks::record(
                crate::hooks::HookEvent::PostSync,
                crate::hooks::sync_payload(stats.pulled, stats.pushed),
            );
            // Missing vectors are durable index work, not part of network sync.
            let index_pending = sync_phase(|| local.missing_embedding())?.len();
            let local_blobs = sync_phase(|| local.all(true))?;
            let local_total = local_blobs.len();
            let local_alive = local_blobs.iter().filter(|m| !m.deleted).count();
            let total_matched = local_total == stats.remote_total;
            let converged = total_matched && local_alive == stats.remote_alive;
            let sync_status = if converged && stats.conflicts == 0 && stats.undecodable == 0 {
                OutputStatus::Ok
            } else if stats.conflicts > 0 || stats.undecodable > 0 || !total_matched {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            };
            if json_mode() {
                let summary = serde_json::json!({
                    "pulled": stats.pulled, "pushed": stats.pushed,
                    "remote_total": stats.remote_total, "remote_alive": stats.remote_alive,
                    "local_total": local_total, "local_alive": local_alive,
                    "total_matched": total_matched, "converged": converged,
                    "protocol": stats.protocol, "pending": stats.pending,
                    "conflicts": stats.conflicts, "conflict_history": stats.conflict_history,
                    "processed_conflicts": stats.processed_conflicts,
                    "historical_conflicts": stats.historical_conflicts,
                    "resolving_conflicts": stats.resolving_conflicts,
                    "resolution_supported": stats.resolution_supported,
                    "undecodable": stats.undecodable, "index_pending": index_pending,
                    "purged": purged,
                });
                let mut result = ResultEnvelope::new("sync", sync_status, summary, Vec::new());
                if stats.conflicts > 0 {
                    result.actions.push("sync-conflicts".into());
                }
                if index_pending > 0 {
                    result.actions.push("reembed".into());
                }
                emit_result(result)?;
                return Ok(());
            }
            let mut result = ResultEnvelope::new(
                "sync",
                sync_status,
                serde_json::json!({
                    "pulled": stats.pulled, "pushed": stats.pushed,
                    "remote_total": stats.remote_total, "remote_alive": stats.remote_alive,
                    "local_total": local_total, "local_alive": local_alive,
                    "total_matched": total_matched, "converged": converged,
                    "protocol": stats.protocol, "pending": stats.pending,
                    "conflicts": stats.conflicts, "conflict_history": stats.conflict_history,
                    "processed_conflicts": stats.processed_conflicts,
                    "historical_conflicts": stats.historical_conflicts,
                    "resolving_conflicts": stats.resolving_conflicts,
                    "resolution_supported": stats.resolution_supported,
                    "undecodable": stats.undecodable, "index_pending": index_pending,
                    "purged": purged,
                }),
                vec![
                    OutputItem::new(
                        "remote",
                        OutputStatus::Ok,
                        format!(
                            "{} total / {} active",
                            stats.remote_total, stats.remote_alive
                        ),
                    ),
                    OutputItem::new(
                        "local",
                        OutputStatus::Ok,
                        format!("{} total / {} active", local_total, local_alive),
                    ),
                    OutputItem::new(
                        "changes",
                        OutputStatus::Ok,
                        format!("pulled {} / pushed {}", stats.pulled, stats.pushed),
                    ),
                    OutputItem::new(
                        "state",
                        sync_status,
                        if converged {
                            "converged"
                        } else if total_matched {
                            "active-count-diff"
                        } else {
                            "not-converged"
                        },
                    ),
                    OutputItem::new(
                        "pending",
                        if stats.pending > 0
                            || stats.conflicts > 0
                            || stats.undecodable > 0
                            || index_pending > 0
                        {
                            OutputStatus::Warn
                        } else {
                            OutputStatus::Ok
                        },
                        format!(
                            "send {} / conflicts {} / decrypt {} / index {}",
                            stats.pending, stats.conflicts, stats.undecodable, index_pending
                        ),
                    ),
                    OutputItem::new("purged", OutputStatus::Ok, purged.to_string()),
                ],
            );
            if stats.conflicts > 0 {
                result.actions.push("sync-conflicts".into());
            }
            if index_pending > 0 {
                result.actions.push("reembed".into());
            }
            emit_result(result)?;
}
