{
            // M5: omitting --title used to silently blank the title (data damage). Now required.
            let Some(new_title) = title.as_ref().filter(|t| !t.trim().is_empty()) else {
                anyhow::bail!("need --title <new-title> (omitting it would blank the title, so there is no empty default)");
            };
            let session = build_session()?;
            let store = build_local()?;
            let all = store.all(true)?;
            // M4: use the shared prefix resolver (used to be exact match; 8-char short id said "not found").
            let full = respire_app::service::resolve_prefix(&all, id.as_str())?;
            let Some(stored) = all.iter().find(|m| m.id == full && !m.deleted) else {
                emit_result(ResultEnvelope::new(
                    "retitle",
                    OutputStatus::Fail,
                    serde_json::json!({"id": id}),
                    vec![OutputItem::new("entry", OutputStatus::Fail, "not found")],
                ))?;
                return Ok(());
            };
            let mut entry = MemoryEngine::open(&session, stored)?;
            respire_app::metadata_gate::ensure_write(&store, &entry)?;
            let old = entry.title.clone();
            entry.title = new_title.clone();
            // Bump updated_at (LWW needs a new timestamp so a remote old blob cannot push back)
            entry.updated_at = store.edit_stamp(&stored.id)?;
            entry.modified_by = respire_app::service::device_tag();
            let user = entry.user.clone();
            let new_stored = MemoryEngine::seal(&session, embedder!(), &entry, &user)?;
            store.put(&new_stored)?;
            // Adoption: a retitle is adoption
            let _ = store.mark_adopted(&stored.id);
            let mut result = ResultEnvelope::new(
                "retitle",
                OutputStatus::Ok,
                serde_json::json!({"id": entry.id, "old_title": old, "new_title": new_title}),
                vec![OutputItem::new(
                    "entry",
                    OutputStatus::Ok,
                    short_id(&entry.id),
                )],
            );
            result.details = serde_json::json!({"entry": entry});
            emit_result(result)?;
            auto_sync(&session, &store);
}
