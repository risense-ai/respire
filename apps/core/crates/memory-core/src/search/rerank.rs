use super::*;

pub fn rerank_semantic(
    entries: &[&StoredMemory],
    query_vec: &[f32],
    query_text: &str,
    tree: &std::collections::HashMap<String, (usize, i64)>,
) -> Vec<(usize, f32)> {
    rerank_semantic_with_settings(entries, query_vec, query_text, tree, &Default::default())
}

/// Document and query use plain text. Do not repeat a body copied into its title.
pub fn document_text(title: &str, content_head: &str) -> String {
    let title = title.trim();
    let content = content_head.trim();
    if title.is_empty() || content.starts_with(title) { content.to_owned() }
    else if content.is_empty() { title.to_owned() }
    else { format!("{title}\n{content}") }
}

fn entry_cosine(entry: &StoredMemory, query: &[f32]) -> Option<f32> {
    if query.is_empty() { return None; }
    entry.local_embedding.iter().chain(entry.local_chunks.iter())
        .filter(|vector| !vector.is_empty())
        .map(|vector| cosine_similarity(vector, query).max(0.0))
        .reduce(f32::max)
}

/// Relative gates use the complete authorized query pool, before mailbox tiers.
pub fn resolve_cos_gate(entries: &[&StoredMemory], query: &[f32], settings: &crate::query::QuerySettings) -> f32 {
    let absolute = settings.cos_min.unwrap_or(match query.len() { 1024 => 0.42, 768 => 0.38, _ => 0.40 });
    if settings.cos_relative_ratio.is_none() && settings.cos_margin.is_none() { return absolute; }
    let mut values: Vec<f32> = entries.iter().filter_map(|entry| entry_cosine(entry, query)).collect();
    values.sort_by(f32::total_cmp);
    let Some(top) = values.last().copied() else { return absolute; };
    let middle = values.len() / 2;
    let median = if values.len() % 2 == 0 { (values[middle - 1] + values[middle]) / 2.0 } else { values[middle] };
    if settings.cos_margin.is_some_and(|margin| top - median < margin) { return 1.0 + f32::EPSILON; }
    absolute.max(settings.cos_relative_ratio.map_or(0.0, |ratio| ratio * top))
}

pub fn rerank_semantic_with_settings(
    entries: &[&StoredMemory],
    query_vec: &[f32],
    query_text: &str,
    tree: &std::collections::HashMap<String, (usize, i64)>,
    settings: &crate::query::QuerySettings,
) -> Vec<(usize, f32)> {
    match settings.now {
        Some(now) => rerank_semantic_with_settings_at(entries, query_vec, query_text, tree, settings, now),
        // shortcut: legacy callers still use the system clock; remove after host now migration.
        None => rerank_semantic_with_time_source(entries, query_vec, query_text, tree, settings, chrono::Utc::now),
    }
}

