//! Core ABI wire values and vocabulary (pure data, no I/O).
//! The canonical JSON contract is in `contracts/core-abi`. Storage, encryption
//! and in-memory algorithm candidates are not shared wire types.

use serde::{Deserialize, Serialize};

mod i18n;
pub mod metadata;
pub use metadata::*;

pub const VERSION_TAG: &str = env!("CARGO_PKG_VERSION");

/// Recall routing receipt; no intermediate ranking features are exposed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallRouting {
    pub temporal: bool,
    pub route_on: bool,
    pub ablate: String,
    pub read_heat: bool,
    pub heat_source: String,
    pub heat_disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallRepresentation {
    Title,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderItem {
    pub id: String,
    pub representation: RecallRepresentation,
    pub rendered_text: String,
}

/// Complete host rendering inputs; Core owns candidate selection and counting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderBudget {
    pub budget_chars: usize,
    pub prefix_by_count: Vec<String>,
    pub separator: String,
    pub suffix: String,
    pub items: Vec<RenderItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBudgetStatus {
    Selected,
    EmptyCandidates,
    AllDroppedByBudget,
    WrapperOverBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderBudgetReceipt {
    pub budget_chars: usize,
    pub chars: usize,
    pub tokens: Option<usize>,
    pub estimated: bool,
    pub tokenizer_id: Option<String>,
    pub representation: RecallRepresentation,
    pub items: Vec<RenderItem>,
    pub rendered_text: String,
    pub candidate_count: usize,
    pub selected_count: usize,
    pub dropped_by_budget: usize,
    pub dropped_by_duplicate: usize,
    pub status: RenderBudgetStatus,
}

fn de_see_also<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HistoricalLinks {
        Text(String),
        Links(Vec<String>),
    }
    match HistoricalLinks::deserialize(deserializer)? {
        HistoricalLinks::Text(text) => Ok(text.split(',').map(str::trim)
            .filter(|id| !id.is_empty()).map(str::to_owned).collect()),
        HistoricalLinks::Links(links) => Ok(links),
    }
}

/// Memory kind (seven classes + knowledge; software-agnostic semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Context,
    Decision,
    Preference,
    Task,
    Emotion,
    Time,
    Skill,
    Knowledge,
    Hypothesis,
    Reflection,
    #[serde(rename = "Self")]
    SelfNote,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Context => "context",
            Kind::Decision => "decision",
            Kind::Preference => "preference",
            Kind::Task => "task",
            Kind::Emotion => "emotion",
            Kind::Time => "time",
            Kind::Skill => "skill",
            Kind::Knowledge => "knowledge",
            Kind::Hypothesis => "hypothesis",
            Kind::Reflection => "reflection",
            Kind::SelfNote => "self",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "decision" => Kind::Decision,
            "preference" => Kind::Preference,
            "task" => Kind::Task,
            "emotion" => Kind::Emotion,
            "time" => Kind::Time,
            "skill" => Kind::Skill,
            "knowledge" => Kind::Knowledge,
            "hypothesis" => Kind::Hypothesis,
            "reflection" => Kind::Reflection,
            "self" => Kind::SelfNote,
            _ => Kind::Context,
        }
    }

    /// Strict parsing for metadata-aware payloads; legacy callers retain from_str.
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "context" => Some(Self::Context), "decision" => Some(Self::Decision),
            "preference" => Some(Self::Preference), "task" => Some(Self::Task),
            "emotion" => Some(Self::Emotion), "time" => Some(Self::Time),
            "skill" => Some(Self::Skill), "knowledge" => Some(Self::Knowledge),
            "hypothesis" => Some(Self::Hypothesis), "reflection" => Some(Self::Reflection),
            "self" => Some(Self::SelfNote), _ => None,
        }
    }

    /// Chinese kind label (display).
    pub fn display_zh(&self) -> &'static str {
        match self {
            Kind::Context => i18n::zh::CONTEXT,
            Kind::Decision => i18n::zh::DECISION,
            Kind::Preference => i18n::zh::PREFERENCE,
            Kind::Task => i18n::zh::TASK,
            Kind::Emotion => i18n::zh::EMOTION,
            Kind::Time => i18n::zh::TIME,
            Kind::Skill => i18n::zh::SKILL,
            Kind::Knowledge => i18n::zh::KNOWLEDGE,
            Kind::Hypothesis => i18n::zh::HYPOTHESIS,
            Kind::Reflection => i18n::zh::REFLECTION,
            Kind::SelfNote => i18n::zh::SELF_NOTE,
        }
    }
}

