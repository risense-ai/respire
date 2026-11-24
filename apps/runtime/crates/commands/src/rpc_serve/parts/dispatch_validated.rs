fn dispatch_validated(request: RpcRequest, tx: &Sender<Job>) -> RpcResponse {
    let operation = if !matches!(
        request.method.as_str(),
        "runtime.result"
            | "runtime.status"
            | "runtime.stop"
            | "assets.status"
            | "cli.progress"
            | "model.catalog"
            | "model.progress"
    ) {
        match operation_identity(&request) {
            Ok(operation) => Some(operation),
            Err((code, error)) => return error_response(&request, &code, &error),
        }
    } else {
        None
    };
    let _admission = if let Some(operation) = operation.as_ref() {
        Some(match admit(&request, operation) {
            Ok(Admission::New(guard)) => guard,
            Ok(Admission::Replay(response)) => return response,
            Ok(Admission::InProgress) => {
                return error_response(
                    &request,
                    "request_in_progress",
                    "operation ID is already executing or queued; inspect runtime.result; command was not replayed",
                )
            }
            Ok(Admission::Unknown) => {
                return error_response(
                    &request,
                    "result_unknown",
                    "operation was admitted but its in-memory receipt is unavailable; non-idempotent writes are not replayed",
                )
            }
            Err((code, error)) => return error_response(&request, &code, &error),
        })
    } else {
        None
    };
    let _scope_held = if request.method == "runtime.result"
        || (operation.is_some() && request.method != "cli.exec")
    {
        let held = if request.method == "runtime.result" {
            SCOPE_EXECUTION.try_read().map_err(|_| ())
        } else {
            SCOPE_EXECUTION.read().map_err(|_| ())
        };
        match held {
            Ok(held) => Some(held),
            Err(_) => {
                let response = error_response(&request, "scope_mismatch", "identity fence poisoned");
                if let Some(operation) = &operation {
                    remember(&operation.storage_id, &response);
                    INFLIGHT.lock().unwrap_or_else(|error| error.into_inner()).remove(&operation.storage_id);
                }
                return response;
            }
        }
    } else { None };
    #[cfg(test)]
    tests::wait_before_direct_gate(&request.id);
    let gate = shared_exclusive();
    let _held = if operation.is_some() && !matches!(request.method.as_str(), "cli.exec" | "model.control" | "cli.progress") {
        Some(gate.acquire(true))
    } else {
        None
    };
    let mut deferred_operation = false;
    let response = (|| {
        // Recheck after waiting for the execution gate, including unscoped reads.
        if !maintenance_control_method(&request.method) && business_paused() {
            return error_response(&request, "runtime_maintenance", "runtime maintenance rejected business execution");
        }
        if operation.is_some() || request.method == "runtime.result" {
            let _fence = match ADMISSIONS.lock() {
                Ok(fence) => fence,
                Err(_) => return error_response(&request, "admission_lock_poisoned", "runtime admission lock is poisoned"),
            };
            let current = match request_current_scope(&request) {
                Ok(current) => current,
                Err((code, error)) => return error_response(&request, &code, &error),
            };
            if let Err((code, error)) = validate_scope(request.scope.as_ref(), &current) {
                return error_response(&request, &code, &error);
            }
        }
        if core_config_method(&request.method) {
            if STOPPING.load(Ordering::Acquire) {
                return error_response(&request, "runtime_maintenance", "runtime is stopping");
            }
            return core_config_response(&request);
        }
        match request.method.as_str() {
        #[cfg(feature = "native-fault-tests")]
        "runtime.test_worker_lifecycle" => {
            let action = request.args.first().map(String::as_str).unwrap_or("");
            match respire_app::core_sdk::worker::fixture_control(action) {
                Ok(result) => {
                    let mut response = status_response(&request);
                    response.envelope = Some(ResultEnvelope::new(
                        "runtime.test_worker_lifecycle",
                        OutputStatus::Ok,
                        result,
                        Vec::new(),
                    ));
                    response
                }
                Err(error) => {
                    error_response(&request, "worker_fixture_failed", &format!("{error:#}"))
                }
            }
        }
        #[cfg(feature = "native-fault-tests")]
        "runtime.test_worker_hold" => {
            match respire_app::core_sdk::execute::<Value>("test_worker_hold", json!({})) {
                Ok(_) => status_response(&request),
                Err(error) => {
                    error_response(&request, "worker_fixture_failed", &format!("{error:#}"))
                }
            }
        }
        #[cfg(feature = "native-fault-tests")]
        "runtime.test_native_fault" => {
            let mode = request
                .args
                .first()
                .and_then(|mode| mode.parse::<u64>().ok())
                .unwrap_or(0);
            match respire_app::core_sdk::execute::<Value>(
                "engine_control",
                json!({"action":"test_native_fault","mode":mode}),
            ) {
                Ok(result) => {
                    let mut response = status_response(&request);
                    response.envelope = Some(ResultEnvelope::new(
                        "runtime.test_native_fault",
                        OutputStatus::Ok,
                        result,
                        Vec::new(),
                    ));
                    response
                }
                Err(error) => {
                    error_response(&request, "native_fixture_failed", &format!("{error:#}"))
                }
            }
        }
        "model.catalog" | "model.configure" | "model.switch" | "model.resume"
        | "model.progress" => {
            let result = (|| -> Result<RpcResponse> {
                let value = match request.method.as_str() {
                    "model.catalog" => {
                        let store = respire_app::service::open_store()?;
                        Ok(
                            json!({"registry":respire_app::embedding::registry()?,"active":store.active_selection()?,"switch":profile_state()}),
                        )
                    }
                    "model.configure" => {
                        let config = serde_json::from_str(
                            request
                                .args
                                .first()
                                .context("configuration JSON required")?,
                        )?;
                        respire_app::embedding::configure(config)
                    }
                    "model.switch" => {
                        let choice = serde_json::from_str(
                            request.args.first().context("selection JSON required")?,
                        )?;
                        let response = start_profile_switch(
                            choice,
                            &request,
                            operation.as_ref().context("model switch requires an operation")?,
                        )?;
                        deferred_operation = true;
                        return Ok(response);
                    }
                    "model.resume" => {
                        let store = respire_app::service::open_store()?;
                        let target = serde_json::from_str(
                            &store
                                .meta_get("embedding_pending_selection_v1")?
                                .context("no pending model selection")?,
                        )?;
                        let response = start_profile_switch_target(
                            target,
                            &request,
                            operation.as_ref().context("model resume requires an operation")?,
                        )?;
                        deferred_operation = true;
                        return Ok(response);
                    }
                    _ => {
                        let store = respire_app::service::open_store()?;
                        Ok(
                            json!({"switch":profile_state(),"operation":crate::model_progress::status()?,"pending_selection":store.meta_get("embedding_pending_selection_v1")?.map(|v|serde_json::from_str::<Value>(&v)).transpose()?,"pending_task":store.meta_get("embedding_pending_task_v1")?.map(|v|serde_json::from_str::<Value>(&v)).transpose()?}),
                        )
                    }
                }?;
                let mut response = status_response(&request);
                response.envelope = Some(ResultEnvelope::new(
                    &request.method, OutputStatus::Ok, value, Vec::new(),
                ));
                Ok(response)
            })();
            match result {
                Ok(response) => response,
                Err(error) => {
                    error_response(&request, "model_profile_failed", &format!("{error:#}"))
                }
            }
        }
        "model.prepare" => {
            let result = (|| -> Result<RpcResponse> {
                anyhow::ensure!(!respire_app::service::service_paused(), "model preparation is paused by service mode");
                let operation = operation.as_ref().context("model preparation requires an operation")?;
                let mut response = status_response(&request);
                response.operation_id = Some(operation.operation_id.clone());
                response.envelope = Some(ResultEnvelope::new(
                    "model.prepare", OutputStatus::Pending,
                    json!({"state":"scheduled", "operation_id":operation.operation_id}), Vec::new(),
                ));
                // The dispatch gate prevents execution before this pending receipt exists.
                remember(&operation.storage_id, &response);
                enqueue_prepare(&request, operation)?;
                deferred_operation = true;
                Ok(response)
            })();
            match result {
                Ok(response) => response,
                Err(error) => error_response(&request, "model_prepare_failed", &format!("{error:#}")),
            }
        }
        "index.prepare" => {
            kick_index();
            let mut response = status_response(&request);
            response.envelope = Some(ResultEnvelope::new(
                "index.prepare",
                OutputStatus::Pending,
                index_status(),
                Vec::new(),
            ));
            response
        }
        "model.control" => {
            // Cancellation must remain callable while a scoped prepare holds the write gate.
            let _fence = match ADMISSIONS.lock() {
                Ok(fence) => fence,
                Err(_) => return error_response(&request, "admission_lock_poisoned", "runtime admission lock is poisoned"),
            };
            if let Err((code, error)) = validate_scope(request.scope.as_ref(), &current_scope()) {
                return error_response(&request, &code, &error);
            }
            match crate::model_progress::control(
                request.args.first().map(String::as_str).unwrap_or(""),
                request.args.get(1).is_some_and(|arg| arg == "true"),
            ) {
                Ok(progress) => {
                    let mut response = status_response(&request);
                    response.envelope = Some(ResultEnvelope::new(
                        "model.control", OutputStatus::Ok, progress, Vec::new(),
                    ));
                    response
                }
                Err(error) => error_response(&request, "model_control_failed", &error.to_string()),
            }
        },
        "runtime.status" => {
            let mut response = status_response(&request);
            response.envelope = Some(ResultEnvelope::new(
                "runtime.status",
                OutputStatus::Ok,
                json!({
                    "state": control_state(),
                    "operation_id": STOP_OPERATION.get(),
                    "admitted": admitted_list(),
                    "background": busy_background(),
                    "unfinished": stop_unfinished(),
                }),
                Vec::new(),
            ));
            response
        }
        "assets.status" => match crate::model_assets::assets_status() {
            Ok(status) => match serde_json::to_value(status) {
                Ok(value) => {
                    let mut response = status_response(&request);
                    response.envelope = Some(ResultEnvelope::new(
                        "assets.status",
                        OutputStatus::Ok,
                        value,
                        Vec::new(),
                    ));
                    response
                }
                Err(error) => error_response(&request, "asset_status_failed", &error.to_string()),
            },
            Err(error) => error_response(&request, "asset_status_failed", &format!("{error:#}")),
        },
        "cli.progress" => {
            let mut response = status_response(&request);
            let mut summary =
                crate::progress::status(request.args.first().map(String::as_str).unwrap_or(""));
            if summary.is_null() {
                summary = json!({});
            }
            summary["inference"] = inference_health_status();
            response.envelope = Some(ResultEnvelope::new(
                "cli.progress",
                OutputStatus::Ok,
                summary,
                Vec::new(),
            ));
            response
        }
        "runtime.result" => {
            let id = request.args.first().map(String::as_str).unwrap_or("");
            let Some(scope) = request.scope.as_ref() else {
                return error_response(&request, "scope_required", "result queries require scope");
            };
            let storage_id = scoped_operation_key(&operation_scope(scope), id);
            let result = cached(&storage_id);
            let pending = INFLIGHT
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains(&storage_id);
            let state = result_state(result.as_ref(), pending);
            let mut response = status_response(&request);
            let mut summary = json!({"request_id":request.id,"operation_id":id,"scope":scope,
                "state":state,"response":result,
                "retention":"last 64 receipts; admission identities retained until runtime exit; account-directory and generation scoped; unknown is not proof of non-commit; never replay a write automatically"});
            if let Some(key) = &request.idempotency_key {
                summary["idempotency_key"] = json!(key);
            }
            response.envelope = Some(ResultEnvelope::new(
                "runtime.result",
                OutputStatus::Ok,
                summary,
                Vec::new(),
            ));
            response
        }
        "runtime.stop" => {
            if crate::host_hooks::managed_id().is_some()
                && request.lifecycle.as_deref() != Some("coordinator-v1")
            {
                error_response(
                    &request,
                    "managed_lifecycle_required",
                    "use the current host runtime lifecycle coordinator",
                )
            } else {
                error_response(
                    &request,
                    "stop_identity_required",
                    "use the lifecycle stop endpoint with the exact instance, account and generation",
                )
            }
        }
        "cli.exec" => match submit_scoped(tx, request.args.clone(), false,
            operation.as_ref().map_or_else(|| request.id.clone(), |op| op.operation_id.clone()),
            request.scope.clone()) {
            Ok(captured) => RpcResponse {
                v: PROTOCOL_V,
                id: request.id.clone(),
                operation_id: operation.as_ref().map(|item| item.operation_id.clone()),
                scope: request.scope.clone(),
                idempotency_key: request.idempotency_key.clone(),
                ok: captured.exit == 0,
                exit: captured.exit,
                bin: bin_version().to_owned(),
                envelope: Some(captured.envelope),
                error: None,
                code: None,
                web_url: Some(current_url()),
                pid: Some(std::process::id()),
                data_dir: Some(respire_app::service::data_dir().display().to_string()),
            },
            Err(error) => error_response(&request,
                command_error_code(&error), &error),
        },
        other => error_response(
            &request,
            "protocol_version_mismatch",
            &format!("unknown method {other}"),
        ),
        }
    })();
    if let Some(operation) = operation.filter(|_| !deferred_operation) {
        remember(&operation.storage_id, &response);
        INFLIGHT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&operation.storage_id);
    }
    response
}

