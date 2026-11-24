{
            let session = build_session()?;
            let store = build_local()?;
            // Hard-delete must handle **already-forgotten tombstones** (purge's point is to clear tombstone ciphertext) -
            // resolve_prefix always filter(!deleted), so this path uses a dedicated include-deleted resolver.
            let full = resolve_prefix_incl_deleted(&store, &id)?;
            let purged = store.purge_entry(&full)?;
            report_orphans(&store, "purge");
            let status = if purged {
                OutputStatus::Ok
            } else {
                OutputStatus::Fail
            };
            emit_result(ResultEnvelope::new(
                "purge",
                status,
                serde_json::json!({"purged":purged,"id":full}),
                vec![OutputItem::new(
                    "entry",
                    status,
                    if purged { "purged" } else { "not found" },
                )],
            ))?;
            if purged {
                auto_sync(&session, &store);
            }
}
