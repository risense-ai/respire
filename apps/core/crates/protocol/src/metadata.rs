//! Encrypted metadata contract. These types do not establish evidence or trust.
use serde::{ser::SerializeMap, Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;

pub type ExtraFields = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryMeta {
    pub memory_contract: u8,
    pub summary: Option<String>,
    pub source_refs: Vec<SourceRef>,
    pub valid_until: Option<String>,
    pub review_at: Option<String>,
    pub artifact: Option<Artifact>,
    pub verification: Option<Verification>,
}

impl Serialize for MemoryMeta {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        // Keep the exact legacy field set; contract 1 includes explicit nulls.
        if self != &Self::default() {
            map.serialize_entry("memory_contract", &self.memory_contract)?;
            map.serialize_entry("summary", &self.summary)?;
            map.serialize_entry("source_refs", &self.source_refs)?;
            map.serialize_entry("valid_until", &self.valid_until)?;
            map.serialize_entry("review_at", &self.review_at)?;
            map.serialize_entry("artifact", &self.artifact)?;
            map.serialize_entry("verification", &self.verification)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub id: String,
    pub revision: String,
    pub required: bool,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: Option<String>,
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub purpose: Option<String>,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub project: Option<String>,
    pub device: Option<String>,
    pub version: Option<String>,
    pub from: Option<String>,
    pub until: Option<String>,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    Memory {
        id: String,
        revision: String,
        #[serde(flatten)]
        unknown_fields: ExtraFields,
    },
    User {
        locator: String,
        revision: String,
        #[serde(flatten)]
        unknown_fields: ExtraFields,
    },
    Primary {
        locator: String,
        revision: String,
        #[serde(flatten)]
        unknown_fields: ExtraFields,
    },
    Run {
        locator: String,
        revision: String,
        #[serde(flatten)]
        unknown_fields: ExtraFields,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmerKind {
    Human,
    TrustedHost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmedBy {
    pub kind: ConfirmerKind,
    pub id: String,
    pub event_id: String,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub record_revision: String,
    pub claim: String,
    pub scope: Scope,
    pub evidence: Vec<Evidence>,
    pub method: String,
    pub confirmed_by: ConfirmedBy,
    pub confirmed_at: String,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestScope {
    pub project: Option<String>,
    pub device: Option<String>,
    pub version: Option<String>,
    pub at: String,
    #[serde(flatten)]
    pub unknown_fields: ExtraFields,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewClaim {
    pub claim: String,
    pub scope: Scope,
    pub evidence: Vec<Evidence>,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewIntent {
    pub owner: String,
    pub local_library_id: String,
    pub expected_memory_revision: String,
    pub next_review_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityOutcome {
    Trusted,
    Unavailable,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootOutcome {
    Available,
    Unavailable,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootCheck {
    pub kind: String,
    pub locator: String,
    pub revision: String,
    pub outcome: RootOutcome,
}

/// Internal host input. Public agent request fields must never be forwarded here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityCheck {
    pub id: String,
    pub record_revision: String,
    pub event_id: String,
    pub claim_digest: String,
    pub confirmed_by: ConfirmedBy,
    pub confirmed_at: String,
    pub outcome: AuthorityOutcome,
    pub roots: Vec<RootCheck>,
}

/// Explicit per-request context. Host proofs must be resolved by the trusted adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceContext {
    pub request_scope: RequestScope,
    pub now: String,
    #[serde(default)]
    pub authority_checks: Vec<AuthorityCheck>,
    #[serde(default)]
    pub unavailable_ids: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Unverified,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataValidation {
    pub record_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEvaluation {
    pub id: String,
    pub record_revision: Option<String>,
    pub evidence_state: EvidenceState,
    pub fact_eligible: bool,
    pub due_reasons: Vec<String>,
    pub reason_codes: Vec<String>,
}
