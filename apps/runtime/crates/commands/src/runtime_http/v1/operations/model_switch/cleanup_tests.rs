use super::*;

#[test]
fn cleanup_uses_exact_library_ownership_without_waiting_for_business_fence() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let directory = tempfile::tempdir()?;
    let profile = directory.path().canonicalize()?;
    let store = LocalStore::open(&profile.join("rsrs.db"))?;
    let ownership = LibraryLock::acquire(&profile, std::time::Duration::from_secs(1))?;
    store.meta_set("embedding_pending_task_v1", r#"{"id":"cancelled"}"#)?;
    store.meta_set("embedding_pending_selection_v1", "candidate")?;
    let writer = rpc_worker::shared_exclusive();
    let _held = writer.acquire(true);
    assert!(cleanup_pending(
        Some(&store),
        &ownership,
        &profile.join("other"),
        "cancelled"
    )
    .is_err());
    cleanup_pending(Some(&store), &ownership, &profile, "stale")?;
    assert!(store.meta_get("embedding_pending_selection_v1")?.is_some());
    cleanup_pending(Some(&store), &ownership, &profile, "cancelled")?;
    assert!(store.meta_get("embedding_pending_selection_v1")?.is_none());
    assert!(store.meta_get("embedding_pending_task_v1")?.is_none());
    Ok(())
}
