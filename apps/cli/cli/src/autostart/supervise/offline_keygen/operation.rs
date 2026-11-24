pub(crate) fn keygen(operation: Option<&str>, force: bool) -> Result<ResultEnvelope> {
    if let Some(id) = operation {
        anyhow::ensure!(canonical_id(id), "auth operation ID must be a canonical random UUID");
    }
    crate::runtime_policy::require_host("offline keygen handoff")?;
    anyhow::ensure!(lifecycle::current_boot_token().is_none(), "offline keygen requires its own controller");
    let (binary,image_sha256)=crate::runtime_fetch::require_offline_keygen_handoff()?;
    #[cfg(test)]
    tests::assert_no_forbidden_admission();
    let (root,profile)=respire_client_host::config::control_roots()?;
    let takeover=crate::runtime_policy::takeover_lock()?;
    if operation.is_none() { std::fs::create_dir_all(&profile)?; }
    let root=root.canonicalize()?; let profile=profile.canonicalize()?;
    let saved=load(&root)?;
    let (mut record,mut digest,mut maintenance)=if let Some((mut record,digest))=saved {
        anyhow::ensure!(record.force == force && operation.is_none_or(|id| id == record.operation_id),
            "offline keygen requires the original operation and force");
        let prepared=keygen_sends_original(record.stage,operation.is_some())?;
        verify_selection(&record)?;
        if prepared {
            anyhow::ensure!(control::identity(&profile)? == record.original_identity,
                "offline keygen prepared source identity changed");
            let health=crate::rpc::probe_runtime()?;
            verify_prepared_owner(&record,health.as_ref())?;
            let mut digest=Some(digest);
            let mut maintenance=resume_prelaunch_maintenance(&mut record,&mut digest)?;
            // Preserve the rebound marker before a drain can fail or time out.
            settle_marker_intent(&mut record,&mut digest,&mut maintenance)?;
            if let Some(health)=health.as_ref() { maintenance.drain_runtime(health)?; }
            (record,digest,maintenance)
        } else {
            lifecycle::verify_offline_keygen_marker(&root,&profile,&record.token,&record.coordinator,&record.marker_sha256)?;
            let maintenance=lifecycle::resume_key_maintenance(&root,&profile,&record.source,&record.token,&record.coordinator)?;
            (record,Some(digest),maintenance)
        }
    } else {
        anyhow::ensure!(operation.is_none(), "offline keygen original record missing; no operation admitted");
        respire_host_control::auth_material::prepare_source(&takeover)?;
        let health=crate::rpc::probe_runtime()?;
        anyhow::ensure!(health.is_some() || !crate::rpc::endpoint_path().try_exists()?, "offline keygen owner is unverifiable");
        if let Some(owner)=health.as_ref() {
            let manifest=crate::runtime_fetch::read_manifest()?.context("offline keygen Runtime manifest missing")?;
            let registration=super::super::read_registration(&root)?;
            let managed=registration.as_ref().filter(|config| config.enabled && !config.manually_stopped);
            let account=match control::identity(&profile)? {
                HostAuthIdentity::Bound { account_id, .. } => account_id,
                _ => hash(profile.display().to_string().as_bytes()),
            };
            super::launch_authorization::model_control::verify_model_owner(owner,&root,&profile,managed,&binary,&manifest,Some(&account))?;
        }
        let mut saved_record=None;
        let mut digest=None;
        let mut maintenance=lifecycle::begin_offline_keygen_maintenance(&root,&profile,|token,coordinator,marker_sha256| {
            let record=Original { schema:1,operation_id:uuid::Uuid::new_v4().to_string(),force,
                root:root.clone(),source:profile.clone(),profile:profile.clone(),binary:binary.clone(),image_sha256:image_sha256.clone(),
                token:token.to_owned(),coordinator:coordinator.to_owned(),marker_sha256:marker_sha256.to_owned(),pending_marker_sha256:None,
                original_identity:control::identity(&profile)?,session_sha256:None,stage:Stage::Prepared,material:None,actor:None,final_actor:None,
                was_serving:health.is_some(),store_ready:health.as_ref().is_some_and(|owner| owner.store_ready) };
            verify_selection(&record)?;
            save(&record,&mut digest)?;
            saved_record=Some(record);
            Ok(())
        })?;
        let record=saved_record.context("offline keygen admission missing")?;
        // The complete original operation is durable before draining any owner.
        verify_selection(&record)?;
        if let Some(health)=health.as_ref() { maintenance.drain_runtime(health)?; }
        (record,digest,maintenance)
    };
    verify_selection(&record)?;
    update_marker(&mut record,&mut maintenance)?;
    let send=keygen_sends_original(record.stage,operation.is_some())?;
    let mut material=if send { None } else {
        record.material.as_ref().map(respire_host_control::auth_material::load).transpose()?
    };
    if send {
        anyhow::ensure!(control::identity(&record.profile)? == record.original_identity, "offline keygen source identity changed");
        require_no_owner()?;
        material=Some(match record.material.as_ref() {
            Some(candidate) => respire_host_control::auth_material::load(candidate)?,
            None => {
                let generated=respire_host_control::auth_material::generate()?;
                respire_host_control::auth_material::persist(&generated, || {
                    record.material=Some(serde_json::json!({"vault_version":4,
                        "kdf_salt":generated.kdf_salt,"wrapped_urk":generated.wrapped_urk,
                        "urk_nonce":generated.urk_nonce,"keyring_account":generated.keyring_account,
                        "keyring_backend":generated.keyring_backend,"auth_operation_id":record.operation_id}));
                    save(&record,&mut digest)
                })?;
                generated
            }
        });
        record.stage=Stage::Sent;
    }
    save(&record,&mut digest)?; // sent + current controller durable before any exchange.
    let envelope=if send { control::auth_keygen_locked(&takeover,&mut maintenance,&record.operation_id,force,
        material.as_ref().context("offline keygen host material is missing")?)? }
        else { control::auth_result_locked(&takeover,&mut maintenance,&record.operation_id,force,material.as_ref())? };
    verify_selection(&record)?;
    let result:HostAuthResult=serde_json::from_value(envelope.summary.clone())?;
    anyhow::ensure!(result.operation_id == record.operation_id && result.path == record.profile.join("session.json"),
        "offline keygen result binding mismatch");
    if result.outcome == HostAuthOutcome::RolledBack {
        record_rollback(&mut record,&mut digest,&result)?;
        if send { finish_initial_rollback(&takeover,&mut record,&mut digest,&mut maintenance)?; }
        // Queries reconcile O but never restore a serving owner. Explicit Start
        // may consume the durable RolledBack pre-launch stage exactly once.
        let mut envelope=envelope;
        if !send { envelope.actions.push("runtime start".into()); }
        return Ok(envelope);
    }
    anyhow::ensure!(result.outcome == HostAuthOutcome::Committed, "offline keygen outcome unresolved; original operation retained");
    let session=result.session_sha256.context("offline keygen receipt digest missing")?;
    anyhow::ensure!(record.session_sha256.as_ref().is_none_or(|old| old == &session)
        && control::identity(&record.profile)? == (HostAuthIdentity::Unbound {operation_id:record.operation_id.clone(),session_sha256:session.clone()}),
        "offline keygen committed identity changed");
    let secret=result.super_password.filter(|s| !s.is_empty()).context("offline keygen recovery material missing")?;
    record.session_sha256=Some(session); record.stage=Stage::Committed;
    update_marker(&mut record,&mut maintenance)?; save(&record,&mut digest)?;
    // Deliberately retain Paused. No serving owner, automatic login or finish here.
    let mut output=ResultEnvelope::new("keygen",Status::Ok,serde_json::json!({"path":result.path,"super":secret}),
        vec![Item::new("keys",Status::Ok,result.path.display().to_string())]);
    output.actions.push("runtime start".into());
    Ok(output)
}

/// Private continuation-file snapshot of existing Health evidence. This is not
/// an RPC DTO or a new auth result and must never accept recovery material.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeIdentity {
    pub pid: u32,
    pub process_start_id: String,
    pub instance_id: String,
    pub generation: u64,
    pub image_path: String,
    pub image_sha256: String,
    pub installation_root: String,
    pub installation_id: String,
    pub profile: String,
    pub account_id: String,
}
