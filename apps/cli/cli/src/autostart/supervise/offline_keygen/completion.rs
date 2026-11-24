pub(crate) fn verify_target(root:&Path,profile:&Path,operation:&str,session:&str) -> Result<RuntimeIdentity> {
    anyhow::ensure!(control::identity(profile)? == (HostAuthIdentity::Unbound {
        operation_id:operation.to_owned(),session_sha256:session.to_owned()}), "offline keygen identity changed");
    verify_original_target(root,profile,&HostAuthIdentity::Unbound {operation_id:operation.to_owned(),session_sha256:session.to_owned()},true)
}
pub(crate) fn verify_original_target(root:&Path,profile:&Path,identity:&HostAuthIdentity,store_ready:bool) -> Result<RuntimeIdentity> {
    anyhow::ensure!(&control::identity(profile)? == identity, "offline keygen rollback source identity changed");
    let health=crate::net_rpc::health()?; crate::net_rpc::request_scope(&health)?;
    let (binary,_)=control::installed_auth_runtime()?;
    let manifest=crate::runtime_fetch::read_manifest()?.context("offline keygen Runtime manifest missing")?;
    let registration=super::super::read_registration(root)?;
    let managed=registration.as_ref().filter(|config| config.enabled && !config.manually_stopped);
    let account=match identity { HostAuthIdentity::Bound {account_id,..} => account_id.clone(),
        _ => hash(profile.display().to_string().as_bytes()) };
    super::launch_authorization::model_control::verify_model_owner(&health,root,profile,managed,&binary,&manifest,Some(&account))?;
    anyhow::ensure!(health.store_ready == store_ready && health.server == "respire" && !health.instance_id.is_empty(),
        "offline keygen store is not ready");
    Ok(RuntimeIdentity { pid:health.pid,process_start_id:health.process_start_id,
        instance_id:health.instance_id,generation:health.generation,image_path:health.image_path,
        image_sha256:health.image_sha256,installation_root:health.installation_root,
        installation_id:health.installation_id,profile:health.data_dir,account_id:health.account_id })
}

fn require_no_owner() -> Result<()> {
    anyhow::ensure!(crate::rpc::probe_runtime()?.is_none() && !crate::rpc::endpoint_path().try_exists()?,
        "offline keygen live or unknown owner exists; no spawn or write attempted");
    Ok(())
}

fn verify_prepared_owner(record:&Original,health:Option<&crate::net_rpc::Health>) -> Result<()> {
    let Some(health)=health else { return require_no_owner(); };
    anyhow::ensure!(record.was_serving && health.store_ready == record.store_ready,
        "offline keygen prepared operation gained a different owner");
    let manifest=crate::runtime_fetch::read_manifest()?.context("offline keygen Runtime manifest missing")?;
    let registration=super::super::read_registration(&record.root)?;
    let managed=registration.as_ref().filter(|config| config.enabled && !config.manually_stopped);
    let account=match &record.original_identity {
        HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
        _ => hash(record.profile.display().to_string().as_bytes()),
    };
    super::launch_authorization::model_control::verify_model_owner(health,&record.root,&record.profile,
        managed,&record.binary,&manifest,Some(&account))
}

fn rollback_receipt(record:&Original,result:&HostAuthResult,identity:&HostAuthIdentity) -> Result<()> {
    anyhow::ensure!(record.stage == Stage::Sent && result.outcome == HostAuthOutcome::RolledBack
        && result.operation_id == record.operation_id && result.path == record.profile.join("session.json")
        && result.session_sha256.is_none() && result.super_password.is_none()
        && identity == &record.original_identity,
        "offline keygen rollback original receipt or identity changed");
    Ok(())
}
fn record_rollback(record:&mut Original,digest:&mut Option<String>,result:&HostAuthResult) -> Result<()> {
    rollback_receipt(record,result,&control::identity(&record.profile)?)?;
    record.stage=Stage::RolledBack;
    save(record,digest)
}
fn claim_rollback(record:&mut Original,digest:&mut Option<String>) -> Result<()> {
    anyhow::ensure!(record.stage == Stage::RolledBack,
        "offline keygen rollback already attempted; no completion or spawn retried");
    // Fence completion as well as spawning, even if the original had no owner.
    record.stage=Stage::RollbackStarting;
    save(record,digest)
}

