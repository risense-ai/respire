//! Post-selection associations. Ranking and vector policy never leave Core.
use anyhow::{ensure, Context, Result};
use respire_protocol::{MemoryQuery, RecallPair, RelatedMemory, RelatedResult, RelationKind};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    enabled: bool,
    max: usize,
    pair_min: u64,
    min_cos: f32,
    knn_seeds: usize,
    knn_per: usize,
    /// Sourcing floor: only seeds[..floor] are suppressed as targets. Absent
    /// defaults to 5 so truncated consumers keep the 6..20 band (measured
    /// waste: related∩top20 = 0/2118 with the old exclude-all behaviour);
    /// send a value >= ids.len() for the legacy exclude-all.
    floor: Option<usize>,
    /// Degree-normalised PPR re-rank over explicit/pair/kNN candidates.
    /// Absent defaults to on; `false` restores the staged first-fit order.
    ppr: Option<bool>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            max: 8,
            pair_min: 2,
            min_cos: 0.85,
            knn_seeds: 3,
            knn_per: 2,
            floor: None,
            ppr: None,
        }
    }
}

/// Resolve only forward edges within the caller's authorized, undeleted scope.
pub(super) fn superseded(
    view: &crate::resident::View,
    allowed: &HashSet<&str>,
) -> BTreeMap<String, String> {
    let entries = &view.entries;
    // The forward edge is authoritative; reverse edges are only persisted hints.
    let mut replacements = BTreeMap::<String, String>::new();
    for entry in entries.values().filter(|e| allowed.contains(e.id.as_str())) {
        if entry.supersedes != entry.id
            && !entry.supersedes.is_empty()
            && allowed.contains(entry.supersedes.as_str())
        {
            match replacements
                .get(&entry.supersedes)
                .and_then(|id| entries.get(id))
            {
                Some(previous)
                    if (&previous.updated_at, &previous.id) >= (&entry.updated_at, &entry.id) => {}
                _ => {
                    replacements.insert(entry.supersedes.clone(), entry.id.clone());
                }
            }
        }
    }
    let direct = replacements.clone();
    replacements.clear();
    for old in direct.keys() {
        let mut next = old.as_str();
        let mut seen = HashSet::new();
        while let Some(id) = direct.get(next) {
            if !seen.insert(next) {
                break;
            }
            next = id;
        }
        if !direct.contains_key(next) && next != old {
            replacements.insert(old.clone(), next.to_owned());
        }
    }
    replacements
}

