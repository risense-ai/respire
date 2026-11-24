use super::*;

impl MemoryEngine {
    fn open<T: Borrow<MemoryEntry>>(
        entries: &std::collections::HashMap<String, T>,
        stored: &StoredMemory,
    ) -> Result<MemoryEntry> {
        entries
            .get(&stored.id)
            .map(|entry| entry.borrow().clone())
            .context("selected record could not be decrypted by the caller")
    }
    pub fn recall_local<E: crate::search::EmbeddingSource, T: Borrow<MemoryEntry>, M: Borrow<StoredMemory>>(
        keys: &std::collections::HashMap<String, T>,
        embedder: &E,
        stored_list: &[M],
        query: &MemoryQuery,
        settings: &QuerySettings,
    ) -> Result<Vec<MemoryEntry>> {
        // Kind/project filter
        settings.validate()?;
        settings.diagnostics.replace(Default::default());
        let filtered: Vec<&StoredMemory> = stored_list
            .iter()
            .map(Borrow::borrow)
            .filter(|s| !s.deleted)
            .filter(|s| match query.kind {
                Some(k) => s.local_kind == k.as_str(),
                None => true,
            })
            .filter(|s| match &query.project {
                Some(p) if !p.is_empty() => s.local_project == *p,
                _ => true,
            })
            .collect();

        if query.text.trim().is_empty() {
            // Empty query: by updated_at desc. A decrypt miss (other-end key) skips, does not sink the list
            let mut entries: Vec<&MemoryEntry> = filtered.iter()
                .filter_map(|s| keys.get(&s.id).map(Borrow::borrow))
                .collect();
            entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            entries.truncate(query.limit.max(1));
            return Ok(entries.into_iter().cloned().collect());
        }

        let q_vec = embedder.try_embed_query(&query.text)?;
        validate_embedding(&q_vec, embedder.dims())?;
        settings.resolved_cos_min.set(Some(crate::search::resolve_cos_gate(&filtered, &q_vec, settings)));
        settings.lexical_scores.replace(if settings.bm25 {
            Some(filtered.iter().zip(settings.score_lexical(&filtered, &query.text)?)
                .map(|(entry, score)| (entry.id.clone(), score)).collect())
        } else { None });
        Ok(
            Self::recall_scored_inner(keys, &filtered, &q_vec, query, settings)?
                .into_iter()
                .map(|(_, e)| e)
                .collect(),
        )
    }

    pub fn recall_local_scored<E: crate::search::EmbeddingSource, T: Borrow<MemoryEntry>, M: Borrow<StoredMemory>>(
        keys: &std::collections::HashMap<String, T>,
        embedder: &E,
        stored_list: &[M],
        query: &MemoryQuery,
        settings: &QuerySettings,
    ) -> Result<Vec<(f32, MemoryEntry)>> {
        settings.validate()?;
        settings.diagnostics.replace(Default::default());
        let filtered: Vec<&StoredMemory> = stored_list
            .iter()
            .map(Borrow::borrow)
            .filter(|s| !s.deleted)
            .filter(|s| match query.kind {
                Some(k) => s.local_kind == k.as_str(),
                None => true,
            })
            .filter(|s| match &query.project {
                Some(p) if !p.is_empty() => s.local_project == *p,
                _ => true,
            })
            .collect();
        if query.text.trim().is_empty() {
            let mut list: Vec<&MemoryEntry> = filtered.iter()
                .filter_map(|s| keys.get(&s.id).map(Borrow::borrow))
                .collect();
            list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            list.truncate(query.limit.max(1));
            return Ok(list.into_iter().map(|entry| (0.0, entry.clone())).collect());
        }
        // Mailbox-style three-tier recall (frozen 2026-09-19): important → normal (legacy; new writes retired) → trivia diary.
        // The host floor controls mailbox routing; cosine admission is performed by the scorer.
        // not filling a count — a low-score hit in the main zone is not found and we fall through; each tier returns once related hits reach half the quota.
        let q_vec = embedder.try_embed_query(&query.text)?;
        validate_embedding(&q_vec, embedder.dims())?;
        settings.resolved_cos_min.set(Some(crate::search::resolve_cos_gate(&filtered, &q_vec, settings)));
        settings.lexical_scores.replace(if settings.bm25 {
            Some(filtered.iter().zip(settings.score_lexical(&filtered, &query.text)?)
                .map(|(entry, score)| (entry.id.clone(), score)).collect())
        } else { None });
        let min_score = settings.recall_min_score;
        let quota = query.limit.max(2) / 2;
        let mut merged: Vec<(f32, MemoryEntry)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (imp, is_last) in [("important", false), ("normal", false), ("trivial", true)] {
            let pool: Vec<&StoredMemory> = filtered
                .iter()
                .copied()
                .filter(|s| s.local_importance == imp)
                .collect();
            if pool.is_empty() {
                continue;
            }
            settings.diagnostics.borrow_mut().tiers.push(imp.to_owned());
            let hits = Self::recall_scored_inner(keys, &pool, &q_vec, query, settings)?;
            // "Found" = related hits (score at floor) filling half the quota; non-last tiers only keep related hits, low scores count as not found
            let relevant: Vec<_> = hits
                .into_iter()
                .filter(|(s, _)| is_last || *s >= min_score)
                .collect();
            let found = relevant.len() >= quota;
            for (score, e) in relevant {
                if !seen.insert(e.id.clone()) {
                    continue;
                }
                if !is_last && merged.len() >= query.limit.max(1) {
                    break; // non-last tiers stop once the quota is filled
                }
                merged.push((score, e));
            }
            if found || merged.len() >= query.limit.max(1) {
                break;
            }
        }
        merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        merged.truncate(query.limit.max(1));
        Ok(merged)
    }

