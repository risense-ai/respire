/// Explicit diary search. Queries never prepare a whole year's embeddings on the foreground path.
pub fn search(
    store: &LocalStore,
    keys: &SessionKeys,
    query: &str,
    limit: usize,
    max_chars: usize,
    hybrid: bool,
) -> Result<FragmentResult> {
    anyhow::ensure!(
        !query.trim().is_empty() && query.chars().count() <= 500,
        "query must contain 1..=500 characters"
    );
    anyhow::ensure!((1..=LIMIT_MAX).contains(&limit), "limit must be 1..=20");
    anyhow::ensure!(
        (512..=BUDGET_MAX).contains(&max_chars),
        "max-chars must be 512..=12000"
    );
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    // Lexical mode does not even open the disposable semantic cache.
    let db = if hybrid {
        model_cache(&store.retrieval_model()?, true).ok().flatten()
    } else {
        None
    };
    let mut all = HashMap::new();
    let mut snapshots = Vec::new();
    let mut hits = Vec::new();
    let mut pending = 0;
    let mut omitted = false;
    let generation = if hybrid && db.is_some() {
        Some(core_sdk::generation_key(&store.retrieval_model()?)?)
    } else {
        None
    };
    for source in diaries(store)? {
        let entry = MemoryEngine::open(keys, &source)
            .with_context(|| format!("cannot decode diary {}", source.id))?;
        let revision = version(&source);
        let (lexical_hits, more) = lexical(&entry, &revision, &terms);
        omitted |= more;
        hits.extend(lexical_hits);
        hits = select(hits, LIMIT_MAX + 1);
        if hits.len() > LIMIT_MAX {
            omitted = true;
            hits.truncate(LIMIT_MAX);
        }
        if !hybrid {
            continue;
        }
        for hit in split(&entry, &revision) {
            let artifact = if let (Some(db), Some(generation)) = (&db, &generation) {
                db.query_row(
                    "SELECT artifact FROM diary_artifacts WHERE id=?1 AND generation=?2",
                    params![hit.index_id, generation],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .optional()
                .unwrap_or(None)
            } else {
                None
            };
            if let Some(artifact) = artifact.filter(|value| !value.is_empty()) {
                let indexed = indexed_entry(&entry, &hit);
                snapshots.push(Snapshot {
                    id: indexed.id.clone(),
                    deleted: false,
                    updated_at: indexed.updated_at.clone(),
                    kind: source.local_kind.clone(),
                    tags: source.local_tags.clone(),
                    title: indexed.title.clone(),
                    project: indexed.project.clone(),
                    computer: indexed.computer.clone(),
                    parent_id: String::new(),
                    created_at: indexed.created_at.clone(),
                    content_head: indexed.content.clone(),
                    recall_count: 0,
                    importance: indexed.importance.clone(),
                    entry: Some(indexed.clone()),
                    features: Some(core_sdk::Prepared { artifact }),
                });
                all.insert(indexed.id, hit);
            } else {
                pending += 1;
            }
        }
    }
    if !snapshots.is_empty()
        && !core_sdk::index_ready(
            &snapshots
                .iter()
                .filter_map(|s| s.features.as_ref().map(|f| f.artifact.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap_or(false)
    {
        pending += snapshots.len();
        snapshots.clear();
    }
    let mut ready = false;
    if hybrid && !snapshots.is_empty() {
        let semantic = (|| -> Result<Vec<(f32, MemoryEntry)>> {
            let provider = crate::memory::bge::BgeEmbedder::load_model(&store.retrieval_model()?)?;
            core_sdk::query(
                &provider,
                &snapshots,
                &MemoryQuery::new(query).limit(LIMIT_MAX),
                "scored",
            )
        })();
        match semantic {
            Ok(results) => {
                ready = pending == 0;
                for (score, entry) in results {
                    if score.is_finite() && score > 0.0 {
                        if let Some(mut hit) = all.remove(&entry.id) {
                            hit.score = score.min(1.0);
                            hits.push(hit);
                        }
                    }
                }
            }
            Err(_) => pending += snapshots.len(),
        }
    }
    let mut result = budget(select(hits, limit + 1), limit, max_chars, ready, pending)?;
    result.truncated |= omitted;
    Ok(result)
}

pub fn read_fragment(
    store: &LocalStore,
    keys: &SessionKeys,
    source_id: &str,
    fragment_id: &str,
    neighbors: usize,
    max_chars: usize,
) -> Result<FragmentResult> {
    anyhow::ensure!(neighbors <= 1, "neighbors must be 0 or 1");
    let source = diaries(store)?
        .into_iter()
        .find(|m| m.id == source_id)
        .context("diary source not found or no longer active")?;
    let entry = MemoryEngine::open(keys, &source)?;
    let revision = version(&source);
    let mut locator = fragment_id.split('.');
    anyhow::ensure!(
        locator.next() == Some("d1"),
        "stale or unknown fragment; search again before reading"
    );
    let start = locator
        .next()
        .and_then(|v| v.parse::<usize>().ok())
        .context("invalid fragment start")?;
    let end = locator
        .next()
        .and_then(|v| v.parse::<usize>().ok())
        .context("invalid fragment end")?;
    let chars: Vec<char> = entry.content.chars().collect();
    anyhow::ensure!(
        start < end && end <= chars.len() && end - start <= fragments::CHUNK_CHARS,
        "stale or unknown fragment; search again before reading"
    );
    let target = piece(&entry, &revision, &chars, start, end);
    anyhow::ensure!(
        target.fragment_id == fragment_id,
        "stale or unknown fragment; search again before reading"
    );
    let mut selected = vec![target];
    if neighbors == 1 {
        let pieces = split(&entry, &revision);
        if let Some(previous) = pieces
            .iter()
            .filter(|f| f.start < start)
            .max_by_key(|f| f.start)
        {
            selected.push(previous.clone());
        }
        if let Some(next) = pieces.iter().filter(|f| f.end > end).min_by_key(|f| f.end) {
            selected.push(next.clone());
        }
    }
    let mut result = budget(selected, 3, max_chars, false, 0)?;
    // Restore source order only after allocating the target's text budget.
    result.items.sort_by_key(|item| item.start);
    Ok(result)
}
