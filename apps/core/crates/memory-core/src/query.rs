//! Private ranking and context selection over authorized snapshots.
use crate::engine::{build_tree_meta, validate_embedding};
use crate::model::{MemoryEntry, MemoryQuery, StoredMemory};
use crate::search::rerank_semantic_with_settings;
use anyhow::{Context, Result};
use std::borrow::Borrow;
pub struct MemoryEngine;

/// Keep complete titles in Core rank order, counting every emitted Unicode scalar.
pub fn select_render_budget(
    ranked_ids: &[&str],
    request: &respire_protocol::RenderBudget,
) -> Result<respire_protocol::RenderBudgetReceipt> {
    use respire_protocol::{RecallRepresentation, RenderBudgetReceipt, RenderBudgetStatus};
    use std::collections::{HashMap, HashSet};

    let mut rendered = HashMap::new();
    let mut dropped_by_duplicate = 0;
    for item in &request.items {
        anyhow::ensure!(!item.id.is_empty(), "render item ID must not be empty");
        if rendered.contains_key(item.id.as_str()) {
            dropped_by_duplicate += 1;
        } else {
            rendered.insert(item.id.as_str(), item);
        }
    }
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for id in ranked_ids {
        if !seen.insert(*id) {
            dropped_by_duplicate += 1;
            continue;
        }
        candidates.push(*rendered.get(id)
            .with_context(|| format!("missing render item for ranked ID {id}"))?);
    }
    anyhow::ensure!(
        request.prefix_by_count.len() > candidates.len(),
        "prefix_by_count must cover zero through the ranked candidate count"
    );

    let prefix_chars: Vec<usize> = request.prefix_by_count.iter()
        .map(|prefix| prefix.chars().count()).collect();
    let suffix_chars = request.suffix.chars().count();
    let separator_chars = request.separator.chars().count();
    let mut items = Vec::new();
    let mut body_chars = 0usize;
    let mut dropped_by_budget = 0;
    for item in &candidates {
        let next_body = body_chars.checked_add(item.rendered_text.chars().count())
            .and_then(|chars| chars.checked_add(if items.is_empty() { 0 } else { separator_chars }));
        let total = next_body
            .and_then(|chars| chars.checked_add(prefix_chars[items.len() + 1]))
            .and_then(|chars| chars.checked_add(suffix_chars));
        if let (Some(body), Some(chars)) = (next_body, total) {
            if chars <= request.budget_chars {
                body_chars = body;
                items.push((*item).clone());
                continue;
            }
        }
        dropped_by_budget += 1;
    }

    let chars = body_chars.checked_add(prefix_chars[items.len()])
        .and_then(|chars| chars.checked_add(suffix_chars));
    let mut status = if !items.is_empty() {
        RenderBudgetStatus::Selected
    } else if candidates.is_empty() {
        RenderBudgetStatus::EmptyCandidates
    } else {
        RenderBudgetStatus::AllDroppedByBudget
    };
    let rendered_text = if chars.is_some_and(|chars| chars <= request.budget_chars) {
        let body = items.iter().map(|item| item.rendered_text.as_str())
            .collect::<Vec<_>>().join(&request.separator);
        format!("{}{}{}", request.prefix_by_count[items.len()], body, request.suffix)
    } else {
        status = RenderBudgetStatus::WrapperOverBudget;
        String::new()
    };
    let chars = rendered_text.chars().count();
    debug_assert!(chars <= request.budget_chars);
    Ok(RenderBudgetReceipt {
        budget_chars: request.budget_chars,
        chars,
        tokens: None,
        estimated: true,
        tokenizer_id: None,
        representation: RecallRepresentation::Title,
        selected_count: items.len(),
        items,
        rendered_text,
        candidate_count: candidates.len(),
        dropped_by_budget,
        dropped_by_duplicate,
        status,
    })
}

/// The legacy CLI detector only widens recall with a skill-only pass.
pub fn howto_intent(query: &str) -> bool {
    let lower = query.to_lowercase();
    crate::i18n::query_terms::HOW_TO_TERMS.iter().any(|needle| lower.contains(needle))
}

/// Host-supplied local run counts; missing IDs remain unrun.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureRunScore {
    pub ok: i64,
    pub fail: i64,
}

impl ProcedureRunScore {
    fn delta(&self) -> f32 {
        // Widen host counts so the legacy adjustment cannot overflow integer arithmetic.
        let total = i128::from(self.ok) + i128::from(self.fail);
        if total <= 0 {
            return 0.0;
        }
        0.03 * (i128::from(self.ok) - i128::from(self.fail)) as f32 / total as f32
    }
}

