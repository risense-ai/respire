fn commit(journal: &mut Journal, index: usize, params: &HostSpaceParams) -> Result<()> {
    crate::service::ensure_writable()?;
    let record = &journal.operations[index];
    ensure!(read_original(&record.profile)?.as_deref().map(digest) == record.original_sha256, "space source changed before commit");
    let operation = op(journal,index)?;
    if let Some(target) = &operation.target {
        safe_path(target)?;
        let _lock = crate::lock::LibraryLock::acquire(target,std::time::Duration::from_secs(1))?;
        verify_join_candidate(journal,index,params)?;
        let current = read_original(target)?;
        ensure!(current.is_none() || current == record.candidate, "join destination no longer owned");
        let agent_path = target.join("agent.json");
        let agent = read_optional(&agent_path)?;
        let readonly = matches!(operation.intent,HostSpaceOperation::Join { readonly:true, .. });
        let expected = br#"{"readonly":true,"readonly_team":true}"#;
        ensure!(agent.is_none() || readonly && agent.as_deref() == Some(expected.as_slice()), "join agent configuration no longer owned");
        // Set the durable team gate before publishing the selectable session.
        if readonly { private_write(&agent_path,expected)?; }
        private_write(&target.join("session.json"),record.candidate.as_deref().context("join candidate missing")?)?;
        verify_join(journal,index,params)?;
    } else {
        let path = record.profile.join(MEMBERS);
        let current = read_optional(&path)?;
        ensure!(current == operation.members_original || current == operation.members_candidate, "space member record changed outside the original operation");
        private_write(&path,operation.members_candidate.as_deref().context("space member candidate missing")?)?;
    }
    if op(journal,index)?.kick.iter().any(|item| matches!(item.state,HostSpaceKickState::Unknown | HostSpaceKickState::Queued | HostSpaceKickState::RemotePending)) {
        journal.operations[index].phase = AuthPhase::RemoteUnknown;
        save_journal(journal)
    } else {
        journal.operations[index].phase = AuthPhase::Committed;
        retire_active(journal)
    }
}

fn result(journal: &Journal, index: usize, params: &HostSpaceParams) -> Result<HostSpaceResult> {
    let record = &journal.operations[index];
    let operation = op(journal,index)?;
    let committed = record.phase == AuthPhase::Committed;
    if committed && operation.target.is_some() { verify_join(journal,index,params)?; }
    let invite_code = if matches!(operation.intent,HostSpaceOperation::Invite { .. }) && committed {
        let code = params.invite_code.as_deref().context("host invite checkpoint required")?;
        ensure!(operation.code_sha256.as_deref() == Some(digest(code.as_bytes()).as_str()), "invite credential digest changed");
        Some(code.to_owned())
    } else { None };
    Ok(HostSpaceResult { operation_id: record.operation_id.clone(), fingerprint: record.fingerprint.clone(),
        profile: record.profile.clone(), identity: operation.source_identity.clone(), operation: operation.intent.clone(),
        outcome: if committed { HostSpaceOutcome::Committed } else if record.phase == AuthPhase::RolledBack { HostSpaceOutcome::Rejected }
        else if matches!(record.phase,AuthPhase::RemotePending | AuthPhase::RemoteUnknown) {
            HostSpaceOutcome::Unknown
        } else { HostSpaceOutcome::Pending }, invite_code, session_id: operation.session_id.clone(),
        target_profile: if record.phase == AuthPhase::RolledBack { None } else { operation.target.clone() },
        target_account_id: if committed { match &operation.intent { HostSpaceOperation::Join { user, .. } => Some(digest(user.as_bytes())), _ => None } } else { None },
        target_session_sha256: if committed && operation.target.is_some() { record.candidate_sha256.clone() } else { None },
        kick: operation.kick.clone(), remaining: if operation.target.is_none() && operation.members_candidate.is_some() {
            Some(members(operation.members_candidate.as_deref())?.len())
        } else { None },
    })
}

