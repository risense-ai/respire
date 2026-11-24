use super::*;

const TIME: &str = "2026-11-24T00:00:00Z";
const ID: &str = "773f9960-ca8e-45f7-afe6-94c2f1f7e264";
const ROOT_ID: &str = "c16f0bfd-55a3-48a6-8eb8-975d937107ef";

#[test]
fn grants_allow_only_public_fields_and_prefixes_are_not_full_ids() {
    let grants = [AccessGrant {
        id: ID.into(),
        label: "synthetic grant".into(),
        root_id: ROOT_ID.into(),
        created_at: TIME.into(),
        revoked: true,
    }];
    assert_eq!(
        project_grants(&grants),
        Ok(json!({"items":[{
            "id":ID,"label":"synthetic grant","root_id":ROOT_ID,"created_at":TIME,"revoked":true
        }]}))
    );
    assert_eq!(full_grant_id(&ID.to_ascii_uppercase()).as_deref(), Ok(ID));
    for bad in [
        &ID[..8],
        &ID.replace('-', ""),
        "%",
        "{773f9960-ca8e-45f7-afe6-94c2f1f7e264}",
    ] {
        assert_eq!(full_grant_id(bad), Err(ProjectionError::InvalidRequest));
    }
}

#[test]
fn stats_omit_unavailable_timestamp_and_reject_bad_metadata() {
    assert_eq!(
        project_stats(6, 2, true, None),
        Ok(json!({
            "local_total":6,"local_alive":2,"remote_configured":true
        }))
    );
    assert_eq!(
        project_stats(6, 2, false, Some(TIME)),
        Ok(json!({
            "local_total":6,"local_alive":2,"remote_configured":false,"max_updated_at":TIME
        }))
    );
    assert_eq!(
        project_stats(1, 2, true, None),
        Err(ProjectionError::InvalidStoredMetadata)
    );
    assert_eq!(
        project_stats(1, -1, true, None),
        Err(ProjectionError::InvalidStoredMetadata)
    );
    assert_eq!(
        project_stats(1, 1, false, Some("/private/profile")),
        Err(ProjectionError::InvalidStoredMetadata)
    );
}

fn query() -> QueryLogRow {
    QueryLogRow {
        request_id: Some("private-request".into()),
        id: i64::MAX,
        ts: TIME.into(),
        query: "private-query".into(),
        project: "private-project".into(),
        scope: "/private/profile".into(),
        candidates: vec!["private-candidate".into(), "another".into()],
        adopted: vec!["private-adoption".into()],
        good: vec!["private-good".into()],
        bad: vec![],
    }
}

#[test]
fn query_log_has_exact_decimal_ids_and_library_wide_aggregates() {
    let totals = QueryLogStats {
        total: 500,
        with_adopted: 90,
        empty_candidates: 31,
        good_total: 150,
        bad_total: 70,
        top: vec![("private-top".into(), 2)],
    };
    let limit = PageLimit::new(Some(1)).expect("valid");
    assert_eq!(PageLimit::new(None).expect("default").get(), 50);
    assert_eq!(PageLimit::new(Some(200)).expect("upper bound").get(), 200);
    assert_eq!(
        PageLimit::new(Some(0)).err(),
        Some(ProjectionError::InvalidRequest)
    );
    assert_eq!(
        PageLimit::new(Some(201)).err(),
        Some(ProjectionError::InvalidRequest)
    );
    let expected = json!({"items":[{
        "id":"9223372036854775807","recorded_at":TIME,"candidate_count":2,
        "adopted_count":1,"good_count":1,"bad_count":0
    }],"stats":{"total":500,"with_adopted":90,"empty_candidates":31,"good_total":150,"bad_total":70}});
    let view = project_query_log(&[query()], &totals, limit).expect("view");
    assert_eq!(view, expected);
    assert!(!view.to_string().contains("private"));
    assert_eq!(
        project_query_log(&[query(), query()], &totals, limit),
        Err(ProjectionError::InvalidStoredMetadata)
    );
}