/// Preserve the CLI's union, run adjustment and top-three procedure promotion.
pub fn merge_howto(
    primary: Vec<RecalledWithContext>,
    secondary: Vec<RecalledWithContext>,
    limit: usize,
    runs: &std::collections::HashMap<String, ProcedureRunScore>,
) -> (Vec<RecalledWithContext>, bool) {
    let mut merged = primary;
    for row in secondary {
        if !merged.iter().any(|item| item.entry.id == row.entry.id) {
            merged.push(row);
        }
    }
    let is_procedure = |item: &RecalledWithContext| {
        matches!(item.entry.kind.as_str(), "skill" | "knowledge")
    };
    let procedure_rank = |item: &RecalledWithContext| {
        if !is_procedure(item) {
            return item.score;
        }
        item.score + runs.get(&item.entry.id).map(ProcedureRunScore::delta).unwrap_or_default()
    };
    merged.sort_by(|a, b| {
        procedure_rank(b).partial_cmp(&procedure_rank(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    if merged.is_empty() {
        return (merged, false);
    }
    let best = merged.iter().enumerate()
        .filter(|(_, item)| is_procedure(item))
        .max_by(|(_, a), (_, b)| {
            procedure_rank(a).partial_cmp(&procedure_rank(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index);
    let mut promoted = false;
    if let Some(index) = best {
        if index >= 3 && merged[..3].iter().all(|item| !is_procedure(item)) {
            let item = merged.remove(index);
            merged.insert(2, item);
            promoted = true;
        }
    }
    merged.truncate(limit);
    (merged, promoted)
}

/// Immutable feedback for this authorized request. Core never loads a store or
/// reads environment/global counters. Disabled settings retain the legacy path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedbackSettings {
    pub enabled: bool,
    pub good: std::collections::BTreeMap<String,u64>,
    pub bad: std::collections::BTreeMap<String,u64>,
    pub bad_penalty: bool,
}
impl Default for FeedbackSettings {
    fn default()->Self {Self{enabled:false,good:Default::default(),bad:Default::default(),bad_penalty:true}}
}

mod query_now {
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(
        value: &Option<chrono::DateTime<chrono::Utc>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        value.map(|now| now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<chrono::DateTime<chrono::Utc>>, D::Error> {
        let value = String::deserialize(deserializer)?;
        crate::metadata::validate_timestamp(&value).map_err(serde::de::Error::custom)?;
        chrono::DateTime::parse_from_rfc3339(&value)
            .map(|now| Some(now.with_timezone(&chrono::Utc)))
            .map_err(serde::de::Error::custom)
    }
}

/// Host-supplied controls; Core never reads host files or environment variables.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct QuerySettings {
    pub feedback: Option<FeedbackSettings>,
    /// Host UTC instant; omission retains legacy system-clock compatibility.
    #[serde(default, with = "query_now", skip_serializing_if = "Option::is_none")]
    pub now: Option<chrono::DateTime<chrono::Utc>>,
    pub read_heat: bool,
    pub route_on: bool,
    pub ablate: String,
    pub recall_min_score: f32,
    pub mmr_lambda: f32,
    pub ancestor_budget: usize,
    pub ancestor_root_floor: usize,
    pub cos_min: Option<f32>,
    pub sim_min: f32,
    pub cos_relative_ratio: Option<f32>,
    pub cos_margin: Option<f32>,
    pub lexical_weight: f32,
    pub semantic_power: f32,
    pub bm25: bool,
    pub max_fusion: bool,
    pub normalize_ablation: bool,
    pub aux_cap_ratio: Option<f32>,
    pub rrf_cap: f32,
    /// Fusion mode for the five axes: `legacy` keeps the weighted-sum score;
    /// `attention` re-ranks admitted candidates with per-axis softmax heads and
    /// independent gates, keeping semantic relevance as the residual anchor.
    pub fusion: String,
    /// Share of the attention mixture given to auxiliary heads (0 = semantic only).
    pub fusion_alpha: f32,
    /// Share of the attention mixture given to the lexical head.
    pub fusion_beta: f32,
    /// Softmax temperature applied after per-head standardization.
    pub fusion_temperature: f32,
    /// Independent prior gates for the attention mixture.
    pub fusion_gate_recency: f32,
    pub fusion_gate_recency_temporal: f32,
    pub fusion_gate_hot: f32,
    pub fusion_gate_tree: f32,
    /// Additional prior share for temporal questions.
    pub fusion_temporal_alpha: f32,
    pub heat_cap: u32,
    pub debug_score: bool,
    #[serde(skip)]
    pub resolved_cos_min: std::cell::Cell<Option<f32>>,
    #[serde(skip)]
    pub lexical_scores: std::cell::RefCell<Option<std::collections::HashMap<String, f32>>>,
    #[serde(skip)]
    pub lexical_indexes: std::cell::RefCell<Option<std::sync::Arc<crate::rank::LexicalIndexCache>>>,
    #[serde(skip)]
    pub diagnostics: std::cell::RefCell<RecallDiagnostics>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RecallDiagnostics {
    pub cos_gate: f32,
    pub sim_min: f32,
    pub best_cos: Option<f32>,
    pub best_pre_floor: Option<f32>,
    pub examined: usize,
    pub dropped_by_cos: usize,
    pub dropped_by_floor: usize,
    pub pending_returned: usize,
    pub tiers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<serde_json::Value>,
}

impl QuerySettings {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("cos_min", self.cos_min), ("cos_relative_ratio", self.cos_relative_ratio),
            ("cos_margin", self.cos_margin), ("lexical_weight", Some(self.lexical_weight)),
            ("rrf_cap", Some(self.rrf_cap)), ("aux_cap_ratio", self.aux_cap_ratio),
            ("sim_min", Some(self.sim_min)), ("recall_min_score", Some(self.recall_min_score)),
            ("fusion_alpha", Some(self.fusion_alpha)),
            ("fusion_beta", Some(self.fusion_beta)),
            ("fusion_gate_recency", Some(self.fusion_gate_recency)),
            ("fusion_gate_recency_temporal", Some(self.fusion_gate_recency_temporal)),
            ("fusion_gate_hot", Some(self.fusion_gate_hot)),
            ("fusion_gate_tree", Some(self.fusion_gate_tree)),
            ("fusion_temporal_alpha", Some(self.fusion_temporal_alpha)),
        ] {
            anyhow::ensure!(value.is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v)), "invalid query setting {name}");
        }
        anyhow::ensure!(matches!(self.fusion.as_str(), "legacy" | "attention" | "attention_anchor" | "auto"), "invalid query setting fusion");
        anyhow::ensure!(self.fusion_alpha + self.fusion_beta <= 1.0, "fusion_alpha + fusion_beta must not exceed 1");
        anyhow::ensure!(self.fusion_temperature.is_finite() && self.fusion_temperature > 0.0 && self.fusion_temperature <= 10.0, "invalid query setting fusion_temperature");
        anyhow::ensure!(self.semantic_power.is_finite() && self.semantic_power > 0.0 && self.semantic_power <= 4.0, "invalid query setting semantic_power");
        anyhow::ensure!(self.heat_cap > 0 && self.heat_cap <= 15, "invalid query setting heat_cap");
        Ok(())
    }

    fn score_lexical(&self, entries: &[&StoredMemory], query: &str) -> Result<Vec<f32>> {
        match self.lexical_indexes.borrow().as_ref() {
            Some(cache) => cache.scores(entries, query),
            None => Ok(crate::rank::LexicalIndex::build(entries).scores(query)),
        }
    }
}
impl Default for QuerySettings {
    fn default() -> Self {
        Self {
            feedback: None,
            now: None,
            // P0-2 read-heat off by default: with feedback=None a recall_count
            // heat axis would reproduce "often recalled -> easier to recall".
            // Hosts opt in explicitly via the read_heat query setting.
            read_heat: false,
            route_on: true,
            ablate: String::new(),
            recall_min_score: 0.10,
            cos_min: None,
            sim_min: 0.10,
            cos_relative_ratio: None,
            cos_margin: None,
            lexical_weight: 0.30,
            semantic_power: 1.3,
            bm25: true,
            max_fusion: false,
            normalize_ablation: true,
            aux_cap_ratio: Some(0.85),
            rrf_cap: 0.0,
            fusion: "legacy".into(),
            fusion_alpha: 0.20,
            fusion_beta: 0.25,
            fusion_temperature: 1.0,
            fusion_gate_recency: 0.02,
            fusion_gate_recency_temporal: 1.0,
            fusion_gate_hot: 0.10,
            fusion_gate_tree: 0.02,
            fusion_temporal_alpha: 0.15,
            heat_cap: 8,
            debug_score: false,
            resolved_cos_min: Default::default(),
            lexical_scores: Default::default(),
            lexical_indexes: Default::default(),
            diagnostics: Default::default(),
            mmr_lambda: MMR_LAMBDA,
            ancestor_budget: DEFAULT_ANCESTOR_BUDGET,
            ancestor_root_floor: DEFAULT_ANCESTOR_ROOT_FLOOR,
        }
    }
}
/// MMR dedup strength (default 0.7): 1.0 = pure relevance (dedup off), 0.0 = pure diversity.
/// Supplied by the host through [`QuerySettings::mmr_lambda`].
pub const MMR_LAMBDA: f32 = 0.7;

/// Ancestor-chain body budget (chars, default 800): cap on ancestor text carried with one hit.
/// Supplied by the host (0 = off, i.e. hit entries only).
///
/// Why 800: at recall --limit 3 the worst extra is ~2400 chars, still tractable;
/// larger and tokens drown the hit body (the fragment is primary, ancestor context is secondary).
pub const DEFAULT_ANCESTOR_BUDGET: usize = 800;

/// Far-ancestor floor (chars, default 200): when splitting the budget, first reserve a floor for root→far ancestors,
/// leftover then fills near→far. Supplied by the host (0 = old near-first).
///
/// Why this change (measured 2026-09-20): old near-first ate the budget layer by layer; a length-7 chain at default 800
/// only carried 3 layers, dropping the L1 outline and root-cause body — opposite of small-to-big (hit a small leaf, take
/// **big** context upward). Mean chain length in the library is 4.83, 79% >2, so eight in ten hits missed the upstream cause.
/// With a floor, far ancestors get a seat; near ancestors (more specific, and the parent already has an L1 overview) yield.
pub const DEFAULT_ANCESTOR_ROOT_FLOOR: usize = 200;

/// Default depth for causal-chain walk (`rsrs chain` and recall descendant hints).
pub const DEFAULT_CHAIN_DEPTH: usize = 3;

/// Cap on the direct parent in the ancestor budget (chars, 140): its body already has an L1 section via content_head
/// (`print_causal` takes the first 140 chars), so extra ancestor budget would just duplicate.
/// Keep in sync with main.rs L1 truncation — change one, change both.
pub const ANCESTOR_PARENT_CAP: usize = 140;

/// Ancestor-budget split (pure, unit-testable): per-layer char cap, same order as `lens` (**root first**).
///
/// Strategy (changed 2026-09-20; was pure near-first):
/// - chain length ≤2 or `floor == 0`: old method — near-first, parent takes leftover, then outward.
/// - else three stages: (1) **far ancestors** (from the root, excluding the last two layers) each get floor `per = max(floor/m, 60)`,
///   total at most 60% of the budget; (2) leftover to the next-near ancestor (n-2); (3) parent capped at [`ANCESTOR_PARENT_CAP`];
///   (4) leftover reflows near→far onto unsaturated layers.
///
/// Why (measured 2026-09-20): under the old method a length-7 chain at default 800 only carried 3 layers,
/// dropping the L1 outline and root-cause body — opposite of small-to-big (hit a small leaf, take **big** context up).
pub fn ancestor_budget_split(lens: &[usize], budget: usize, floor: usize) -> Vec<usize> {
    let n = lens.len();
    let mut caps = vec![0usize; n];
    if n == 0 || budget == 0 {
        return caps;
    }
    if n <= 2 || floor == 0 {
        // Old method: near-first
        let mut used = 0usize;
        for i in (0..n).rev() {
            let c = budget.saturating_sub(used).min(lens[i]);
            caps[i] = c;
            used += c;
        }
        return caps;
    }
    let m = n - 2; // far-ancestor layer count
    let per = (floor / m).max(60);
    let far_cap = (per * m).min(budget * 3 / 5);
    let mut used = 0usize;
    for i in 0..m {
        let c = far_cap.saturating_sub(used).min(per).min(lens[i]);
        caps[i] = c;
        used += c;
    }
    // Direct parent: reserve the cap first (its body already has an L1 view; extra budget would duplicate)
    let parent_cap = ANCESTOR_PARENT_CAP.min(lens[n - 1]);
    let rest = budget.saturating_sub(used);
    let after_parent = rest.saturating_sub(parent_cap);
    caps[n - 2] = after_parent.min(lens[n - 2]);
    caps[n - 1] = rest.saturating_sub(caps[n - 2]).min(parent_cap);
    // Leftover reflow: near→far onto unsaturated layers (parent does not join — already capped)
    let mut left = budget.saturating_sub(caps.iter().sum::<usize>());
    for i in (0..n - 1).rev() {
        if left == 0 {
            break;
        }
        let room = lens[i].saturating_sub(caps[i]);
        let add = room.min(left);
        caps[i] += add;
        left -= add;
    }
    caps
}

/// Hit entry + its ancestor chain (small-to-big).
///
/// `entry` is the main hit (full text); `ancestors` is root-to-parent causal order (bodies truncated to budget).
/// Purpose: the fragment in hand already has context, so the AI need not `show` several more times.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecalledWithContext {
    pub score: f32,
    pub entry: MemoryEntry,
    pub ancestors: Vec<MemoryEntry>,
}

mod recall;
