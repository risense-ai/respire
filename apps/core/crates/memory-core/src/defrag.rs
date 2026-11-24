//! memory::defrag — existing-memory tidy analysis (fragmentation reconcile)
//!
//! Pairwise cosine in the **centered domain** of in-library BGE vectors: cluster similar, mark orphan-leaves, suggest a tree.
//! **Read-only, does not touch the store** — apply with existing judged commands (--merge-ids / --parent / forget).
//! Vectors are BGE semantics (computed at seal); if stale, run `reembed` first.
//!
//! Why center: BGE anisotropy — same-domain Chinese doc-doc raw cosine piles at 0.6–0.9 (measured in a real library
//! median 0.727), unrelated vs same-topic cannot be split. After subtracting library mean μ: unrelated pairs land near zero (median
//! −0.04), true-duplicate pairs stay ≥0.93 — only then does a threshold discriminate.

use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

use crate::engine::build_tree_meta;
use crate::search::{centered_cosine, mean_vector};
use crate::StoredMemory;

/// Cluster member display.
#[derive(Debug, Serialize)]
pub struct Member {
    pub id: String,
    pub title: String,
    pub date: String,
    pub depth: usize,
}

/// Similar cluster (≥2 members).
#[derive(Debug, Serialize)]
pub struct Cluster {
    /// Members (created_at early→late)
    pub members: Vec<Member>,
    /// Max similarity in the cluster (centered domain)
    pub max_sim: f32,
}

/// Analysis report.
#[derive(Debug, Serialize)]
pub struct Report {
    pub total: usize,
    pub roots: usize,
    /// Orphan leaf: no parent, no children
    pub orphans: usize,
    pub max_depth: usize,
    /// Similar clusters (size desc; "already placed" clusters whose members share a parent are dropped)
    pub clusters: Vec<Cluster>,
    /// Already-placed cluster count (all members share a parent — already under an outline, not fragments)
    pub settled: usize,
    /// Centered-domain doc-doc cosine distribution (p10/p50/p90) — a mirror for calibrating the threshold
    pub cos_p10: f32,
    pub cos_p50: f32,
    pub cos_p90: f32,
}