fn finish_initial_rollback(takeover:&respire_client_host::lock::LibraryLock,record:&mut Original,
    digest:&mut Option<String>,maintenance:&mut lifecycle::Maintenance) -> Result<()> {
    verify_selection(record)?;
    anyhow::ensure!(takeover.protects(&record.root.join("runtime/takeover"))?, "offline keygen takeover mismatch");
    anyhow::ensure!(control::identity(&record.profile)? == record.original_identity,
        "offline keygen rollback original identity changed");
    lifecycle::verify_offline_keygen_marker(&record.root,&record.profile,&record.token,&record.coordinator,&record.marker_sha256)?;
    require_no_owner()?;
    if let Some(candidate)=record.material.as_ref() {
        let account=candidate["keyring_account"].as_str().context("offline keygen cleanup account missing")?;
        let backend=candidate["keyring_backend"].as_str().context("offline keygen cleanup backend missing")?;
        if respire_client_host::host_credentials::read_auth_super(account,backend)?.is_some() {
            let material=respire_host_control::auth_material::load(candidate)?;
            respire_host_control::auth_material::remove(&material,None)?;
        } else {
            anyhow::ensure!(respire_client_host::host_credentials::read_auth_password(account,backend)?.is_none(),
                "offline keygen cleanup found a foreign password slot; original operation retained");
        }
    }
    claim_rollback(record,digest)?;
    if record.was_serving {
        maintenance.start_account_runtime()?;
        let actor=verify_original_target(&record.root,&record.profile,&record.original_identity,record.store_ready)?;
        let binding=maintenance.offline_keygen_start_binding(&record.profile,&record.token,&record.coordinator)?;
        let launch=launch::Evidence::capture(record,&actor,&binding)?;
        let process=crate::net_rpc::runtime_process_identity(actor.pid)?;
        maintenance.offline_keygen_rollback_verified(&record.profile,&record.original_identity,record.store_ready)?;
        finish_checked(&actor,|| {
            launch.verify()?;
            verify_selection(record)?;
            anyhow::ensure!(takeover.protects(&record.root.join("runtime/takeover"))?
                && load(&record.root)?.is_some_and(|(_,actual)| Some(actual) == *digest)
                && crate::net_rpc::runtime_process_identity(actor.pid)? == process,
                "offline keygen rollback evidence changed before completion");
            let live=verify_original_target(&record.root,&record.profile,&record.original_identity,record.store_ready)?;
            anyhow::ensure!(crate::net_rpc::runtime_process_identity(actor.pid)? == process
                && crate::net_rpc::process_is_running(actor.pid)?,
                "offline keygen rollback OS actor changed during completion");
            Ok(live)
        },|| {
            maintenance.verify_offline_keygen_completion(&record.profile)?;
            maintenance.finish()
        })?;
        record.actor=Some(actor);
    } else {
        maintenance.finish()?;
    }
    lifecycle::verify_offline_keygen_finished(&record.root)?;
    record.stage=Stage::RollbackFinished; save(record,digest)?;
    maintenance.finish_handoff()?;
    record.stage=Stage::RollbackHandedOff; save(record,digest)?;
    remove_rollback(record,digest.as_deref().context("offline keygen rollback cleanup digest missing")?)
}
fn remove_rollback(record:&Original,digest:&str) -> Result<()> {
    anyhow::ensure!(record.stage == Stage::RollbackHandedOff, "offline keygen rollback cleanup is premature");
    let file=path(&record.root); no_links(&file)?;
    let (_,actual)=load(&record.root)?.context("offline keygen rollback evidence missing")?;
    anyhow::ensure!(actual == digest, "offline keygen rollback evidence changed");
    std::fs::remove_file(&file)?;
    respire_spawn::durable::sync_directory(file.parent().context("offline keygen rollback cleanup parent missing")?)?;
    Ok(())
}

