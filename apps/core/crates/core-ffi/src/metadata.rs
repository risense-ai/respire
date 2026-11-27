//! JSON adapter for pure metadata algorithms; host authority is request-local.

use anyhow::{ensure, Context, Result};
use respire_core::metadata::{self, MetadataRecord};
use respire_protocol::{
    EvidenceContext, EvidenceEvaluation, ExtraFields, MemoryEntry, RenderBudget, ReviewClaim,
    ReviewIntent,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateRequest {
    entry: MemoryEntry,
    #[serde(default)]
    review: Option<ReviewClaim>,
    #[serde(default)]
    intent: Option<ReviewIntent>,
}

pub(super) fn validate(input: Value) -> Result<Value> {
    let input: ValidateRequest = serde_json::from_value(input)?;
    let entry = &input.entry;
    let record = MetadataRecord {
        id: &entry.id,
        kind: entry.kind.as_str(),
        title: &entry.title,
        content: Some(&entry.content),
        metadata: &entry.metadata,
        unknown_fields: &entry.unknown_fields,
        deleted: false,
        superseded: false,
    };
    Ok(serde_json::to_value(metadata::validate(
        &record,
        input.review.as_ref(),
        input.intent.as_ref(),
    )?)?)
}

fn evaluations(
    view: &crate::resident::View,
    context: &EvidenceContext,
) -> Result<Vec<EvidenceEvaluation>> {
    let allowed: HashSet<_> = view.memories.iter()
        .filter(|memory| !memory.deleted)
        .map(|memory| memory.id.as_str())
        .collect();
    let replacements = crate::related::superseded(view, &allowed);
    let empty = ExtraFields::new();
    let records: Vec<_> = view
        .memories
        .iter()
        .map(|memory| match view.entries.get(&memory.id) {
            Some(entry) => MetadataRecord {
                id: &entry.id,
                kind: entry.kind.as_str(),
                title: &entry.title,
                content: Some(&entry.content),
                metadata: &entry.metadata,
                unknown_fields: &entry.unknown_fields,
                deleted: memory.deleted,
                superseded: replacements.contains_key(&entry.id),
            },
            None => MetadataRecord {
                id: &memory.id,
                kind: &memory.local_kind,
                title: &memory.local_title,
                content: None,
                metadata: &memory.local_meta,
                unknown_fields: &empty,
                deleted: memory.deleted,
                superseded: replacements.contains_key(&memory.id),
            },
        })
        .collect();
    Ok(metadata::evaluate(
        &records,
        &context.request_scope,
        &context.now,
        &context.authority_checks,
        &context.unavailable_ids,
    )?)
}

pub(super) fn evaluate(mut input: Value) -> Result<Value> {
    let object = input
        .as_object()
        .context("metadata evaluation must be an object")?;
    ensure!(
        object.keys().all(|key| matches!(
            key.as_str(),
            "model" | "snapshots" | "resident_key" | "evidence_context"
        )),
        "unknown metadata evaluation field"
    );
    ensure!(
        object.contains_key("snapshots") != object.contains_key("resident_key"),
        "metadata evaluation requires snapshots or resident_key, exclusively"
    );
    let context: EvidenceContext = serde_json::from_value(input["evidence_context"].clone())?;
    let model = input["model"]
        .as_str()
        .context("missing metadata snapshot model")?
        .to_owned();
    let view = crate::resident::view(&mut input, &model)?;
    Ok(serde_json::to_value(evaluations(&view, &context)?)?)
}

pub(super) struct QueryContext {
    pub view: crate::resident::View,
    evaluations: Vec<EvidenceEvaluation>,
    filtered: usize,
}

/// Explicit evidence time also drives ranking; legacy calls retain their old default.
pub(super) fn align_query_time(input: &mut Value) -> Result<()> {
    let Some(context) = input.get("evidence_context") else {
        return Ok(());
    };
    let now = context
        .get("now")
        .and_then(Value::as_str)
        .context("missing evidence context time")?
        .to_owned();
    metadata::validate_timestamp(&now)?;
    let input = input
        .as_object_mut()
        .context("query input must be an object")?;
    let settings = input.entry("query_settings").or_insert_with(|| json!({}));
    let settings = settings
        .as_object_mut()
        .context("query_settings must be an object")?;
    match settings.get("now") {
        Some(value) => ensure!(
            value.as_str() == Some(now.as_str()),
            "query and evidence time must match"
        ),
        None => {
            settings.insert("now".to_owned(), json!(now));
        }
    }
    Ok(())
}

pub(super) fn query_context(
    input: &Value,
    view: &crate::resident::View,
) -> Result<Option<QueryContext>> {
    let Some(value) = input.get("evidence_context") else {
        return Ok(None);
    };
    let context: EvidenceContext = serde_json::from_value(value.clone())?;
    let evaluations = evaluations(view, &context)?;
    let allowed: HashSet<_> = evaluations
        .iter()
        .filter(|item| {
            item.fact_eligible
                || item.reason_codes.iter().all(|reason| {
                    matches!(
                        reason.as_str(),
                        "unverified"
                            | "authority_unavailable"
                            | "verification_revision_mismatch"
                            | "review_due"
                    )
                })
        })
        .map(|item| item.id.as_str())
        .collect();
    let filtered = view.memories.len() - allowed.len();
    let view = crate::resident::filtered(view, &allowed);
    Ok(Some(QueryContext {
        view,
        evaluations,
        filtered,
    }))
}

impl QueryContext {
    /// Labels are inserted before the complete scalar budget is measured.
    pub fn mark_budget(&self, budget: Option<&mut RenderBudget>) {
        let Some(budget) = budget else {
            return;
        };
        let statuses: HashMap<_, _> = self
            .evaluations
            .iter()
            .map(|item| (item.id.as_str(), item.fact_eligible))
            .collect();
        for item in &mut budget.items {
            if let Some(eligible) = statuses.get(item.id.as_str()) {
                let label = if *eligible {
                    "[verified] "
                } else {
                    "[unverified] "
                };
                item.rendered_text.insert_str(0, label);
            }
        }
    }

    /// Borrow the evaluated bodies from this request's filtered immutable view.
    pub fn render_contexts(&self) -> Vec<respire_core::representation::RenderContext<'_>> {
        self.evaluations.iter().filter_map(|evaluation| {
            self.view.entries.get(&evaluation.id).map(|entry| {
                respire_core::representation::RenderContext { entry: entry.as_ref(), evaluation }
            })
        }).collect()
    }

    pub fn attach(&self, result: Value, mode: &str) -> Result<Value> {
        let mut result = if result.is_array() {
            json!({"items": result})
        } else {
            result
        };
        let items = result["items"]
            .as_array()
            .context("metadata query result must contain items")?;
        let mut emitted = HashSet::new();
        for item in items {
            let id = match mode {
                "plain" => &item["id"],
                "scored" => &item[1]["id"],
                _ => &item["entry"]["id"],
            };
            emitted.insert(
                id.as_str()
                    .context("metadata query item is missing its ID")?,
            );
            if let Some(ancestors) = item.get("ancestors").and_then(Value::as_array) {
                for entry in ancestors {
                    emitted.insert(
                        entry["id"]
                            .as_str()
                            .context("metadata ancestor is missing its ID")?,
                    );
                }
            }
        }
        let evidence = self
            .evaluations
            .iter()
            .filter(|item| emitted.contains(item.id.as_str()))
            .collect::<Vec<_>>();
        let evidence = serde_json::to_value(evidence)?;
        result["evidence"] = evidence;
        result["filtered_by_evidence"] = json!(self.filtered);
        Ok(result)
    }
}
