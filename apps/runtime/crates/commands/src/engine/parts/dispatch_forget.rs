{
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(true)?;
            let full = respire_app::service::resolve_prefix(&all, &id)?;
            let deleted = store.forget(&full)?;
            report_orphans(&store, "forget");
            if deleted {
                // Plugin hook post-forget: observe only
                crate::hooks::record(
                    crate::hooks::HookEvent::PostForget,
                    crate::hooks::forget_payload(&full),
                );
            }
            let status = if deleted {
                OutputStatus::Ok
            } else {
                OutputStatus::Fail
            };
            emit_result(ResultEnvelope::new(
                "forget",
                status,
                serde_json::json!({"deleted":deleted,"id":full}),
                vec![OutputItem::new(
                    "entry",
                    status,
                    if deleted { "deleted" } else { "not found" },
                )],
            ))?;
            if deleted {
                schedule_autosync(&session, &store);
            }
}
