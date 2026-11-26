use super::*;
use axum::{http::StatusCode, response::IntoResponse};
use serde_json::{json, Value};
use std::cell::Cell;

fn scope() -> RestScope {
    let mut scope = RestScope::receipt_fixture();
    scope.canonical_profile = std::env::current_dir().unwrap().join("synthetic-owner-profile");
    scope
}

const TARGET: [u8; 32] = [7; 32];

fn record(owner: &RestScope, id: Uuid, outcome: &str) -> Value {
    json!({
        "schema_version": 1, "operation_id": id.to_string(),
        "account_sha256": format!("{:x}", Sha256::digest(owner.account.as_bytes())),
        "target_library_sha256": hex_digest(TARGET), "state": outcome, "revision": 9,
    })
}

fn reader(value: &Value) -> V1Result<Option<Vec<u8>>> {
    serde_json::to_vec(value).map(Some).map_err(|_| transport::unavailable())
}

fn checked<T>(result: V1Result<T>) -> anyhow::Result<T> {
    result.map_err(|problem| anyhow::anyhow!("projection failed: {problem:?}"))
}

#[test]
fn requires_fresh_scope_but_projects_terminal_receipt_across_restart() -> anyhow::Result<()> {
    let owner = scope();
    let mut restarted = owner.clone();
    restarted.instance = Uuid::new_v4();
    restarted.generation += 1;
    restarted.vault_epoch = Uuid::new_v4();
    let id = Uuid::new_v4();
    let receipt = record(&owner, id, "completed");
    let verified = Cell::new(0);
    let result = checked(lookup_terminal(&restarted, id, TARGET, || {
        verified.set(verified.get() + 1);
        Ok(())
    }, |root| {
        assert_eq!(root, restarted.canonical_profile.as_path());
        reader(&receipt)
    }))?;
    assert_eq!(verified.get(), 2);
    assert_eq!(serde_json::to_value(result)?, json!({
        "operation_id": id.to_string(), "kind": "backup_import", "state": "completed",
        "progress_percent": 100,
    }));

    let result = checked(lookup_terminal(&restarted, id, TARGET, || Ok(()), |_| {
        reader(&record(&owner, id, "failed"))
    }))?;
    let projected = serde_json::to_value(result)?;
    assert_eq!(projected["state"], "failed");
    assert_eq!(projected["error"]["code"], "backup_restore_failed");
    assert!(projected.get("progress_percent").is_none());
    assert!(projected.get("canonical_profile").is_none());
    Ok(())
}

#[test]
fn refuses_cross_owner_profile_or_operation_without_leaking_receipt() -> anyhow::Result<()> {
    let owner = scope();
    let id = Uuid::new_v4();
    let archived = record(&owner, id, "completed");
    let mut other_owner = owner.clone();
    other_owner.account = "another-synthetic-owner".into();
    for (scope, id, target) in [(&other_owner, id, TARGET), (&owner, id, [9; 32]), (&owner, Uuid::new_v4(), TARGET)] {
        assert!(checked(lookup_terminal(scope, id, target, || Ok(()), |_| reader(&archived)))?.is_none());
    }
    let mut child = archived;
    child["target_space_id"] = json!("private-child");
    assert!(checked(lookup_terminal(&owner, id, TARGET, || Ok(()), |_| reader(&child)))?.is_none());
    Ok(())
}

#[test]
fn fails_closed_before_staging_or_after_mid_read_revocation() {
    let owner = scope();
    let id = Uuid::new_v4();
    let read = Cell::new(false);
    let result = lookup_terminal(&owner, id, TARGET, || Err(transport::unauthorized()), |_| {
        read.set(true);
        Ok(None)
    });
    assert_eq!(result.err().map(|e| e.into_response().status()), Some(StatusCode::UNAUTHORIZED));
    assert!(!read.get());

    let result = lookup_terminal(&owner, id, TARGET, || {
        if read.get() { Err(transport::unauthorized()) } else { Ok(()) }
    }, |_| {
        read.set(true);
        reader(&record(&owner, id, "completed"))
    });
    assert_eq!(result.err().map(|e| e.into_response().status()), Some(StatusCode::UNAUTHORIZED));
}

#[test]
fn rejects_legacy_pending_malformed_and_oversized_records() {
    let owner = scope();
    let id = Uuid::new_v4();
    let mut legacy = record(&owner, id, "completed");
    if let Some(fields) = legacy.as_object_mut() {
        fields.remove("schema_version");
    }
    for bytes in [
        serde_json::to_vec(&legacy).unwrap(),
        serde_json::to_vec(&record(&owner, id, "installed")).unwrap(),
        b"{".to_vec(),
        vec![b' '; LIMIT + 1],
    ] {
        let result = lookup_terminal(&owner, id, TARGET, || Ok(()), |_| Ok(Some(bytes)));
        assert_eq!(result.err().map(|e| e.into_response().status()), Some(StatusCode::SERVICE_UNAVAILABLE));
    }
}
