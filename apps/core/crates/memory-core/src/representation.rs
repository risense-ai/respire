//! Opt-in complete representations; callers supply one authorized, evaluated view.

use anyhow::{Context, Result};
use respire_protocol::{EvidenceEvaluation, MemoryEntry, Scope, SourceRef};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefMode {
    Brief,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    Full,
    Summary,
    Title,
}

impl Representation {
    fn priority(self) -> usize {
        match self {
            Self::Full => 0,
            Self::Summary => 1,
            Self::Title => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefRenderItem {
    pub id: String,
    pub record_revision: String,
    pub representation: Representation,
    pub rendered_text: String,
    /// Host assertion that this whole rendering preserves all necessary qualifiers.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefRenderBudget {
    pub mode: BriefMode,
    pub budget_chars: usize,
    pub prefix_by_count: Vec<String>,
    pub separator: String,
    pub suffix: String,
    pub items: Vec<BriefRenderItem>,
}

impl BriefRenderBudget {
    /// Run before retrieval; rank coverage and revision binding need the final view.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.prefix_by_count.is_empty(),
            "prefix_by_count must not be empty"
        );
        for item in &self.items {
            anyhow::ensure!(!item.id.is_empty(), "render item ID must not be empty");
            respire_protocol::metadata::validate_revision(&item.record_revision)?;
        }
        Ok(())
    }
}

/// Both references must come from the same Core-authorized, evidence-filtered view.
/// Evaluations are never accepted as public render request fields.
pub struct RenderContext<'a> {
    pub entry: &'a MemoryEntry,
    pub evaluation: &'a EvidenceEvaluation,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedRenderItem {
    pub id: String,
    pub representation: Representation,
    pub rendered_text: String,
    pub evidence: EvidenceEvaluation,
    pub source_refs: Vec<SourceRef>,
    pub scope: Option<Scope>,
    pub valid_until: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedRepresentation {
    Full,
    Summary,
    Title,
    Mixed,
}

impl From<Representation> for SelectedRepresentation {
    fn from(value: Representation) -> Self {
        match value {
            Representation::Full => Self::Full,
            Representation::Summary => Self::Summary,
            Representation::Title => Self::Title,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefBudgetStatus {
    Selected,
    EmptyCandidates,
    AllDroppedByBudget,
    AllDroppedByIncomplete,
    AllDropped,
    WrapperOverBudget,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefRenderReceipt {
    pub budget_chars: usize,
    pub chars: usize,
    pub tokens: Option<usize>,
    pub estimated: bool,
    pub tokenizer_id: Option<String>,
    pub representation: Option<SelectedRepresentation>,
    pub items: Vec<SelectedRenderItem>,
    pub rendered_text: String,
    pub candidate_count: usize,
    pub selected_count: usize,
    pub dropped_by_budget: usize,
    pub dropped_by_incomplete: usize,
    pub dropped_by_duplicate: usize,
    pub status: BriefBudgetStatus,
}

fn validate_variant(item: &BriefRenderItem, context: &RenderContext<'_>) -> Result<()> {
    let id = &item.id;
    let revision = context.evaluation.record_revision.as_deref()
        .with_context(|| format!("missing evaluated record revision for ID {id}"))?;
    anyhow::ensure!(item.record_revision == revision, "render revision mismatch for ID {id}");
    anyhow::ensure!(item.representation != Representation::Summary || context.entry.metadata.summary.is_some(),
        "summary rendering has no stored summary for ID {id}");
    Ok(())
}

/// Bind available renderings before retrieval, without selecting or requiring a rank pool.
pub fn validate_brief_bindings(
    request: &BriefRenderBudget,
    contexts: &[RenderContext<'_>],
) -> Result<()> {
    request.validate()?;
    let mut authorized = HashMap::new();
    for context in contexts {
        anyhow::ensure!(context.entry.id == context.evaluation.id, "render context ID mismatch");
        anyhow::ensure!(authorized.insert(context.entry.id.as_str(), context).is_none(), "duplicate render context ID");
    }
    let mut seen = HashSet::new();
    for item in &request.items {
        if !seen.insert((item.id.as_str(), item.representation.priority())) { continue; }
        if let Some(context) = authorized.get(item.id.as_str()) {
            validate_variant(item, context)?;
        }
    }
    Ok(())
}

fn restriction_marker(context: &RenderContext<'_>) -> Result<String> {
    let mut marker = if context.evaluation.fact_eligible {
        "[verified"
    } else {
        "[unverified"
    }
    .to_owned();
    if let Some(verification) = &context.entry.metadata.verification {
        marker.push_str(" scope=");
        marker.push_str(&serde_json::to_string(&verification.scope)?);
    }
    if let Some(until) = &context.entry.metadata.valid_until {
        marker.push_str(" valid_until=");
        marker.push_str(until);
    }
    marker.push_str("]\n");
    Ok(marker)
}

/// Select complete representations in existing rank order without truncation.
/// `complete` is a host rendering contract, not a semantic proof by this function.
pub fn select_brief_budget(
    ranked_ids: &[&str],
    request: &BriefRenderBudget,
    contexts: &[RenderContext<'_>],
) -> Result<BriefRenderReceipt> {
    request.validate()?;
    let mut rendered: HashMap<&str, [Option<&BriefRenderItem>; 3]> = HashMap::new();
    let mut dropped_by_duplicate = 0;
    for item in &request.items {
        let variants = rendered.entry(&item.id).or_default();
        let slot = &mut variants[item.representation.priority()];
        if slot.is_some() {
            dropped_by_duplicate += 1;
        } else {
            *slot = Some(item);
        }
    }
    let mut authorized = HashMap::new();
    for context in contexts {
        anyhow::ensure!(
            context.entry.id == context.evaluation.id,
            "render context ID mismatch"
        );
        anyhow::ensure!(
            authorized
                .insert(context.entry.id.as_str(), context)
                .is_none(),
            "duplicate render context ID"
        );
    }
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for id in ranked_ids {
        if !seen.insert(*id) {
            dropped_by_duplicate += 1;
            continue;
        }
        let variants = rendered
            .get(id)
            .with_context(|| format!("missing render item for ranked ID {id}"))?;
        let context = authorized
            .get(id)
            .with_context(|| format!("missing authorized render context for ID {id}"))?;
        for item in variants.iter().flatten() {
            validate_variant(item, context)?;
        }
        candidates.push((variants, *context));
    }
    anyhow::ensure!(
        request.prefix_by_count.len() > candidates.len(),
        "prefix_by_count must cover zero through the ranked candidate count"
    );

    let prefix_chars: Vec<_> = request
        .prefix_by_count
        .iter()
        .map(|prefix| prefix.chars().count())
        .collect();
    let suffix_chars = request.suffix.chars().count();
    let separator_chars = request.separator.chars().count();
    let mut items: Vec<SelectedRenderItem> = Vec::new();
    let mut body_chars = 0usize;
    let mut dropped_by_budget = 0;
    let mut dropped_by_incomplete = 0;
    for (variants, context) in &candidates {
        let marker = restriction_marker(context)?;
        let marker_chars = marker.chars().count();
        let mut complete = false;
        let mut selected = false;
        for item in variants.iter().flatten().filter(|item| item.complete) {
            complete = true;
            let next_body = body_chars
                .checked_add(marker_chars)
                .and_then(|chars| chars.checked_add(item.rendered_text.chars().count()))
                .and_then(|chars| {
                    chars.checked_add(if items.is_empty() { 0 } else { separator_chars })
                });
            let total = next_body
                .and_then(|chars| chars.checked_add(prefix_chars[items.len() + 1]))
                .and_then(|chars| chars.checked_add(suffix_chars));
            if let (Some(body), Some(chars)) = (next_body, total) {
                if chars <= request.budget_chars {
                    body_chars = body;
                    items.push(SelectedRenderItem {
                        id: item.id.clone(),
                        representation: item.representation,
                        rendered_text: format!("{marker}{}", item.rendered_text),
                        evidence: context.evaluation.clone(),
                        source_refs: context.entry.metadata.source_refs.clone(),
                        scope: context
                            .entry
                            .metadata
                            .verification
                            .as_ref()
                            .map(|value| value.scope.clone()),
                        valid_until: context.entry.metadata.valid_until.clone(),
                    });
                    selected = true;
                    break;
                }
            }
        }
        if !selected {
            if complete {
                dropped_by_budget += 1;
            } else {
                dropped_by_incomplete += 1;
            }
        }
    }
    let chars = body_chars
        .checked_add(prefix_chars[items.len()])
        .and_then(|chars| chars.checked_add(suffix_chars));
    let mut status = if !items.is_empty() {
        BriefBudgetStatus::Selected
    } else if candidates.is_empty() {
        BriefBudgetStatus::EmptyCandidates
    } else if dropped_by_budget == candidates.len() {
        BriefBudgetStatus::AllDroppedByBudget
    } else if dropped_by_incomplete == candidates.len() {
        BriefBudgetStatus::AllDroppedByIncomplete
    } else {
        BriefBudgetStatus::AllDropped
    };
    let rendered_text = if chars.is_some_and(|chars| chars <= request.budget_chars) {
        let body = items
            .iter()
            .map(|item| item.rendered_text.as_str())
            .collect::<Vec<_>>()
            .join(&request.separator);
        format!(
            "{}{}{}",
            request.prefix_by_count[items.len()],
            body,
            request.suffix
        )
    } else {
        status = BriefBudgetStatus::WrapperOverBudget;
        String::new()
    };
    let representation = items.first().map(|first| {
        if items
            .iter()
            .all(|item| item.representation == first.representation)
        {
            SelectedRepresentation::from(first.representation)
        } else {
            SelectedRepresentation::Mixed
        }
    });
    let chars = rendered_text.chars().count();
    debug_assert!(chars <= request.budget_chars);
    Ok(BriefRenderReceipt {
        budget_chars: request.budget_chars,
        chars,
        tokens: None,
        estimated: true,
        tokenizer_id: None,
        representation,
        selected_count: items.len(),
        items,
        rendered_text,
        candidate_count: candidates.len(),
        dropped_by_budget,
        dropped_by_incomplete,
        dropped_by_duplicate,
        status,
    })
}
