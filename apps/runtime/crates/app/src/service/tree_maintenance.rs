/// Existing-library analysis (read-only; does not touch the store).
pub fn defrag_report(min: f32, top: usize) -> Result<crate::memory::defrag::Report> {
    let app = App::open()?;
    let list = tree_scope_list(&app.store.all(false)?);
    require_candidates_ready(&app.store, &list)?;
    crate::memory::defrag::analyze(&list, min).map(|mut r| {
        r.clusters.truncate(top);
        r
    })
}

/// Unified tree-view scope (2026-09-20): drop trivia and tombstones, and clear parent when the parent is trivia/deleted.
///
/// Why: inject §3.4 says trivia goes to the diary only; `tree` / `defrag` / `tree-cure`
/// each filtered differently (tree once reported 23 roots, defrag 27). This fn is the shared source:
/// orphans become roots after the drop; important subtrees are unchanged.
///
/// Caller args: `tree` uses `all(true)` (includes tombstones, to look up titles), so this also filters `deleted`;
/// `defrag` uses `all(false)` (already filtered); a second filter is harmless.
pub fn tree_scope_list(
    all: &[crate::memory::model::StoredMemory],
) -> Vec<crate::memory::model::StoredMemory> {
    use std::collections::HashSet;
    let out_of_tree: HashSet<&str> = all
        .iter()
        .filter(|m| m.deleted || m.local_importance == "trivial")
        .map(|m| m.id.as_str())
        .collect();
    let mut list: Vec<crate::memory::model::StoredMemory> = all
        .iter()
        .filter(|m| !m.deleted && m.local_importance != "trivial")
        .cloned()
        .collect();
    for m in list.iter_mut() {
        if !m.local_parent_id.is_empty() && out_of_tree.contains(m.local_parent_id.as_str()) {
            m.local_parent_id.clear();
        }
    }
    list
}

