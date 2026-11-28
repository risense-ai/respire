use super::*;
use serde_json::json;

fn source(id: &str, content: &str) -> Result<MemoryEntry> {
    Ok(serde_json::from_value(json!({
        "id": id, "kind": "Time", "tags": [], "title": "Synthetic diary",
        "content": content, "user": "fixture", "computer": "fixture", "project": "fixture",
        "created_at": "2026-10-27T09:00:00Z", "updated_at": "2026-10-27T09:00:00Z",
        "emotion": -1, "parent_id": "", "importance": "trivial"
    }))?)
}

fn put(store: &LocalStore, keys: &SessionKeys, entry: &MemoryEntry) -> Result<()> {
    assert!(store.put(&MemoryEngine::seal_for_indexing(keys, entry, "fixture")?)?);
    Ok(())
}

fn raw_artifact(
    root: &std::path::Path,
    prepared: core_sdk::Prepared,
) -> Result<core_sdk::Prepared> {
    let locator = prepared
        .artifact
        .strip_prefix(b"rsi1:")
        .context("fixture locator missing")?;
    let artifact = std::fs::read(root.join("core-index").join(std::str::from_utf8(locator)?))?;
    assert!(!artifact.is_empty() && !artifact.starts_with(b"rsi1:"));
    Ok(core_sdk::Prepared { artifact })
}

fn selection(model: &str, generation: &str) -> Result<core_sdk::profiles::Selection> {
    Ok(serde_json::from_value(json!({
        "model": model, "backend": "remote", "engine": "cpu", "generation": generation,
        "profile": {"id": model, "embedding": {
            "model_id": model, "revision": "r1", "dimensions": 8, "max_tokens": 32,
            "pooling": "sentence_embedding", "normalize": true, "query_prefix": "", "document_prefix": ""
        }},
        "backend_profile": {"id": "remote", "backend": "host_transport", "provider": "fixture", "protocol": "openai_embeddings"}
    }))?)
}

#[test]
fn diary_candidate_resumes_fragments_and_catches_up_without_touching_incumbents() -> Result<()> {
    let isolate = crate::test_lock::Isolate::new()?;
    let library = isolate.path().join("diary.db");
    let store = LocalStore::open(&library)?;
    let keys = SessionKeys::from_urk([37; 32])?;
    let provider = core_sdk::search::HashingEmbedder::new(8);
    let generation = "fixture-candidate";
    let task_id = "fixture-switch";
    let active = selection(provider.model_name(), "fixture-incumbent")?;
    let active_wire = serde_json::to_string(&active)?;
    store.meta_set(core_sdk::profiles::ACTIVE_SELECTION_KEY, &active_wire)?;
    store.queue_selection(&selection("candidate", generation)?, task_id)?;
    let open = |row: &StoredMemory| MemoryEngine::open(&keys, row);
    let incumbent_paths = [cache_path(), candidate_cache_path(&active.generation)?];
    for path in &incumbent_paths {
        writable_cache(path)?.execute(
            "INSERT INTO diary_sources VALUES('incumbent','original')",
            [],
        )?;
    }
    let incumbents = incumbent_paths
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let path = candidate_cache_path(generation)?;
    assert_eq!(
        path,
        cache_path().with_file_name(format!("diary-fragments-{}.db", digest(generation)))
    );
    assert!(incumbent_paths.iter().all(|incumbent| incumbent != &path));
    assert!(!candidate_needs_rebuild(&store, generation)?);
    let mut original = source(
        "original",
        "first diary record\n[10:00] stable second record",
    )?;
    put(&store, &keys, &original)?;
    assert!(candidate_needs_rebuild(&store, generation)?);

    let other = LocalStore::open_existing(&library)?;
    let mut ordinary = source("ordinary", "unrelated memory written during preparation")?;
    ordinary.importance = "important".to_owned();
    let mut prepared_count = 0;
    let mut prepare = |entry: &MemoryEntry| {
        if prepared_count == 0 {
            put(&other, &keys, &ordinary)?;
        }
        prepared_count += 1;
        raw_artifact(isolate.path(), provider.prepare(entry)?)
    };
    let interrupted = rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        &mut prepare,
        |action| action(),
        |done| {
            anyhow::ensure!(done < 1, "fixture interrupted after a checkpoint");
            Ok(())
        },
    );
    assert!(interrupted
        .unwrap_err()
        .to_string()
        .contains("fixture interrupted"));
    assert!(candidate_needs_rebuild(&store, generation)?);
    let mut progress = Vec::new();
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        &mut prepare,
        |action| action(),
        |done| {
            progress.push(done);
            Ok(())
        },
    )?;
    assert_eq!(progress, [0, 1, 2]);
    assert!(!candidate_needs_rebuild(&store, generation)?);
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        |_| anyhow::bail!("already cached"),
        |action| action(),
        |_| Ok(()),
    )?;

    let added = source("added", "new diary added during catch-up")?;
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        |_| anyhow::bail!("already cached"),
        |action| action(),
        |done| {
            if done == 1 {
                put(&other, &keys, &added)?;
            }
            Ok(())
        },
    )?;
    assert!(candidate_needs_rebuild(&store, generation)?);
    original.content = "edited diary record".into();
    original.updated_at = "2026-10-27T09:00:01Z".into();
    put(&store, &keys, &original)?;
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        &mut prepare,
        |action| action(),
        |_| Ok(()),
    )?;
    assert_eq!(prepared_count, 4);
    assert!(!candidate_needs_rebuild(&store, generation)?);
    assert!(store.forget_memory_exact(&original.id)?);
    assert!(candidate_needs_rebuild(&store, generation)?);
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        |_| anyhow::bail!("already cached"),
        |action| action(),
        |_| Ok(()),
    )?;
    assert!(!candidate_needs_rebuild(&store, generation)?);
    let db = cache_at(&path, true)?.context("candidate cache missing")?;
    let remaining = db.query_row("SELECT source_id FROM diary_artifacts", [], |row| {
        row.get::<_, String>(0)
    })?;
    assert_eq!(remaining, added.id);
    assert_eq!(
        db.query_row("SELECT count(*) FROM diary_artifacts", [], |row| row
            .get::<_, usize>(0))?,
        1
    );
    drop(db);
    assert!(store.forget_memory_exact(&added.id)?);
    assert!(candidate_needs_rebuild(&store, generation)?);
    rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        open,
        |_| anyhow::bail!("no sources"),
        |action| action(),
        |_| Ok(()),
    )?;
    assert!(!candidate_needs_rebuild(&store, generation)?);

    store.queue_selection(&active, task_id)?;
    assert!(rebuild_candidate_with_progress(
        &store,
        &active.generation,
        task_id,
        open,
        |_| anyhow::bail!("active"),
        |action| action(),
        |_| Ok(()),
    )
    .is_err());
    assert_eq!(
        store
            .meta_get(core_sdk::profiles::ACTIVE_SELECTION_KEY)?
            .as_deref(),
        Some(active_wire.as_str())
    );
    for (path, before) in incumbent_paths.iter().zip(incumbents) {
        assert_eq!(std::fs::read(path)?, before);
    }
    Ok(())
}

