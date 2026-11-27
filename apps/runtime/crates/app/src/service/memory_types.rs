// ── data shapes (CLI print and client serde share these) ──

#[derive(Debug, Serialize)]
pub struct StatusInfo {
    pub local_total: usize,
    pub local_alive: usize,
    pub remote_configured: bool,
    pub max_updated_at: Option<String>,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ScoredEntry {
    pub score: f32,
    pub entry: MemoryEntry,
}

#[derive(Debug, Serialize)]
pub struct EntryDetail {
    pub entry: MemoryEntry,
    pub ancestors: Vec<NodeRef>,
    pub children: Vec<ChildRef>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct NodeRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct ChildRef {
    pub id: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub is_leaf: bool,
    /// Full descendant count (not truncated by depth — badge shows true size)
    pub descendants: usize,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct CandidateReport {
    pub merge: Vec<ScoredEntry>,
    pub parent: Vec<ScoredEntry>,
}

/// Outer None preserves a field; Some(None) explicitly clears it.
#[derive(Debug, Clone, Default)]
pub struct MetadataPatch {
    pub summary: Option<Option<String>>,
    pub source_refs: Option<Vec<crate::memory::model::SourceRef>>,
    pub valid_until: Option<Option<String>>,
    pub review_at: Option<Option<String>>,
    pub artifact: Option<Option<crate::memory::model::Artifact>>,
}

impl MetadataPatch {
    pub fn apply(&self, metadata: &mut crate::memory::model::MemoryMeta) {
        metadata.memory_contract = 1;
        if let Some(value) = &self.summary { metadata.summary = value.clone(); }
        if let Some(value) = &self.source_refs { metadata.source_refs = value.clone(); }
        if let Some(value) = &self.valid_until { metadata.valid_until = value.clone(); }
        if let Some(value) = &self.review_at { metadata.review_at = value.clone(); }
        if let Some(value) = &self.artifact { metadata.artifact = value.clone(); }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreateReq {
    pub metadata: crate::memory::model::MemoryMeta,
    pub content: String,
    pub title: String,
    pub tags: String,
    pub kind: String,
    pub project: String,
    pub computer: String,
    pub emotion: Option<f32>,
    pub parent: Option<String>,
    pub merge_ids: Option<Vec<String>>,
    pub merge_metadata: Option<MetadataPatch>,
    pub supersedes: Option<String>,
    pub see_also: Vec<String>,
    pub force: bool,
    /// importance (two-tier): important (main library) | trivial (diary); default trivial (normal retired)
    pub importance: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub pulled: usize,
    pub pushed: usize,
    pub remote_total: usize,
    pub remote_alive: usize,
    pub local_total: usize,
    pub local_alive: usize,
    pub converged: bool,
}

/// Store-candidate report shared by App and CLI, produced by the private Core.
pub fn candidate_report<E: crate::memory::search::Embedder>(
    keys: &crate::memory::SessionKeys,
    embedder: &E,
    all: &[crate::memory::model::StoredMemory],
    content: &str,
) -> Result<CandidateReport> {
    respire_core_sdk::execute(
        "candidate_report",
        serde_json::json!({
            "model":embedder.model_name(), "snapshots":respire_crypto::engine::snapshots(keys, all)?, "content":content,
        }),
    )
}

/// Attach (change parent) — reseal ciphertext: payload.parent_id and the plaintext column change together so the two sources cannot drift.
/// (Hit 2026-09-08: changing only the plaintext column left list/recall reading the old parent from payload; client tree broke, other devices too.)
pub fn reparent(
    keys: &SessionKeys,
    store: &LocalStore,
    id_prefix: &str,
    new_parent: &str,
) -> Result<(String, String)> {
    let all = store.all(true)?;
    let id = resolve_prefix(&all, id_prefix)?;
    let Some(stored) = all.iter().find(|m| m.id == id && !m.deleted) else {
        anyhow::bail!("no such entry: {id_prefix}");
    };
    if id == new_parent {
        anyhow::bail!("cannot attach to self");
    }
    for anc in store.ancestor_chain(new_parent)? {
        if anc == id {
            anyhow::bail!("cycle: new parent is a descendant of this entry");
        }
    }
    crate::metadata_gate::ensure_write(store, &MemoryEngine::open(keys, stored)?)?;
    let stamp = store.edit_stamp(&stored.id)?;
    let out = MemoryEngine::reseal_parent(keys, stored, new_parent, &stamp)?;
    store.put(&out)?;
    Ok((id, new_parent.to_owned()))
}

// ── accounts (no App state; session-level) ──