#[test]
fn reviews_hide_private_approval_material_and_do_not_invent_sequence() {
    let mut row = ReviewHistoryRow {
        seq: Some(i64::MAX),
        entry_id: ID.into(),
        record_revision: "review-revision".into(),
        action: "approve".into(),
        actor: "private-actor".into(),
        happened_at: TIME.into(),
        previous_verification: Some("private-approval".into()),
        next_verification: None,
        previous_review_at: None,
        next_review_at: None,
        reason: Some("private-reason".into()),
    };
    let limit = PageLimit::new(None).expect("default");
    assert_eq!(
        project_reviews(std::slice::from_ref(&row), limit),
        Ok(json!({"items":[{
            "sequence":"9223372036854775807","record_revision":"review-revision","action":"approve","happened_at":TIME
        }]}))
    );
    row.seq = None;
    assert_eq!(
        project_reviews(&[row], limit),
        Err(ProjectionError::InvalidStoredMetadata)
    );
}

#[test]
fn conflict_projection_preserves_unavailable_head_and_revision_precision() {
    let mut rows = [
        ConflictMetadata {
            epoch: "new",
            revision: i64::MAX,
            memory_id: ID,
            state: ConflictState::Resolving,
            current_head_revision: Some(9_007_199_254_740_993),
            resolution: None,
            queued_action: Some("merge"),
        },
        ConflictMetadata {
            epoch: "old",
            revision: 5,
            memory_id: ID,
            state: ConflictState::HistoricalEpoch,
            current_head_revision: None,
            resolution: Some("keep_current"),
            queued_action: None,
        },
    ];
    assert_eq!(
        project_conflicts(&rows),
        Ok(json!({"items":[
            {"epoch":"new","revision":"9223372036854775807","memory_id":ID,"state":"resolving",
             "current_head_revision":"9007199254740993","queued_action":"merge"},
            {"epoch":"old","revision":"5","memory_id":ID,"state":"historical_epoch",
             "resolution":"keep_current"}
        ]}))
    );
    rows[1].current_head_revision = Some(6);
    assert_eq!(
        project_conflicts(&rows),
        Err(ProjectionError::InvalidStoredMetadata)
    );
    rows[1].current_head_revision = None;
    rows[1].state = ConflictState::Processed;
    assert_eq!(
        project_conflicts(&rows).expect("processed")["items"][1]["state"],
        "processed"
    );
    rows[0].state = ConflictState::Pending;
    rows[0].queued_action = None;
    assert_eq!(
        project_conflicts(&rows).expect("pending")["items"][0]["state"],
        "pending"
    );
    rows.swap(0, 1);
    assert_eq!(
        project_conflicts(&rows),
        Err(ProjectionError::InvalidStoredMetadata)
    );
}

#[test]
fn borrowed_empty_library_serves_metadata_and_rejects_invalid_inputs() -> anyhow::Result<()> {
    use super::service::{self, ServiceError};
    use respire_app::transport::local::LocalStore;

    let dir = tempfile::tempdir()?;
    let store = LocalStore::open(&dir.path().join("synthetic.db"))?;
    let explain = |err| anyhow::anyhow!("{err:?}");
    assert_eq!(
        service::list_grants(&store).map_err(explain)?,
        json!({"items": []})
    );
    assert_eq!(
        service::stats(&store, false).map_err(explain)?,
        json!({"local_total": 0, "local_alive": 0, "remote_configured": false})
    );
    assert_eq!(
        service::query_log(&store, None).map_err(explain)?["stats"]["total"],
        0
    );
    assert_eq!(
        service::reviews(&store, "synthetic", None).map_err(explain)?,
        json!({"items": []})
    );
    assert_eq!(
        service::conflicts(&store, None, true).map_err(explain)?,
        json!({"items": []})
    );
    assert!(matches!(
        service::query_log(&store, Some(201)),
        Err(ServiceError::InvalidRequest)
    ));
    assert!(matches!(
        service::reviews(&store, "", None),
        Err(ServiceError::InvalidRequest)
    ));
    assert!(matches!(
        service::conflicts(&store, Some(""), false),
        Err(ServiceError::InvalidRequest)
    ));
    assert!(matches!(
        service::revoke_grant(&store, "00000000"),
        Err(ServiceError::InvalidRequest)
    ));
    assert!(service::revoke_grant(&store, "00000000-0000-4000-8000-000000000001").is_ok());
    Ok(())
}
