//! Review candidates and CAS commits. Authority/root inputs never come from CLI JSON.
#[path = "review_recall.rs"]
mod recall;
pub use recall::AuthorizedRecallSource;
use anyhow::{ensure, Context, Result};
use respire_protocol::{Evidence, MemoryEntry, Scope, Verification};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::review_approval::{OpaqueApproval, ReviewAuthority, ReviewBinding};
use crate::transport::{
    local::{LocalStore, ReviewHistoryRow},
    MemoryTransport,
};
use crate::{MemoryEngine, SessionKeys, StoredMemory};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    #[serde(default)]
    pub expected_memory_revision: Option<String>,
    #[serde(default)]
    pub expected_record_revision: Option<String>,
    pub claim: String,
    pub scope: Scope,
    pub evidence: Vec<Evidence>,
    pub method: String,
    // Value distinguishes an explicitly requested null from an omitted field.
    pub next_review_at: Value,
}

impl ReviewInput {
    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            !self.claim.trim().is_empty() && !self.method.trim().is_empty(),
            "invalid_metadata: claim and method are required"
        );
        self.scope.validate()?;
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        if let Some(revision) = &self.expected_record_revision {
            respire_protocol::metadata::validate_revision(revision)?;
        }
        if let Some(revision) = &self.expected_memory_revision {
            ensure!(
                revision.len() == 32
                    && revision
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "invalid_metadata: invalid memory revision"
            );
        }
        if !self.next_review_at.is_null() {
            respire_protocol::metadata::validate_timestamp(
                self.next_review_at
                    .as_str()
                    .context("invalid_metadata: next_review_at must be timestamp or null")?,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Evaluation {
    pub id: String,
    pub record_revision: Option<String>,
    pub evidence_state: String,
    pub fact_eligible: bool,
    pub due_reasons: Vec<String>,
    pub reason_codes: Vec<String>,
}

#[derive(Deserialize)]
struct Validation {
    record_revision: String,
    claim_digest: Option<String>,
    intent_digest: Option<String>,
}

#[derive(Debug)]
pub struct ReviewCommittedError(anyhow::Error);
impl std::fmt::Display for ReviewCommittedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "review write committed, but post-commit verification failed: {}",
            self.0
        )
    }
}
impl std::error::Error for ReviewCommittedError {}

pub fn require_support() -> Result<()> {
    let caps: Value = crate::core_sdk::execute("capabilities", Value::Null)?;
    ensure!(
        caps["memory_metadata_contract"].as_u64() == Some(1),
        "Core SDK does not support memory_metadata_contract 1; install the paired metadata SDK/CLI"
    );
    ensure!(
        caps["superseded_status_contract"].as_u64() == Some(1),
        "Core SDK does not support superseded_status_contract 1"
    );
    Ok(())
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct ReviewService<'a> {
    keys: &'a SessionKeys,
    store: &'a LocalStore,
    owner: &'a str,
}

struct View {
    revision: String,
    rows: Vec<StoredMemory>,
    entries: HashMap<String, MemoryEntry>,
    snapshots: Vec<crate::core_sdk::Snapshot>,
    unavailable: Vec<String>,
}

include!("review/review_service.rs");