/// The Paused auth.result has already proved the original operation/key/force and
/// committed candidate before launch. Runtime rechecks that Unbound identity
/// after taking the library lock and holds it through serving. Do not acquire a
/// second library lock or introduce a completion RPC beside that live actor.
fn complete(takeover: &respire_client_host::lock::LibraryLock,
    maintenance: &mut lifecycle::Maintenance, record: &Original, actor: &RuntimeIdentity,
    launch: &launch::Evidence,
) -> Result<()> {
    anyhow::ensure!(record.stage == Stage::Verified && record.actor.as_ref() == Some(actor),
        "offline keygen completion lacks its durable verified actor");
    let process = crate::net_rpc::runtime_process_identity(actor.pid)?;
    finish_checked(actor, || {
        launch.verify()?;
        verify_selection(record)?;
        anyhow::ensure!(takeover.protects(&record.root.join("runtime/takeover"))?,
            "offline keygen takeover mismatch");
        let (durable, _) = load(&record.root)?.context("offline keygen original evidence missing")?;
        anyhow::ensure!(serde_json::to_value(&durable)? == serde_json::to_value(record)?,
            "offline keygen original evidence changed during completion");
        let session = record.session_sha256.as_deref().context("offline keygen committed digest missing")?;
        anyhow::ensure!(crate::net_rpc::runtime_process_identity(actor.pid)? == process,
            "offline keygen OS actor changed before completion check");
        let live = verify_target(&record.root, &record.profile, &record.operation_id, session)?;
        anyhow::ensure!(crate::net_rpc::runtime_process_identity(actor.pid)? == process
            && crate::net_rpc::process_is_running(actor.pid)?,
            "offline keygen OS actor changed during completion check");
        Ok(live)
    }, || {
        maintenance.verify_offline_keygen_completion(&record.profile)?;
        maintenance.finish()
    })?;
    lifecycle::verify_offline_keygen_finished(&record.root)
}

/// Retain the expected exact actor across the local state transition. A failed
/// post-check is an uncertain completion, never permission to spawn or replay O.
fn finish_checked(actor: &RuntimeIdentity,
    mut observe: impl FnMut() -> Result<RuntimeIdentity>, finish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    anyhow::ensure!(observe()? == *actor, "offline keygen actor changed before completion");
    finish()?;
    anyhow::ensure!(observe()? == *actor, "offline keygen actor changed during completion");
    Ok(())
}

/// Authentication status alone is not live/terminal evidence. This admission
/// runs only after the original Paused auth.result exchange succeeds.
fn committed_start_session(record: &Original, receipt: &HostAuthResult,
    identity: &HostAuthIdentity,
) -> Result<String> {
    require_startable(record.stage)?;
    anyhow::ensure!(receipt.outcome == HostAuthOutcome::Committed
        && receipt.operation_id == record.operation_id
        && receipt.path == record.profile.join("session.json"),
        "offline keygen original is not committed");
    let session = receipt.session_sha256.as_deref().filter(|value| valid_digest(value))
        .context("offline keygen receipt digest missing or invalid")?;
    anyhow::ensure!(record.session_sha256.as_deref().is_none_or(|saved| saved == session)
        && identity == &(HostAuthIdentity::Unbound {
            operation_id: record.operation_id.clone(), session_sha256: session.to_owned(),
        }), "offline keygen original identity changed");
    Ok(session.to_owned())
}

