#[cfg(test)]
pub(super) fn assert_existing_recovery(root: &Path) -> Result<()> {
    let ownership = AuthOwnership::acquire(root,root)?;
    let original = serde_json::to_vec(&json!({"user":"space-fixture","addr":"https://never-contact.invalid",
        "token":"synthetic-unused-token"}))?;
    private_write(&root.join("session.json"),&original)?;
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    let rows = vec![Member { session_id: first.clone(),device_name:"member-first".into(),readonly:false,issued_at:"fixture".into() },
        Member { session_id: second.clone(),device_name:"member-second".into(),readonly:true,issued_at:"fixture".into() }];
    private_write(&root.join(MEMBERS),&serde_json::to_vec(&rows)?)?;
    let identity = HostAuthIdentity::Bound { account_id:digest(b"space-fixture"),session_sha256:digest(&original) };
    let mut params = HostSpaceParams { schema_version: HOST_SPACE_SCHEMA_V,
        binding: respire_protocol::ControlBinding { installation_root:root.to_owned(),profile:root.to_owned(),
            launch_id:uuid::Uuid::new_v4().to_string(),expected_account_id:Some(digest(b"space-fixture")) },
        identity:identity.clone(),source_identity:identity,operation:HostSpaceOperation::Kick { session_id:None,all:true },invite_code:None,material:None };
    let mut unsupported = serde_json::to_value(&params)?;
    unsupported["argv"] = json!(["unsupported"]);
    assert!(serde_json::from_value::<HostSpaceParams>(unsupported).is_err());
    let id = uuid::Uuid::new_v4().to_string();
    let before = std::fs::read(recovery_path())?;
    crate::service::set_workspace_mode("readonly")?;
    assert!(control_space(&ownership,&id,&id,&params,false).is_err());
    assert_eq!(std::fs::read(recovery_path())?,before);
    crate::service::set_workspace_mode("off")?;
    let config = std::fs::read(crate::service::client_config_path())?;
    let rejected_id = uuid::Uuid::new_v4().to_string();
    params.operation = HostSpaceOperation::Kick { session_id:Some(uuid::Uuid::new_v4().to_string()),all:false };
    assert!(control_space(&ownership,&rejected_id,&rejected_id,&params,false)?.outcome == HostSpaceOutcome::Rejected);
    assert!(control_space(&ownership,&rejected_id,&rejected_id,&params,true)?.outcome == HostSpaceOutcome::Rejected);
    assert!(!auth_recovery_pending()?);
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow!(error.to_string()))?;
    let addr = format!("http://{}",server.server_addr());
    let code = format!("{PREFIX}{}",hex::encode(serde_json::to_vec(&json!({"space":"rejected-join",
        "addr":addr,"user":"rejected-user","token":"revoked-token","super":"synthetic-super"}))?));
    params.operation = HostSpaceOperation::Join { invite_sha256:digest(code.as_bytes()),space:"rejected-join".into(),
        addr,user:"rejected-user".into(),readonly:false };
    let before_missing_material = std::fs::read(recovery_path())?;
    let missing_id = uuid::Uuid::new_v4().to_string();
    for query_only in [false,true] {
        assert!(control_space(&ownership,&missing_id,&missing_id,&params,query_only).is_err());
    }
    params.operation = HostSpaceOperation::Invite { note:None,readonly:false };
    for query_only in [false,true] {
        assert!(control_space(&ownership,&missing_id,&missing_id,&params,query_only).is_err());
    }
    assert_eq!(std::fs::read(recovery_path())?,before_missing_material);
    params.operation = HostSpaceOperation::Join { invite_sha256:digest(code.as_bytes()),space:"rejected-join".into(),
        addr: format!("http://{}",server.server_addr()),user:"rejected-user".into(),readonly:false };
    params.material = Some(HostSpaceMaterial { super_password:"synthetic-super".into(),
        keyring_account:format!("login-{}",uuid::Uuid::new_v4().simple()),keyring_backend:"native".into(),legacy_secret:None });
    params.invite_code = Some(code);
    let worker = std::thread::spawn(move || -> std::result::Result<tiny_http::Server,String> {
        let request = server.recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|error| error.to_string())?.ok_or("rejected join never fetched its identity")?;
        assert_eq!(request.method(),&tiny_http::Method::Get);
        assert_eq!(request.url(),"/api/self");
        request.respond(tiny_http::Response::empty(401)).map_err(|error| error.to_string())?;
        Ok(server)
    });
    let join_id = uuid::Uuid::new_v4().to_string();
    let rejected = control_space(&ownership,&join_id,&join_id,&params,false);
    let server = worker.join().map_err(|_| anyhow!("rejected join fixture panicked"))?
        .map_err(|error| anyhow!(error))?;
    let rejected = rejected?;
    assert!(rejected.outcome == HostSpaceOutcome::Rejected);
    assert_eq!(rejected.operation_id,join_id);
    assert!(rejected.target_profile.is_none() && rejected.target_account_id.is_none()
        && rejected.target_session_sha256.is_none());
    let rejected_journal = load_journal()?;
    let index = owned_index(&rejected_journal,&ownership,&join_id,&join_id,&params)?
        .context("rejected join receipt missing")?;
    assert!(rejected_journal.operations[index].phase == AuthPhase::RolledBack);
    assert!(rejected_journal.active.is_none() && !auth_recovery_pending()?);
    assert!(!root.join("accounts/rejected-join").try_exists()?);
    let rejected_bytes = std::fs::read(recovery_path())?;
    for query_only in [true,false] {
        let queried = control_space(&ownership,&join_id,&join_id,&params,query_only)?;
        assert_eq!(serde_json::to_value(&queried)?,serde_json::to_value(&rejected)?);
    }
    assert!(server.recv_timeout(std::time::Duration::from_millis(50))?.is_none(), "terminal join was sent again");
    assert_eq!(std::fs::read(recovery_path())?,rejected_bytes);
    assert_eq!(std::fs::read(root.join("session.json"))?,original);
    assert_eq!(std::fs::read(crate::service::client_config_path())?,config);
    params.invite_code = None;
    params.operation = HostSpaceOperation::Invite { note:None,readonly:false };
    let invite_id = uuid::Uuid::new_v4().to_string();
    let (mut interrupted,invite_index) = reject_unadmitted(&ownership,&invite_id,&invite_id,&params)?;
    let material = host_material(&params)?;
    interrupted.operations[invite_index].alias = material.keyring_account.clone();
    interrupted.operations[invite_index].backend = Some(material.keyring_backend.clone());
    interrupted.operations[invite_index].phase = AuthPhase::RemotePending;
    interrupted.active = Some(Recovery { directory:root.to_owned(),original:Some(original.clone()),
        alias:material.keyring_account.clone(),directory_existed:true,operation_id:Some(invite_id.clone()),
        owns_config:false,..Recovery::default() });
    save_journal(&mut interrupted)?;
    let unknown = std::fs::read(recovery_path())?;
    for query_only in [true,false] {
        let receipt = control_space(&ownership,&invite_id,&invite_id,&params,query_only)?;
        assert!(receipt.outcome == HostSpaceOutcome::Unknown && receipt.invite_code.is_none());
    }
    assert_eq!(std::fs::read(recovery_path())?,unknown);
    assert!(server.recv_timeout(std::time::Duration::from_millis(50))?.is_none(), "unknown invite was sent again");
    let checkpoint = format!("{PREFIX}{}",hex::encode(serde_json::to_vec(&json!({"space":"space-fixture",
        "addr":"https://never-contact.invalid","user":"space-fixture","token":"synthetic-token","super":"synthetic-super"}))?));
    op_mut(&mut interrupted,invite_index)?.code_sha256 = Some(digest(b"different-code"));
    assert!(stage_invite_code(&mut interrupted,invite_index,&checkpoint,&params).is_err());
    interrupted.operations[invite_index].phase = AuthPhase::RolledBack;
    retire_active(&mut interrupted)?;
    params.material = None;
    params.operation = HostSpaceOperation::Kick { session_id:None,all:true };
    let (mut journal,index) = admit(&ownership,&id,&id,&params)?;
    op_mut(&mut journal,index)?.kick[0].state = HostSpaceKickState::Revoked;
    op_mut(&mut journal,index)?.kick[1].state = HostSpaceKickState::RemotePending;
    journal.operations[index].phase = AuthPhase::RemotePending;
    save_journal(&mut journal)?;
    // Recovery must not route a space write through keygen/login rollback.
    ownership.recover(&params.identity)?;
    drop(ownership);
    assert!(recover_interrupted().is_err());
    let ownership = AuthOwnership::acquire(root,root)?;
    let receipt = control_space(&ownership,&id,&id,&params,true)?;
    assert!(receipt.outcome == HostSpaceOutcome::Unknown && receipt.remaining == Some(1));
    assert!(receipt.kick[0].state == HostSpaceKickState::Revoked && receipt.kick[1].state == HostSpaceKickState::Unknown);
    assert_eq!(members(read_optional(&root.join(MEMBERS))?.as_deref())?[0].session_id,second);
    // Repeating even the execute method cannot repeat the unknown remote POST.
    assert!(control_space(&ownership,&id,&id,&params,false)?.outcome == HostSpaceOutcome::Unknown);
    assert_eq!(load_journal()?.active.as_ref().and_then(|active| active.operation_id.as_deref()),Some(id.as_str()));
    assert!(control_space(&ownership,&id,"foreign-key",&params,true).is_err());
    params.operation = HostSpaceOperation::Kick { session_id:Some(first),all:false };
    assert!(control_space(&ownership,&id,&id,&params,true).is_err());
    params.operation = HostSpaceOperation::Kick { session_id:None,all:true };
    params.source_identity = HostAuthIdentity::Absent;
    assert!(control_space(&ownership,&id,&id,&params,true).is_err());
    assert_eq!(std::fs::read(root.join("session.json"))?,original);
    assert_eq!(std::fs::read(crate::service::client_config_path())?,config);
    assert!(auth_recovery_pending()?);
    let code = format!("{PREFIX}{}",hex::encode(serde_json::to_vec(&json!({"space":"joined-space","addr":"https://never-contact.invalid",
        "user":"joined-user","token":"synthetic-unused-token","super":"synthetic-unused-factor","readonly":true}))?));
    let mut intent = HostSpaceOperation::Join { invite_sha256:digest(code.as_bytes()),space:"joined-space".into(),
        addr:"https://never-contact.invalid".into(),user:"joined-user".into(),readonly:true };
    check_code(&intent,&code)?;
    if let HostSpaceOperation::Join { readonly, .. } = &mut intent { *readonly = false; }
    assert!(check_code(&intent,&code).is_err());
    assert!(members(Some(b"not json")).is_err());
    Ok(())
}
