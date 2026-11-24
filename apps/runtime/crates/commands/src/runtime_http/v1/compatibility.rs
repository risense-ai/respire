//! Allowlisted projections for the scoped compatibility endpoints.
//!
//! These helpers do not perform reads or authorize a request. Callers must
//! supply rows already bound to a verified owner, active library and action.
//! Never serialize a storage row, which can include private material.

use respire_app::transport::local::{AccessGrant, QueryLogRow, QueryLogStats, ReviewHistoryRow};
use serde_json::{json, Map, Value};
use uuid::Uuid;

const PAGE_MAX: usize = 200;
const PAGE_DEFAULT: usize = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProjectionError {
    InvalidRequest,
    InvalidStoredMetadata,
}

#[derive(Clone, Copy)]
pub(super) struct PageLimit(usize);

impl PageLimit {
    pub(super) fn new(requested: Option<usize>) -> Result<Self, ProjectionError> {
        let count = requested.unwrap_or(PAGE_DEFAULT);
        if !(1..=PAGE_MAX).contains(&count) {
            return Err(ProjectionError::InvalidRequest);
        }
        Ok(Self(count))
    }

    pub(super) fn get(self) -> usize {
        self.0
    }
}

fn count(value: i64) -> Result<u64, ProjectionError> {
    u64::try_from(value).map_err(|_| ProjectionError::InvalidStoredMetadata)
}

fn decimal_id(value: i64) -> Result<String, ProjectionError> {
    Ok(count(value)?.to_string())
}

fn revision(value: i64) -> Result<String, ProjectionError> {
    if value <= 0 {
        return Err(ProjectionError::InvalidStoredMetadata);
    }
    decimal_id(value)
}

fn timestamp(raw: &str) -> Result<&str, ProjectionError> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|_| raw)
        .map_err(|_| ProjectionError::InvalidStoredMetadata)
}

fn exact_uuid(raw: &str) -> Option<Uuid> {
    if raw.len() != 36 || [8, 13, 18, 23].iter().any(|&at| raw.as_bytes()[at] != b'-') {
        return None;
    }
    Uuid::parse_str(raw).ok()
}

/// Validate before calling the legacy revoke method, which also accepts SQL
/// prefix matches. The returned complete ID is in canonical storage form.
pub(super) fn full_grant_id(raw: &str) -> Result<String, ProjectionError> {
    exact_uuid(raw)
        .map(|id| id.hyphenated().to_string())
        .ok_or(ProjectionError::InvalidRequest)
}

pub(super) fn project_grants(grants: &[AccessGrant]) -> Result<Value, ProjectionError> {
    let items = grants
        .iter()
        .map(|row| {
            if exact_uuid(&row.id).is_none() || exact_uuid(&row.root_id).is_none() {
                return Err(ProjectionError::InvalidStoredMetadata);
            }
            Ok(json!({
                "id": row.id,
                "label": row.label,
                "root_id": row.root_id,
                "created_at": timestamp(&row.created_at)?,
                "revoked": row.revoked,
            }))
        })
        .collect::<Result<Vec<_>, ProjectionError>>()?;
    Ok(json!({"items": items}))
}

pub(super) fn project_stats(
    total: i64,
    alive: i64,
    remote_configured: bool,
    max_updated_at: Option<&str>,
) -> Result<Value, ProjectionError> {
    if alive > total {
        return Err(ProjectionError::InvalidStoredMetadata);
    }
    let mut stats = Map::from_iter([
        ("local_total".into(), json!(count(total)?)),
        ("local_alive".into(), json!(count(alive)?)),
        ("remote_configured".into(), json!(remote_configured)),
    ]);
    if let Some(updated_at) = max_updated_at {
        stats.insert("max_updated_at".into(), json!(timestamp(updated_at)?));
    }
    Ok(Value::Object(stats))
}

