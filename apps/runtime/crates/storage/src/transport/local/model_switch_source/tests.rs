use super::*;
use respire_core_sdk::profiles::{self, BackendProfile, ModelProfile};
use serde_json::json;

fn candidate(id: &str) -> Selection {
    Selection {
        model: id.to_owned(),
        backend: "onnx".into(),
        engine: respire_core_sdk::onnx::Engine::Cpu,
        variant: None,
        profile: ModelProfile {
            id: id.into(),
            embedding: json!({}),
            onnx: json!({}),
            weights: None,
            tokenizer: None,
        },
        backend_profile: BackendProfile {
            id: "onnx".into(),
            backend: "onnx".into(),
            native: None,
            provider: None,
            protocol: None,
            endpoint: None,
            keyring_service: None,
            keyring_account: None,
        },
        generation: format!("test-{id}"),
    }
}

fn store(root: &std::path::Path, name: &str) -> Result<LocalStore> {
    let store = LocalStore::open(&root.join(name))?;
    store.connection.execute_batch(
        "INSERT INTO memories(id,ciphertext,updated_at,deleted) VALUES
         ('live','ciphertext-v1','2026-11-25T01:00:00Z',0),
         ('deleted','never-copy','2026-11-25T00:00:00Z',1);
         INSERT INTO core_artifacts(memory_id,model,source,artifact)
         VALUES('live','test-prior','ciphertext-v1',X'0102');",
    )?;
    Ok(store)
}

#[test]
fn committed_input_copy_does_not_attach_prior_artifacts_or_change_profile() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = store(dir.path(), "source.db")?;
    let prior = candidate("prior");
    let target = candidate("target");
    assert!(store.capture_model_switch_source(|| Ok(())).is_err());
    store.queue_selection(&target, &uuid::Uuid::new_v4().to_string())?;
    store.meta_set(ACTIVE_SELECTION_KEY, &serde_json::to_string(&prior)?)?;
    let snapshot = profiles::scoped(prior, || {
        let before = profiles::current()?.context("foreground profile")?;
        let snapshot = store.capture_model_switch_source(|| Ok(()))?;
        assert_eq!(
            profiles::current()?
                .context("unchanged foreground profile")?
                .model,
            before.model
        );
        Ok(snapshot)
    })?;
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.rows[0].id, "live");
    assert_eq!(snapshot.rows[0].ciphertext, "ciphertext-v1");
    assert!(snapshot.rows[0].local_artifact.is_empty());
    assert_eq!(store.light_status_counts("test-prior")?.2, 1);
    assert_eq!(snapshot.source.revision, store.memory_revision()?);
    Ok(())
}

#[test]
fn publish_cas_rejects_changed_content_prior_selection_and_foreign_library() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let live = store(dir.path(), "live.db")?;
    let foreign = store(dir.path(), "foreign.db")?;
    let target = candidate("target");
    let prior = candidate("prior");
    let replacement = candidate("other");
    let prior_wire = serde_json::to_string(&prior)?;
    let replacement_wire = serde_json::to_string(&replacement)?;
    live.meta_set(ACTIVE_SELECTION_KEY, &prior_wire)?;
    foreign.meta_set(ACTIVE_SELECTION_KEY, &prior_wire)?;
    let admitted = uuid::Uuid::new_v4().to_string();
    live.queue_selection(&target, &admitted)?;
    foreign.queue_selection(&target, &admitted)?;
    let first = live.capture_model_switch_source(|| Ok(()))?;
    foreign.meta_set("memory_revision_v1", &first.source.revision)?;
    assert!(foreign
        .publish_model_switch_snapshot(&target, &first.source, &[])
        .is_err());
    assert_eq!(
        foreign.meta_get(ACTIVE_SELECTION_KEY)?.as_deref(),
        Some(prior_wire.as_str())
    );

    live.meta_set(ACTIVE_SELECTION_KEY, &replacement_wire)?;
    assert_eq!(live.memory_revision()?, first.source.revision);
    assert!(live
        .publish_model_switch_snapshot(&target, &first.source, &[])
        .is_err());
    assert_eq!(
        live.meta_get(ACTIVE_SELECTION_KEY)?.as_deref(),
        Some(replacement_wire.as_str())
    );
    let old_task = live.capture_model_switch_source(|| Ok(()))?;
    let new_task = uuid::Uuid::new_v4().to_string();
    live.queue_selection(&target, &new_task)?;
    assert!(live
        .publish_model_switch_snapshot(&target, &old_task.source, &[])
        .is_err());
    let second = live.capture_model_switch_source(|| Ok(()))?;

    live.connection.execute_batch(
        "UPDATE memories SET ciphertext='ciphertext-v2' WHERE id='live';
         INSERT INTO core_artifacts(memory_id,model,source,artifact)
         VALUES('live','test-target','ciphertext-v2',X'03');",
    )?;
    assert_ne!(live.memory_revision()?, second.source.revision);
    assert!(live
        .publish_model_switch_snapshot(&target, &second.source, &[])
        .is_err());
    assert_eq!(
        live.meta_get(ACTIVE_SELECTION_KEY)?.as_deref(),
        Some(replacement_wire.as_str())
    );

    let stable = live.capture_model_switch_source(|| Ok(()))?;
    assert_eq!(stable.rows[0].ciphertext, "ciphertext-v2");
    let missing = candidate("still-unbuilt");
    // A wrong advertised target cannot use a stamp for this task either.
    assert!(live
        .publish_model_switch_snapshot(&missing, &stable.source, &[])
        .is_err());
    live.queue_selection(&missing, &new_task)?;
    assert!(live
        .publish_model_switch_snapshot(&target, &stable.source, &[])
        .is_err());
    assert!(live
        .publish_model_switch_snapshot(&missing, &stable.source, &[])
        .is_err());
    assert_eq!(
        live.meta_get(ACTIVE_SELECTION_KEY)?.as_deref(),
        Some(replacement_wire.as_str())
    );
    live.queue_selection(&target, &new_task)?;
    live.publish_model_switch_snapshot(
        &target,
        &stable.source,
        &[ModelSwitchArtifact {
            id: "live",
            ciphertext: "ciphertext-v2",
            opaque: b"test prepared eligible target row",
        }],
    )?;
    assert_eq!(
        live.active_selection()?
            .context("published active selection")?
            .model,
        target.model
    );
    Ok(())
}

