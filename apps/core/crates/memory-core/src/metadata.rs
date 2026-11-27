//! Deterministic metadata checks. The host supplies plaintext, time and verified events.

mod validation;
pub use validation::*;

use respire_protocol::metadata::{
    AuthorityCheck, AuthorityOutcome, Evidence, EvidenceEvaluation, EvidenceState, ExtraFields,
    MemoryMeta, MetadataValidation, RequestScope, ReviewClaim, ReviewIntent, RootOutcome, Scope,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Borrowed view of the existing Snapshot.entry, never a second serialized snapshot.
pub struct MetadataRecord<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub content: Option<&'a str>,
    pub metadata: &'a MemoryMeta,
    pub unknown_fields: &'a ExtraFields,
    pub deleted: bool,
    /// Resolved from forward supersedes edges in the authorized snapshot.
    pub superseded: bool,
}

fn invalid(message: &'static str) -> MetadataError {
    MetadataError {
        code: "invalid_metadata",
        message,
    }
}

fn unsupported(message: &'static str) -> MetadataError {
    MetadataError {
        code: "unsupported_metadata",
        message,
    }
}

fn digest(prefix: &str, value: &impl Serialize) -> Result<String, MetadataError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| invalid("canonical JSON could not be encoded"))?;
    let mut hash = Sha256::new();
    hash.update(prefix.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn evidence_parts(evidence: &Evidence) -> (&str, &str, &str) {
    match evidence {
        Evidence::Memory { id, revision, .. } => ("memory", id, revision),
        Evidence::User {
            locator, revision, ..
        } => ("user", locator, revision),
        Evidence::Primary {
            locator, revision, ..
        } => ("primary", locator, revision),
        Evidence::Run {
            locator, revision, ..
        } => ("run", locator, revision),
    }
}

fn validate_body(record: &MetadataRecord<'_>) -> Result<(), MetadataError> {
    validate_extra_fields(record.unknown_fields)?;
    validate_metadata(record.metadata)?;
    match record.kind {
        "context" | "decision" | "preference" | "task" | "emotion" | "time" | "skill"
        | "knowledge" => {}
        "hypothesis" | "reflection" | "self" if record.metadata.memory_contract == 1 => {}
        "hypothesis" | "reflection" | "self" => {
            return Err(invalid("new memory kinds require contract 1"));
        }
        _ => return Err(unsupported("unknown memory kind")),
    }
    Ok(())
}

fn record_revision(record: &MetadataRecord<'_>) -> Result<String, MetadataError> {
    let content = record
        .content
        .ok_or_else(|| invalid("plaintext is required to compute a revision"))?;
    let mut sources: Vec<_> = record
        .metadata
        .source_refs
        .iter()
        .map(|source| {
            (
                source.id.as_str(),
                source.revision.as_str(),
                source.required,
            )
        })
        .collect();
    sources.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let artifact = record.metadata.artifact.as_ref().map(|value| {
        (
            value.path.as_deref(),
            value.repo.as_deref(),
            value.commit.as_deref(),
            value.purpose.as_deref(),
        )
    });
    digest(
        "respire-memory-revision-v1",
        &(
            1,
            record.kind,
            record.title,
            content,
            record.metadata.summary.as_deref(),
            sources,
            record.metadata.valid_until.as_deref(),
            artifact,
        ),
    )
}

fn evidence_set_matches(meta: &MemoryMeta, evidence: &[Evidence]) -> bool {
    let required: BTreeSet<_> = meta
        .source_refs
        .iter()
        .filter(|source| source.required)
        .map(|source| (source.id.as_str(), source.revision.as_str()))
        .collect();
    let provided: BTreeSet<_> = evidence
        .iter()
        .filter_map(|item| match item {
            Evidence::Memory { id, revision, .. } => Some((id.as_str(), revision.as_str())),
            _ => None,
        })
        .collect();
    required == provided
}

fn validate_claim(
    claim: &str,
    scope: &Scope,
    evidence: &[Evidence],
    method: &str,
) -> Result<(), MetadataError> {
    if claim.trim().is_empty() || method.trim().is_empty() || evidence.is_empty() {
        return Err(invalid("review requires claim, method and evidence"));
    }
    validate_scope(scope)?;
    let mut unique = BTreeSet::new();
    for item in evidence {
        validate_evidence(item)?;
        if !unique.insert(evidence_parts(item)) {
            return Err(invalid("duplicate evidence"));
        }
    }
    Ok(())
}

fn claim_digest(
    id: &str,
    revision: &str,
    claim: &str,
    scope: &Scope,
    evidence: &[Evidence],
    method: &str,
) -> Result<String, MetadataError> {
    let mut triples: Vec<_> = evidence.iter().map(evidence_parts).collect();
    triples.sort_unstable();
    digest(
        "respire-review-claim-v1",
        &(
            1,
            id,
            revision,
            claim,
            (
                scope.project.as_deref(),
                scope.device.as_deref(),
                scope.version.as_deref(),
                scope.from.as_deref(),
                scope.until.as_deref(),
            ),
            triples,
            method,
        ),
    )
}

/// Does not read time, storage, a network, or a caller-supplied digest.
pub fn validate(
    record: &MetadataRecord<'_>,
    review: Option<&ReviewClaim>,
    intent: Option<&ReviewIntent>,
) -> Result<MetadataValidation, MetadataError> {
    validate_body(record)?;
    if record
        .metadata
        .source_refs
        .iter()
        .any(|source| source.required && source.id == record.id)
    {
        return Err(invalid(
            "required source cannot reference the record itself",
        ));
    }
    let revision = record_revision(record)?;
    let mut result = MetadataValidation {
        record_revision: revision,
        claim_digest: None,
        intent_digest: None,
    };
    if let Some(review) = review {
        validate_full_id(record.id)?;
        validate_claim(
            &review.claim,
            &review.scope,
            &review.evidence,
            &review.method,
        )?;
        if !evidence_set_matches(record.metadata, &review.evidence) {
            return Err(invalid(
                "memory evidence must equal the required source revisions",
            ));
        }
        result.claim_digest = Some(claim_digest(
            record.id,
            &result.record_revision,
            &review.claim,
            &review.scope,
            &review.evidence,
            &review.method,
        )?);
    }
    if let Some(intent) = intent {
        let claim = result
            .claim_digest
            .as_deref()
            .ok_or_else(|| invalid("intent requires a review claim"))?;
        if intent.owner.trim().is_empty()
            || intent.local_library_id.trim().is_empty()
            || intent.expected_memory_revision.len() != 32
            || !intent
                .expected_memory_revision
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(invalid("invalid review intent binding"));
        }
        if let Some(value) = &intent.next_review_at {
            validate_timestamp(value)?;
        }
        result.intent_digest = Some(digest(
            "respire-review-intent-v1",
            &(
                1,
                &intent.owner,
                &intent.local_library_id,
                &intent.expected_memory_revision,
                claim,
                intent.next_review_at.as_deref(),
            ),
        )?);
    }
    Ok(result)
}

