//! Private smart maintenance policies, extracted without changing thresholds.
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use respire_protocol::{MemoryEntry, MemoryQuery};
use respire_core::StoredMemory;
use respire_core::query::MemoryEngine;
use crate::localized_notation::SEGMENT_LABELS;
use crate::reports::*;
fn short_id(id: &str) -> String { id.chars().take(8).collect() }
pub fn tree_cure(all: &[StoredMemory], top: usize, min_sim: f32) -> Result<TreeCureReport> {
        use respire_core::search::{centered_cosine, cosine_similarity, mean_vector};
        // Trivia out of the tree: drop trivial entries entirely (neither orphan-leaf suggestions nor attach targets)
        let all: Vec<StoredMemory> = all.iter().filter(|m| m.local_importance != "trivial").cloned().collect();
        let by_id: HashMap<&str, &StoredMemory> =
            all.iter().map(|m| (m.id.as_str(), m)).collect();
        let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
        for m in &all {
            if !m.local_parent_id.is_empty() {
                children_of
                    .entry(m.local_parent_id.as_str())
                    .or_default()
                    .push(m.id.as_str());
            }
        }
        // Walk up to the root per entry (store root id; 64-step cycle cap)
        let root_of = |id: &str| -> String {
            let mut cur = id.to_owned();
            for _ in 0..64 {
                match by_id.get(cur.as_str()) {
                    Some(p) if !p.local_parent_id.is_empty() => cur = p.local_parent_id.clone(),
                    _ => break,
                }
            }
            cur
        };
        // Root size (BFS subtree count)
        let subtree_size = |root: &str| -> usize {
            let mut n = 0usize;
            let mut stack = vec![root.to_owned()];
            while let Some(cur) = stack.pop() {
                if let Some(kids) = children_of.get(cur.as_str()) {
                    n += kids.len();
                    stack.extend(kids.iter().map(|k| (*k).to_owned()));
                }
            }
            n
        };
        let mut roots: Vec<RootStat> = all
            .iter()
            .filter(|m| m.local_parent_id.is_empty())
            .map(|m| RootStat {
                id: m.id.clone(),
                title: if m.local_title.is_empty() {
                    short_id(&m.id)
                } else {
                    m.local_title.clone()
                },
                descendants: subtree_size(&m.id),
            })
            .collect();
        roots.sort_by(|a, b| {
            b.descendants
                .cmp(&a.descendants)
                .then(b.title.cmp(&a.title))
        });
        let is_ontology = |m: &StoredMemory| {
            m.local_tags.split(',').any(|t| t.trim() == include_str!("i18n/legacy_taxonomy_tag.txt"))
        };
        let lone: Vec<&StoredMemory> = roots
            .iter()
            .filter(|r| r.descendants == 0)
            .filter_map(|r| by_id.get(r.id.as_str()))
            .copied()
            // Catalog roots (built-in class roots) are not orphan-leaves to cure — an empty outline with keywords but no children is normal; auto must not hang them under another outline
            .filter(|m| !is_ontology(m))
            .collect();
        let is_lone_root = |m: &StoredMemory| {
            m.local_parent_id.is_empty() && !children_of.contains_key(m.id.as_str())
        };
        // Centered-domain mean (entries with a vector join the mean)
        let vecs: Vec<&[f32]> = all
            .iter()
            .filter_map(|m| m.local_embedding.as_deref())
            .filter(|v| !v.is_empty())
            .collect();
        let mu = mean_vector(&vecs).unwrap_or_default();
        // Orphan-leaf roots × non-orphan entries, take top-1
        let mut suggests: Vec<CureSuggest> = Vec::new();
        for orph in &lone {
            let Some(ov) = orph.local_embedding.as_deref() else {
                continue;
            };
            let mut best: Option<(&StoredMemory, f32)> = None;
            for m in &all {
                if m.id == orph.id || is_lone_root(m) || is_ontology(m) {
                    continue;
                }
                let Some(mv) = m.local_embedding.as_deref() else {
                    continue;
                };
                if mv.is_empty() {
                    continue;
                }
                let sim = if mu.is_empty() {
                    cosine_similarity(ov, mv)
                } else {
                    centered_cosine(ov, mv, &mu)
                };
                if best.map(|(_, s)| sim > s).unwrap_or(true) {
                    best = Some((m, sim));
                }
            }
            if let Some((target, sim)) = best {
                if sim >= min_sim {
                    let tree_root = root_of(&target.id);
                    suggests.push(CureSuggest {
                        orphan: NodeRef {
                            id: orph.id.clone(),
                            title: if orph.local_title.is_empty() {
                                short_id(&orph.id)
                            } else {
                                orph.local_title.clone()
                            },
                        },
                        target: NodeRef {
                            id: target.id.clone(),
                            title: if target.local_title.is_empty() {
                                short_id(&target.id)
                            } else {
                                target.local_title.clone()
                            },
                        },
                        target_tree: NodeRef {
                            id: tree_root.clone(),
                            title: by_id
                                .get(tree_root.as_str())
                                .map(|r| {
                                    if r.local_title.is_empty() {
                                        short_id(&r.id)
                                    } else {
                                        r.local_title.clone()
                                    }
                                })
                                .unwrap_or_default(),
                        },
                        sim,
                    });
                }
            }
        }
        suggests.sort_by(|a, b| {
            b.sim
                .partial_cmp(&a.sim)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        suggests.truncate(top);
        Ok(TreeCureReport {
            lone_roots: lone.len(),
            roots,
            suggests,
        })
    }

pub fn deepen_plan(
    all: &[StoredMemory],
    root_prefix: &str,
    min_sim: f32,
    min_members: usize,
) -> Result<(String, DeepenPlan)> {
    let root_id = {
        let prefix = root_prefix.get(..root_prefix.len().min(8));
        let mut hits = Vec::new();
        for row in all {
            // Keep v1's exact-ID short circuit. Only the old panicking
            // partial-byte comparison takes the malformed-input safety exit.
            if row.id == root_prefix || row.id.starts_with(prefix
                .ok_or_else(|| anyhow::anyhow!("no such root: {root_prefix}"))?) {
                hits.push(row);
            }
        }
        match hits.len() {
            1 => hits[0].id.clone(),
            0 => anyhow::bail!("no such root: {root_prefix}"),
            _ => anyhow::bail!("prefix is not unique: {root_prefix}"),
        }
    };
    let flatness = crate::deepen_topology::Topology::new(all.iter().map(|row|
        (row.id.as_str(), row.local_parent_id.as_str())))?.check(&root_id)?;
    if !flatness.eligible {
        anyhow::bail!(
            "this root is not flat ({} direct children, {} in the grandchild chain) — hierarchy already exists or too few entries to deepen",
            flatness.direct_count,
            flatness.descendant_count
        );
    }
    // Preserve request order for grouping and equal-candidate ranking.
    let direct: Vec<&StoredMemory> = all.iter().filter(|row| row.local_parent_id == root_id).collect();
    // Centered-domain clustering (union-find)
    let with_emb: Vec<&&StoredMemory> = direct
        .iter()
        .filter(|m| m.local_embedding.as_deref().is_some_and(|v| !v.is_empty()))
        .collect();
    let vecs: Vec<&[f32]> = with_emb
        .iter()
        .filter_map(|m| m.local_embedding.as_deref())
        .collect();
    let mu = respire_core::search::mean_vector(&vecs).unwrap_or_default();
    let n = with_emb.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        if p[x] != x {
            let r = find(p, p[x]);
            p[x] = r;
            r
        } else {
            x
        }
    }
    for i in 0..n.saturating_sub(1) {
        let similarity = respire_core::search::centered_cosine_from(vecs[i], &mu);
        for j in (i + 1)..n {
            let sim = similarity(vecs[j]);
            if sim >= min_sim {
                let a = find(&mut parent, i);
                let b = find(&mut parent, j);
                parent[a] = b;
            }
        }
    }
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        clusters.entry(find(&mut parent, i)).or_default().push(i);
    }
    let mut sub_roots = Vec::new();
    // Membership is only used to filter leftovers in their original order.
    let mut clustered: HashSet<&str> = HashSet::new();
    for (_, members) in clusters {
        if members.len() < min_members {
            continue;
        }
        let mut ids = Vec::new();
        let mut titles = Vec::new();
        for &i in &members {
            let m = with_emb[i];
            ids.push(m.id.clone());
            titles.push(if m.local_title.is_empty() {
                short_id(&m.id)
            } else {
                m.local_title.clone()
            });
            clustered.insert(m.id.as_str());
        }
        let longest = titles
            .iter()
            .max_by_key(|t| t.chars().count())
            .cloned()
            .unwrap_or_default();
        sub_roots.push(DeepenSubRoot {
            title: longest.chars().take(16).collect(),
            member_ids: ids,
            member_titles: titles,
        });
    }
    sub_roots.sort_by(|a, b| b.member_ids.len().cmp(&a.member_ids.len()));
    let leftovers = direct
        .iter()
        .filter(|m| !clustered.contains(m.id.as_str()))
        .map(|m| NodeRef {
            id: m.id.clone(),
            title: if m.local_title.is_empty() {
                short_id(&m.id)
            } else {
                m.local_title.clone()
            },
        })
        .collect();
    Ok((
        root_id,
        DeepenPlan {
            sub_roots,
            leftovers,
        },
    ))
}