    fn recall_scored_inner<T: Borrow<MemoryEntry>>(
        keys: &std::collections::HashMap<String, T>,
        filtered: &[&StoredMemory],
        q_vec: &[f32],
        query: &MemoryQuery,
        settings: &QuerySettings,
    ) -> Result<Vec<(f32, MemoryEntry)>> {
        // Tree meta: id → (depth, subtree latest updated_at ms). Used for outline boost and subtree activity.
        let tree = build_tree_meta(filtered);
        let order =
            rerank_semantic_with_settings(filtered, q_vec, &query.text, &tree, settings);
        let limit = query.limit.max(1);

        // MMR dedup pick (added 2026-09-12): top-k slots are not filled by same-topic copies.
        // Take a 10x candidate pool (normally capped at 200), then select by relevance and diversity.
        // Host-supplied lambda (1.0 = dedup off); non-recall (store-candidate)
        // uses the same rule — store-judge wants to see the same-topic rows.
        let lambda = if (0.0..=1.0).contains(&settings.mmr_lambda) {
            settings.mmr_lambda
        } else {
            MMR_LAMBDA
        };
        let pool_size = limit
            .saturating_mul(10)
            .min(200.max(limit))
            .min(order.len());
        let pool: Vec<(usize, f32)> = order.iter().copied().take(pool_size).collect();
        let picked = if lambda >= 1.0 || pool.len() <= limit {
            pool.iter()
                .map(|(i, _)| *i)
                .take(limit)
                .collect::<Vec<usize>>()
        } else {
            // sim: cosine of in-library plaintext vectors (same-topic copies nearly coincide)
            let emb_of = |i: usize| filtered[i].local_embedding.as_deref().unwrap_or(&[]);
            crate::rank::mmr_select(&pool, limit, lambda, |a, b| {
                crate::search::cosine_similarity(emb_of(a), emb_of(b)).max(0.0)
            })
        };
        let score_of: std::collections::HashMap<usize, f32> = order.iter().copied().collect();

        let mut out = Vec::new();
        for idx in picked {
            let score = score_of.get(&idx).copied().unwrap_or(0.0);
            out.push((score, Self::open(keys, filtered[idx])?));
        }
        Ok(out)
    }

    pub fn recall_local_contextual<E: crate::search::EmbeddingSource, T: Borrow<MemoryEntry>, M: Borrow<StoredMemory>>(
        keys: &std::collections::HashMap<String, T>,
        embedder: &E,
        stored_list: &[M],
        query: &MemoryQuery,
        settings: &QuerySettings,
    ) -> Result<Vec<RecalledWithContext>> {
        let hits = Self::recall_local_scored(keys, embedder, stored_list, query, settings)?;
        let budget = settings.ancestor_budget;
        let floor = settings.ancestor_root_floor;
        let by_id: std::collections::HashMap<&str, &StoredMemory> =
            stored_list.iter().map(Borrow::borrow).map(|s| (s.id.as_str(), s)).collect();

        let mut out = Vec::with_capacity(hits.len());
        for (score, entry) in hits {
            let mut ancestors: Vec<MemoryEntry> = Vec::new();
            if budget > 0 {
                // (1) collect the whole ancestor chain near→far first (no truncate yet), cycle cap 32
                let mut chain: Vec<MemoryEntry> = Vec::new();
                let mut cur = entry.parent_id.clone();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for _ in 0..32 {
                    if cur.is_empty() || !seen.insert(cur.clone()) {
                        break;
                    }
                    let Some(s) = by_id.get(cur.as_str()) else {
                        break;
                    };
                    if s.deleted {
                        break;
                    }
                    let a = Self::open(keys, s)?;
                    let next = a.parent_id.clone();
                    chain.push(a);
                    cur = next;
                }
                chain.reverse(); // root first — causal order (parent=cause, child=effect)
                                 // (2) compute per-layer quota (far floor + near leftover), then truncate each layer
                let lens: Vec<usize> = chain.iter().map(|a| a.content.chars().count()).collect();
                let caps = ancestor_budget_split(&lens, budget, floor);
                for (i, a) in chain.iter_mut().enumerate() {
                    if caps[i] < lens[i] {
                        a.content = a.content.chars().take(caps[i]).collect();
                    }
                }
                ancestors = chain;
            }
            out.push(RecalledWithContext {
                score,
                entry,
                ancestors,
            });
        }
        Ok(out)
    }
}