/// Only the user-facing Start dispatcher calls this with its existing takeover.
/// An uncertain prior spawn is never retried here.
pub(crate) fn start_explicit(takeover:&respire_client_host::lock::LibraryLock) -> Result<bool> {
    let (root,profile)=respire_client_host::config::control_roots()?;
    let Some((mut record,digest))=load(&root)? else { return Ok(false); };
    let root=root.canonicalize()?; let profile=profile.canonicalize()?;
    anyhow::ensure!(takeover.protects(&root.join("runtime/takeover"))?, "offline keygen takeover mismatch");
    if record.stage == Stage::RollbackHandedOff {
        verify_selection(&record)?; lifecycle::verify_offline_keygen_finished(&root)?;
        anyhow::ensure!(control::identity(&profile)? == record.original_identity,
            "offline keygen rolled-back identity changed");
        if record.was_serving {
            anyhow::ensure!(completed_actor(&record)? == &verify_original_target(&root,&profile,&record.original_identity,record.store_ready)?,
                "offline keygen rollback actor changed; no spawn retried");
        } else {
            anyhow::ensure!(crate::rpc::probe_runtime()?.is_none() && !crate::rpc::endpoint_path().try_exists()?,
                "offline keygen rollback gained an unknown owner");
        }
        remove_rollback(&record,&digest)?;
        return Ok(record.was_serving);
    }
    if record.stage == Stage::HandedOff {
        verify_selection(&record)?;
        lifecycle::verify_offline_keygen_finished(&root)?;
        let session=record.session_sha256.as_deref().context("offline keygen completed digest missing")?;
        anyhow::ensure!(completed_actor(&record)? == &verify_target(&root,&profile,&record.operation_id,session)?,
            "offline keygen finished actor changed; no spawn attempted");
        remove_finished(&record,&digest)?;
        return Ok(true);
    }
    if record.stage == Stage::RolledBack {
        verify_selection(&record)?;
        anyhow::ensure!(control::identity(&profile)? == record.original_identity,
            "offline keygen rollback original identity changed");
        require_no_owner()?;
        let mut digest=Some(digest);
        let mut maintenance=resume_prelaunch_maintenance(&mut record,&mut digest)?;
        settle_marker_intent(&mut record,&mut digest,&mut maintenance)?;
        finish_initial_rollback(takeover,&mut record,&mut digest,&mut maintenance)?;
        return Ok(record.was_serving);
    }
    require_startable(record.stage)?;
    verify_selection(&record)?;
    lifecycle::verify_offline_keygen_marker(&root,&profile,&record.token,&record.coordinator,&record.marker_sha256)?;
    anyhow::ensure!(crate::rpc::probe_runtime()?.is_none() && !crate::rpc::endpoint_path().try_exists()?,
        "offline keygen owner already exists; no spawn attempted");
    let mut maintenance=lifecycle::resume_key_maintenance(&root,&profile,&record.source,&record.token,&record.coordinator)?;
    let mut digest=Some(digest);
    update_marker(&mut record,&mut maintenance)?; save(&record,&mut digest)?;
    let material=record.material.as_ref().map(respire_host_control::auth_material::load).transpose()?;
    let envelope=control::auth_result_locked(takeover,&mut maintenance,&record.operation_id,record.force,material.as_ref())?;
    let receipt:HostAuthResult=serde_json::from_value(envelope.summary)
        .map_err(|_| anyhow::anyhow!("offline keygen query receipt invalid"))?;
    if receipt.outcome == HostAuthOutcome::RolledBack {
        record_rollback(&mut record,&mut digest,&receipt)?;
        finish_initial_rollback(takeover,&mut record,&mut digest,&mut maintenance)?;
        return Ok(record.was_serving);
    }
    let session = committed_start_session(&record, &receipt, &control::identity(&profile)?)?;
    // Never retain the auth.result recovery code, including in the continuation file.
    drop(receipt.super_password);
    record.session_sha256=Some(session.clone()); record.stage=Stage::Starting;
    update_marker(&mut record,&mut maintenance)?; save(&record,&mut digest)?;
    maintenance.start_account_runtime()?;
    let actor=verify_target(&root,&profile,&record.operation_id,&session)?;
    let binding = maintenance.offline_keygen_start_binding(&profile, &record.token, &record.coordinator)?;
    let launch = launch::Evidence::capture(&record, &actor, &binding)?;
    maintenance.offline_keygen_target_verified(&profile,&record.operation_id,&session)?;
    record.stage=Stage::Verified; record.actor=Some(actor.clone()); save(&record,&mut digest)?;
    complete(takeover,&mut maintenance,&record,&actor,&launch)?;
    record.stage=Stage::Finished; save(&record,&mut digest)?;
    maintenance.finish_handoff()?;
    record.stage=Stage::HandedOff; save(&record,&mut digest)?;
    remove_finished(&record,digest.as_deref().context("offline keygen cleanup digest missing")?)?;
    Ok(true)
}

/// Standalone CLI does not inherit an embedded host's compatibility handoff.
/// Recognize the record schema, but leave its successor and evidence untouched.
fn completed_actor(record:&Original) -> Result<&RuntimeIdentity> {
    anyhow::ensure!(record.final_actor.is_none(),
        "offline keygen embedded successor requires its original host; no owner adopted");
    record.actor.as_ref().context("offline keygen completed actor missing")
}

