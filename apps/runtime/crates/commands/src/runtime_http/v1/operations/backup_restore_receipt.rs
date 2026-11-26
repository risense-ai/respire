//! Projection boundary for a supervisor-published terminal backup import.
//! A legacy snapshot operation ID is not evidence of a typed HTTP import.
use super::{Phase, Snapshot};
use crate::runtime_http::v1::{transport, transport::RestScope, V1Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fmt::Write, path::Path};
use uuid::Uuid;

const LIMIT: usize = 4096;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u8,
    operation_id: String,
    account_sha256: String,
    target_library_sha256: String,
    target_space_id: Option<String>,
    state: Outcome,
    revision: u64,
}

/// The target fingerprint must come from the resolved serving parent store
/// for this authenticated scope, not from the receipt or an HTTP parameter.
/// This remains unmounted until the authorized supervisor terminal writer
/// and the authoritative parent-store resolution are both present.
#[cfg(target_os = "linux")]
pub(super) fn read_parent_terminal(
    scope: &RestScope,
    id: Uuid,
    target_library_sha256: [u8; 32],
) -> V1Result<Option<Snapshot>> {
    use respire_spawn::owner_credential::{read_backup_terminal, ArchiveOperation};

    let Ok(operation) = ArchiveOperation::from_v4_bytes(*id.as_bytes()) else {
        return Ok(None);
    };
    lookup_terminal(
        scope,
        id,
        target_library_sha256,
        || transport::with_scope(scope, || Ok(())),
        |root| read_backup_terminal(root, operation).map_err(|_| transport::unavailable()),
    )
}

fn lookup_terminal(
    scope: &RestScope,
    id: Uuid,
    target_library_sha256: [u8; 32],
    verify_live: impl Fn() -> V1Result<()>,
    read: impl FnOnce(&Path) -> V1Result<Option<Vec<u8>>>,
) -> V1Result<Option<Snapshot>> {
    if scope.account.is_empty() || !scope.canonical_profile.is_absolute() {
        return Err(transport::unavailable());
    }
    verify_live()?;
    let raw = read(&scope.canonical_profile);
    // Never expose a result or a storage error for an authority revoked
    // during disk I/O. The durable record does not inherit an old bearer.
    verify_live()?;
    let Some(raw) = raw? else { return Ok(None) };
    if raw.is_empty() || raw.len() > LIMIT {
        return Err(transport::unavailable());
    }
    let record: Receipt = serde_json::from_slice(&raw).map_err(|_| transport::unavailable())?;
    if record.schema_version != 1 || record.revision == 0 {
        return Err(transport::unavailable());
    }
    if record.operation_id != id.to_string()
        || record.account_sha256 != format!("{:x}", Sha256::digest(scope.account.as_bytes()))
        || record.target_library_sha256 != hex_digest(target_library_sha256)
        // No active-space guessing. A child needs fresh explicit resolution
        // against the current owner before it can be projected.
        || record.target_space_id.is_some()
    {
        return Ok(None);
    }
    let state = match record.state {
        Outcome::Completed => Phase::Completed,
        Outcome::Failed => Phase::Failed,
        Outcome::Cancelled => Phase::Cancelled,
    };
    Ok(Some(Snapshot::durable_backup(id, state, record.revision)))
}

fn hex_digest(bytes: [u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
#[path = "backup_restore_receipt_tests.rs"]
mod tests;
