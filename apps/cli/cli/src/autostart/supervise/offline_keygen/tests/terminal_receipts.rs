#[test]
fn prior_spawn_or_finished_state_never_authorizes_another_spawn() {
    for stage in [Stage::Prepared,Stage::Starting,Stage::Verified,Stage::Finished,Stage::HandedOff,Stage::RolledBack,Stage::RollbackStarting,Stage::RollbackFinished,Stage::RollbackHandedOff] {
        assert!(require_startable(stage).is_err());
    }
    assert!(require_startable(Stage::Sent).is_ok());
    assert!(require_startable(Stage::Committed).is_ok());
}
#[test]
fn generic_recovery_rejects_even_malformed_pending_evidence() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    reject_generic_recovery(&root)?;
    std::fs::create_dir_all(path(&root).parent().context("synthetic journal parent missing")?)?;
    std::fs::write(path(&root),b"malformed-original")?;
    assert!(reject_generic_recovery(&root).is_err());
    assert_eq!(std::fs::read(path(&root))?,b"malformed-original");
    Ok(())
}
#[test]
fn cleanup_is_cas_bound_and_only_after_handoff() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); let mut digest=None;
    save(&record,&mut digest)?;
    assert!(remove_finished(&record,digest.as_deref().context("synthetic journal digest missing")?).is_err());
    record.stage=Stage::Sent; save(&record,&mut digest)?;
    record.session_sha256=Some("c".repeat(64)); record.stage=Stage::Starting; save(&record,&mut digest)?;
    record.actor=Some(RuntimeIdentity {pid:12,process_start_id:"synthetic".into(),instance_id:"synthetic".into(),generation:1,
        image_path:record.binary.display().to_string(),image_sha256:record.image_sha256.clone(),
        installation_root:root.display().to_string(),installation_id:"synthetic".into(),profile:root.display().to_string(),account_id:"synthetic".into()});
    record.stage=Stage::Verified; save(&record,&mut digest)?;
    let mut premature=record.clone(); premature.final_actor=record.actor.clone();
    assert!(validate_record(&premature,&root).is_err());
    let mut changed=record.clone(); changed.stage=Stage::Finished;
    changed.actor.as_mut().context("synthetic actor missing")?.instance_id="changed".into();
    assert!(validate_transition(&record,&changed).is_err());
    record.stage=Stage::Finished; save(&record,&mut digest)?;
    assert!(require_startable(record.stage).is_err());
    let mut successor=record.actor.clone().context("synthetic actor missing")?;
    successor.instance_id="successor".into();
    let mut terminal=record.clone(); terminal.stage=Stage::HandedOff;
    let old_json=serde_json::to_value(&terminal)?;
    assert!(old_json.get("final_actor").is_none());
    let old:Original=serde_json::from_value(old_json)?;
    assert!(completed_actor(&old).is_ok());
    terminal.final_actor=Some(successor.clone());
    validate_record(&terminal,&root)?;
    validate_transition(&record,&terminal)?;
    assert!(completed_actor(&terminal).is_err());
    let mut changed_terminal=terminal.clone();
    changed_terminal.final_actor.as_mut().context("synthetic successor missing")?.instance_id="foreign".into();
    assert!(validate_transition(&terminal,&changed_terminal).is_err());
    let mut foreign=terminal.clone(); foreign.operation_id=uuid::Uuid::new_v4().to_string();
    assert!(validate_transition(&record,&foreign).is_err());
    let mut rollback=record.clone(); rollback.stage=Stage::RollbackFinished;
    rollback.session_sha256=None; rollback.was_serving=true;
    validate_record(&rollback,&root)?;
    let mut rollback_terminal=rollback.clone(); rollback_terminal.stage=Stage::RollbackHandedOff;
    rollback_terminal.final_actor=Some(successor);
    validate_record(&rollback_terminal,&root)?;
    validate_transition(&rollback,&rollback_terminal)?;
    assert!(completed_actor(&rollback_terminal).is_err());
    assert!(require_startable(rollback.stage).is_err());
    let mut early_rollback=rollback.clone(); early_rollback.final_actor=rollback_terminal.final_actor.clone();
    assert!(validate_record(&early_rollback,&root).is_err());
    assert!(remove_finished(&record,digest.as_deref().context("synthetic journal digest missing")?).is_err());
    record.stage=Stage::HandedOff; save(&record,&mut digest)?;
    assert!(remove_finished(&record,"wrong-digest").is_err());
    assert!(path(&root).exists());
    remove_finished(&record,digest.as_deref().context("synthetic journal digest missing")?)?;
    assert!(!path(&root).exists());
    Ok(())
}
#[test]
fn completed_actor_never_accepts_recovery_secret() {
    let value=serde_json::json!({"schema_version":1,"operation_id":uuid::Uuid::new_v4().to_string(),
        "session_sha256":"a".repeat(64),"path":"/synthetic/session.json","runtime":{
            "pid":12,"process_start_id":"synthetic","instance_id":"synthetic","generation":1,
            "image_path":"/synthetic/runtime","image_sha256":"a".repeat(64),
            "installation_root":"/synthetic","installation_id":"synthetic","profile":"/synthetic","account_id":"synthetic"},
        "super_password":"synthetic-only"});
    let mut actor = value["runtime"].clone();
    actor["super_password"] = serde_json::json!("synthetic-only");
    assert!(serde_json::from_value::<RuntimeIdentity>(actor).is_err());
}
fn fixture_actor(record: &Original) -> RuntimeIdentity {
    RuntimeIdentity { pid: 42, process_start_id: "fixture-birth".into(),
        instance_id: "fixture-instance".into(), generation: 1,
        image_path: record.binary.display().to_string(), image_sha256: record.image_sha256.clone(),
        installation_root: record.root.display().to_string(), installation_id: "fixture-root".into(),
        profile: record.profile.display().to_string(), account_id: "fixture-account".into() }
}