pub(super) fn collect(mut input: Value) -> Result<Value> {
    let all_superseded = input.get("all_superseded")
        .map(|value| serde_json::from_value::<bool>(value.clone())).transpose()?.unwrap_or(false);
    let settings: Settings = match input.get("related_settings") {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Default::default(),
    };
    let query: MemoryQuery = serde_json::from_value(input["query"].clone())?;
    let seeds: Vec<String> = serde_json::from_value(input["ids"].clone())?;
    ensure!(seeds.len() <= 256, "association seed limit is 256");
    ensure!(settings.max <= 64 && settings.knn_seeds <= 16 && settings.knn_per <= 16,
        "association settings exceed bounded graph limits");
    ensure!(
        seeds.iter().collect::<HashSet<_>>().len() == seeds.len(),
        "duplicate association seed"
    );
    let pairs: Vec<RecallPair> = serde_json::from_value(input["pairs"].clone())?;
    ensure!(!all_superseded || (!settings.enabled && seeds.is_empty() && pairs.is_empty()),
        "all_superseded requires disabled related suggestions and empty ids/pairs");
    let model = input["model"].as_str().context("missing model")?.to_owned();
    let view = crate::resident::view(&mut input, &model)?;
    let (memories, entries) = (&view.memories, &view.entries);
    let allowed: HashSet<&str> = memories
        .iter()
        .filter(|m| {
            !m.deleted
                && query.project.as_ref().is_none_or(|p| &m.local_project == p)
                && query
                    .computer
                    .as_ref()
                    .is_none_or(|c| &m.local_computer == c)
                && query.kind.is_none_or(|k| m.local_kind == k.as_str())
        })
        .map(|m| m.id.as_str())
        .collect();
    ensure!(
        seeds
            .iter()
            .all(|id| allowed.contains(id.as_str()) && entries.contains_key(id)),
        "association seed is outside the authorized scope"
    );
    let mut result = RelatedResult::default();
    let replacements = superseded(&view, &allowed);
    if all_superseded {
        result.superseded = replacements;
        return Ok(serde_json::to_value(result)?);
    }
    for id in &seeds {
        if let Some(next) = replacements.get(id) {
            result.superseded.insert(id.clone(), next.clone());
        }
    }
    if !settings.enabled {
        return Ok(serde_json::to_value(result)?);
    }
    let floor = settings.floor.unwrap_or(5).min(seeds.len());
    let use_ppr = settings.ppr.unwrap_or(true);
    let budget = settings.max.min(seeds.len()).max(settings.max.min(floor));
    // Sourcing floor: seeds[..floor] are suppressed as targets (and still act
    // as sources); ancestors of every seed stay suppressed — already context.
    let mut excluded: HashSet<String> = seeds[..floor].iter().cloned().collect();
    for id in &seeds {
        let mut cursor = entries.get(id).map(|e| e.parent_id.as_str()).unwrap_or("");
        while !cursor.is_empty() && excluded.insert(cursor.to_owned()) {
            cursor = entries
                .get(cursor)
                .map(|e| e.parent_id.as_str())
                .unwrap_or("");
        }
    }
    if !use_ppr {
        // Legacy staged first-fit (explicit -> pair -> kNN), unchanged order.
        let budget = settings.max.min(seeds.len());
        let mut add = |id: &str, source: &str, relation: RelationKind| {
            if result.related.len() >= budget || !allowed.contains(id) || excluded.contains(id) {
                return false;
            }
            if let Some(entry) = entries.get(id) {
                excluded.insert(id.to_owned());
                result.related.push(RelatedMemory {
                    id: id.to_owned(),
                    title: entry.title.clone(),
                    source_id: source.to_owned(),
                    relation,
                });
                return true;
            }
            false
        };
        for id in &seeds {
            if let Some(entry) = entries.get(id) {
                if let Some(next) = replacements.get(id) {
                    add(next, id, RelationKind::NewVersion);
                }
                if !entry.supersedes.is_empty() {
                    add(&entry.supersedes, id, RelationKind::OldVersion);
                }
                for other in &entry.see_also {
                    add(other, id, RelationKind::SeeAlso);
                }
            }
        }
        let minimum = settings.pair_min;
        let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
        let mut partners = BTreeMap::<String, (u64, String)>::new();
        for pair in pairs.iter().filter(|p| p.n >= minimum) {
            let edge = match (
                seed_set.contains(pair.a.as_str()),
                seed_set.contains(pair.b.as_str()),
            ) {
                (true, false) => Some((&pair.b, &pair.a)),
                (false, true) => Some((&pair.a, &pair.b)),
                _ => None,
            };
            if let Some((other, seed)) = edge {
                let item = partners.entry(other.clone()).or_insert((0, seed.clone()));
                item.0 = item.0.saturating_add(pair.n);
            }
        }
        let mut partners: Vec<_> = partners.into_iter().collect();
        partners.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        for (id, (_, source)) in partners {
            add(&id, &source, RelationKind::CoRecall);
        }
        let cosine = settings.min_cos;
        ensure!(
            cosine.is_finite() && (0.0..=1.0).contains(&cosine),
            "invalid association cosine threshold"
        );
        let vectors: HashMap<&str, &[f32]> = memories
            .iter()
            .filter_map(|m| m.local_embedding.as_deref().map(|v| (m.id.as_str(), v)))
            .collect();
        for source in seeds.iter().take(settings.knn_seeds) {
            let Some(vector) = vectors.get(source.as_str()) else {
                continue;
            };
            let mut nearby: Vec<_> = memories
                .iter()
                .filter(|m| allowed.contains(m.id.as_str()) && m.id != *source)
                .filter_map(|m| {
                    m.local_embedding.as_ref().map(|v| {
                        (
                            m.id.as_str(),
                            respire_core::search::cosine_similarity(vector, v),
                        )
                    })
                })
                .filter(|(_, score)| *score >= cosine)
                .collect();
            nearby.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            // add() removes seeds/ancestors; skip them before applying the per-seed cap.
            let mut added = 0;
            for (id, _) in nearby {
                if added >= settings.knn_per {
                    break;
                }
                if add(id, source, RelationKind::Neighbor) {
                    added += 1;
                }
            }
        }
        return Ok(serde_json::to_value(result)?);
    }

    // ---- PPR branch: gather every source's candidates with labels+weights,
    // then degree-normalised PPR re-ranks them (tests/23 ②). ----
    let mut seen: HashSet<String> = excluded.clone();
    let mut ordered: Vec<(String, String, RelationKind, f64)> = Vec::new();
    let mut push = |ordered: &mut Vec<(String, String, RelationKind, f64)>,
                    seen: &mut HashSet<String>,
                    id: &str,
                    source: &str,
                    relation: RelationKind,
                    weight: f64| {
        if allowed.contains(id) && entries.contains_key(id) && seen.insert(id.to_owned()) {
            ordered.push((id.to_owned(), source.to_owned(), relation, weight));
        }
    };
    for id in &seeds {
        if let Some(entry) = entries.get(id) {
            if let Some(next) = replacements.get(id) {
                push(&mut ordered, &mut seen, next, id, RelationKind::NewVersion, 1.0);
            }
            if !entry.supersedes.is_empty() {
                push(&mut ordered, &mut seen, &entry.supersedes, id, RelationKind::OldVersion, 1.0);
            }
            for other in &entry.see_also {
                push(&mut ordered, &mut seen, other, id, RelationKind::SeeAlso, 1.0);
            }
        }
    }
    // Bound each gather stage so the dense PPR graph stays quadratic only in
    // a fixed cap (large see_also lists must not inflate the node set).
    let stage_cap = settings.max.saturating_mul(4).max(16);
    ordered.truncate(stage_cap);
    let minimum = settings.pair_min;
    let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut partners = BTreeMap::<String, (u64, String)>::new();
    for pair in pairs.iter().filter(|p| p.n >= minimum) {
        let edge = match (
            seed_set.contains(pair.a.as_str()),
            seed_set.contains(pair.b.as_str()),
        ) {
            (true, false) => Some((&pair.b, &pair.a)),
            (false, true) => Some((&pair.a, &pair.b)),
            _ => None,
        };
        if let Some((other, seed)) = edge {
            let item = partners.entry(other.clone()).or_insert((0, seed.clone()));
            item.0 = item.0.saturating_add(pair.n);
        }
    }
    let mut partners: Vec<_> = partners.into_iter().collect();
    partners.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
    let mut pair_added = 0usize;
    for (id, (n, source)) in partners {
        if pair_added >= stage_cap {
            break;
        }
        let before = ordered.len();
        let weight = (n.min(5)) as f64;
        push(&mut ordered, &mut seen, &id, &source, RelationKind::CoRecall, weight);
        if ordered.len() > before {
            pair_added += 1;
        }
    }
    let cosine = settings.min_cos;
    ensure!(
        cosine.is_finite() && (0.0..=1.0).contains(&cosine),
        "invalid association cosine threshold"
    );
    let vectors: HashMap<&str, &[f32]> = memories
        .iter()
        .filter_map(|m| m.local_embedding.as_deref().map(|v| (m.id.as_str(), v)))
        .collect();
    let wide = settings.knn_per.saturating_mul(4).max(8);
    for source in seeds.iter().take(settings.knn_seeds) {
        let Some(vector) = vectors.get(source.as_str()) else {
            continue;
        };
        let mut nearby: Vec<_> = memories
            .iter()
            .filter(|m| allowed.contains(m.id.as_str()) && m.id != *source)
            .filter_map(|m| {
                m.local_embedding.as_ref().map(|v| {
                    (
                        m.id.as_str(),
                        respire_core::search::cosine_similarity(vector, v),
                    )
                })
            })
            .filter(|(_, score)| *score >= cosine)
            .collect();
        nearby.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let mut added = 0;
        for (id, score) in nearby {
            if added >= wide {
                break;
            }
            let before = ordered.len();
            push(&mut ordered, &mut seen, id, source, RelationKind::Neighbor, f64::from(score));
            if ordered.len() > before {
                added += 1;
            }
        }
    }
    // The input settings and every source are bounded, including explicit edges.
    // At most 256 seeds + 256 candidates reach the dense walk.
    ordered.truncate(256);
    if ordered.is_empty() {
        return Ok(serde_json::to_value(result)?);
    }
    // Graph: seeds first, then gathered candidates. Edges: recorded
    // source->candidate weights, candidate-candidate kNN (>= min_cos, top 3),
    // and co-recall pairs among candidates (weight min(n,5)).
    let n_s = seeds.len();
    let seed_index: HashMap<&str, usize> = seeds
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let cand_index: HashMap<&str, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, (id, ..))| (id.as_str(), i))
        .collect();
    let n = n_s + ordered.len();
    let mut w = vec![vec![0f64; n]; n];
    {
        let mut add_e = |a: usize, b: usize, weight: f64| {
            if a != b && weight > 0.0 {
                w[a][b] += weight;
                w[b][a] += weight;
            }
        };
        for (pos, (_id, source, _, weight)) in ordered.iter().enumerate() {
            if let Some(&si) = seed_index.get(source.as_str()) {
                add_e(si, n_s + pos, *weight);
            }
        }
        for (i, (id_i, ..)) in ordered.iter().enumerate() {
            let mut links: Vec<(f64, usize)> = Vec::new();
            for (j, (id_j, ..)) in ordered.iter().enumerate() {
                if i >= j {
                    continue;
                }
                let (Some(vi), Some(vj)) = (
                    vectors.get(id_i.as_str()).copied(),
                    vectors.get(id_j.as_str()).copied(),
                ) else {
                    continue;
                };
                if vi.len() != vj.len() || vi.is_empty() {
                    continue;
                }
                let c = f64::from(respire_core::search::cosine_similarity(vi, vj));
                if c >= f64::from(cosine) {
                    links.push((c, j));
                }
            }
            links.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            for (c, j) in links.into_iter().take(3) {
                add_e(n_s + i, n_s + j, c);
            }
        }
        for pair in pairs.iter().filter(|p| p.n >= minimum) {
            let ai = cand_index.get(pair.a.as_str()).copied();
            let bi = cand_index.get(pair.b.as_str()).copied();
            if let (Some(a), Some(b)) = (ai, bi) {
                add_e(n_s + a, n_s + b, pair.n.min(5) as f64);
            }
        }
    }
    // Uniform restart over seeds, row-normalised walk, power iteration.
    let alpha = 0.25f64;
    let mut r = vec![0f64; n];
    for slot in r.iter_mut().take(n_s) {
        *slot = 1.0 / n_s.max(1) as f64;
    }
    let mut deg = vec![0f64; n];
    for (a, row) in w.iter().enumerate() {
        deg[a] = row.iter().sum::<f64>();
    }
    let mut pi = r.clone();
    for _ in 0..60 {
        let mut next = vec![0f64; n];
        for a in 0..n {
            if deg[a] == 0.0 {
                continue;
            }
            let share = pi[a] * (1.0 - alpha);
            if share == 0.0 {
                continue;
            }
            for (b, wt) in w[a].iter().enumerate() {
                if *wt > 0.0 {
                    next[b] += share * wt / deg[a];
                }
            }
        }
        for (i, ri) in r.iter().enumerate() {
            next[i] += alpha * ri;
        }
        pi = next;
    }
    let mut ranked: Vec<(f64, usize)> = pi
        .iter()
        .enumerate()
        .skip(n_s)
        .map(|(i, mass)| (*mass, i - n_s))
        .filter(|(mass, _)| *mass > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, pos) in ranked.into_iter().take(budget) {
        let (id, source, relation, _) = &ordered[pos];
        if let Some(entry) = entries.get(id) {
            result.related.push(RelatedMemory {
                id: id.to_owned(),
                title: entry.title.clone(),
                source_id: source.to_owned(),
                relation: *relation,
            });
        }
    }
    Ok(serde_json::to_value(result)?)
}