// ── tree health (orphan-leaf roots attach under an outline) ──

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct RootStat {
    pub id: String,
    pub title: String,
    pub descendants: usize,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CureSuggest {
    pub orphan: NodeRef,
    pub target: NodeRef,
    pub target_tree: NodeRef,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct TreeCureReport {
    pub roots: Vec<RootStat>,
    pub lone_roots: usize,
    pub suggests: Vec<CureSuggest>,
}

/// Obtain a tree maintenance proposal from Core; applying it remains public.
/// P0-2: pass explicit query-log --good counts; Core no longer reads recall_count.
pub fn float_up_candidates(
    all: &[crate::memory::model::StoredMemory],
    min_hits: i64,
    good_counts: &HashMap<String, i64>,
) -> Result<Vec<(String, String)>> {
    crate::core_sdk::execute(
        "tree_float",
        serde_json::json!({"snapshots":crate::core_sdk::metadata_snapshots(all), "min_hits":min_hits, "good_counts":good_counts}),
    )
}

#[derive(Debug, Serialize)]
pub struct TreeFloatItem {
    pub id: String,
    pub title: String,
    pub good_marks: i64,
    pub old_parent: String,
    pub old_parent_title: String,
    pub new_parent: String,
    pub new_parent_title: String,
}

#[derive(Debug, Serialize)]
pub struct TreeFloatReport {
    pub items: Vec<TreeFloatItem>,
    pub applied: usize,
}

impl App {
    /// Tree-health report (read-only): root-size bill + orphan-leaf attach suggestions.
    ///
    /// Ask the private Core for attachment suggestions for orphan roots.
    /// Orphan-leaf roots are not matched to each other (same-topic merge is defrag's cluster bill).
    pub fn tree_cure(&self, top: usize) -> Result<TreeCureReport> {
        self.tree_cure_with_min(top, 0.50)
    }

    /// Ask Core for tree adjustments; `go` applies the returned parent changes and syncs.
    /// P0-2: candidates float on explicit query-log --good marks (>= min_hits,
    /// strictly above the parent's marks), never on recall_count.
    pub fn tree_float(&self, go: bool, min_hits: i64) -> Result<TreeFloatReport> {
        let all = self.store.all(false)?;
        require_candidates_ready(&self.store, &all)?;
        let good_counts = self
            .store
            .query_good_counts(chrono::Utc::now().timestamp())?;
        let cands = float_up_candidates(&all, min_hits, &good_counts)?;
        let mut items = Vec::new();
        for (child_id, new_parent) in &cands {
            let Some(m) = all.iter().find(|s| &s.id == child_id) else {
                continue;
            };
            let title_of = |id: &str| -> String {
                all.iter()
                    .find(|s| s.id == id)
                    .map(|s| {
                        if s.local_title.is_empty() {
                            short_id(id)
                        } else {
                            s.local_title.clone()
                        }
                    })
                    .unwrap_or_else(|| short_id(id))
            };
            items.push(TreeFloatItem {
                id: m.id.clone(),
                title: title_of(&m.id),
                good_marks: good_counts.get(&m.id).copied().unwrap_or(0),
                old_parent: m.local_parent_id.clone(),
                old_parent_title: title_of(&m.local_parent_id),
                new_parent: new_parent.clone(),
                new_parent_title: title_of(new_parent),
            });
        }
        if !go {
            return Ok(TreeFloatReport { items, applied: 0 });
        }
        let applied = self.store.write_transaction(|| {
            let current = self.store.all(false)?;
            for (child_id, _) in &cands {
                if let Some(row) = current.iter().find(|row| &row.id == child_id) {
                    crate::metadata_gate::ensure_write(&self.store, &MemoryEngine::open(&self.keys, row)?)?;
                }
            }
            let mut applied = 0;
            for (child_id, new_parent) in &cands {
                if reparent(&self.keys, &self.store, child_id, new_parent).is_ok() {
                    applied += 1;
                }
            }
            Ok(applied)
        })?;
        self.auto_sync();
        Ok(TreeFloatReport { items, applied })
    }

    /// Request tree suggestions with a user-selected similarity floor.
    pub fn tree_cure_with_min(&self, top: usize, min_sim: f32) -> Result<TreeCureReport> {
        let candidates = self.store.all(false)?;
        require_candidates_ready(&self.store, &candidates)?;
        respire_core_sdk::execute(
            "tree_cure",
            serde_json::json!({
                "snapshots":respire_core_sdk::metadata_snapshots(&candidates), "top":top, "min":min_sim,
            }),
        )
    }
}

// ── tree deepen (flat fat root → root / sub-outline / leaf) ──

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct DeepenPlan {
    /// Proposed sub-outlines under the selected root.
    pub sub_roots: Vec<DeepenSubRoot>,
    /// Leaves that did not cluster (stay put, not under a sub-outline)
    pub leftovers: Vec<NodeRef>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct DeepenSubRoot {
    pub title: String,
    pub member_ids: Vec<String>,
    pub member_titles: Vec<String>,
}

/// Apply deepen: write the AI-chosen sub-outline titles. spec.title = sub-outline title (sub_roots order).
/// Per sub-outline: create a child under root → move cluster members under it. Returns (sub-outlines created, members moved).
pub fn deepen_apply<E: crate::memory::search::Embedder>(
    keys: &crate::memory::SessionKeys,
    embedder: &E,
    store: &LocalStore,
    root_id: &str,
    plan: &DeepenPlan,
    titles: &[String],
) -> Result<(usize, usize)> {
    if titles.len() != plan.sub_roots.len() {
        anyhow::bail!(
            "title count {} != sub-root count {} \u{2014} supply them in plan order",
            titles.len(),
            plan.sub_roots.len()
        );
    }
    let (built, moved) = store.write_transaction(|| {
        let all = store.all(true)?;
        let root = all.iter().find(|m| m.id == root_id)
            .ok_or_else(|| anyhow!("root does not exist: {root_id}"))?;
        for mid in plan.sub_roots.iter().flat_map(|sub| &sub.member_ids) {
            if let Ok(full) = resolve_prefix(&all, mid) {
                if let Some(row) = all.iter().find(|row| row.id == full && !row.deleted) {
                    crate::metadata_gate::ensure_write(store, &MemoryEngine::open(keys, row)?)?;
                }
            }
        }
        let user = root.user.clone();
        let stamp = now_stamp();
        let mut built = 0usize;
        let mut moved = 0usize;
        for (sub, title) in plan.sub_roots.iter().zip(titles) {
            let id = uuid::Uuid::new_v4().to_string();
            let node = deepen_node(root, id, title, sub.member_ids.len(), &stamp);
            let stored = MemoryEngine::seal(keys, embedder, &node, &user)?;
            store.put(&stored)?;
            built += 1;
            for mid in &sub.member_ids {
                if reparent(keys, store, mid, &node.id).is_ok() {
                    moved += 1;
                }
            }
        }
        Ok((built, moved))
    })?;
    notify_autosync();
    Ok((built, moved))
}

pub(crate) fn deepen_node(
    root: &crate::StoredMemory,
    id: String,
    title: &str,
    count: usize,
    stamp: &str,
) -> MemoryEntry {
    MemoryEntry {
        metadata: Default::default(),
        unknown_fields: Default::default(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
        id,
        kind: Kind::Context,
        tags: {
            let mut tags = root.local_tags.split(',').map(str::trim).filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned).collect::<Vec<_>>();
            tags.push("\u{5b50}\u{7eb2}".to_owned());
            tags
        },
        title: title.to_owned(),
        content: format!("sub-outline of `{}` \u{2014} absorbs {} same-topic entries (auto-outline from tree-deepen 2026-09-06).", root.local_title, count),
        user: root.user.clone(),
        computer: root.local_computer.clone(),
        device: device_tag(),
        modified_by: device_tag(),
        project: root.local_project.clone(),
        created_at: stamp.to_owned(),
        updated_at: stamp.to_owned(),
        emotion: -1.0,
        parent_id: root.id.clone(),
        importance: "normal".to_owned(),
    }
}

// ── split engine (CLI emits material, AI decides, CLI executes) ──

#[derive(Debug, Serialize)]
pub struct SplitMaterial {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub parent_id: String,
    pub created_at: String,
    pub content: String,
    pub content_len: usize,
    pub ancestors: Vec<NodeRef>,
}

/// Split material: original full text + ancestor chain + owning tree — the AI reads this and writes the split spec.
pub fn split_material(entry: &MemoryEntry, ancestors: &[NodeRef]) -> SplitMaterial {
    SplitMaterial {
        id: entry.id.clone(),
        title: entry.title.clone(),
        kind: entry.kind.as_str().to_owned(),
        tags: entry.tags.clone(),
        parent_id: entry.parent_id.clone(),
        created_at: entry.created_at.clone(),
        content_len: entry.content.chars().count(),
        content: entry.content.clone(),
        ancestors: ancestors.to_vec(),
    }
}

#[derive(Debug, Deserialize)]
pub struct SplitOp {
    /// Child title (≤20 chars, one line on the tree)
    pub title: String,
    /// Child body (self-contained: cause + action + effect; readable three months later alone)
    pub content: String,
    /// One of the seven kinds
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Attach under this outline or an explicit parent ID/prefix.
    #[serde(default = "default_split_parent")]
    pub parent: String,
}

fn default_split_parent() -> String {
    "\u{7eb2}".to_owned()
}

#[derive(Debug, Deserialize)]
pub struct SplitSpec {
    /// Optionally convert the original entry to an outline with a summary.
    #[serde(default)]
    pub summary: Option<String>,
    pub items: Vec<SplitOp>,
}

/// Execute the AI split spec: resolve each parent and seal the children,
/// if the original has a summary it becomes an outline (body replaced, resealed). Returns children actually split.
pub fn split_exec<E: crate::memory::search::Embedder>(
    keys: &crate::memory::SessionKeys,
    embedder: &E,
    store: &LocalStore,
    entry: &MemoryEntry,
    spec: &SplitSpec,
) -> Result<usize> {
    let count = split_exec_with(store, entry, spec, |entry, owner| {
        let stored = MemoryEngine::seal(keys, embedder, entry, owner)?;
        store.put(&stored)?;
        Ok(())
    })?;
    notify_autosync();
    Ok(count)
}

pub(crate) fn split_exec_with(
    store: &LocalStore,
    entry: &MemoryEntry,
    spec: &SplitSpec,
    mut write: impl FnMut(&MemoryEntry, &str) -> Result<()>,
) -> Result<usize> {
    if spec.items.is_empty() {
        return Err(crate::input_error::InputError("split spec has no children".into()).into());
    }
    if spec.summary.is_none() && spec.items.len() == 1 {
        return Err(crate::input_error::InputError(
            "a single child without keeping the outline is a rewrite \u{2014} edit instead of split".into()
        ).into());
    }
    crate::metadata_gate::ensure_write(store, entry)?;
    let user = entry.user.clone();
    let catalog = if spec.items.iter().any(|op| !op.parent.is_empty() && op.parent != "\u{7eb2}") {
        store.all(true)?
    } else {
        Vec::new()
    };
    for op in &spec.items {
        let parent_id = if op.parent.is_empty() || op.parent == "\u{7eb2}" {
            entry.id.clone()
        } else {
            resolve_prefix(&catalog, &op.parent)?
        };
        let stamp = now_stamp();
        let child = MemoryEntry {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id: uuid::Uuid::new_v4().to_string(),
            kind: Kind::try_from_str(&op.kind.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?,
            tags: {
                let mut t = op.tags.clone();
                t.push("\u{62c6}\u{5206}".to_owned());
                t
            },
            title: op.title.clone(),
            content: op.content.clone(),
            user: user.clone(),
            computer: entry.computer.clone(),
            device: device_tag(),
            modified_by: device_tag(),
            project: entry.project.clone(),
            created_at: stamp.clone(),
            updated_at: stamp,
            emotion: entry.emotion,
            parent_id,
            importance: entry.importance.clone(),
        };
        write(&child, &user)?;
    }
    if let Some(summary) = &spec.summary {
        let mut e = entry.clone();
        e.content = summary.clone();
        e.updated_at = store.edit_stamp(&entry.id)?;
        write(&e, &user)?;
    }
    Ok(spec.items.len())
}