#[test]
fn wal_read_stays_old_across_interleaved_write_and_stale_publication_refuses() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let live = store(dir.path(), "live.db")?;
    let writer = LocalStore::open(&dir.path().join("live.db"))?;
    let target = candidate("target");
    live.queue_selection(&target, &uuid::Uuid::new_v4().to_string())?;
    let reader = live.begin_model_switch_read()?;
    let (staging, entered) = std::sync::mpsc::sync_channel(0);
    let (release, resumed) = std::sync::mpsc::sync_channel(0);
    let job = std::thread::spawn(move || -> Result<_> {
        let mut first = Some(staging);
        let mut rows = Vec::new();
        let source = reader.stage(
            || {
                if let Some(staging) = first.take() {
                    staging.send(())?;
                    resumed.recv_timeout(Duration::from_secs(5))?;
                }
                Ok(())
            },
            |row, _| {
                rows.push(row);
                Ok(())
            },
        )?;
        Ok((source, rows))
    });
    entered.recv_timeout(Duration::from_secs(5))?;
    writer
        .connection
        .execute_batch("UPDATE memories SET ciphertext='ciphertext-v2' WHERE id='live'")?;
    release.send(())?;
    let (source, rows) = job
        .join()
        .map_err(|_| anyhow::anyhow!("stage thread failed"))??;
    assert_eq!(rows[0].ciphertext, "ciphertext-v1");
    assert_ne!(source.revision, live.memory_revision()?);
    assert!(live
        .publish_model_switch_snapshot(&target, &source, &[])
        .is_err());
    assert!(
        matches!(live.capture_source_with_budget(|| Ok(()), 0), Err(e) if e.to_string().contains("snapshot budget"))
    );
    assert!(live
        .capture_model_switch_source(|| anyhow::bail!("test cancelled"))
        .is_err());
    Ok(())
}

#[test]
fn pending_target_and_physical_database_are_bound() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("live.db");
    let old_path = dir.path().join("retired.db");
    let live = store(dir.path(), "live.db")?;
    let target = candidate("target");
    let mut alternate = target.clone();
    alternate.generation = "retargeted".into();
    let task = uuid::Uuid::new_v4().to_string();
    live.queue_selection(&target, &task)?;
    let source = live.capture_model_switch_source(|| Ok(()))?.source;
    live.queue_selection(&alternate, &task)?;
    assert!(source.check(&live.connection, &alternate).is_err());
    live.queue_selection(&target, &task)?;
    source.check(&live.connection, &target)?;
    drop(live);

    if cfg!(windows) {
        // The held file does not share delete: same-path replacement cannot proceed.
        assert!(fs::rename(&path, &old_path).is_err());
    } else {
        fs::rename(&path, &old_path)?;
        fs::copy(&old_path, &path)?;
        let replacement = Connection::open(&path)?;
        assert_eq!(replacement.path(), Some(source.database.as_str()));
        assert!(source.check(&replacement, &target).is_err());
    }
    Ok(())
}

