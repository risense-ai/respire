use serde::{Serialize, Deserialize};
use respire_protocol::MemoryEntry;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredEntry {
    pub score: f32,
    pub entry: MemoryEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateReport {
    pub merge: Vec<ScoredEntry>,
    pub parent: Vec<ScoredEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootStat {
    pub id: String,
    pub title: String,
    pub descendants: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CureSuggest {
    pub orphan: NodeRef,
    pub target: NodeRef,
    pub target_tree: NodeRef,
    #[serde(skip_serializing)]
    pub sim: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeCureReport {
    pub roots: Vec<RootStat>,
    pub lone_roots: usize,
    pub suggests: Vec<CureSuggest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepenPlan {
    /// New sub-outline (to hang under root): title = truncated longest member title in the cluster
    pub sub_roots: Vec<DeepenSubRoot>,
    /// Leaves that did not cluster (stay put, not under a sub-outline)
    pub leftovers: Vec<NodeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepenSubRoot {
    pub title: String,
    pub member_ids: Vec<String>,
    pub member_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub title: String,
    pub date: String,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cluster {
    /// Members (created_at early→late)
    pub members: Vec<Member>,
    /// Max similarity in the cluster (centered domain)
    pub max_sim: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
