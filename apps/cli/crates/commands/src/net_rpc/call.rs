use super::*;

pub fn rpc_exec(args: Vec<String>) -> Result<Value> {
    rpc_method("cli.exec", args)
}

pub fn rpc_method(method: &str, args: Vec<String>) -> Result<Value> {
    rpc_method_with_timeout(method, args, None)
}

pub fn request_scope(health: &Health) -> Result<respire_protocol::RuntimeRequestScope> {
    anyhow::ensure!(
        health.v == crate::rpc_client::PROTOCOL_V
            && !health.account_id.is_empty()
            && !health.instance_id.is_empty()
            && health.generation > 0
            && !health.installation_root.is_empty(),
        "runtime request scope is incomplete or incompatible"
    );
    anyhow::ensure!(
        fs::canonicalize(&health.installation_root)?
            == fs::canonicalize(respire_client_host::paths::main_data_dir())?,
        "runtime request belongs to another installation"
    );
    Ok(respire_protocol::RuntimeRequestScope {
        account_id: health.account_id.clone(),
        instance_id: health.instance_id.clone(),
        generation: health.generation,
    })
}

/// Inspect the original operation without refreshing health or replaying it.
pub fn rpc_result(operation: &OperationContext) -> Result<Value> {
    anyhow::ensure!(
        !operation.scope.account_id.is_empty()
            && !operation.scope.instance_id.is_empty()
            && operation.scope.generation > 0
            && !operation.operation_id.is_empty()
            && !operation.idempotency_key.is_empty(),
        "runtime result requires the complete original operation context"
    );
    rpc_request(
        "runtime.result",
        vec![operation.operation_id.clone()],
        Some(operation),
        None,
    )
}

fn rpc_timeout(method: &str, args: &[String], test_timeout: Option<Duration>) -> Duration {
    if let Some(timeout) = test_timeout {
        return timeout;
    }
    match method {
        "cli.progress" => return Duration::from_secs(1),
        "model.control" => return Duration::from_secs(3),
        "cli.exec" => {}
        _ => return HTTP_TIMEOUT,
    }
    // Native inference stays inside the runtime. Tracking IDs are values, not
    // command words, and longer write budgets never authorize a request replay.
    let command: Vec<_> = crate::rpc_client::command_words(args).take(2).collect();
    if matches!(
        command.as_slice(),
        ["model", "install" | "install-m3" | "activate"]
    ) || command
        .first()
        .is_some_and(|c| matches!(*c, "reembed" | "sync" | "import"))
    {
        Duration::from_secs(30 * 60)
    } else if command.first().is_some_and(|c| *c == "defrag") {
        // Core's single-call budget is 365s; wait exactly that long before
        // outcome-unknown reconciliation instead of the generic 120s HTTP cap.
        Duration::from_secs(365)
    } else {
        HTTP_TIMEOUT
    }
}