#[test]
fn late_inline_promotion_failure_rolls_back_every_new_row() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let live = store(dir.path(), "live.db")?;
    let prior = candidate("prior");
    let target = candidate("target");
    live.meta_set(ACTIVE_SELECTION_KEY, &serde_json::to_string(&prior)?)?;
    live.connection.execute_batch(
        "INSERT INTO memories(id,ciphertext,updated_at,deleted) VALUES
           ('second','ciphertext-v2','2026-11-25T01:00:00Z',0);
         ",
    )?;
    live.queue_selection(&target, &uuid::Uuid::new_v4().to_string())?;
    let stamp = live
        .begin_model_switch_read()?
        .stage(|| Ok(()), |_, _| Ok(()))?;
    let rows = [
        ModelSwitchArtifact {
            id: "live",
            ciphertext: "ciphertext-v1",
            opaque: b"first inline opaque artifact",
        },
        ModelSwitchArtifact {
            id: "second",
            ciphertext: "ciphertext-v2",
            opaque: b"second inline opaque artifact",
        },
    ];
    // Existing valid inactive rows cannot fill a gap in the new exact input.
    live.connection.execute(
        "INSERT INTO core_artifacts(memory_id,model,source,artifact)
         VALUES('second','test-target','ciphertext-v2',X'4455')",
        [],
    )?;
    let truncated = live
        .publish_model_switch_snapshot(&target, &stamp, &rows[..1])
        .unwrap_err();
    assert!(truncated.to_string().contains("row count"));
    let duplicate = [
        ModelSwitchArtifact { ..rows[0] },
        ModelSwitchArtifact { ..rows[0] },
    ];
    let repeated = live
        .publish_model_switch_snapshot(&target, &stamp, &duplicate)
        .unwrap_err();
    assert!(repeated.to_string().contains("duplicate"));
    assert_eq!(live.light_status_counts("test-target")?.2, 1);
    live.connection
        .execute("DELETE FROM core_artifacts WHERE model='test-target'", [])?;
    live.connection.execute_batch(
        "CREATE TRIGGER reject_candidate_second BEFORE INSERT ON core_artifacts
         WHEN NEW.memory_id='second' AND NEW.model='test-target'
         BEGIN SELECT RAISE(ABORT,'late candidate abort'); END;",
    )?;
    let error = live
        .publish_model_switch_snapshot(&target, &stamp, &rows)
        .unwrap_err();
    assert!(error.to_string().contains("late candidate abort"));
    assert_eq!(live.light_status_counts("test-target")?.2, 0);
    assert_eq!(
        live.active_indexed_memory_exact("live", "test-prior")?
            .context("prior row")?
            .local_artifact,
        vec![1, 2]
    );
    assert_eq!(
        live.active_selection()?.context("active selection")?.model,
        prior.model
    );

    live.connection
        .execute_batch("DROP TRIGGER reject_candidate_second")?;
    live.publish_model_switch_snapshot(&target, &stamp, &rows)?;
    assert_eq!(live.light_status_counts("test-target")?.2, 2);
    assert_eq!(
        live.active_selection()?.context("target selection")?.model,
        target.model
    );
    Ok(())
}

#[test]
fn bounded_inline_promotion_measures_worst_eligible_sqlite_copy() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let live = LocalStore::open(&dir.path().join("real.db"))?;
    let target = candidate("target");
    let prior = candidate("prior");
    live.meta_set(ACTIVE_SELECTION_KEY, &serde_json::to_string(&prior)?)?;
    let ids: Vec<_> = (0..MODEL_SWITCH_MAX_PROMOTION_ROWS)
        .map(|i| format!("row-{i:05}"))
        .collect();
    for id in &ids {
        live.connection.execute(
            "INSERT INTO memories(id,ciphertext,updated_at,deleted) VALUES(?1,'sealed','2026-11-25T01:00:00Z',0)",
            [id],
        )?;
    }
    live.queue_selection(&target, &uuid::Uuid::new_v4().to_string())?;
    let stamp = live
        .begin_model_switch_read()?
        .stage(|| Ok(()), |_, _| Ok(()))?;
    let opaque = vec![0x42; (MODEL_SWITCH_MAX_PROMOTION_BYTES / ids.len()) - 15];
    let mut rows: Vec<_> = ids
        .iter()
        .map(|id| ModelSwitchArtifact {
            id,
            ciphertext: "sealed",
            opaque: &opaque,
        })
        .collect();
    rows.push(ModelSwitchArtifact {
        id: &ids[0],
        ciphertext: "sealed",
        opaque: &opaque,
    });
    assert!(live
        .publish_model_switch_snapshot(&target, &stamp, &rows)
        .is_err());
    assert_eq!(live.light_status_counts("test-target")?.2, 0);
    rows.pop();
    let start = std::time::Instant::now();
    live.publish_model_switch_snapshot(&target, &stamp, &rows)?;
    eprintln!(
        "native SQLite bounded final promotion: {} rows, {} bytes, {} us",
        rows.len(),
        rows.iter()
            .map(|row| row.id.len() + row.ciphertext.len() + row.opaque.len())
            .sum::<usize>(),
        start.elapsed().as_micros()
    );
    assert_eq!(
        live.light_status_counts("test-target")?.2,
        rows.len() as u64
    );
    Ok(())
}