#[test]
fn diary_candidate_stops_at_rejected_commit() -> Result<()> {
    let isolate = crate::test_lock::Isolate::new()?;
    let library = isolate.path().join("diary.db");
    let store = LocalStore::open(&library)?;
    let keys = SessionKeys::from_urk([37; 32])?;
    let provider = core_sdk::search::HashingEmbedder::new(8);
    let generation = "fixture-candidate";
    let task_id = "fixture-switch";
    store.queue_selection(&selection("candidate", generation)?, task_id)?;
    let entry = source("original", "first diary record\n[10:00] second diary record")?;
    put(&store, &keys, &entry)?;
    let mut commits = 0;
    let mut progress = Vec::new();
    let result = rebuild_candidate_with_progress(
        &store,
        generation,
        task_id,
        |row| MemoryEngine::open(&keys, row),
        |entry| raw_artifact(isolate.path(), provider.prepare(entry)?),
        |action| {
            commits += 1;
            anyhow::ensure!(commits < 3, "fixture cancelled before the second fragment");
            action()
        },
        |done| {
            progress.push(done);
            Ok(())
        },
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "fixture cancelled before the second fragment"
    );
    assert_eq!(progress, [0, 1]);
    let db = cache_at(&library.with_file_name(candidate_filename(generation)), true)?
        .context("candidate cache missing")?;
    let ids = db
        .prepare("SELECT id FROM diary_artifacts")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let stored = store.active_diary_entries(None, |_| String::new())?;
    let fragments = split(
        &entry,
        &version(stored.first().context("diary source missing")?),
    );
    assert_eq!(fragments.len(), 2);
    assert_eq!(ids, [fragments[0].index_id.clone()]);
    assert!(candidate_needs_rebuild(&store, generation)?);
    Ok(())
}

#[test]
fn diary_search_and_readiness_accept_locators_and_raw_core_artifacts() -> Result<()> {
    let isolate = crate::test_lock::Isolate::new()?;
    let store = LocalStore::open(&isolate.path().join("diary.db"))?;
    let keys = SessionKeys::from_urk([37; 32])?;
    let provider = core_sdk::search::HashingEmbedder::new(8);
    let generation = core_sdk::generation_key(provider.model_name())?;
    let active = selection(provider.model_name(), &generation)?;
    store.meta_set(
        core_sdk::profiles::ACTIVE_SELECTION_KEY,
        &serde_json::to_string(&active)?,
    )?;
    put(
        &store,
        &keys,
        &source("fixture", "searchable diary keyword")?,
    )?;
    rebuild(&store, &keys, &provider, || Ok(()), |action| action())?;
    let check = || -> Result<()> {
        assert!(!needs_rebuild(&store)?);
        let result = search(&store, &keys, "keyword", 5, 4000, true)?;
        assert!(result.semantic_ready);
        assert_eq!(result.pending_fragments, 0);
        assert_eq!(result.items.len(), 1);
        Ok(())
    };
    check()?;
    let db = writable_cache(&cache_path())?;
    let locator = db.query_row("SELECT artifact FROM diary_artifacts", [], |row| row.get(0))?;
    let prepared = raw_artifact(isolate.path(), core_sdk::Prepared { artifact: locator })?;
    db.execute(
        "UPDATE diary_artifacts SET artifact=?1",
        [&prepared.artifact],
    )?;
    drop(db);
    check()
}