#[test]
fn existing_result_never_turns_unknown_or_foreign_evidence_into_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let mut record = original(&root);
    record.stage = Stage::Sent;
    let session = "c".repeat(64);
    let identity = HostAuthIdentity::Unbound {
        operation_id: record.operation_id.clone(), session_sha256: session.clone(),
    };
    let mut receipt = HostAuthResult { operation_id: record.operation_id.clone(),
        outcome: HostAuthOutcome::Committed, session_sha256: Some(session.clone()),
        path: root.join("session.json"), super_password: Some("synthetic-private-pipe-only".into()) };
    assert_eq!(committed_start_session(&record, &receipt, &identity)?, session);
    for outcome in [HostAuthOutcome::Unknown, HostAuthOutcome::RolledBack] {
        receipt.outcome = outcome;
        assert!(committed_start_session(&record, &receipt, &identity).is_err());
    }
    receipt.outcome = HostAuthOutcome::Committed;
    receipt.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(committed_start_session(&record, &receipt, &identity).is_err());
    receipt.operation_id = record.operation_id.clone();
    receipt.path = root.join("other/session.json");
    assert!(committed_start_session(&record, &receipt, &identity).is_err());
    receipt.path = root.join("session.json");
    receipt.session_sha256 = None;
    assert!(committed_start_session(&record, &receipt, &identity).is_err());
    receipt.session_sha256 = Some("d".repeat(64));
    assert!(committed_start_session(&record, &receipt, &identity).is_err());
    receipt.session_sha256 = Some(session);
    assert!(committed_start_session(&record, &receipt, &HostAuthIdentity::Absent).is_err());
    record.session_sha256 = Some("e".repeat(64));
    assert!(committed_start_session(&record, &receipt, &identity).is_err());
    record.session_sha256 = None;
    for stage in [Stage::Starting, Stage::Verified, Stage::Finished, Stage::HandedOff] {
        record.stage = stage;
        assert!(committed_start_session(&record, &receipt, &identity).is_err());
    }
    Ok(())
}