/// Plaintext form of one memory (client memory/display only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    #[serde(default, flatten)]
    pub metadata: MemoryMeta,
    #[serde(default, flatten)]
    pub unknown_fields: ExtraFields,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub supersedes: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub superseded_by: String,
    #[serde(default, deserialize_with = "de_see_also", skip_serializing_if = "Vec::is_empty")]
    pub see_also: Vec<String>,
    pub id: String,
    pub kind: Kind,
    pub tags: Vec<String>,
    pub title: String,
    pub content: String,
    pub user: String,
    pub computer: String,
    pub project: String,
    pub created_at: String,
    pub updated_at: String,
    /// Emotion intensity 0.0~1.0; -1 means unlabeled.
    pub emotion: f32,
    /// Forward-star parent id (empty = root; tree is restored from the ciphertext payload across devices).
    pub parent_id: String,
    /// Importance (two-tier): important (main library) | trivial (diary); normal is retired (read for compatibility).
    #[serde(default = "default_importance")]
    pub importance: String,
    /// Storage device (host/platform, e.g. fslong-hasee/linux): first-write device. Empty on old data.
    #[serde(default)]
    pub device: String,
    /// Last-modified device: refreshed on update/append/merge (together with updated_at: which device changed it when). Empty on old data.
    #[serde(default)]
    pub modified_by: String,
}

fn default_importance() -> String {
    "normal".to_owned()
}

/// Query (local search; text is lexical prefilter + the engine embeds a semantic vector).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub text: String,
    pub kind: Option<Kind>,
    pub limit: usize,
    pub project: Option<String>,
    pub computer: Option<String>,
}

impl MemoryQuery {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            kind: None,
            limit: 3,
            project: None,
            computer: None,
        }
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn of_kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn of_project(mut self, project: &str) -> Self {
        self.project = Some(project.to_owned());
        self
    }

    pub fn of_computer(mut self, computer: &str) -> Self {
        self.computer = Some(computer.to_owned());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        assert_eq!(Kind::from_str("decision"), Kind::Decision);
        assert_eq!(Kind::from_str("SKILL"), Kind::Skill);
        assert_eq!(Kind::from_str("knowledge"), Kind::Knowledge);
        assert_eq!(Kind::from_str("whatever"), Kind::Context);
        assert_eq!(Kind::Decision.as_str(), "decision");
        assert_eq!(Kind::Knowledge.display_zh(), i18n::zh::EXPECTED_KNOWLEDGE_LABEL);
    }

    #[test]
    fn query_builder() {
        let q = MemoryQuery::new("rust").limit(5).of_kind(Kind::Task);
        assert_eq!(q.limit, 5);
        assert_eq!(q.kind, Some(Kind::Task));
    }


    #[test]
    fn kind_all_labels() {
        for (k, s) in [
            (Kind::Context, "context"),
            (Kind::Decision, "decision"),
            (Kind::Preference, "preference"),
            (Kind::Task, "task"),
            (Kind::Emotion, "emotion"),
            (Kind::Time, "time"),
            (Kind::Skill, "skill"),
            (Kind::Knowledge, "knowledge"),
        ] {
            assert_eq!(k.as_str(), s);
            assert_eq!(Kind::from_str(s), k);
            assert!(!k.display_zh().is_empty());
        }
    }

}

/// Final association result; no intermediate vector or ranking details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedMemory {
    pub id: String,
    pub title: String,
    pub source_id: String,
    pub relation: RelationKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind { NewVersion, OldVersion, SeeAlso, CoRecall, Neighbor }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallPair {
    pub a: String,
    pub b: String,
    pub n: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelatedResult {
    pub related: Vec<RelatedMemory>,
    pub superseded: std::collections::BTreeMap<String, String>,
}