/// The caller has consumed a Paused launch grant and owns the source locks.
pub fn control_space(ownership: &AuthOwnership, id: &str, key: &str, params: &HostSpaceParams, query_only: bool) -> Result<HostSpaceResult> {
    selection(ownership,id,key,params)?;
    let (mut journal,index) = if query_only {
        let journal = load_journal()?;
        match owned_index(&journal,ownership,id,key,params)? {
            Some(index) => (journal,index),
            None => reject_unadmitted(ownership,id,key,params)?,
        }
    } else {
        match admit(ownership,id,key,params) {
            Ok(admission) => admission,
            Err(error) => match reject_unadmitted(ownership,id,key,params) {
                Ok(rejected) => rejected,
                Err(_) => return Err(error),
            },
        }
    };
    if journal.operations[index].phase == AuthPhase::RollingBack { reject_invite(&mut journal,index)?; }
    if matches!(params.operation,HostSpaceOperation::Invite { .. })
        && journal.operations[index].phase == AuthPhase::Authorized {
        if let Some(code) = params.invite_code.as_deref() {
            stage_invite_code(&mut journal,index,code,params)?;
        }
    }
    let mut private_code = None;
    let resume_join = matches!(params.operation,HostSpaceOperation::Join { .. }) && params.invite_code.is_some()
        && matches!(journal.operations[index].phase,AuthPhase::Admitted | AuthPhase::Authorized);
    if !query_only && journal.operations[index].phase == AuthPhase::Admitted || resume_join {
        match params.operation {
            HostSpaceOperation::Invite { .. } => private_code = invite(&mut journal,index,params)?,
            HostSpaceOperation::Join { .. } => {
                if let Err(error) = join(&mut journal,index,params) {
                    if !error.is::<RemoteRejected>() { return Err(error); }
                    ensure!(owned_index(&journal,ownership,id,key,params)? == Some(index), "rejected join owner changed");
                    let record = &journal.operations[index];
                    let operation = op(&journal,index)?;
                    let active = journal.active.as_ref().context("rejected join recovery missing")?;
                    let target = operation.target.as_ref().context("rejected join target missing")?;
                    safe_path(target)?;
                    ensure!(record.phase == AuthPhase::Admitted && record.candidate.is_none()
                        && record.candidate_sha256.is_none()
                        && !operation.credential_ready && operation.code_backend.is_none()
                        && operation.code_sha256.is_none() && operation.session_id.is_none()
                        && operation.members_candidate.is_none() && operation.kick.is_empty()
                        && active.backends.is_empty() && active.pass_backends.is_empty()
                        && active.totp_backends.is_empty() && !target.try_exists()?,
                        "rejected join may already own effects; retain the original operation");
                    // Publish the terminal receipt and clear only this admission in one journal write.
                    journal.operations[index].phase = AuthPhase::RolledBack;
                    retire_active(&mut journal)?;
                }
            }
            HostSpaceOperation::Kick { .. } => kick(&mut journal,index)?,
        }
    }
    // A query never sends a remote write. Checkpointed per-member outcomes survive interruption.
    if matches!(params.operation,HostSpaceOperation::Kick { .. }) && matches!(journal.operations[index].phase,AuthPhase::RemotePending | AuthPhase::RemoteUnknown) {
        prepare_kick(&mut journal,index)?;
    }
    if journal.operations[index].phase == AuthPhase::Prepared { commit(&mut journal,index,params)?; }
    let mut receipt = result(&journal,index,params)?;
    if private_code.is_some() { receipt.invite_code = private_code; }
    else if matches!(params.operation,HostSpaceOperation::Invite { .. })
        && journal.operations[index].phase == AuthPhase::Authorized {
        // The remote reply was received, but no host-owned code survived to finish it.
        receipt.outcome = HostSpaceOutcome::Unknown;
        receipt.session_id = None;
    }
    if matches!(params.operation, HostSpaceOperation::Invite { .. }) && receipt.outcome == HostSpaceOutcome::Committed {
        // The durable terminal record also reconciles GET after interrupted cleanup.
        let _ = invite_projection::pending::finish(ownership, id, key, params);
    }
    Ok(receipt)
}