fn remove_finished(record:&Original,digest:&str) -> Result<()> {
    anyhow::ensure!(record.stage == Stage::HandedOff, "offline keygen cleanup requires completed handoff");
    let file=path(&record.root); no_links(&file)?;
    let (_,actual)=load(&record.root)?.context("offline keygen cleanup record disappeared")?;
    anyhow::ensure!(actual == digest, "offline keygen cleanup record changed");
    std::fs::remove_file(&file)?;
    respire_spawn::durable::sync_directory(file.parent().context("offline keygen cleanup parent missing")?)?;
    Ok(())
}

/// Hand the original Paused keygen controller to a separately durable account O.
/// The coordinator proves the original committed receipt; caller input is not proof.
/// New account authentication is sent only by the existing coordinator after this
/// function releases takeover. Pending records never store account credentials.
pub(crate) fn prepare_account(register:bool,user:&str,addr:&str) -> Result<()> {
    use respire_host_control::KeygenAccountTransfer;
    let (root,_) = respire_client_host::config::control_roots()?;
    // Ordinary account paths remain read-only/no-op here, including first use.
    // The shared coordinator repeats its pending fence under takeover.
    if load(&root)?.is_none() { return Ok(()); }
    let takeover=crate::runtime_policy::takeover_lock()?;
    let Some((mut record,observed))=load(&root)? else { return Ok(()); };
    let root=root.canonicalize()?;
    require_startable(record.stage)?;
    let (binary,image)=crate::runtime_fetch::require_offline_keygen_handoff()?;
    anyhow::ensure!(binary == record.binary && image == record.image_sha256,
        "offline keygen account transfer image changed");
    let source=|record:&Original| KeygenAccountTransfer {root:record.root.clone(),profile:record.profile.clone(),
        original_operation_id:record.operation_id.clone(),force:record.force,token:record.token.clone(),
        coordinator:record.coordinator.clone(),marker_sha256:record.marker_sha256.clone()};
    let accounts=super::launch_authorization::shared_host::accounts();
    // Account selection may already have moved after a lost response. Durable
    // mapping lookup comes before old-profile/Paused checks, and never sends auth.
    if let Some(admission)=accounts.lookup_existing_keygen_transfer(&takeover,&source(&record),register,user,addr)? {
        return remove_transferred(&record,&observed,&admission);
    }
    verify_selection(&record)?;
    lifecycle::verify_offline_keygen_marker(&root,&record.profile,&record.token,&record.coordinator,&record.marker_sha256)?;
    anyhow::ensure!(crate::rpc::probe_runtime()?.is_none() && !crate::rpc::endpoint_path().try_exists()?,
        "offline keygen account transfer cannot replace a live or unknown owner");
    let mut maintenance=lifecycle::resume_key_maintenance(&root,&record.profile,&record.source,&record.token,&record.coordinator)?;
    let mut digest=Some(observed);
    update_marker(&mut record,&mut maintenance)?; save(&record,&mut digest)?;
    // Drop only the CLI process-local active slot. The durable exact Paused
    // marker remains and the shared coordinator independently adopts it.
    drop(maintenance);
    let admission=accounts.prepare_keygen_account(&takeover,&source(&record),register,user,addr)?;
    remove_transferred(&record,digest.as_deref().context("offline keygen transfer digest missing")?,&admission)
}

fn remove_transferred(record:&Original,digest:&str,admission:&respire_host_control::KeygenAccountAdmission) -> Result<()> {
    anyhow::ensure!(admission.root() == record.root && admission.source_profile() == record.source
        && admission.keygen_operation_id() == record.operation_id && admission.force() == record.force
        && admission.maintenance_token() == record.token && canonical_id(admission.operation_id())
        && admission.operation_id() != record.operation_id && valid_digest(admission.session_sha256())
        && record.session_sha256.as_deref().is_none_or(|saved| saved == admission.session_sha256()),
        "offline keygen durable account transfer mismatch");
    let file=path(&record.root); no_links(&file)?;
    let (_,actual)=load(&record.root)?.context("offline keygen transfer record disappeared")?;
    anyhow::ensure!(actual == digest, "offline keygen transfer record changed");
    // Never remove the marker: account-active now owns the same maintenance token.
    std::fs::remove_file(&file)?;
    respire_spawn::durable::sync_directory(file.parent().context("offline keygen transfer parent missing")?)?;
    Ok(())
}