pub(crate) fn rpc_method_with_timeout(
    method: &str,
    args: Vec<String>,
    test_timeout: Option<Duration>,
) -> Result<Value> {
    anyhow::ensure!(
        method != "runtime.result",
        "runtime.result requires rpc_result with the original operation context"
    );
    let mut _remote_progress = None;
    let operation = if matches!(
        method,
        "cli.progress" | "runtime.status" | "assets.status" | "model.catalog" | "model.progress"
    ) {
        None
    } else {
        let scope = request_scope(&health()?)?;
        if method == "cli.exec" {
            let progress_id = args.iter().take_while(|arg| arg.as_str() != "--")
                .enumerate().find_map(|(index, arg)| {
                    if arg == "--progress-id" {
                        args.get(index + 1).map(String::as_str)
                    } else {
                        arg.strip_prefix("--progress-id=")
                    }
                });
            if let Some(id) = progress_id {
                _remote_progress = Some(crate::progress::bind_remote_scope(id, &scope)?);
            }
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        Some(OperationContext {
            scope,
            idempotency_key: operation_id.clone(),
            operation_id,
        })
    };
    rpc_request(method, args, operation.as_ref(), test_timeout)
}

fn rpc_request(
    method: &str,
    args: Vec<String>,
    operation: Option<&OperationContext>,
    test_timeout: Option<Duration>,
) -> Result<Value> {
    let url = format!("{}/api/rpc", rpc_base_url());
    let request_id = uuid::Uuid::new_v4().to_string();
    let progress_scope = if method == "cli.progress" {
        let id = args.first().context("foreground progress ID is missing")?;
        Some(crate::progress::remote_scope(id)?)
    } else {
        None
    };
    let (operation_id, idempotency_key, scope) = match operation {
        Some(operation) => (
            Some(operation.operation_id.clone()),
            Some(operation.idempotency_key.clone()),
            Some(&operation.scope),
        ),
        None => (None, None, progress_scope.as_ref()),
    };
    let body = json!({
        "v": crate::rpc_client::PROTOCOL_V,
        "id": request_id.clone(),
        "operation_id": operation_id,
        "idempotency_key": idempotency_key,
        "scope": scope,
        "method": method,
        "args": args,
        "lifecycle": if method == "runtime.stop" { Some("coordinator-v1") } else { None },
    });
    let timeout = rpc_timeout(method, &args, test_timeout);
    let resp = send_loopback(agent().post(&url).timeout(timeout), |request| {
        request.send_json(body.clone())
    })
    .map_err(|error| {
        if let Some(operation) = operation.filter(|_| method != "runtime.result") {
            if matches!(
                error.downcast_ref::<RuntimeError>(),
                Some(RuntimeError::Transport(_))
            ) {
                return RuntimeError::OutcomeUnknown {
                    request_id: request_id.clone(),
                    operation: operation.clone(),
                    detail: error.to_string(),
                }
                .into();
            }
        }
        error
    })?;
    let parsed: Value = resp.into_json().map_err(|error| {
        if let Some(operation) = operation.filter(|_| method != "runtime.result") {
            anyhow::Error::from(RuntimeError::OutcomeUnknown {
                request_id: request_id.clone(),
                operation: operation.clone(),
                detail: format!("runtime reply unavailable: {error}"),
            })
        } else {
            anyhow::anyhow!("runtime rpc is not JSON: {error}")
        }
    })?;
    validate_rpc_response(&body, parsed, operation)
}

fn validate_rpc_response(
    request: &Value,
    response: Value,
    operation: Option<&OperationContext>,
) -> Result<Value> {
    let request_id = request["id"]
        .as_str()
        .context("missing original RPC request ID")?;
    let checked = (|| -> Result<()> {
        let header: respire_protocol::RpcResponse<crate::output::ResultEnvelope> =
            serde_json::from_value(response.clone()).context("malformed runtime RPC response")?;
        anyhow::ensure!(
            header.v == crate::rpc_client::PROTOCOL_V
                && header.id == request_id
                && header.code.as_deref() != Some("protocol_version_mismatch"),
            "runtime RPC response request ID or protocol version mismatch"
        );
        anyhow::ensure!(
            header.exit >= 0 && header.ok == (header.exit == 0),
            "runtime RPC response ok/exit mismatch"
        );
        let Some(operation) = operation else {
            return Ok(());
        };
        for key in ["operation_id", "idempotency_key"] {
            if let Some(value) = response.get(key) {
                anyhow::ensure!(
                    value == &request[key],
                    "runtime RPC response {key} mismatch"
                );
            }
        }
        if matches!(
            header.code.as_deref(),
            Some("scope_required" | "scope_mismatch")
        ) {
            // Admission rejection can omit scope or report the current one; never rebind to it.
            anyhow::ensure!(
                !header.ok
                    && header.exit > 0
                    && header
                        .envelope
                        .as_ref()
                        .is_none_or(|envelope| matches!(envelope.status, crate::output::Status::Fail)),
                "runtime scope rejection must remain an explicit failure"
            );
            if let Some(scope) = response.get("scope").filter(|scope| !scope.is_null()) {
                serde_json::from_value::<respire_protocol::RuntimeRequestScope>(scope.clone())
                    .context("malformed runtime scope rejection")?;
            }
            return Ok(());
        }
        let receipt = if request["method"] == "runtime.result" {
            &response["envelope"]["summary"]
        } else {
            &response
        };
        anyhow::ensure!(
            receipt["operation_id"].as_str() == Some(operation.operation_id.as_str()),
            "runtime RPC response operation ID is missing or foreign"
        );
        let scope: respire_protocol::RuntimeRequestScope =
            serde_json::from_value(receipt["scope"].clone())
                .context("runtime RPC response scope is missing or malformed")?;
        anyhow::ensure!(
            scope == operation.scope,
            "runtime RPC response scope mismatch"
        );
        if let Some(key) = receipt.get("idempotency_key") {
            anyhow::ensure!(
                key.as_str() == Some(operation.idempotency_key.as_str()),
                "runtime RPC receipt idempotency key mismatch"
            );
        }
        if request["method"] == "runtime.result" {
            anyhow::ensure!(
                receipt["request_id"].as_str() == Some(request_id),
                "runtime result receipt request ID mismatch"
            );
            if let Some(scope) = response.get("scope") {
                anyhow::ensure!(
                    scope == &receipt["scope"],
                    "runtime result outer scope mismatch"
                );
            }
        }
        Ok(())
    })();
    checked.map_err(|error| {
        if let Some(operation) = operation.filter(|_| request["method"] != "runtime.result") {
            anyhow::Error::from(RuntimeError::OutcomeUnknown {
                request_id: request_id.to_owned(),
                operation: operation.clone(),
                detail: format!("unverified runtime reply: {error:#}"),
            })
        } else {
            anyhow::anyhow!("unverified runtime reply: {error:#}")
        }
    })?;
    Ok(response)
}

#[cfg(test)]
mod tests;