/// Deterministic scoring with one host-supplied instant for every temporal axis.
pub fn rerank_semantic_with_settings_at(
    entries: &[&StoredMemory],
    query_vec: &[f32],
    query_text: &str,
    tree: &std::collections::HashMap<String, (usize, i64)>,
    settings: &crate::query::QuerySettings,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(usize, f32)> {
    rerank_semantic_with_time_source(entries, query_vec, query_text, tree, settings, || now)
}

fn rerank_semantic_with_time_source(
    entries: &[&StoredMemory],
    query_vec: &[f32],
    query_text: &str,
    tree: &std::collections::HashMap<String, (usize, i64)>,
    settings: &crate::query::QuerySettings,
    now: impl Fn() -> chrono::DateTime<chrono::Utc>,
) -> Vec<(usize, f32)> {
    let routing = routing(query_text, settings);
    let ablate: std::collections::HashSet<&str> = routing.ablate.split(',').collect();
    let q_words: Vec<String> = query_text.split_whitespace().map(str::to_lowercase).collect();
    let q_bigrams = char_bigrams(query_text);
    let exact_query = query_text.trim().to_lowercase();
    let lexical = if settings.bm25 {
        match settings.lexical_scores.borrow().as_ref() {
            Some(scores) => entries.iter().map(|entry| scores.get(&entry.id).copied().unwrap_or(0.0)).collect(),
            None => crate::rank::LexicalIndex::build(entries).scores(query_text),
        }
    } else { Vec::new() };
    let cosines: Vec<_> = entries.iter().map(|entry| entry_cosine(entry, query_vec)).collect();
    let cos_min = settings.resolved_cos_min.get().unwrap_or_else(|| resolve_cos_gate(entries, query_vec, settings));
    let consensus = if settings.rrf_cap > 0.0 && !ablate.contains("kw") && !ablate.contains("semantic") {
        let mut vector_order: Vec<usize> = (0..entries.len()).filter(|&i| cosines[i].is_some_and(|cos| cos > 0.0)).collect();
        vector_order.sort_by(|&a, &b| cosines[b].partial_cmp(&cosines[a]).unwrap_or(std::cmp::Ordering::Equal));
        let mut lexical_order: Vec<usize> = (0..lexical.len()).filter(|&i| lexical[i] > 0.0).collect();
        lexical_order.sort_by(|&a, &b| lexical[b].total_cmp(&lexical[a]));
        crate::rank::rrf(&[&vector_order, &lexical_order], 60.0)
    } else { Default::default() };
    let w_sem = if ablate.contains("semantic") { 0.0 } else { 0.60 };
    let w_rec = if ablate.contains("recency") || (routing.route_on && !routing.temporal) { 0.0 } else { 0.20 };
    let w_hot = if ablate.contains("hot") { 0.0 } else { 0.10 };
    let w_tree = if ablate.contains("tree") { 0.0 } else if routing.route_on { 0.02 } else { 0.08 };
    let w_rrf = if consensus.is_empty() { 0.0 } else { settings.rrf_cap };
    // Emotion is deliberately disabled; a dead axis must not dilute ablation normalization.
    let total = if settings.normalize_ablation { (w_sem + w_rec + w_hot + w_tree + w_rrf).max(f32::EPSILON) } else { 1.0 };
    let mut diagnostics = settings.diagnostics.borrow_mut();
    diagnostics.cos_gate = cos_min;
    diagnostics.sim_min = settings.sim_min;
    struct HeadRow {
        idx: usize,
        pending: bool,
        stretch: f32,
        kw: f32,
        semantic: f32,
        recency: f32,
        hot: f32,
        tree_axis: f32,
        rrf: f32,
        aux: f32,
        admitted: bool,
        legacy_score: f32,
    }
    let feedback = settings.feedback.as_ref().filter(|feedback| feedback.enabled);
    let mut rows = Vec::with_capacity(entries.len());
    for (idx, entry) in entries.iter().enumerate() {
        let cosine = cosines[idx].unwrap_or(0.0);
        let pending = cosines[idx].is_none();
        let legacy_kw = if !settings.bm25 || pending {
            keyword_score(&q_words, &q_bigrams, &entry.local_kind,
                &entry.local_tags, &entry.local_title, &entry.local_content_head).min(1.0)
        } else { 0.0 };
        let kw = if ablate.contains("kw") { 0.0 } else if settings.bm25 { lexical[idx] } else { legacy_kw };
        let exact_pending = pending && !ablate.contains("kw") && !exact_query.is_empty()
            && [&entry.local_title, &entry.local_tags, &entry.local_content_head].iter()
                .any(|field| exact_phrase_matches(&field.to_lowercase(), &exact_query));
        let span = if query_vec.len() == 1024 { 0.65 } else { 0.50 };
        let stretch = ((cosine - 0.20) / span).clamp(0.0, 1.0);
        let fused = if exact_pending { 1.0 }
            else if pending { kw.max(if ablate.contains("kw") { 0.0 } else { legacy_kw }) }
            else if ablate.contains("kw") && settings.normalize_ablation { stretch }
            else if settings.max_fusion { stretch.max(kw) }
            else { (1.0 - settings.lexical_weight) * stretch + settings.lexical_weight * kw };
        let semantic = fused.clamp(0.0, 1.0).powf(settings.semantic_power);
        let created = if entry.local_created_at.is_empty() { entry.updated_at.as_str() } else { &entry.local_created_at };
        let recency = recency_score(created, &now);
        let hot = match feedback {
            Some(feedback) => hit_score(feedback.good.get(&entry.id).copied().unwrap_or(0).min(15) as i64),
            None if settings.read_heat => hit_score(entry.local_recall_count.min(settings.heat_cap as i64)),
            None => 0.0,
        };
        let (depth, latest) = tree.get(&entry.id).copied().unwrap_or((0, 0));
        let active = latest > 0 && now().timestamp_millis() - latest <= 7 * 24 * 3600 * 1000;
        let tree_axis = if w_tree == 0.0 { 0.0 } else {
            ((match depth { 0 => 0.04, 1 => 0.02, _ => 0.0 }) + if active { 0.04 } else { 0.0 })
                * if routing.route_on { 0.25 } else { 1.0 }
        };
        let rrf = consensus.get(&idx).copied().unwrap_or(0.0) * 61.0 / 2.0 * settings.rrf_cap;
        let aux_raw = w_rec * recency + w_hot * hot + tree_axis + rrf;
        let aux = if w_sem > 0.0 { settings.aux_cap_ratio.map_or(aux_raw, |ratio| aux_raw.min(ratio * semantic)) } else { aux_raw };
        let legacy_score = (w_sem * semantic + aux) / total;
        // Pending recall keeps the established lexical admission test; it has no cosine.
        let admitted = if pending { exact_pending || (!ablate.contains("kw") && legacy_kw >= 0.50) }
            else { cos_min == 0.0 || cosine >= cos_min };
        diagnostics.examined += 1;
        if !pending { diagnostics.best_cos = Some(diagnostics.best_cos.unwrap_or(0.0).max(cosine)); }
        rows.push(HeadRow {
            idx, pending, stretch, kw, semantic, recency, hot, tree_axis, rrf, aux,
            admitted, legacy_score,
        });
    }

    // Attention fusion: heads produce distributions over admitted candidates;
    // independent gates (not shares) decide how much each prior matters for this
    // query. Semantic relevance stays the residual anchor so priors cannot
    // manufacture relevance for unrelated rows.
    let mode = settings.fusion.as_str();
    let anchor = mode == "attention_anchor";
    let compute_attention = matches!(mode, "attention" | "attention_anchor" | "auto")
        && w_sem > 0.0 && !ablate.contains("semantic")
        && (mode != "auto" || auto_attention_gate(entries.len(), query_text));
    let mut attention_scores: std::collections::HashMap<usize, f32> = Default::default();
    if compute_attention {
        let admitted_rows: Vec<&HeadRow> = rows.iter()
            .filter(|row| row.admitted && !row.pending)
            .collect();
        if !admitted_rows.is_empty() {
            let temperature = settings.fusion_temperature;
            let values = |pick: fn(&HeadRow) -> f32| admitted_rows.iter().map(|row| pick(row)).collect::<Vec<_>>();
            // `attention_anchor` keeps the proven legacy semantic+lexical blend
            // as the relevance anchor and lets only the prior heads attend.
            let semantic_source = if anchor {
                head_distribution(&values(|row| row.semantic), temperature)
            } else {
                head_distribution(&values(|row| row.stretch), temperature)
            };
            let semantic = semantic_source;
            let lexical = head_distribution(&values(|row| row.kw), temperature);
            let recency = head_distribution(&values(|row| row.recency), temperature);
            let hot = head_distribution(&values(|row| row.hot), temperature);
            let tree = head_distribution(&values(|row| row.tree_axis), temperature);
            let lexical_active = admitted_rows.iter().any(|row| row.kw > 0.0);
            let hot_active = admitted_rows.iter().any(|row| row.hot > 0.0);
            let tree_active = admitted_rows.iter().any(|row| row.tree_axis > 0.0);
            let gate_recency: f32 = if ablate.contains("recency") { 0.0 }
                else if routing.temporal { settings.fusion_gate_recency_temporal }
                else if routing.route_on { settings.fusion_gate_recency } else { 0.35 };
            let gate_hot: f32 = if ablate.contains("hot") || !hot_active { 0.0 } else { settings.fusion_gate_hot };
            let gate_tree: f32 = if ablate.contains("tree") || !tree_active { 0.0 } else { settings.fusion_gate_tree };
            let gate_mass = (gate_recency + gate_hot + gate_tree).max(f32::EPSILON);
            // Temporal questions may lean harder on the time head; the lexical
            // share is independent, so raising one does not lower the other.
            let alpha = if routing.temporal {
                (settings.fusion_alpha + settings.fusion_temporal_alpha).min(1.0 - settings.fusion_beta)
            } else { settings.fusion_alpha };
            let beta = if anchor || ablate.contains("kw") || !lexical_active {
                0.0
            } else { settings.fusion_beta };
            let residual = (1.0 - alpha - beta).max(0.0);
            let fused: Vec<f32> = (0..admitted_rows.len()).map(|position| {
                let prior = (gate_recency * recency[position]
                    + gate_hot * hot[position]
                    + gate_tree * tree[position]) / gate_mass;
                residual * semantic[position] + beta * lexical[position] + alpha * prior
            }).collect();
            let peak = fused.iter().copied().fold(0.0, f32::max).max(f32::EPSILON);
            for (position, row) in admitted_rows.iter().enumerate() {
                attention_scores.insert(row.idx, fused[position] / peak);
            }
        }
    }
    let use_attention = compute_attention;

    let mut scored = Vec::with_capacity(rows.len());
    for row in rows {
        let mut score = if use_attention {
            attention_scores.get(&row.idx).copied().unwrap_or(row.legacy_score)
        } else { row.legacy_score };
        score = match settings.feedback.as_ref().filter(|feedback| feedback.enabled && feedback.bad_penalty) {
            Some(feedback) => score * (1.0 - 0.4 * feedback.bad.get(&entries[row.idx].id).copied().unwrap_or(0).min(5) as f32 / 5.0),
            None => score,
        };
        diagnostics.best_pre_floor = Some(diagnostics.best_pre_floor.unwrap_or(0.0).max(score));
        if !row.admitted { diagnostics.dropped_by_cos += 1; }
        else if score < settings.sim_min { diagnostics.dropped_by_floor += 1; }
        else {
            if row.pending { diagnostics.pending_returned += 1; }
            scored.push((row.idx, score));
        }
        if settings.debug_score {
            diagnostics.scores.push(serde_json::json!({"id":entries[row.idx].id,"cos":cosines[row.idx],"stretch":row.stretch,
                "kw":row.kw,"sem":row.semantic,"rec":row.recency,"hot":row.hot,"tree":row.tree_axis,"rrf":row.rrf,"aux":row.aux,
                "attention":attention_scores.get(&row.idx),"score":score,
                "admitted":row.admitted && score >= settings.sim_min,"pending":row.pending}));
        }
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored
}

/// Pure-keyword rerank (fallback when there is no vector).
pub fn rerank_locally(
    entries: &[(String, String, String, String)], // (kind, tags, title, content)
    query: &str,
) -> Vec<usize> {
    let q: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    if q.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut scored: Vec<(usize, f64)> = entries
        .iter()
        .enumerate()
        .map(|(idx, (kind, tags, title, content))| {
            let mut score = 0.0;
            for word in &q {
                if word.is_ascii() && word.len() < 2 { continue; }
                if exact_phrase_matches(&title.to_lowercase(), word) {
                    score += 3.0;
                }
                if exact_phrase_matches(&tags.to_lowercase(), word) {
                    score += 2.0;
                }
                if exact_phrase_matches(&content.to_lowercase(), word) {
                    score += 1.0;
                }
            }
            if *kind == "decision" || *kind == "skill" {
                score += 0.2;
            }
            (idx, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(idx, _)| idx).collect()
}