pub(super) fn project_query_log(
    rows: &[QueryLogRow],
    totals: &QueryLogStats,
    limit: PageLimit,
) -> Result<Value, ProjectionError> {
    if rows.len() > limit.get() {
        return Err(ProjectionError::InvalidStoredMetadata);
    }
    let items = rows
        .iter()
        .map(|row| {
            Ok(json!({
                "id": decimal_id(row.id)?,
                "recorded_at": timestamp(&row.ts)?,
                "candidate_count": row.candidates.len(),
                "adopted_count": row.adopted.len(),
                "good_count": row.good.len(),
                "bad_count": row.bad.len(),
            }))
        })
        .collect::<Result<Vec<_>, ProjectionError>>()?;
    // Aggregate inputs are read separately from the whole active library;
    // do not derive counts from the one bounded page.
    Ok(json!({
        "items": items,
        "stats": {
            "total": count(totals.total)?,
            "with_adopted": count(totals.with_adopted)?,
            "empty_candidates": count(totals.empty_candidates)?,
            "good_total": count(totals.good_total)?,
            "bad_total": count(totals.bad_total)?,
        }
    }))
}

pub(super) fn project_reviews(
    rows: &[ReviewHistoryRow],
    limit: PageLimit,
) -> Result<Value, ProjectionError> {
    if rows.len() > limit.get() {
        return Err(ProjectionError::InvalidStoredMetadata);
    }
    let items = rows
        .iter()
        .map(|row| {
            Ok(json!({
                "sequence": decimal_id(row.seq.ok_or(ProjectionError::InvalidStoredMetadata)?)?,
                "record_revision": row.record_revision,
                "action": row.action,
                "happened_at": timestamp(&row.happened_at)?,
            }))
        })
        .collect::<Result<Vec<_>, ProjectionError>>()?;
    Ok(json!({"items": items}))
}

#[derive(Clone, Copy)]
pub(super) enum ConflictState {
    HistoricalEpoch,
    Processed,
    Resolving,
    Pending,
}

impl ConflictState {
    fn wire(self) -> &'static str {
        match self {
            Self::HistoricalEpoch => "historical_epoch",
            Self::Processed => "processed",
            Self::Resolving => "resolving",
            Self::Pending => "pending",
        }
    }
}

/// Only metadata intended for this endpoint. The legacy conflict listing
/// decrypts candidates and heads, and must not be used to obtain these rows.
pub(super) struct ConflictMetadata<'a> {
    pub(super) epoch: &'a str,
    pub(super) revision: i64,
    pub(super) memory_id: &'a str,
    pub(super) state: ConflictState,
    pub(super) current_head_revision: Option<i64>,
    pub(super) resolution: Option<&'a str>,
    pub(super) queued_action: Option<&'a str>,
}

