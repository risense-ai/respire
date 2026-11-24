{
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(true)?;
            // Short prefix is ok (counted in chars, CJK/multibyte-safe)
            let full = respire_app::service::resolve_prefix(&all, &id)?;
            let by_id: std::collections::HashMap<String, &respire_app::StoredMemory> =
                all.iter().map(|m| (m.id.clone(), m)).collect();
            let Some(me) = by_id.get(&full) else {
                emit_result(ResultEnvelope::new(
                    "promote",
                    OutputStatus::Fail,
                    serde_json::json!({"id": id}),
                    vec![OutputItem::new("entry", OutputStatus::Fail, "not found")],
                ))?;
                return Ok(());
            };
            if me.deleted || me.local_parent_id.is_empty() {
                emit_result(ResultEnvelope::new(
                    "promote",
                    OutputStatus::Skip,
                    serde_json::json!({"id": full, "reason": "already_root_or_deleted"}),
                    vec![OutputItem::new("entry", OutputStatus::Skip, "no change")],
                ))?;
                return Ok(());
            }
            let parent_id = me.local_parent_id.clone();
            // New cause = the cause's cause (grandparent); none -> promote to root
            let grandparent = by_id
                .get(&parent_id)
                .and_then(|p| {
                    if p.local_parent_id.is_empty() {
                        None
                    } else {
                        Some(p.local_parent_id.clone())
                    }
                })
                .unwrap_or_default();
            respire_app::service::reparent(&session, &store, &full, &grandparent)?;
            emit_result(ResultEnvelope::new(
                "promote",
                OutputStatus::Ok,
                serde_json::json!({"id": full, "new_parent": grandparent}),
                vec![OutputItem::new("entry", OutputStatus::Ok, short_id(&full))],
            ))?;
            auto_sync(&session, &store);
}
