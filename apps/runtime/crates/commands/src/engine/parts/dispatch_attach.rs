{
            let session = build_session()?;
            let store = build_local()?;
            // Always resolve_prefix: exact id -> exact catalog title -> 8-char prefix (char-safe)
            let all = store.all(true)?;
            let child = respire_app::service::resolve_prefix(&all, &id)?;
            let parent_full = respire_app::service::resolve_prefix(&all, &parent)?;
            if child == parent_full {
                return Err(respire_app::input_error::InputError("cannot attach to itself".into()).into());
            }
            // Cycle guard: the new parent's cause chain must not contain the child
            for anc in store.ancestor_chain(&parent_full)? {
                if anc == child {
                    return Err(respire_app::input_error::InputError("cycle: new parent is a descendant of this entry".into()).into());
                }
            }
            respire_app::service::reparent(&session, &store, &child, &parent_full)?;
            // Adoption: explicit attach is adoption (parent and child both count - choosing the hang and the attached entry are recall decisions)
            let _ = store.mark_adopted(&parent_full);
            let _ = store.mark_adopted(&child);
            emit_result(ResultEnvelope::new(
                "attach",
                OutputStatus::Ok,
                serde_json::json!({"child":child,"parent":parent_full}),
                vec![
                    OutputItem::new(
                        "child",
                        OutputStatus::Ok,
                        respire_app::service::short_id(&child),
                    ),
                    OutputItem::new(
                        "parent",
                        OutputStatus::Ok,
                        respire_app::service::short_id(&parent_full),
                    ),
                ],
            ))?;
            auto_sync(&session, &store);
}
