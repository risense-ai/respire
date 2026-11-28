use super::*;

#[test]
fn entry_error_envelopes_match_blocking_and_deferred_dispatch() -> Result<()> {
    let (tx, queued) = mpsc::channel();
    for (id, version, method, code) in [
        ("good-id", PROTOCOL_V + 1, "cli.exec", "protocol_version_mismatch"),
        ("bad id", PROTOCOL_V, "cli.exec", "invalid_request_id"),
        ("good-id", PROTOCOL_V, "cli.exec", "scope_required"),
        ("good-id", PROTOCOL_V, "model.configure", "scope_required"),
    ] {
        let request: RpcRequest = serde_json::from_value(json!({
            "v":version, "id":id, "method":method, "args":[],
        }))?;
        let direct = dispatch(request.clone(), &tx);
        let (reply, received) = mpsc::channel();
        dispatch_deferred(request, &tx, move |response| { let _ = reply.send(response); });
        let deferred = received.recv_timeout(Duration::from_secs(1))?;
        assert_eq!(direct.code.as_deref(), Some(code));
        assert_eq!(serde_json::to_value(direct)?, serde_json::to_value(deferred)?);
    }
    assert!(matches!(queued.try_recv(), Err(mpsc::TryRecvError::Empty)));
    Ok(())
}

#[test]
fn active_execution_context_restores_nested_and_unwound_scope() {
    let initial_scope = ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone());
    let initial_sync = SYNC_CONTEXT.with(|context| context.get());
    let outer = RuntimeRequestScope {
        account_id: "outer-account".into(), instance_id: "outer-instance".into(), generation: 8,
    };
    let inner = RuntimeRequestScope {
        account_id: "inner-account".into(), instance_id: "inner-instance".into(), generation: 9,
    };
    {
        let _outer = ActiveExecutionContext::enter(&outer, 7);
        assert_eq!(ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone()), Some(outer.clone()));
        assert_eq!(SYNC_CONTEXT.with(|context| context.get()), Some((7, -1)));
        let unwound = std::panic::catch_unwind(|| {
            let _inner = ActiveExecutionContext::enter(&inner, 8);
            assert_eq!(ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone()), Some(inner.clone()));
            assert_eq!(SYNC_CONTEXT.with(|context| context.get()), Some((8, -1)));
            panic!("exercise execution context unwind");
        });
        assert!(unwound.is_err());
        assert_eq!(ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone()), Some(outer));
        assert_eq!(SYNC_CONTEXT.with(|context| context.get()), Some((7, -1)));
    }
    assert_eq!(ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone()), initial_scope);
    assert_eq!(SYNC_CONTEXT.with(|context| context.get()), initial_sync);
}

static DIRECT_GATE_BARRIER: std::sync::OnceLock<(String, Arc<std::sync::Barrier>)> = std::sync::OnceLock::new();
pub(super) fn wait_before_direct_gate(id: &str) {
    if let Some((_, barrier)) = DIRECT_GATE_BARRIER.get().filter(|(expected, _)| expected == id) {
        barrier.wait(); barrier.wait();
    }
}
fn fixture_scope(account: &str, generation: u64) -> RuntimeRequestScope {
    RuntimeRequestScope {
        account_id: account.to_owned(),
        instance_id: "instance-1".to_owned(),
        generation,
    }
}