#[test]
fn local_completion_rejects_every_changed_actor_before_and_after_finish() -> Result<()> {
    use std::cell::Cell;
    let temp = tempfile::tempdir()?;
    let actor = fixture_actor(&original(&temp.path().canonicalize()?));
    let wire = serde_json::to_value(&actor)?;
    for (field, changed) in [
        ("pid", serde_json::json!(43)), ("process_start_id", serde_json::json!("other-birth")),
        ("instance_id", serde_json::json!("other-instance")), ("generation", serde_json::json!(2)),
        ("image_path", serde_json::json!("other-image")), ("image_sha256", serde_json::json!("f".repeat(64))),
        ("installation_root", serde_json::json!("other-root")), ("installation_id", serde_json::json!("other-root-id")),
        ("profile", serde_json::json!("other-profile")), ("account_id", serde_json::json!("other-account")),
    ] {
        let mut changed_wire = wire.clone(); changed_wire[field] = changed;
        let foreign: RuntimeIdentity = serde_json::from_value(changed_wire)?;
        for after_finish in [false, true] {
            let finished = Cell::new(false);
            let result = finish_checked(&actor, || {
                Ok(if finished.get() || !after_finish { foreign.clone() } else { actor.clone() })
            }, || { finished.set(true); Ok(()) });
            assert!(result.is_err(), "changed {field} accepted");
            assert_eq!(finished.get(), after_finish, "changed actor advanced maintenance");
        }
    }
    Ok(())
}

#[test]
fn local_completion_errors_are_not_success_and_never_retry_finish() -> Result<()> {
    use std::cell::Cell;
    let temp = tempfile::tempdir()?;
    let actor = fixture_actor(&original(&temp.path().canonicalize()?));
    for after_finish in [false, true] {
        let finishes = Cell::new(0);
        let result = finish_checked(&actor, || {
            if !after_finish || finishes.get() > 0 {
                anyhow::bail!("synthetic dead process, changed session or unreadable original");
            }
            Ok(actor.clone())
        }, || { finishes.set(finishes.get() + 1); Ok(()) });
        assert!(result.is_err());
        assert_eq!(finishes.get(), usize::from(after_finish));
    }
    let observations = Cell::new(0);
    let finishes = Cell::new(0);
    assert!(finish_checked(&actor, || {
        observations.set(observations.get() + 1); Ok(actor.clone())
    }, || {
        finishes.set(finishes.get() + 1);
        anyhow::bail!("synthetic marker durability failure");
    }).is_err());
    assert_eq!(observations.get(), 1);
    assert_eq!(finishes.get(), 1);
    finish_checked(&actor, || { observations.set(observations.get() + 1); Ok(actor.clone()) }, || Ok(()))?;
    assert_eq!(observations.get(), 3);
    Ok(())
}

#[test]
fn existing_auth_result_cannot_claim_a_finished_handoff() -> Result<()> {
    let wire = serde_json::json!({"operation_id": uuid::Uuid::new_v4().to_string(),
        "outcome": "finished", "session_sha256": "a".repeat(64),
        "path": "/fixture/session.json"});
    assert!(serde_json::from_value::<HostAuthResult>(wire).is_err());
    Ok(())
}

#[test]
fn post_finish_failure_retains_verified_original_without_replay_or_cleanup() -> Result<()> {
    use std::cell::Cell;
    let temp = tempfile::tempdir()?; let root = temp.path().canonicalize()?;
    let mut record = original(&root); let mut digest = None;
    save(&record, &mut digest)?;
    record.stage = Stage::Sent; save(&record, &mut digest)?;
    record.session_sha256 = Some("c".repeat(64)); record.stage = Stage::Starting;
    save(&record, &mut digest)?;
    let actor = fixture_actor(&record);
    record.actor = Some(actor.clone()); record.stage = Stage::Verified;
    save(&record, &mut digest)?;
    let before = std::fs::read(path(&root))?;
    let finished = Cell::new(false);
    assert!(finish_checked(&actor, || {
        let (durable, _) = load(&root)?.context("fixture original missing")?;
        anyhow::ensure!(serde_json::to_value(&durable)? == serde_json::to_value(&record)?,
            "fixture original changed");
        anyhow::ensure!(!finished.get(), "synthetic actor exited after marker removal");
        Ok(actor.clone())
    }, || { finished.set(true); Ok(()) }).is_err());
    assert!(finished.get());
    assert_eq!(std::fs::read(path(&root))?, before);
    let (durable, observed) = load(&root)?.context("fixture original lost")?;
    assert!(durable.stage == Stage::Verified);
    assert!(require_startable(durable.stage).is_err());
    assert!(remove_finished(&durable, &observed).is_err());
    assert!(path(&root).is_file());
    Ok(())
}