pub(super) fn project_conflicts(rows: &[ConflictMetadata<'_>]) -> Result<Value, ProjectionError> {
    if rows.len() > PAGE_MAX
        || rows
            .windows(2)
            .any(|pair| pair[0].revision < pair[1].revision)
    {
        return Err(ProjectionError::InvalidStoredMetadata);
    }
    let items = rows
        .iter()
        .map(|row| {
            let mut data = Map::from_iter([
                ("epoch".into(), json!(row.epoch)),
                ("revision".into(), json!(revision(row.revision)?)),
                ("memory_id".into(), json!(row.memory_id)),
                ("state".into(), json!(row.state.wire())),
            ]);
            if let Some(head) = row.current_head_revision {
                if matches!(row.state, ConflictState::HistoricalEpoch) {
                    return Err(ProjectionError::InvalidStoredMetadata);
                }
                data.insert("current_head_revision".into(), json!(revision(head)?));
            }
            if let Some(action) = row.resolution {
                data.insert("resolution".into(), json!(action));
            }
            if let Some(action) = row.queued_action {
                data.insert("queued_action".into(), json!(action));
            }
            Ok(Value::Object(data))
        })
        .collect::<Result<Vec<_>, ProjectionError>>()?;
    Ok(json!({"items": items}))
}

#[cfg(test)]
#[path = "compatibility/tests.rs"]
mod tests;

/// Adapters for a borrowed authoritative store. The caller must keep the
/// owner/profile/instance/action fence held until the operation completes.
pub(super) mod service {
    use super::{
        count, decimal_id, full_grant_id, project_conflicts, project_grants, project_stats,
        timestamp, ConflictMetadata, ConflictState, PageLimit, ProjectionError,
    };
    use respire_app::transport::local::{ConflictMetadataState, LocalStore};
    use respire_app::MemoryTransport;
    use serde_json::{json, Value};

    #[derive(Debug)]
    pub(in super::super) enum ServiceError {
        InvalidRequest,
        StorageUnavailable,
        InvalidStoredMetadata,
    }

    impl From<ProjectionError> for ServiceError {
        fn from(error: ProjectionError) -> Self {
            match error {
                ProjectionError::InvalidRequest => Self::InvalidRequest,
                ProjectionError::InvalidStoredMetadata => Self::InvalidStoredMetadata,
            }
        }
    }

    fn storage<T>(result: anyhow::Result<T>) -> Result<T, ServiceError> {
        result.map_err(|_| ServiceError::StorageUnavailable)
    }

    pub(in super::super) fn list_grants(store: &LocalStore) -> Result<Value, ServiceError> {
        Ok(project_grants(&storage(store.list_grants())?)?)
    }

    pub(in super::super) fn revoke_grant(
        store: &LocalStore,
        grant_id: &str,
    ) -> Result<(), ServiceError> {
        let id = full_grant_id(grant_id)?;
        // Unknown and already inactive have the same response as successful revoke.
        storage(store.revoke_grant(&id))?;
        Ok(())
    }

    pub(in super::super) fn stats(
        store: &LocalStore,
        remote_configured: bool,
    ) -> Result<Value, ServiceError> {
        let (total, alive) = storage(store.catalog_counts())?;
        let total = i64::try_from(total).map_err(|_| ServiceError::InvalidStoredMetadata)?;
        let alive = i64::try_from(alive).map_err(|_| ServiceError::InvalidStoredMetadata)?;
        let updated = storage(store.max_updated_at())?;
        Ok(project_stats(
            total,
            alive,
            remote_configured,
            updated.as_deref(),
        )?)
    }

    pub(in super::super) fn query_log(
        store: &LocalStore,
        limit: Option<usize>,
    ) -> Result<Value, ServiceError> {
        let limit = PageLimit::new(limit)?;
        let (rows, totals) = storage(store.scoped_query_log(limit.get()))?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "id": decimal_id(row.id)?,
                    "recorded_at": timestamp(&row.recorded_at)?,
                    "candidate_count": count(row.candidate_count)?,
                    "adopted_count": count(row.adopted_count)?,
                    "good_count": count(row.good_count)?,
                    "bad_count": count(row.bad_count)?,
                }))
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        Ok(json!({
            "items": items,
            "stats": {
                "total": count(totals.total)?,
                "with_adopted": count(totals.with_adopted)?,
                "empty_candidates": count(totals.empty_candidates)?,
                "good_total": count(totals.good_total)?,
                "bad_total": count(totals.bad_total)?,
            },
        }))
    }

    pub(in super::super) fn reviews(
        store: &LocalStore,
        memory_id: &str,
        limit: Option<usize>,
    ) -> Result<Value, ServiceError> {
        if memory_id.is_empty() {
            return Err(ServiceError::InvalidRequest);
        }
        let limit = PageLimit::new(limit)?;
        let rows = storage(store.scoped_review_history(memory_id, limit.get()))?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "sequence": decimal_id(row.sequence)?,
                    "record_revision": row.record_revision,
                    "action": row.action,
                    "happened_at": timestamp(&row.happened_at)?,
                }))
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        Ok(json!({"items": items}))
    }

    pub(in super::super) fn conflicts(
        store: &LocalStore,
        memory_id: Option<&str>,
        include_processed: bool,
    ) -> Result<Value, ServiceError> {
        if memory_id == Some("") {
            return Err(ServiceError::InvalidRequest);
        }
        let rows = storage(store.list_conflict_metadata(memory_id, include_processed))?;
        let metadata = rows
            .iter()
            .map(|row| ConflictMetadata {
                epoch: &row.epoch,
                revision: row.revision,
                memory_id: &row.memory_id,
                state: match row.state {
                    ConflictMetadataState::HistoricalEpoch => ConflictState::HistoricalEpoch,
                    ConflictMetadataState::Processed => ConflictState::Processed,
                    ConflictMetadataState::Resolving => ConflictState::Resolving,
                    ConflictMetadataState::Pending => ConflictState::Pending,
                },
                current_head_revision: row.current_head_revision,
                resolution: row.resolution.as_deref(),
                queued_action: row.queued_action.as_deref(),
            })
            .collect::<Vec<_>>();
        Ok(project_conflicts(&metadata)?)
    }
}
