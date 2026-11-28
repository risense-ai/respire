use super::*;
use anyhow::Context;
use respire_core_sdk::profiles::ACTIVE_SELECTION_KEY;
use serde_json::json;

fn selection(model: &str) -> Result<Selection> {
    Ok(serde_json::from_value(json!({
        "model": model, "backend": "onnx", "engine": "cpu", "generation": model,
        "profile": {"id": model, "embedding": {}},
        "backend_profile": {"id": "onnx", "backend": "onnx"}
    }))?)
}

fn admitted(store: &LocalStore) -> Result<(Selection, String)> {
    let prior = selection("prior")?;
    store.meta_set(ACTIVE_SELECTION_KEY, &serde_json::to_string(&prior)?)?;
    store.meta_set("retrieval_model", &prior.model)?;
    let target = selection("candidate")?;
    let task_id = uuid::Uuid::new_v4().to_string();
    store.queue_selection(&target, &task_id)?;
    Ok((target, task_id))
}

fn checkpoint(
    store: &LocalStore,
    target: &Selection,
    task_id: &str,
    rows: &[StoredMemory],
) -> Result<usize> {
    let batch: Vec<_> = rows
        .iter()
        .map(|row| ModelSwitchArtifact {
            id: &row.id,
            ciphertext: &row.ciphertext,
            opaque: b"candidate opaque bytes",
        })
        .collect();
    store.checkpoint_candidate_artifacts(target, task_id, &batch)
}

#[test]
fn resume_and_activate_more_than_six_hundred_live_rows() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shadow.db");
    let store = LocalStore::open(&path)?;
    let (target, task_id) = admitted(&store)?;
    store.connection.execute_batch(
        "WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<620)
         INSERT INTO memories(id,ciphertext,updated_at,dirty,deleted)
         SELECT cast(n AS TEXT),'source',printf('%04d',n),n%2,0 FROM rows;
         INSERT INTO memories(id,ciphertext,deleted) VALUES('tombstone','deleted',1);
         INSERT INTO core_artifacts(memory_id,model,source,artifact) VALUES('619','prior','source',X'0102');",
    )?;
    assert_eq!(
        store.pending_candidate_counts(&target.generation)?,
        (0, 620)
    );
    assert!(store.activate_shadow_selection(&target, &task_id).is_err());
    let reader = LocalStore::open_read_only(&path)?;
    assert!(reader
        .fetch_pending_candidate_batch(&target.generation, 0)?
        .is_empty());
    let first = reader.fetch_pending_candidate_batch(&target.generation, 128)?;
    assert_eq!(first.first().context("expected first dirty row")?.id, "619");
    assert!(first.iter().all(|row| row.local_artifact.is_empty()));
    assert_eq!(checkpoint(&store, &target, &task_id, &first)?, 128);
    drop(reader);
    drop(store);

    let store = LocalStore::open_existing(&path)?;
    assert_eq!(
        store.pending_candidate_counts(&target.generation)?,
        (128, 620)
    );
    let mut resumed = 0;
    loop {
        let batch = store.fetch_pending_candidate_batch(&target.generation, 128)?;
        if batch.is_empty() {
            break;
        }
        assert!(batch
            .iter()
            .all(|row| first.iter().all(|done| row.id != done.id)));
        resumed += checkpoint(&store, &target, &task_id, &batch)?;
    }
    assert_eq!(resumed, 492);
    assert_eq!(
        store.pending_candidate_counts(&target.generation)?,
        (620, 620)
    );
    assert_eq!(store.retrieval_model()?, "prior");
    assert!(store
        .activate_shadow_selection(&target, "other-task")
        .is_err());
    store.activate_shadow_selection(&target, &task_id)?;
    assert_eq!(
        store.model_switch_foreground_generation()?,
        target.generation
    );
    assert_eq!(
        store.meta_get("retrieval_model")?.as_deref(),
        Some(target.model.as_str())
    );
    assert!(store.pending_selection()?.is_none());
    assert!(store.meta_get("embedding_pending_task_v1")?.is_none());
    let prior: Vec<u8> = store.connection.query_row(
        "SELECT artifact FROM core_artifacts WHERE memory_id='619' AND model='prior' AND source='source'",
        [], |row| row.get(0),
    )?;
    assert_eq!(prior, [1, 2]);
    Ok(())
}

#[test]
fn checkpoint_rejects_bad_admission_and_retries_edited_ciphertext() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("catch-up.db");
    let store = LocalStore::open(&path)?;
    let (target, task_id) = admitted(&store)?;
    store.connection.execute_batch(
        "INSERT INTO memories(id,ciphertext,updated_at,deleted) VALUES
         ('edited','old','2026-11-25T01:00:00Z',0),('removed','old','2026-11-25T00:00:00Z',0);",
    )?;
    let batch = store.fetch_pending_candidate_batch(&target.generation, 10)?;
    let other = LocalStore::open_existing(&path)?;
    other.connection.execute_batch(
        "UPDATE memories SET ciphertext='new' WHERE id='edited';
         UPDATE memories SET deleted=1 WHERE id='removed';",
    )?;
    assert_eq!(checkpoint(&store, &target, &task_id, &batch)?, 0);
    let batch = store.fetch_pending_candidate_batch(&target.generation, 10)?;
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, "edited");
    assert_eq!(batch[0].ciphertext, "new");
    assert!(checkpoint(&store, &target, "wrong-task", &batch).is_err());
    let mut wrong = target.clone();
    wrong.profile.embedding = json!({"changed": true});
    assert!(checkpoint(&store, &wrong, &task_id, &batch).is_err());
    for invalid in [b"".as_slice(), b"rsi1:locator"] {
        let bad = [
            ModelSwitchArtifact {
                id: "edited",
                ciphertext: "new",
                opaque: b"valid",
            },
            ModelSwitchArtifact {
                id: "removed",
                ciphertext: "old",
                opaque: invalid,
            },
        ];
        assert!(store
            .checkpoint_candidate_artifacts(&target, &task_id, &bad)
            .is_err());
        assert_eq!(store.pending_candidate_counts(&target.generation)?, (0, 1));
    }
    assert_eq!(checkpoint(&store, &target, &task_id, &batch)?, 1);
    assert_eq!(store.pending_candidate_counts(&target.generation)?, (1, 1));
    store.activate_shadow_selection(&target, &task_id)?;
    store.queue_selection(&target, &task_id)?;
    assert!(checkpoint(&store, &target, &task_id, &batch).is_err());
    assert!(store.activate_shadow_selection(&target, &task_id).is_err());
    Ok(())
}