pub fn candidate_report<E: respire_core::search::EmbeddingSource, T: std::borrow::Borrow<MemoryEntry>, M: std::borrow::Borrow<StoredMemory>>(
    keys: &HashMap<String, T>,
    embedder: &E,
    all: &[M],
    content: &str,
    settings: &respire_core::query::QuerySettings,
) -> Result<CandidateReport> {
    let q = MemoryQuery::new(content).limit(5);
    let ranked = MemoryEngine::recall_local_scored(keys, embedder, all, &q, settings)?;
    let mut merge = Vec::new();
    let mut parent = Vec::new();
    for (score, entry) in ranked {
        if score >= 0.50 { merge.push(ScoredEntry { score, entry }); }
        else if score >= 0.35 && score < 0.50 { parent.push(ScoredEntry { score, entry }); }
    }
    Ok(CandidateReport {
        merge,
        parent,
    })
}

pub fn float_up_candidates(
    all: &[StoredMemory],
    min_hits: i64,
    good_counts: &HashMap<String, i64>,
) -> Vec<(String, String)> {
    let by_id: HashMap<&str, &StoredMemory> =
        all.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut out = Vec::new();
    for m in all {
        if m.deleted || m.local_importance == "trivial" || m.local_parent_id.is_empty() {
            continue;
        }
        let Some(parent) = by_id.get(m.local_parent_id.as_str()) else {
            continue;
        };
        let grandparent = parent.local_parent_id.clone();
        if grandparent.is_empty() || !by_id.contains_key(grandparent.as_str()) {
            continue;
        }
        // P0-2: float on explicit query-log --good marks only. recall_count is
        // deliberately not consulted — "often recalled" must not decide tree
        // shape, or the popularity loop re-enters through the side door.
        let heat = |id: &str| good_counts.get(id).copied().unwrap_or(0);
        let child_heat = heat(m.id.as_str());
        if child_heat >= min_hits && child_heat > heat(parent.id.as_str()) {
            out.push((m.id.clone(), grandparent));
        }
    }
    out
}

