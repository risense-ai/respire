/// Remove obsolete derived rows after the final diary is removed/promoted,
/// without preparing or downloading an otherwise unused model.
pub fn clear_if_empty(
    store: &LocalStore,
    mut commit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
) -> Result<bool> {
    let revision = store
        .meta_get("memory_revision_v1")?
        .context("memory snapshot token unavailable")?;
    if !diaries(store)?.is_empty() {
        return Ok(false);
    }
    let path = model_cache_path(&store.retrieval_model()?)?;
    guarded(store, &revision, &mut commit, || {
        let mut db = writable_cache(&path)?;
        let tx = db.transaction()?;
        tx.execute("DELETE FROM diary_artifacts", [])?;
        tx.execute("DELETE FROM diary_sources", [])?;
        tx.commit()?;
        Ok(())
    })?;
    Ok(true)
}

/// Core preparation stays outside locks. Every durable cache mutation must be
/// executed exactly once by the runtime's short context/cancellation commit guard.
pub fn rebuild<E: Embedder>(
    store: &LocalStore,
    keys: &SessionKeys,
    provider: &E,
    check: impl FnMut() -> Result<()>,
    commit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
) -> Result<()> {
    rebuild_with_progress(store, keys, provider, check, commit, |_| Ok(()))
}

/// Report completed fragment work so cancellation/idle accounting reflects actual progress.
pub fn rebuild_with_progress<E: Embedder>(
    store: &LocalStore,
    keys: &SessionKeys,
    provider: &E,
    mut check: impl FnMut() -> Result<()>,
    mut commit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
    mut progress: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    let mut completed = 0;
    progress(completed)?;
    let revision = store
        .meta_get("memory_revision_v1")?
        .context("memory snapshot token unavailable")?;
    let sources = diaries(store)?;
    let path = model_cache_path(provider.model_name())?;
    let mut db = None;
    check()?;
    guarded(store, &revision, &mut commit, || {
        db = Some(writable_cache(&path)?);
        Ok(())
    })?;
    let mut db = db.context("diary commit guard did not initialize cache")?;
    let generation = core_sdk::generation_key(provider.model_name())?;
    let mut live_sources = HashSet::new();
    for source in sources {
        check()?;
        live_sources.insert(source.id.clone());
        let current_version = version(&source);
        let entry = MemoryEngine::open(keys, &source)
            .with_context(|| format!("cannot decode diary {}", source.id))?;
        let cached = {
            let mut stmt = db.prepare(
                "SELECT id,artifact FROM diary_artifacts WHERE source_id=?1 AND generation=?2",
            )?;
            let rows = stmt
                .query_map(params![source.id, generation], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            rows
        };
        let ready = cached.is_empty()
            || core_sdk::index_ready(&cached.values().cloned().collect::<Vec<_>>())
                .unwrap_or(false);
        let mut live = HashSet::new();
        for fragment in split(&entry, &current_version) {
            check()?;
            live.insert(fragment.index_id.clone());
            if !(ready && cached.contains_key(&fragment.index_id)) {
                let prepared = provider.prepare(&indexed_entry(&entry, &fragment))?;
                guarded(store, &revision, &mut commit, || {
                    db.execute("INSERT OR REPLACE INTO diary_artifacts(id,source_id,generation,artifact) VALUES(?1,?2,?3,?4)",params![fragment.index_id,source.id,generation,prepared.artifact])?;
                    Ok(())
                })?;
            }
            completed += 1;
            progress(completed)?;
        }
        check()?;
        guarded(store, &revision, &mut commit, || {
            let tx = db.transaction()?;
            let ids = {
                let mut stmt = tx.prepare("SELECT id FROM diary_artifacts WHERE source_id=?1")?;
                let rows = stmt
                    .query_map([&source.id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for id in ids {
                if !live.contains(&id) {
                    tx.execute("DELETE FROM diary_artifacts WHERE id=?1", [id])?;
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO diary_sources(id,source) VALUES(?1,?2)",
                params![source.id, current_version],
            )?;
            tx.commit()?;
            Ok(())
        })?;
    }
    check()?;
    guarded(store, &revision, &mut commit, || {
        let tx = db.transaction()?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM diary_sources UNION SELECT source_id FROM diary_artifacts",
            )?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for id in ids {
            if !live_sources.contains(&id) {
                tx.execute("DELETE FROM diary_artifacts WHERE source_id=?1", [&id])?;
                tx.execute("DELETE FROM diary_sources WHERE id=?1", [&id])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}
fn guarded(
    store: &LocalStore,
    revision: &str,
    commit: &mut impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
    action: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut action = Some(action);
    commit(&mut || {
        let action = action
            .take()
            .context("diary commit executed more than once")?;
        // Existing SQLite writer reservation serializes source changes with the
        // derived commit. O(1) revision check replaces per-source full-catalog reads.
        store.write_transaction(|| {
            anyhow::ensure!(
                store.meta_get("memory_revision_v1")?.as_deref() == Some(revision),
                "diary source changed while indexing; retry"
            );
            action()
        })
    })?;
    anyhow::ensure!(
        action.is_none(),
        "diary commit guard did not execute its action"
    );
    Ok(())
}

fn budget(
    mut matches: Vec<Fragment>,
    limit: usize,
    max_chars: usize,
    ready: bool,
    pending: usize,
) -> Result<FragmentResult> {
    anyhow::ensure!((1..=LIMIT_MAX).contains(&limit), "limit must be 1..=20");
    anyhow::ensure!(
        (512..=BUDGET_MAX).contains(&max_chars),
        "max-chars must be 512..=12000 (Unicode characters, not tokens)"
    );
    let mut out = FragmentResult {
        items: Vec::new(),
        semantic_ready: ready,
        pending_fragments: pending,
        truncated: matches.len() > limit,
        max_chars,
        offset_unit: "unicode_scalar",
    };
    matches.truncate(limit);
    for mut hit in matches {
        let used = serde_json::to_string(&out)?.chars().count();
        let overhead = {
            let mut empty = hit.clone();
            empty.text.clear();
            serde_json::to_string(&empty)?.chars().count() + 2
        };
        if used + overhead >= max_chars {
            out.truncated = true;
            break;
        }
        let remaining = max_chars - used - overhead;
        if hit.text.chars().count() > remaining {
            let window = hit.anchor.saturating_sub(remaining / 3);
            hit.text = hit.text.chars().skip(window).take(remaining).collect();
            hit.start += window;
            hit.end = hit.start + hit.text.chars().count();
            hit.truncated = true;
            out.truncated = true;
        }
        // JSON escaping can require more space than the raw text.
        out.items.push(hit);
        while serde_json::to_string(&out)?.chars().count() > max_chars {
            let last = out.items.last_mut().unwrap();
            if last.text.pop().is_none() {
                out.items.pop();
                break;
            }
            last.end = last.start + last.text.chars().count();
            last.truncated = true;
            out.truncated = true;
        }
    }
    Ok(out)
}