/// Pairwise cosine clustering (union-find). **Centered domain** — strip BGE anisotropy first.
/// min_sim suggested start 0.60 (centered domain; true dups typically ≥0.85).
pub fn analyze(list: &[StoredMemory], min_sim: f32) -> Result<Report> {
    let refs: Vec<&StoredMemory> = list.iter().collect();
    let tree = build_tree_meta(&refs);
    let n = list.len();

    // Centered μ: mean of every library vector
    let with_emb: Vec<&StoredMemory> = list
        .iter()
        .filter(|s| s.local_embedding.as_ref().is_some_and(|e| !e.is_empty()))
        .collect();
    let mu_vecs: Vec<&[f32]> = with_emb
        .iter()
        .filter_map(|s| s.local_embedding.as_deref())
        .collect();
    let mu = mean_vector(&mu_vecs).ok_or_else(|| anyhow::anyhow!("no usable vectors in the library (run reembed first)"))?;

    // Distribution sample (≤400 pairs) for the bill — a mirror for calibrating the threshold
    let sample: Vec<&StoredMemory> = with_emb.iter().take(60).copied().collect();
    let mut dist: Vec<f32> = Vec::new();
    for i in 0..sample.len() {
        for j in (i + 1)..sample.len() {
            let Some(a) = sample[i].local_embedding.as_deref() else { continue };
            let Some(b) = sample[j].local_embedding.as_deref() else { continue };
            dist.push(centered_cosine(a, b, &mu));
        }
    }
    dist.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |p: f32| -> f32 {
        let idx = ((dist.len() as f32 - 1.0) * p).round() as usize;
        dist.get(idx).copied().unwrap_or(0.0)
    };
    let (cos_p10, cos_p50, cos_p90) = if dist.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        (pick(0.10), pick(0.50), pick(0.90))
    };

    // Union-find cluster similar pairs (centered domain)
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut edges: Vec<(usize, f32)> = Vec::new(); // (member index, similarity)
    for i in 0..n {
        let Some(ei) = list[i].local_embedding.as_ref() else {
            continue;
        };
        if ei.is_empty() {
            continue;
        }
        for j in (i + 1)..n {
            let Some(ej) = list[j].local_embedding.as_ref() else {
                continue;
            };
            if ej.is_empty() {
                continue;
            }
            let sim = centered_cosine(ei, ej, &mu);
            if sim >= min_sim {
                let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                if ra != rb {
                    parent[rb] = ra;
                }
                edges.push((i, sim));
                edges.push((j, sim));
            }
        }
    }
    // Cluster aggregate
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        groups.entry(find(&mut parent, i)).or_default().push(i);
    }
    let mut clusters: Vec<Cluster> = groups
        .into_values()
        .filter(|g| g.len() >= 2)
        .map(|g| {
            let mut members: Vec<Member> = g
                .iter()
                .map(|&i| {
                    let s = &list[i];
                    let date_raw = if s.local_created_at.is_empty() {
                        s.updated_at.as_str()
                    } else {
                        s.local_created_at.as_str()
                    };
                    Member {
                        id: s.id.clone(),
                        title: s.local_title.clone(),
                        date: date_raw.chars().take(10).collect(),
                        depth: tree.get(&s.id).copied().unwrap_or((0, 0)).0,
                    }
                })
                .collect();
            members.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
            let member_idx: std::collections::HashSet<usize> = g.into_iter().collect();
            let max_sim = edges
                .iter()
                .filter(|(i, _)| member_idx.contains(i))
                .map(|(_, s)| *s)
                .fold(0.0f32, f32::max);
            Cluster { members, max_sim }
        })
        .collect();
    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.max_sim.partial_cmp(&a.max_sim).unwrap_or(std::cmp::Ordering::Equal))
    });
    // Already-placed clusters: all members share a parent (including under an outline) — still similar, but the tree is built, not fragments
    let by_idx: HashMap<&str, &StoredMemory> = list.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut settled = 0usize;
    clusters.retain(|c| {
        let parents: std::collections::HashSet<&str> = c
            .members
            .iter()
            .filter_map(|m| by_idx.get(m.id.as_str()).and_then(|s| if s.local_parent_id.is_empty() { None } else { Some(s.local_parent_id.as_str()) }))
            .collect();
        // All parentless (scattered) or all same parent — the former is real fragments; all same non-empty parent = already placed
        let all_rooted = c.members.iter().all(|m| {
            by_idx.get(m.id.as_str()).map(|s| s.local_parent_id.is_empty()).unwrap_or(false)
        });
        // Same-chain test: any two members in the cluster are ancestor/descendant (tree exists, not fragments)
        let ids_c: Vec<&str> = c.members.iter().map(|m| m.id.as_str()).collect();
        let mut same_chain = false;
        'outer: for &a_id in ids_c.iter() {
            let mut cur = a_id.to_owned();
            for _ in 0..64 {
                let Some(p) = by_idx.get(cur.as_str()).map(|s| s.local_parent_id.clone()) else { break };
                if p.is_empty() { break; }
                if ids_c.contains(&p.as_str()) {
                    same_chain = true;
                    break 'outer;
                }
                cur = p;
            }
        }
        let outline_only = c.members.iter().all(|m| m.title.contains(crate::i18n::query_terms::OUTLINE_MARKER));
        if outline_only || same_chain {
            settled += 1;
            false
        } else if !all_rooted && parents.len() == 1 {
            settled += 1;
            false
        } else {
            true
        }
    });

    // Stats: roots / orphan-leaves / max depth
    let roots = list
        .iter()
        .filter(|s| tree.get(&s.id).copied().unwrap_or((0, 0)).0 == 0)
        .count();
    let max_depth = tree.values().map(|(d, _)| *d).max().unwrap_or(0);
    let has_child: std::collections::HashSet<&str> = list
        .iter()
        .filter(|s| !s.local_parent_id.is_empty())
        .map(|s| s.local_parent_id.as_str())
        .collect();
    let orphans = list
        .iter()
        .filter(|s| s.local_parent_id.is_empty() && !has_child.contains(s.id.as_str()))
        .count();

    Ok(Report {
        total: n,
        roots,
        orphans,
        max_depth,
        clusters,
        settled,
        cos_p10,
        cos_p50,
        cos_p90,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::fixtures::defrag as text;
    use crate::search::{Embedder, HashingEmbedder};
    use crate::StoredMemory;

    #[test]
    fn clusters_similar_pairs_and_excludes_outlier() -> anyhow::Result<()> {
        let e = HashingEmbedder::default();
        let mut a = StoredMemory::new_pending("a".into());
        a.local_title = text::DIVIDE_AND_CONQUER_TITLE.into();
        a.local_embedding = Some(e.embed(text::DIVIDE_AND_CONQUER_TEXT));
        let mut b = StoredMemory::new_pending("b".into());
        b.local_title = text::KNAPSACK_TITLE.into();
        b.local_embedding = Some(e.embed(text::KNAPSACK_TEXT));
        let mut c = StoredMemory::new_pending("c".into());
        c.local_title = text::PROGRAMMING_HOMEWORK_TITLE.into();
        c.local_embedding = Some(e.embed(text::PROGRAMMING_HOMEWORK_TEXT));
        // 3 off-topic fillers — μ needs statistical mass; too small a sample over-centers
        let mut d = StoredMemory::new_pending("d".into());
        d.local_title = text::GO_JOSEKI_TITLE.into();
        d.local_embedding = Some(e.embed(text::GO_JOSEKI_TEXT));
        let mut f = StoredMemory::new_pending("f".into());
        f.local_title = text::COOKING_TITLE.into();
        f.local_embedding = Some(e.embed(text::COOKING_TEXT));
        let mut g = StoredMemory::new_pending("g".into());
        g.local_title = text::BICYCLE_TITLE.into();
        g.local_embedding = Some(e.embed(text::BICYCLE_TEXT));
        let list = vec![a, b, c, d, f, g];
        // Small sample + 256-d hashing embed over-centers (μ sucks out common lexical mass) —
        // production is a BGE 768-d large library: unrelated median −0.04, true-dup 0.93 (measured).
        // This test's mechanism: the only pair over the line is the notebook pair.
        let report = analyze(&list, 0.2)?;
        assert_eq!(report.clusters.len(), 1, "The two related notebook entries should form the only cluster");
        let ids: Vec<&str> = report.clusters[0]
            .members
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
        assert!(analyze(&[], 0.2).is_err());
        let mut empty = StoredMemory::new_pending("z".into());
        empty.local_embedding = Some(vec![]);
        assert!(analyze(&[empty], 0.2).is_err());
        assert!(!ids.contains(&"c"));
        Ok(())
    }
}
