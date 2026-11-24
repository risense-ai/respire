use super::*;
std::thread_local! {
    static FORBID_ADMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_JOURNAL_AFTER_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
pub(super) fn journal_publish_result() -> Result<()> {
    anyhow::ensure!(!FAIL_JOURNAL_AFTER_PUBLISH.with(|fail| fail.replace(false)),
        "synthetic unknown durability after journal publication");
    Ok(())
}
pub(super) fn assert_no_forbidden_admission() {
    FORBID_ADMISSION.with(|forbidden| assert!(!forbidden.get(), "unsupported keygen crossed the side-effect boundary"));
}
struct AdmissionTrap;
impl AdmissionTrap {
    fn install() -> Self { FORBID_ADMISSION.with(|value| value.set(true)); Self }
}
impl Drop for AdmissionTrap {
    fn drop(&mut self) { FORBID_ADMISSION.with(|value| value.set(false)); }
}
fn snapshot_tree(root:&Path) -> Result<std::collections::BTreeMap<PathBuf,Option<Vec<u8>>>> {
    fn visit(root:&Path,at:&Path,out:&mut std::collections::BTreeMap<PathBuf,Option<Vec<u8>>>) -> Result<()> {
        for entry in std::fs::read_dir(at)? {
            let entry=entry?; let path=entry.path(); let kind=entry.file_type()?;
            anyhow::ensure!(!kind.is_symlink(), "unexpected test fixture link");
            let relative=path.strip_prefix(root)?.to_owned();
            if kind.is_dir() { out.insert(relative,None); visit(root,&path,out)?; }
            else { out.insert(relative,Some(std::fs::read(path)?)); }
        }
        Ok(())
    }
    let mut result=std::collections::BTreeMap::new(); visit(root,root,&mut result)?; Ok(result)
}
#[test]
fn unsupported_metadata_keygen_never_enters_side_effects() -> Result<()> {
    let _env=crate::runtime_fetch::runtime_fetch_tests::env_scope(&[
        "RSRS_DATA_DIR","RSRS_BIN_DIR","RSRS_RUNTIME_BIN","RSRS_CLIENT_ONLY","RSRS_NO_AUTOSTART"])?;
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    std::env::set_var("RSRS_DATA_DIR",&root);
    std::env::set_var("RSRS_BIN_DIR",root.join("bin"));
    for key in ["RSRS_RUNTIME_BIN","RSRS_CLIENT_ONLY","RSRS_NO_AUTOSTART"] { std::env::remove_var(key); }
    assert!(crate::embedding::selection().is_none());
    crate::runtime_fetch::runtime_fetch_tests::install_lifecycle_fixture()?;
    // This has authenticated schema-2 metadata and an intact file inventory,
    // but deliberately lacks offline_keygen_handoff. No installer is called.
    let before=snapshot_tree(&root)?;
    let _trap=AdmissionTrap::install();
    let error=keygen(None,false).err().context("unsupported Runtime accepted keygen")?;
    assert_eq!(error.to_string(),"offline_keygen_handoff_unsupported: upgrade Runtime before generating local keys");
    assert_eq!(snapshot_tree(&root)?,before);
    assert!(!path(&root).exists());
    assert!(!root.join("runtime/autostart/maintenance.json").exists());
    assert!(!root.join("runtime/endpoint.json").exists());
    assert!(!root.join("session.json").exists());
    Ok(())
}

fn original(root:&Path) -> Original {
    Original { schema:1,operation_id:uuid::Uuid::new_v4().to_string(),force:false,
        root:root.to_owned(),source:root.to_owned(),profile:root.to_owned(),binary:root.join("runtime-image"),
        image_sha256:"a".repeat(64),token:uuid::Uuid::new_v4().to_string(),coordinator:"synthetic-coordinator".into(),
        marker_sha256:"b".repeat(64),pending_marker_sha256:None,original_identity:HostAuthIdentity::Absent,session_sha256:None,stage:Stage::Prepared,material:None,actor:None,final_actor:None,was_serving:false,store_ready:false }
}
#[test]
fn absent_pending_probe_does_not_create_first_use_paths() -> Result<()> {
    let _lock=crate::TEST_ENV_LOCK.lock().map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("RSRS_DATA_DIR",value),
                None => std::env::remove_var("RSRS_DATA_DIR"),
            }
        }
    }
    let _restore=Restore(std::env::var_os("RSRS_DATA_DIR"));
    let temp=tempfile::tempdir()?;
    let root=temp.path().canonicalize()?.join("not-yet-created");
    std::env::set_var("RSRS_DATA_DIR",&root);
    let (selected,profile)=respire_client_host::config::control_roots()?;
    assert_eq!(selected,root);
    assert!(!profile.exists());
    assert!(load(&root)?.is_none());
    assert!(!pending_for_selected_root()?);
    prepare_account(true,"synthetic-user","https://example.invalid")?;
    prepare_account(false,"synthetic-user","https://example.invalid")?;
    assert!(!root.exists() && !profile.exists());
    assert_eq!(std::fs::read_dir(temp.path())?.count(),0);
    Ok(())
}
#[test]
fn existing_pending_uses_canonical_root_without_accepting_a_foreign_binding() -> Result<()> {
    let temp=tempfile::tempdir()?;
    let canonical=temp.path().canonicalize()?;
    #[cfg(not(windows))]
    let lexical=canonical.clone();
    #[cfg(windows)]
    let lexical={
        // Test-only spellings of one real root, including a verbatim TMP.
        // Production authorization never strips prefixes or normalizes strings.
        let value=canonical.to_str().context("synthetic root is not UTF-8")?;
        if let Some(unc)=value.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{}",unc))
        } else {
            PathBuf::from(value.strip_prefix(r"\\?\").context("synthetic canonical root lacks verbatim prefix")?)
        }
    };
    let record=original(&canonical);
    save(&record,&mut None)?;
    let (loaded,_)=load(&lexical)?.context("canonical pending unavailable")?;
    assert_eq!(loaded.root,canonical);
    #[cfg(windows)] assert_ne!(lexical,canonical);
    let mut foreign=record.clone(); foreign.root=canonical.join("different-root");
    std::fs::write(path(&lexical),serde_json::to_vec(&foreign)?)?;
    assert!(load(&lexical).is_err());
    std::fs::write(path(&lexical),b"invalid pending")?;
    assert!(load(&lexical).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_pending_does_not_authorize_a_symlink_alias() -> Result<()> {
    let temp=tempfile::tempdir()?;
    let base=temp.path().canonicalize()?;
    let root=base.join("real-root"); std::fs::create_dir(&root)?;
    save(&original(&root),&mut None)?;
    let alias=base.join("alias");
    std::os::unix::fs::symlink(&root,&alias)?;
    assert!(load(&alias).is_err());
    assert!(reject_generic_recovery(&alias).is_err());
    Ok(())
}

#[test]
fn journal_is_secret_free_exact_owner_and_cas_bound() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); let mut digest=None;
    save(&record,&mut digest)?;
    record.stage=Stage::Sent; save(&record,&mut digest)?;
    let (actual,observed)=load(&root)?.context("missing original")?;
    assert!(actual.stage == Stage::Sent && actual.operation_id == record.operation_id);
    assert_eq!(Some(observed),digest);
    let bytes=std::fs::read(path(&root))?;
    let json:serde_json::Value=serde_json::from_slice(&bytes)?;
    assert!(json.get("super_password").is_none() && json.get("secret").is_none());
    let mut other=original(&root); other.stage=Stage::Sent;
    assert!(save(&other,&mut None).is_err());
    assert_eq!(std::fs::read(path(&root))?,bytes);
    let mut changed=json; changed["super_password"]=serde_json::json!("synthetic-only");
    std::fs::write(path(&root),serde_json::to_vec(&changed)?)?;
    assert!(load(&root).is_err());
    assert!(save(&record,&mut digest).is_err());
    Ok(())
}
#[test]
fn record_rejects_changed_selection_and_phase_replay() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); let mut digest=None;
    save(&record,&mut digest)?;
    let original_id=record.operation_id.clone();
    record.operation_id="not-original-operation".into(); record.stage=Stage::Sent;
    assert!(save(&record,&mut digest).is_err());
    record.operation_id=original_id;
    save(&record,&mut digest)?;
    record.source=root.join("other");
    assert!(save(&record,&mut digest).is_err());
    record.source=root.clone(); record.session_sha256=Some("a".repeat(64)); record.stage=Stage::Starting;
    save(&record,&mut digest)?;
    record.stage=Stage::Sent;
    assert!(save(&record,&mut digest).is_err());
    Ok(())
}
#[test]
fn rollback_evidence_retains_original_context_and_cannot_replay() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); let mut digest=None;
    save(&record,&mut digest)?;
    record.stage=Stage::Sent; save(&record,&mut digest)?;
    record.was_serving=true;
    assert!(save(&record,&mut digest).is_err());
    record.was_serving=false; record.stage=Stage::RolledBack; save(&record,&mut digest)?;
    assert!(require_startable(record.stage).is_err());
    record.stage=Stage::Sent;
    assert!(save(&record,&mut digest).is_err());
    record.stage=Stage::RollbackFinished;
    assert!(save(&record,&mut digest).is_err());
    record.stage=Stage::RolledBack; claim_rollback(&mut record,&mut digest)?;
    record.stage=Stage::RollbackFinished; save(&record,&mut digest)?;
    assert!(remove_rollback(&record,digest.as_deref().context("synthetic journal digest missing")?).is_err());
    record.stage=Stage::RollbackHandedOff; save(&record,&mut digest)?;
    assert!(remove_rollback(&record,"foreign-digest").is_err());
    remove_rollback(&record,digest.as_deref().context("synthetic journal digest missing")?)?;
    assert!(!path(&root).exists());
    Ok(())
}
#[test]
fn prepared_retry_keeps_original_and_query_never_sends() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); let mut digest=None;
    record.force=true;
    save(&record,&mut digest)?;
    let original_id=record.operation_id.clone();
    let original_token=record.token.clone();
    // A successfully rebound Paused marker is persisted before retrying a
    // failed drain. Prepared proves that no keygen exchange has begun.
    record_marker_intent(&mut record,&mut digest,Some(&"b".repeat(64)),&"c".repeat(64))?;
    record.marker_sha256="c".repeat(64); record.pending_marker_sha256=None; save(&record,&mut digest)?;
    let (mut recovered,observed)=load(&root)?.context("prepared original missing")?;
    assert_eq!(recovered.operation_id,original_id);
    assert_eq!(recovered.token,original_token);
    assert!(recovered.force);
    assert_eq!(recovered.marker_sha256,record.marker_sha256);
    assert!(keygen_sends_original(recovered.stage,true).is_err());
    assert!(keygen_sends_original(recovered.stage,false)?);
    let mut changed=recovered.clone(); changed.force=false;
    assert!(save(&changed,&mut Some(observed.clone())).is_err());
    changed=recovered.clone(); changed.original_identity=HostAuthIdentity::Unbound {
        operation_id:original_id,session_sha256:"d".repeat(64) };
    assert!(save(&changed,&mut Some(observed.clone())).is_err());
    let material=serde_json::json!({"vault_version":4,"kdf_salt":"synthetic-salt",
        "wrapped_urk":"synthetic-wrapped","urk_nonce":"synthetic-nonce",
        "keyring_account":"login-synthetic","keyring_backend":"native",
        "auth_operation_id":recovered.operation_id});
    changed=recovered.clone(); changed.material=Some(material.clone()); changed.stage=Stage::Sent;
    assert!(save(&changed,&mut digest).is_err());
    changed.stage=Stage::Prepared;
    changed.material.as_mut().context("synthetic material missing")?["auth_operation_id"]=serde_json::json!(uuid::Uuid::new_v4().to_string());
    assert!(save(&changed,&mut digest).is_err());
    recovered.material=Some(material);
    save(&recovered,&mut digest)?;
    assert_eq!(load(&root)?.context("prepared material missing")?.0.material,recovered.material);
    for stage in [Stage::Prepared,Stage::Sent] {
        recovered.stage=stage; save(&recovered,&mut digest)?;
        changed=recovered.clone(); changed.material=None;
        assert!(save(&changed,&mut digest).is_err());
        changed.material=recovered.material.clone();
        changed.material.as_mut().context("synthetic material missing")?["wrapped_urk"]=serde_json::json!("changed");
        assert!(save(&changed,&mut digest).is_err());
    }
    assert_eq!(load(&root)?.context("sent material missing")?.0.material,recovered.material);
    for query_only in [false,true] {
        assert!(!keygen_sends_original(Stage::Sent,query_only)?);
        assert!(!keygen_sends_original(Stage::Committed,query_only)?);
        for stage in [Stage::Starting,Stage::Verified,Stage::Finished,Stage::HandedOff,
            Stage::RolledBack,Stage::RollbackStarting,Stage::RollbackFinished,Stage::RollbackHandedOff] {
            assert!(keygen_sends_original(stage,query_only).is_err());
        }
    }
    recovered.stage=Stage::Prepared;
    assert!(save(&recovered,&mut digest).is_err());
    assert_eq!(load(&root)?.context("sent original missing")?.0.stage,Stage::Sent);
    Ok(())
}
#[test]
fn reconciled_rollback_claim_is_durable_for_serving_and_idle_originals() -> Result<()> {
    for was_serving in [false,true] {
        let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
        let mut record=original(&root); let mut digest=None;
        record.was_serving=was_serving;
        save(&record,&mut digest)?;
        record.stage=Stage::Sent; save(&record,&mut digest)?;
        let receipt=HostAuthResult {operation_id:record.operation_id.clone(),
            outcome:HostAuthOutcome::RolledBack,path:root.join("session.json"),
            session_sha256:None,super_password:None};
        rollback_receipt(&record,&receipt,&HostAuthIdentity::Absent)?;
        record.stage=Stage::RolledBack; save(&record,&mut digest)?;
        let before=std::fs::read(path(&root))?;
        let mut stale=record.clone();
        assert!(claim_rollback(&mut stale,&mut Some("foreign-digest".into())).is_err());
        assert_eq!(std::fs::read(path(&root))?,before);
        let (mut recovered,observed)=load(&root)?.context("rollback original missing")?;
        claim_rollback(&mut recovered,&mut Some(observed))?;
        let (mut interrupted,observed)=load(&root)?.context("rollback claim missing")?;
        assert_eq!(interrupted.stage,Stage::RollbackStarting);
        assert_eq!(interrupted.was_serving,was_serving);
        assert!(claim_rollback(&mut interrupted,&mut Some(observed)).is_err());
        assert!(require_startable(interrupted.stage).is_err());
        assert!(keygen_sends_original(interrupted.stage,false).is_err());
        assert!(rollback_receipt(&interrupted,&receipt,&HostAuthIdentity::Absent).is_err());
    }
    Ok(())
}
#[test]
fn rollback_reconciliation_rejects_unknown_foreign_and_committed_evidence() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    let mut record=original(&root); record.stage=Stage::Sent;
    let original_id=record.operation_id.clone();
    let fresh=|| HostAuthResult {operation_id:original_id.clone(),outcome:HostAuthOutcome::RolledBack,
        path:root.join("session.json"),session_sha256:None,super_password:None};
    let valid=fresh();
    rollback_receipt(&record,&valid,&HostAuthIdentity::Absent)?;
    let mut changed=fresh(); changed.operation_id=uuid::Uuid::new_v4().to_string();
    assert!(rollback_receipt(&record,&changed,&HostAuthIdentity::Absent).is_err());
    changed=fresh(); changed.path=root.join("other/session.json");
    assert!(rollback_receipt(&record,&changed,&HostAuthIdentity::Absent).is_err());
    changed=fresh(); changed.session_sha256=Some("a".repeat(64));
    assert!(rollback_receipt(&record,&changed,&HostAuthIdentity::Absent).is_err());
    changed=fresh(); changed.super_password=Some("synthetic-must-not-be-retained".into());
    assert!(rollback_receipt(&record,&changed,&HostAuthIdentity::Absent).is_err());
    for outcome in [HostAuthOutcome::Unknown,HostAuthOutcome::Committed] {
        changed=fresh(); changed.outcome=outcome;
        assert!(rollback_receipt(&record,&changed,&HostAuthIdentity::Absent).is_err());
    }
    assert!(rollback_receipt(&record,&valid,&HostAuthIdentity::Unbound {
        operation_id:record.operation_id.clone(),session_sha256:"a".repeat(64) }).is_err());
    for stage in [Stage::Prepared,Stage::Committed,Stage::Starting,Stage::Verified,Stage::Finished,
        Stage::HandedOff,Stage::RolledBack,Stage::RollbackStarting,Stage::RollbackFinished,Stage::RollbackHandedOff] {
        record.stage=stage;
        assert!(rollback_receipt(&record,&valid,&HostAuthIdentity::Absent).is_err());
    }
    Ok(())
}
#[test]
fn marker_intent_cannot_escape_pre_effect_stages_or_authorize_foreign_hashes() -> Result<()> {
    let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
    for stage in [Stage::Prepared,Stage::RolledBack] {
        let mut record=original(&root); record.stage=stage;
        let mut pending=record.clone(); pending.pending_marker_sha256=Some("c".repeat(64));
        validate_record(&pending,&root)?; validate_transition(&record,&pending)?;
        let mut target=pending.clone(); target.marker_sha256="c".repeat(64); target.pending_marker_sha256=None;
        validate_transition(&pending,&target)?;
        let mut foreign=pending.clone(); foreign.marker_sha256="d".repeat(64);
        assert!(validate_transition(&pending,&foreign).is_err());
        let mut abandoned=pending.clone(); abandoned.pending_marker_sha256=None;
        assert!(validate_transition(&pending,&abandoned).is_err());
        let mut effect=target.clone(); effect.stage=if stage == Stage::Prepared {Stage::Sent} else {Stage::RollbackStarting};
        assert!(validate_transition(&pending,&effect).is_err());
        validate_transition(&target,&effect)?;
        record.pending_marker_sha256=Some("not-a-digest".into());
        assert!(validate_record(&record,&root).is_err());
    }
    for stage in [Stage::Sent,Stage::Committed,Stage::Starting,Stage::Verified,Stage::Finished,
        Stage::HandedOff,Stage::RollbackStarting,Stage::RollbackFinished,Stage::RollbackHandedOff] {
        let mut record=original(&root); record.stage=stage; record.pending_marker_sha256=Some("c".repeat(64));
        assert!(validate_record(&record,&root).is_err());
        assert!(resume_prelaunch_maintenance(&mut record,&mut None).is_err());
    }
    Ok(())
}