pub fn maintenance_plan(all: &[StoredMemory], max_chars: usize, min_kids: usize, segments: usize) -> Result<serde_json::Value> {
    let rows: Vec<(String, String, String, bool)> = all
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.local_parent_id.clone(),
                m.local_title.clone(),
                m.local_importance == "important",
            )
        })
        .collect();
    let nodes = crate::classify::build_tree_nodes(&rows, 3, 1);
    let (outline, _idx) = crate::classify::render_tree_outline(&nodes);
    let by_id: std::collections::HashMap<&str, &StoredMemory> =
        all.iter().map(|m| (m.id.as_str(), m)).collect();

    // Entries to tidy: every child of a parent with >= min_kids kids (most likely to hold same-topic scatter)
    let mut kids: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for r in &rows {
        if !r.1.is_empty() && by_id.contains_key(r.1.as_str()) {
            kids.entry(r.1.as_str()).or_default().push(r.0.as_str());
        }
    }
    let mut parents: Vec<(&str, usize)> = kids
        .iter()
        .filter(|(_, v)| v.len() >= min_kids)
        .map(|(p, v)| (*p, v.len()))
        .collect();
    parents.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

    let mut detail = String::new();
    // Segment cap: --segments can limit (default 12 - a longer plan nobody reads, and the AI chokes)
    let seg_limit = if segments > 0 { segments } else { 12 };
    for (p, _) in parents.iter().take(seg_limit) {
        let ptitle = by_id
            .get(p)
            .map(|m| m.local_title.clone())
            .unwrap_or_default();
        detail.push_str(&format!(
            "\n### parent [{}] {} ({} children)\n",
            &p[..8.min(p.len())],
            ptitle,
            kids[p].len()
        ));
        for k in &kids[p] {
            if let Some(m) = by_id.get(k) {
                let head: String = m
                    .local_content_head
                    .chars()
                    .take(max_chars.min(300))
                    .collect();
                detail.push_str(&format!(
                    "- [{}] {}\n  {}\n",
                    &k[..8.min(k.len())],
                    m.local_title,
                    head.replace('\n', " ")
                ));
            }
        }
    }

    let prompt = format!(
        r#"# respire memory-tree reparent task

Tidy a **causal memory tree**: parent = cause (upstream / happened first), child = effect (downstream / happened later).

## Three actions (ordered; earlier wins)

1. **Merge what can be merged** (first) - keep the entry count from growing. A person's reusable experience and preferences are finite; there should not be this many entries.
   The same entry is often recorded many times (many iterations of one project, the same decision discussed again, the same pit stepped in twice).
   Those should **become one self-contained combined version** (it may be long, keeping every side's essence; 800 chars is the "per-write cap", not an entry-length cap).
2. **Sort the causal chain** - hang each entry where it belongs: whose cause, whose effect.
3. **Demote trivia** (when you can) - some entries are just **process log / one-shot ops / env params**, with no reuse value:
     time-stamped logs ("XX:XX reported...", "that afternoon...", "continued from...")
     one-shot op records (install a package, change a config, run a command this once)
     old process records whose conclusion already lives in another entry
   Those should **drop to trivial** (leave the primary store, stay on the diary chain) - not a merge, not a reparent.

## Criteria

**Merge** (all three): (1) same event (same object / same goal) (2) overlapping or complementary content (3) the combined entry is self-contained and loses nothing.
**Reparent** (all three): (1) same-event upstream/downstream (the latter is the former's direct reason or prior action) (2) the latter is not later in time (3) without the latter, the former does not stand or cannot be understood.
**Demote** (any one): (1) body is mostly a timed/one-shot process with no "do this next time" conclusion (2) just an env change or parameter record (3) the conclusion already lives in another entry.
**Do not force it**: same topic / same project / same period but each is an independent event - **neither merge nor reparent nor demote**. Leave them.

## Current tree (indent=depth, [n]=node, parentheses=descendant count)

{outline}

## Parents to tidy and their children (with body summaries)

{detail}

## Output format (strict)

```
## merge
- combined title: <<=24 chars>
- merge-ids: [id1], [id2], [id3]
- combined body: <self-contained body, all sides' essence, use {SEGMENT_LABELS}>
- why: <one sentence>

## reparent
- [child-id] > [new-parent-id]  # why: <one sentence>

## demote
- [id]  # why: <one sentence, why it has no reuse value>

## leave
- <which ones look movable but you leave, and why>
```

Output only those four sections. **If a section has no action, write "none"** - fewer is better than forced.
"#,
        outline = outline,
        detail = detail
    );


    Ok(serde_json::json!({"tree_nodes":nodes.len(), "parents":parents.len(), "prompt":prompt}))
}

#[cfg(test)]
mod heat_tests;
#[cfg(test)]
mod deepen_tests;