fn record_index<'a>(
    records: &'a [MetadataRecord<'_>],
) -> Result<HashMap<&'a str, usize>, MetadataError> {
    let mut indices = HashMap::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        if record.id.is_empty() || indices.insert(record.id, index).is_some() {
            return Err(invalid("record IDs must be nonempty and unique"));
        }
    }
    Ok(indices)
}

fn dependency_graph(
    records: &[MetadataRecord<'_>],
    indices: &HashMap<&str, usize>,
) -> Vec<Vec<usize>> {
    records
        .iter()
        .map(|record| {
            record
                .metadata
                .source_refs
                .iter()
                .filter(|source| source.required)
                .filter_map(|source| indices.get(source.id.as_str()).copied())
                .collect()
        })
        .collect()
}

/// Iterative Kosaraju traversal marks all cycle members in O(V + E).
fn graph_order(edges: &[Vec<usize>]) -> (Vec<usize>, Vec<bool>) {
    let mut visited = vec![false; edges.len()];
    let mut order = Vec::with_capacity(edges.len());
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut reverse = vec![Vec::new(); edges.len()];
    for (node, targets) in edges.iter().enumerate() {
        for &target in targets {
            reverse[target].push(node);
        }
    }
    for start in 0..edges.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        stack.push((start, 0));
        while let Some(&(node, offset)) = stack.last() {
            if let Some(&target) = edges[node].get(offset) {
                if let Some(frame) = stack.last_mut() {
                    frame.1 += 1;
                }
                if !visited[target] {
                    visited[target] = true;
                    stack.push((target, 0));
                }
            } else {
                stack.pop();
                order.push(node);
            }
        }
    }
    visited.fill(false);
    let mut pending = Vec::new();
    let mut component = Vec::new();
    let mut cycles = vec![false; edges.len()];
    for &start in order.iter().rev() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        pending.push(start);
        component.clear();
        while let Some(node) = pending.pop() {
            component.push(node);
            for &target in &reverse[node] {
                if !visited[target] {
                    visited[target] = true;
                    pending.push(target);
                }
            }
        }
        if component.len() > 1 || edges[start].contains(&start) {
            for &node in &component {
                cycles[node] = true;
            }
        }
    }
    (order, cycles)
}

/// Called on writes with the authorized source view; missing sources remain unverified.
pub fn validate_source_graph(records: &[MetadataRecord<'_>]) -> Result<(), MetadataError> {
    let indices = record_index(records)?;
    for record in records {
        validate_body(record)?;
    }
    let edges = dependency_graph(records, &indices);
    if graph_order(&edges).1.into_iter().any(|cycle| cycle) {
        return Err(invalid("required sources contain a cycle"));
    }
    Ok(())
}

mod evaluation;
pub use evaluation::evaluate;