#[test]
fn business_scope_rejects_missing_empty_and_stale_identity() {
    for scope in [None, Some(fixture_scope("", 0))] {
        let (jobs, queued) = std::sync::mpsc::channel();
        let (reply, received) = std::sync::mpsc::channel();
        dispatch_deferred(RpcRequest {
            v: PROTOCOL_V + 1,
            id: "wrong-version".to_owned(),
            method: "cli.exec".to_owned(),
            args: Vec::new(),
            scope,
            operation_id: None,
            idempotency_key: None,
            parent_operation_id: None,
            queue_wait_ms: None,
            lifecycle: None,
        }, &jobs, move |response| { let _ = reply.send(response); });
        assert!(matches!(received.try_recv(), Ok(response)
            if response.code.as_deref() == Some("protocol_version_mismatch")));
        assert!(matches!(queued.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    }
    let current = fixture_scope("account-a", 8);
    assert_eq!(scope_generation(&current), Ok(7));
    let initial: u64 = crate::rpc_worker::generation_on_wire(0);
    assert_eq!(initial, 1);
    assert_eq!(crate::rpc_worker::generation_on_wire(7), current.generation);
    assert_eq!(crate::rpc_worker::generation_on_wire(usize::MAX), usize::MAX as u64);
    assert!(scope_generation(&fixture_scope("account-a", 0)).is_err());
    assert!(matches!(validate_scope(None, &current), Err((code, _)) if code == "scope_required"));
    assert!(validate_scope(Some(&current), &current).is_ok());
    for stale in [
        fixture_scope("account-b", 8),
        fixture_scope("account-a", 7),
        fixture_scope("account-a", 0),
        fixture_scope("", 8),
        RuntimeRequestScope { instance_id: String::new(), ..current.clone() },
        RuntimeRequestScope { instance_id: "instance-2".to_owned(), ..current.clone() },
    ] {
        assert!(matches!(validate_scope(Some(&stale), &current), Err((code, _)) if code == "scope_mismatch"));
    }
    assert!(validate_scope(Some(&fixture_scope("", 8)), &fixture_scope("", 8)).is_err());
    for method in ["cli.exec", "model.configure", "model.switch", "model.resume", "model.prepare", "index.prepare", "model.control", "runtime.result"] {
        assert!(requires_scope(method), "{method} must be scoped");
    }
    for method in ["runtime.status", "model.catalog", "model.progress", "assets.status", "cli.progress"] {
        assert!(!requires_scope(method), "{method} must remain queryable during drain");
    }
}

#[test]
fn disabled_automatic_sync_does_not_wait_for_the_foreground_writer() -> Result<()> {
    let gate = shared_exclusive();
    let foreground = gate.acquire(true);
    let (result, receipt) = mpsc::channel();
    let attempt = std::thread::spawn(move || {
        let _ = result.send(automatic_sync_boundary(0, false));
    });
    let disabled = receipt.recv_timeout(Duration::from_secs(1));
    drop(foreground);
    attempt.join().map_err(|_| anyhow::anyhow!("automatic sync preflight panicked"))?;
    assert!(matches!(disabled, Ok(Ok(None))));
    Ok(())
}

#[test]
fn completed_sync_preserves_pending_work_after_host_observer_failure() {
    for due in [None, Some(Instant::now())] {
        let mut schedule = SyncSchedule {
            due,
            backoff: 3,
            blocked_generation: Some(7),
            manual: VecDeque::new(),
            last_success: None,
        };
        schedule.record_success(true);
        assert_eq!(schedule.due, due, "observer failure must not schedule a replay or discard new work");
        assert_eq!(schedule.blocked_generation, None, "core autosync must remain eligible");
        assert_eq!(schedule.backoff, 0);
        assert!(schedule.last_success.is_some(), "core sync already committed");
    }
}

#[test]
fn idempotent_admission_replays_or_reports_unknown() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    struct RestoreConfigEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreConfigEnv {
        fn drop(&mut self) {
            for (key, value) in [("RSRS_DATA_DIR", self.0.as_ref()), ("RSRS_M3_DIR", self.1.as_ref())] {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    let root = tempfile::tempdir()?;
    let _environment = RestoreConfigEnv(std::env::var_os("RSRS_DATA_DIR"), std::env::var_os("RSRS_M3_DIR"));
    std::env::set_var("RSRS_DATA_DIR", root.path());
    std::env::set_var("RSRS_M3_DIR", root.path().join("models"));
    fn request(id: &str, args: &[&str]) -> RpcRequest {
        RpcRequest {
            scope: Some(fixture_scope("account-a", 1)),
            v: PROTOCOL_V,
            id: id.to_owned(),
            operation_id: Some("operation-1".to_owned()),
            idempotency_key: Some("idempotency-1".to_owned()),
            parent_operation_id: None,
            method: "cli.exec".to_owned(),
            args: args.iter().map(|item| item.to_string()).collect(),
            queue_wait_ms: None,
            lifecycle: None,
        }
    }

    let first = request("request-1", &["remember", "value"]);
    for (error, code) in [
        ("runtime_maintenance: queued execution was fenced", "runtime_maintenance"),
        ("invalid_config: malformed client config", "invalid_config"),
        ("scope_mismatch: stale generation", "scope_mismatch"),
        ("runtime worker is gone", "daemon_unavailable"),
    ] {
        let response = command_response(&first, Err(error.to_owned()));
        assert!(!response.ok);
        assert_eq!(response.code.as_deref(), Some(code));
        assert_eq!(result_state(Some(&response), false), "failed");
    }
    let current = fixture_scope("account-a", 1);
    let Some(operation) = operation_identity_for_scope(&first, &current).ok() else {
        panic!("operation identity must be valid");
    };
    *CACHE.lock().unwrap_or_else(|error| error.into_inner()) = Vec::new();
    IDEMPOTENCY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
    OPERATION_KEYS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
    INFLIGHT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();

    let admission = match admit_scoped(&first, &operation, &current) {
        Ok(Admission::New(guard)) => guard,
        other => panic!("expected admission, got {other:?}"),
    };
    assert!(ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner())
        .iter().any(|item| item.operation_id == operation.operation_id));
    assert!(matches!(
        admit_scoped(&request("request-in-flight", &["remember", "value"]), &operation, &current),
        Ok(Admission::InProgress)
    ));
    for stale in [
        fixture_scope("account-b", 1),
        fixture_scope("account-a", 2),
        RuntimeRequestScope { instance_id: "new-instance".to_owned(), ..current.clone() },
    ] {
        assert!(matches!(admit_scoped(&first, &operation, &stale),
            Err((code, _)) if code == "scope_mismatch"));
        assert_ne!(scoped_operation_key(&operation_scope(&stale), "operation-1"), operation.storage_id);
    }

    let mut pending_response = status_response(&first);
    pending_response.envelope = Some(ResultEnvelope::new("model.prepare", OutputStatus::Pending,
        json!({"operation_id":operation.operation_id}), Vec::new()));
    remember(&operation.storage_id, &pending_response);
    assert_eq!(result_state(cached(&operation.storage_id).as_ref(), true), "pending");
    assert!(matches!(admit_scoped(&first, &operation, &current), Ok(Admission::Replay(_))));
    let mut response = status_response(&first);
    response.operation_id = Some(operation.operation_id.clone());
    remember(&operation.storage_id, &response);
    INFLIGHT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&operation.storage_id);
    drop(admission);
    assert_eq!(result_state(cached(&operation.storage_id).as_ref(), false), "completed");
    match admit_scoped(&request("request-2", &["remember", "value"]), &operation, &current) {
        Ok(Admission::Replay(replay)) => {
            assert_eq!(replay.id, "request-2");
            assert_eq!(replay.operation_id.as_deref(), Some("operation-1"));
            assert_eq!(replay.v, PROTOCOL_V);
            assert_eq!(replay.scope, first.scope);
            assert_eq!(replay.idempotency_key, first.idempotency_key);
            assert_eq!(cached(&operation.storage_id).map(|receipt| receipt.id), Some(first.id.clone()));
        }
        other => panic!("expected replay, got {other:?}"),
    }

    for index in 0..4096 {
        let id = format!("capacity-operation-{index}");
        let mut fresh = request(&id, &["status"]);
        fresh.operation_id = Some(id.clone());
        fresh.idempotency_key = Some(id);
        let identity = operation_identity_for_scope(&fresh, &current)
            .map_err(|(code, error)| anyhow::anyhow!("{code}: {error}"))?;
        let guard = match admit_scoped(&fresh, &identity, &current) {
            Ok(Admission::New(guard)) => guard,
            other => panic!("expected admission beyond the old limit, got {other:?}"),
        };
        remember(&identity.storage_id, &status_response(&fresh));
        INFLIGHT
            .lock()
            .map_err(|_| anyhow::anyhow!("inflight lock poisoned"))?
            .remove(&identity.storage_id);
        drop(guard);
        if index == 4095 {
            assert!(matches!(admit_scoped(&fresh, &identity, &current), Ok(Admission::Replay(_))));
        }
    }
    assert_eq!(IDEMPOTENCY.lock().map_err(|_| anyhow::anyhow!("idempotency lock poisoned"))?.len(), 4097);
    assert!(cached(&operation.storage_id).is_none());
    assert!(matches!(
        admit_scoped(&request("request-3", &["remember", "value"]), &operation, &current),
        Ok(Admission::Unknown)
    ));

    let changed = request("request-4", &["remember", "different"]);
    let Some(changed_operation) = operation_identity_for_scope(&changed, &current).ok()
    else {
        panic!("changed operation identity must be valid");
    };
    match admit_scoped(&changed, &changed_operation, &current) {
        Err((code, _)) => assert_eq!(code, "idempotency_conflict"),
        other => panic!("expected conflict, got {other:?}"),
    }
    let other_scope = fixture_scope("account-b", 1);
    let mut other_request = request("request-other-account", &["remember", "value"]);
    other_request.scope = Some(other_scope.clone());
    let other_account = match operation_identity_for_scope(&other_request, &other_scope) {
        Ok(value) => value,
        Err(error) => panic!("isolated account identity failed: {error:?}"),
    };
    assert_ne!(operation.storage_id, other_account.storage_id);
    assert!(cached(&other_account.storage_id).is_none());
    assert!(matches!(admit_scoped(&other_request, &other_account, &other_scope), Ok(Admission::New(_))));
    for method in ["model.prepare", "model.switch", "model.resume"] {
        for succeeded in [true, false] {
            let id = format!("deferred-{method}-{succeeded}");
            let mut deferred = request(&id, &["{}"]);
            deferred.method = method.to_owned();
            deferred.operation_id = Some(id.clone());
            deferred.idempotency_key = None;
            let identity = match operation_identity_for_scope(&deferred, &current) {
                Ok(identity) => identity,
                Err(error) => panic!("deferred identity failed: {error:?}"),
            };
            let guard = match admit_scoped(&deferred, &identity, &current) {
                Ok(Admission::New(guard)) => guard,
                other => panic!("deferred admission failed: {other:?}"),
            };
            let mut pending = status_response(&deferred);
            pending.envelope = Some(ResultEnvelope::new(method, OutputStatus::Pending,
                json!({"state":"pending"}), Vec::new()));
            remember(&identity.storage_id, &pending);
            let reply = deferred_reply(&deferred, &identity, guard);
            assert!(INFLIGHT.lock().unwrap_or_else(|error| error.into_inner())
                .contains(&identity.storage_id));
            assert_eq!(result_state(cached(&identity.storage_id).as_ref(), true), "pending");
            assert!(ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner())
                .iter().any(|item| item.operation_id == id));
            let completion = if succeeded {
                Ok(crate::output_state::Captured { exit: 0, envelope: ResultEnvelope::new(
                    method, OutputStatus::Ok, json!({"state":"ready"}), Vec::new(),
                ) })
            } else {
                Err("model switch cancelled".to_owned())
            };
            assert!(reply.send(completion).is_ok());
            assert!(!INFLIGHT.lock().unwrap_or_else(|error| error.into_inner())
                .contains(&identity.storage_id));
            assert!(!ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner())
                .iter().any(|item| item.operation_id == id));
            let receipt = cached(&identity.storage_id);
            assert_eq!(result_state(receipt.as_ref(), false),
                if succeeded { "completed" } else { "failed" });
            match receipt {
                Some(receipt) => {
                    assert_eq!(receipt.operation_id.as_deref(), Some(id.as_str()));
                    assert_eq!(receipt.ok, succeeded);
                }
                None => panic!("deferred receipt is missing"),
            }
            let next_scope = fixture_scope("account-a", current.generation + 1);
            assert!(cached(&scoped_operation_key(&operation_scope(&next_scope), &id)).is_none());
            assert!(matches!(admit_scoped(&deferred, &identity, &current), Ok(Admission::Replay(_))));
        }
    }
    let mut missing = request("request-missing-scope", &["a"]);
    missing.scope = None;
    assert!(matches!(admit_scoped(&missing, &operation, &current),
        Err((code, _)) if code == "scope_required"));
    // The same lock fences stop and ledger insertion. Replays remain available.
    struct RestoreStopping(bool);
    impl Drop for RestoreStopping {
        fn drop(&mut self) {
            let _fence = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
            STOPPING.store(self.0, Ordering::Release);
        }
    }
    let restore = {
        let _fence = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
        RestoreStopping(STOPPING.swap(true, Ordering::AcqRel))
    };
    assert!(matches!(admit_scoped(&first, &operation, &current), Ok(Admission::Unknown)));
    let stopped_generation = GENERATION.load(Ordering::Acquire);
    assert!(advance_generation(stopped_generation).is_err());
    assert_eq!(GENERATION.load(Ordering::Acquire), stopped_generation);
    let mut fresh = request("request-after-stop", &["a"]);
    fresh.operation_id = Some("operation-after-stop".to_owned());
    fresh.idempotency_key = Some("key-after-stop".to_owned());
    let fresh_operation = match operation_identity(&fresh) {
        Ok(operation) => operation,
        Err(error) => panic!("fresh operation identity failed: {error:?}"),
    };
    assert!(matches!(admit_scoped(&fresh, &fresh_operation, &current),
        Err((code, _)) if code == "runtime_maintenance"));
    assert!(!INFLIGHT.lock().unwrap_or_else(|error| error.into_inner())
        .contains(&fresh_operation.storage_id));
    drop(restore);
    let marker = root.path().join("runtime/autostart/maintenance.json");
    std::fs::create_dir_all(marker.parent().context("maintenance marker parent missing")?)?;
    for phase in ["Paused", "Starting", "Verified", "malformed"] {
        let bytes = if phase == "malformed" { b"invalid JSON".to_vec() }
            else { serde_json::to_vec(&json!({"phase":phase}))? };
        std::fs::write(&marker, bytes)?;
        assert!(business_paused(), "{phase} must fence business without STOPPING");
        assert!(begin_admission(format!("background-{phase}"), "background_write").is_none());
        assert!(matches!(admit_scoped(&fresh, &fresh_operation, &current),
            Err((code, _)) if code == "runtime_maintenance"));
        assert!(!INFLIGHT.lock().unwrap_or_else(|error| error.into_inner())
            .contains(&fresh_operation.storage_id));
        assert!(matches!(admit_scoped(&first, &operation, &current), Ok(Admission::Unknown)));
        let mut control = request("maintenance-cancel", &["cancel"]);
        control.method = "model.control".to_owned();
        control.operation_id = Some(format!("cancel-{phase}"));
        control.idempotency_key = None;
        let cancellation = operation_identity_for_scope(&control, &current)
            .map_err(|(code, error)| anyhow::anyhow!("{code}: {error}"))?;
        match admit_scoped(&control, &cancellation, &current) {
            Ok(Admission::New(guard)) => drop(guard),
            _ => anyhow::bail!("maintenance must retain the scoped cancellation channel"),
        }
        INFLIGHT.lock().unwrap_or_else(|error| error.into_inner()).remove(&cancellation.storage_id);
        let (jobs, queued) = mpsc::channel();
        for args in [vec!["recall".to_owned(), "query".to_owned()], vec!["show".to_owned(), "entry".to_owned()], vec!["remember".to_owned(), "value".to_owned()]] {
            assert!(matches!(enqueue_job_scoped(&jobs, args.clone(), false, Reply::new(|_| {}),
                Duration::from_secs(1), format!("queued-{phase}"), current.clone()),
                Err(error) if error.starts_with("runtime_maintenance")));
            assert!(matches!(capture_scoped_locked(&current, args),
                Err(error) if error.starts_with("runtime_maintenance")));
        }
        assert!(matches!(queued.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let mut catalog = request("fenced-catalog", &[]);
        catalog.method = "model.catalog".to_owned();
        assert_eq!(dispatch(catalog, &jobs).code.as_deref(), Some("runtime_maintenance"));
        let mut status = request("maintenance-status", &[]);
        status.method = "runtime.status".to_owned();
        assert!(dispatch(status, &jobs).ok);
        let live = current_scope();
        let mut lookup = request("original-result-query", &["original-operation"]);
        lookup.method = "runtime.result".to_owned();
        lookup.scope = Some(live.clone());
        let storage_id = scoped_operation_key(&operation_scope(&live), "original-operation");
        remember(&storage_id, &status_response(&lookup));
        let result = dispatch(lookup, &jobs);
        assert!(result.ok);
        assert!(result.envelope.is_some_and(|envelope| envelope.summary["state"] == "completed"));
        for method in ["runtime.status", "runtime.stop", "runtime.result", "model.control", "cli.progress"] {
            assert!(maintenance_control_method(method));
        }
        for method in ["cli.exec", "model.catalog", "model.progress", "assets.status", "model.prepare", "index.prepare"] {
            assert!(!maintenance_control_method(method));
        }
    }
    std::fs::remove_file(&marker)?;
    assert!(!business_paused());
    {
        let _scope = SCOPE_EXECUTION.write()
            .map_err(|_| anyhow::anyhow!("test scope fence poisoned"))?;
        let gate = shared_exclusive();
        let _held = gate.acquire(true);
        let config = root.path().join("client.json");
        std::fs::write(&config, br#"{"service_mode":"readonly"}"#)?;
        let live = current_scope();
        let before = GENERATION.load(Ordering::Acquire);
        let args = vec!["--json".to_owned(), "sync-reset".to_owned()];
        let rejected = readonly_sync_reset_rejection_locked(&live, &args)
            .context("readonly sync-reset must be rejected before capture")?;
        let mut reset = request("readonly-reset", &["--json", "sync-reset"]);
        reset.method = "cli.exec".to_owned();
        reset.scope = Some(live.clone());
        let response = command_response(&reset, rejected);
        assert!(!response.ok);
        assert_eq!(response.exit, 1);
        assert_eq!(response.operation_id, reset.operation_id);
        assert_eq!(response.idempotency_key, reset.idempotency_key);
        assert_eq!(response.scope, reset.scope);
        assert!(response.envelope.context("readonly rejection envelope missing")?
            .errors.iter().any(|error| error.contains("read-only")));
        assert_eq!(GENERATION.load(Ordering::Acquire), before);
        assert_eq!(current_scope(), live);
        assert!(validate_job_scope(&live).is_ok(), "next request must retain the original scope");
        assert!(!root.path().join("rsrs.db").exists());
        assert!(readonly_sync_reset_rejection_locked(&live, &["remember".into(), "value".into()]).is_none());
        assert!(readonly_sync_reset_rejection_locked(&live, &["sync-reset".into(), "--invalid-option".into()]).is_none());
        let mut stale = live.clone();
        stale.generation += 1;
        assert!(matches!(readonly_sync_reset_rejection_locked(&stale, &args),
            Some(Err(error)) if error == "scope_mismatch"));
        std::fs::write(&config, br#"{"service_mode":"normal"}"#)?;
        assert!(readonly_sync_reset_rejection_locked(&live, &args).is_none(),
            "writable sync-reset must retain execution and generation fencing");
        std::fs::remove_file(config)?;
    }
    {
        struct RestoreGeneration(usize);
        impl Drop for RestoreGeneration {
            fn drop(&mut self) {
                let _fence = ADMISSIONS.lock().unwrap_or_else(|error| error.into_inner());
                GENERATION.store(self.0, Ordering::Release);
            }
        }
        let original_generation = GENERATION.load(Ordering::Acquire);
        let _restore = RestoreGeneration(original_generation);
        let _scope = SCOPE_EXECUTION.read().unwrap_or_else(|error| error.into_inner());
        assert!(advance_generation(original_generation).is_ok(), "generation advance must not upgrade a held read lock");
        assert_eq!(GENERATION.load(Ordering::Acquire), original_generation.saturating_add(1));
        assert!(advance_generation(original_generation).is_err(), "stale generation must not advance again");
    }
    let mut model_a = request("request-json-a", &[r#"{"engine":"cpu","model":"bge"}"#]);
    model_a.method = "model.configure".to_owned();
    let mut model_b = request("request-json-b", &[r#"{"model":"bge","engine":"cpu"}"#]);
    model_b.method = "model.configure".to_owned();
    let a = operation_identity_for_scope(&model_a, &current);
    let b = operation_identity_for_scope(&model_b, &current);
    match (a, b) {
        (Ok(a), Ok(b)) => assert_eq!(a.fingerprint, b.fingerprint),
        other => panic!("model fingerprints failed: {other:?}"),
    }
    let internal = mcp_request(vec!["status".to_owned()], current.clone());
    assert_eq!(internal.scope.as_ref(), Some(&current));
    assert_eq!(internal.method, "cli.exec");
    let internal_identity = match operation_identity(&internal) {
        Ok(identity) => identity,
        Err(error) => panic!("internal MCP identity failed: {error:?}"),
    };
    let internal_admission = match admit_scoped(&internal, &internal_identity, &current) {
        Ok(Admission::New(guard)) => guard,
        other => panic!("internal MCP admission failed: {other:?}"),
    };
    let failed = command_response(&internal, Err("scope_mismatch".to_owned()));
    remember(&internal_identity.storage_id, &failed);
    INFLIGHT.lock().unwrap_or_else(|error| error.into_inner()).remove(&internal_identity.storage_id);
    drop(internal_admission);
    assert_eq!(result_state(cached(&internal_identity.storage_id).as_ref(), false), "failed");
    let switched = fixture_scope("account-a", current.generation + 1);
    assert!(cached(&scoped_operation_key(&operation_scope(&switched), &internal_identity.operation_id)).is_none());
    assert!(matches!(admit_scoped(&internal, &internal_identity, &current), Ok(Admission::Replay(_))));
    Ok(())
}

#[test]
fn normal_drain_waits_past_the_legacy_four_second_budget() {
    for supervised in [false, true] {
        let mut polls = 0;
        wait_for_drain(supervised, || {
            polls += 1;
            polls < 83
        });
        assert_eq!(polls, 83, "normal drain returned while work remained");
    }
}

#[test]
fn stop_target_rejects_stale_instance_account_and_generation() -> Result<()> {
    let process = ProcessIdentity {
        installation_id: "installation-1".to_owned(),
        instance_id: "instance-1".to_owned(),
        process_start_id: "start-1".to_owned(),
        image_path: "/isolated/rsrs-runtime".to_owned(),
        image_sha256: "2".repeat(64),
    };
    let expected = json!({"installation_id":"installation-1", "instance_id":"instance-1", "account_id":"account-1", "generation":7});
    validate_stop_target(&expected, &process, "account-1", 7)?;
    assert!(!validate_stop_force(&expected, None)?);
    let mut force = expected.clone();
    force["force"] = json!(false);
    force["operation_id"] = json!("unrelated-old-field");
    assert!(!validate_stop_force(&force, None)?);
    force["force"] = json!(true);
    assert!(validate_stop_force(&force, None).is_err());
    assert!(validate_stop_force(&force, Some("accepted-stop")).is_err());
    force["operation_id"] = json!("accepted-stop");
    assert!(validate_stop_force(&force, Some("accepted-stop"))?);
    for invalid in [Value::Null, json!(1), json!("true"), json!({})] {
        force["force"] = invalid;
        assert!(validate_stop_force(&force, Some("accepted-stop")).is_err());
    }
    assert_eq!(stop_outcome(true, false, true), RuntimeStopOutcome::Drained);
    assert_eq!(stop_outcome(true, false, false), RuntimeStopOutcome::Failed);
    assert_eq!(stop_outcome(true, true, false), RuntimeStopOutcome::ForcedTimeout);
    assert_eq!(stop_outcome(false, true, false), RuntimeStopOutcome::Failed);
    assert_eq!(FORCE_DRAIN_GRACE, Duration::from_secs(4));
    for key in ["installation_id", "instance_id", "account_id", "generation"] {
        let mut missing = expected.clone();
        missing
            .as_object_mut()
            .context("stop target object")?
            .remove(key);
        assert!(validate_stop_target(&missing, &process, "account-1", 7).is_err());
        let mut stale = expected.clone();
        stale[key] = if key == "generation" {
            json!(6)
        } else {
            json!("other")
        };
        assert!(validate_stop_target(&stale, &process, "account-1", 7).is_err());
    }
    Ok(())
}
#[path = "doctor_lock_tests.rs"]
mod doctor_lock_tests;