#[test]
fn prelaunch_marker_rebind_recovers_process_and_publication_interruptions() -> Result<()> {
    const PHASE:&str="RSRS_TEST_KEYGEN_REBIND_PHASE";
    const STAGE:&str="RSRS_TEST_KEYGEN_REBIND_STAGE";
    fn child(phase:&str,stage:&str,root:&Path) -> Result<()> {
        let current=std::thread::current();
        let name=current.name().context("rebind test name missing")?;
        let output=std::process::Command::new(std::env::current_exe()?)
            .args(["--exact",name,"--nocapture"])
            .env(PHASE,phase).env(STAGE,stage).env("RSRS_DATA_DIR",root).output()?;
        anyhow::ensure!(output.status.success(),"rebind {stage}/{phase} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        anyhow::ensure!(String::from_utf8_lossy(&output.stdout).contains("keygen-rebind-cut-checked"),
            "rebind test child did not execute");
        Ok(())
    }
    let Ok(phase)=std::env::var(PHASE) else {
        for (stage,initial) in [("prepared","initial"),("prepared","initial_absent"),("rolled_back","initial")] {
            let temp=tempfile::tempdir()?; let root=temp.path().canonicalize()?;
            for phase in [initial,"intent_unknown","before_marker","after_marker","journal_failure","journal_unknown","recover"] {
                child(phase,stage,&root)?;
            }
        }
        return Ok(());
    };
    let _env=crate::runtime_fetch::runtime_fetch_tests::env_scope(&["RSRS_DATA_DIR","RSRS_RUNTIME_BOOT_TOKEN"])?;
    std::env::remove_var("RSRS_RUNTIME_BOOT_TOKEN");
    let root=PathBuf::from(std::env::var_os("RSRS_DATA_DIR").context("rebind root missing")?).canonicalize()?;
    let stage=std::env::var(STAGE)?;
    let marker=root.join("runtime/autostart/maintenance.json");
    if phase == "initial" || phase == "initial_absent" {
        let mut record=original(&root); let mut digest=None;
        let result=lifecycle::begin_offline_keygen_maintenance(&root,&root,|token,coordinator,marker| {
            record.token=token.to_owned(); record.coordinator=coordinator.to_owned(); record.marker_sha256=marker.to_owned();
            save(&record,&mut digest)?;
            anyhow::ensure!(phase != "initial_absent", "synthetic interruption before initial marker");
            Ok(())
        });
        if phase == "initial_absent" {
            assert!(result.is_err() && !marker.exists());
        } else {
            let maintenance=result?;
            if stage == "rolled_back" {
                record.stage=Stage::Sent; save(&record,&mut digest)?;
                record.stage=Stage::RolledBack; save(&record,&mut digest)?;
            }
            // Another process cannot rebind while the exact original controller lives.
            child("live_controller",&stage,&root)?;
            drop(maintenance);
        }
    } else {
        let (mut record,observed)=load(&root)?.context("rebind original missing")?;
        let original=record.clone(); let mut digest=Some(observed);
        let before=match std::fs::read(&marker) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        match phase.as_str() {
            "live_controller" => {
                let journal=std::fs::read(path(&root))?;
                assert!(resume_prelaunch_maintenance(&mut record,&mut digest).is_err());
                assert_eq!(std::fs::read(path(&root))?,journal);
                assert_eq!(std::fs::read(&marker).ok(),before);
            }
            "intent_unknown" => {
                FAIL_JOURNAL_AFTER_PUBLISH.with(|fail| fail.set(true));
                let error=resume_prelaunch_maintenance(&mut record,&mut digest).err().context("intent fault did not fire")?;
                assert!(error.to_string().contains("offline_keygen_record_durability_unknown"));
                assert_eq!(std::fs::read(&marker).ok(),before);
                assert!(load(&root)?.context("published intent missing")?.0.pending_marker_sha256.is_some());
            }
            "before_marker" => {
                let token=record.token.clone(); let coordinator=record.coordinator.clone();
                let expected=record.marker_sha256.clone(); let pending=record.pending_marker_sha256.clone();
                let result=lifecycle::resume_offline_keygen_prelaunch_maintenance(&root,&root,&token,&coordinator,
                    &expected,pending.as_deref(),record.stage == Stage::Prepared,|previous,target| {
                        record_marker_intent(&mut record,&mut digest,previous,target)?;
                        anyhow::bail!("synthetic error after intent but before marker publication")
                    });
                assert!(result.is_err());
                assert_eq!(std::fs::read(&marker).ok(),before);
                assert!(load(&root)?.context("durable intent missing")?.0.pending_marker_sha256.is_some());
            }
            "after_marker" => {
                lifecycle::fail_keygen_rebind_after_marker_for_test();
                assert!(resume_prelaunch_maintenance(&mut record,&mut digest).is_err());
                let actual=hash(&std::fs::read(&marker)?);
                let saved=load(&root)?.context("durable target missing")?.0;
                assert_ne!(actual,saved.marker_sha256);
                assert_eq!(saved.pending_marker_sha256.as_deref(),Some(actual.as_str()));
            }
            "journal_failure" => {
                let mut maintenance=resume_prelaunch_maintenance(&mut record,&mut digest)?;
                assert!(settle_marker_intent(&mut record,&mut Some("foreign-digest".into()),&mut maintenance).is_err());
                let actual=hash(&std::fs::read(&marker)?);
                let saved=load(&root)?.context("failed journal lost intent")?.0;
                assert_eq!(saved.pending_marker_sha256.as_deref(),Some(actual.as_str()));
            }
            "journal_unknown" => {
                let mut maintenance=resume_prelaunch_maintenance(&mut record,&mut digest)?;
                FAIL_JOURNAL_AFTER_PUBLISH.with(|fail| fail.set(true));
                let error=settle_marker_intent(&mut record,&mut digest,&mut maintenance).err().context("journal fault did not fire")?;
                assert!(error.to_string().contains("offline_keygen_record_durability_unknown"));
                let saved=load(&root)?.context("published convergence missing")?.0;
                assert!(saved.pending_marker_sha256.is_none());
                assert_eq!(saved.marker_sha256,hash(&std::fs::read(&marker)?));
            }
            "recover" => {
                let mut maintenance=resume_prelaunch_maintenance(&mut record,&mut digest)?;
                settle_marker_intent(&mut record,&mut digest,&mut maintenance)?;
                let (saved,_)=load(&root)?.context("settled original missing")?;
                assert!(saved.pending_marker_sha256.is_none());
                assert_eq!(saved.marker_sha256,hash(&std::fs::read(&marker)?));
                assert_eq!(saved.stage,original.stage);
                assert_eq!(saved.operation_id,original.operation_id);
                assert_eq!(saved.original_identity,original.original_identity);
                assert_eq!(saved.force,original.force); assert_eq!(saved.token,original.token);
                drop(maintenance);
                let journal=std::fs::read(path(&root))?;
                let mut foreign:serde_json::Value=serde_json::from_slice(&std::fs::read(&marker)?)?;
                foreign["owner_birth"]=serde_json::json!("foreign-controller");
                std::fs::write(&marker,serde_json::to_vec(&foreign)?)?;
                assert!(resume_prelaunch_maintenance(&mut record,&mut digest).is_err());
                assert_eq!(std::fs::read(path(&root))?,journal);
            }
            _ => anyhow::bail!("unknown synthetic rebind phase"),
        }
    }
    assert!(!root.join("session.json").exists() && !root.join("runtime/endpoint.json").exists()
        && !root.join("runtime/autostart/launch").exists());
    println!("keygen-rebind-cut-checked");
    Ok(())
}
include!("tests/terminal_receipts.rs");
