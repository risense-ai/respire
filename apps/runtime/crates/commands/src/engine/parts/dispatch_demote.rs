{
            let session = build_session()?;
            let store = build_local()?;
            // Same resolve_prefix as attach (fixed 2026-09-21): --parent used to be exact-equal,
            // so a short prefix/catalog title always said "target cause missing", unlike attach.
            let all = store.all(true)?;
            let id_full = respire_app::service::resolve_prefix(&all, &id)?;
            let parent_full = respire_app::service::resolve_prefix(&all, &parent)?;
            if id_full == parent_full {
                anyhow::bail!("cannot demote onto itself");
            }
            // Cycle guard: the new cause (parent) chain must not contain this id
            let chain = store.ancestor_chain(&parent_full)?;
            if chain.iter().any(|p| *p == id_full) {
                anyhow::bail!("target cause is a descendant of this entry; that would cycle");
            }
            respire_app::service::reparent(&session, &store, &id_full, &parent_full)?;
            // Adoption: a demote-attach is adoption
            let _ = store.mark_adopted(&parent_full);
            let _ = store.mark_adopted(&id_full);
            emit_result(ResultEnvelope::new(
                "demote",
                OutputStatus::Ok,
                serde_json::json!({"id": id_full, "parent": parent_full}),
                vec![
                    OutputItem::new("entry", OutputStatus::Ok, short_id(&id_full)),
                    OutputItem::new("parent", OutputStatus::Ok, short_id(&parent_full)),
                ],
            ))?;
            auto_sync(&session, &store);
}