fn command_error_code(error: &str) -> &'static str {
    if error.starts_with("runtime_maintenance") {
        "runtime_maintenance"
    } else if error.starts_with("invalid_config:") {
        "invalid_config"
    } else if error.starts_with("scope_mismatch") {
        "scope_mismatch"
    } else if error == "model_switch_busy" {
        "model_switch_busy"
    } else {
        "daemon_unavailable"
    }
}

fn command_response(request: &RpcRequest, result: CommandResult) -> RpcResponse {
    match result {
        Ok(captured) => RpcResponse {
            v: PROTOCOL_V,
            id: request.id.clone(),
            operation_id: request.operation_id.clone(),
            scope: request.scope.clone(),
            idempotency_key: request.idempotency_key.clone(),
            ok: captured.exit == 0,
            exit: captured.exit,
            bin: bin_version().to_owned(),
            envelope: Some(captured.envelope),
            error: None,
            code: None,
            web_url: Some(current_url()),
            pid: Some(std::process::id()),
            data_dir: Some(respire_app::service::data_dir().display().to_string()),
        },
        Err(error) => error_response(request,
            command_error_code(&error), &error),
    }
}

pub fn dispatch_deferred(
    request: RpcRequest,
    tx: &Sender<Job>,
    action: impl FnOnce(RpcResponse) + Send + 'static,
) {
    if let Some(response) = validate_rpc_entry(&request) {
        action(response);
        return;
    }
    if request.method != "cli.exec" {
        action(dispatch_validated(request, tx));
        return;
    }
    let operation = match operation_identity(&request) {
        Ok(operation) => operation,
        Err((code, error)) => {
            action(error_response(&request, &code, &error));
            return;
        }
    };
    let admission = match admit(&request, &operation) {
        Ok(Admission::New(guard)) => guard,
        Ok(Admission::Replay(response)) => {
            action(response);
            return;
        }
        Ok(Admission::InProgress) => {
            action(error_response(
                    &request,
                    "request_in_progress",
                    "operation ID is already executing or queued; inspect runtime.result; command was not replayed",
                ));
            return;
        }
        Ok(Admission::Unknown) => {
            action(error_response(
                    &request,
                    "result_unknown",
                    "operation was admitted but its in-memory receipt is unavailable; non-idempotent writes are not replayed",
                ));
            return;
        }
        Err((code, error)) => {
            action(error_response(&request, &code, &error));
            return;
        }
    };
    let scope = operation.scope.clone();
    let args = request.args.clone();
    let wait = request
        .queue_wait_ms
        .map(Duration::from_millis)
        .unwrap_or_else(queue_wait)
        .min(queue_wait());
    let operation_id = operation.operation_id.clone();
    let reply = Reply::new(move |result| {
        let _admission = admission;
        let response = command_response(&request, result);
        remember(&operation.storage_id, &response);
        INFLIGHT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&operation.storage_id);
        action(response);
    });
    if let Err(error) = enqueue_job_scoped(tx, args, false, reply.clone(), wait, operation_id, scope) {
        let _ = reply.send(Err(error));
    }
}

pub fn status_response(request: &RpcRequest) -> RpcResponse {
    RpcResponse {
        v: PROTOCOL_V,
        id: request.id.clone(),
        operation_id: request.operation_id.clone(),
        scope: request.scope.clone(),
        idempotency_key: request.idempotency_key.clone(),
        ok: true,
        exit: 0,
        bin: bin_version().to_owned(),
        envelope: None,
        error: None,
        code: None,
        web_url: Some(current_url()),
        pid: Some(std::process::id()),
        data_dir: Some(respire_app::service::data_dir().display().to_string()),
    }
}

pub fn error_response(request: &RpcRequest, code: &str, error: &str) -> RpcResponse {
    RpcResponse {
        v: PROTOCOL_V,
        id: request.id.clone(),
        operation_id: request.operation_id.clone(),
        scope: request.scope.clone(),
        idempotency_key: request.idempotency_key.clone(),
        ok: false,
        exit: 1,
        bin: bin_version().to_owned(),
        envelope: None,
        error: Some(error.to_owned()),
        code: Some(code.to_owned()),
        web_url: None,
        pid: Some(std::process::id()),
        data_dir: None,
    }
}

