pub fn remember(operation_id: &str, response: &RpcResponse) {
    if operation_id.is_empty() {
        return;
    }
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|entry| entry.operation_id != operation_id);
        cache.push(CachedResponse {
            operation_id: operation_id.to_owned(),
            response: response.clone(),
        });
        if cache.len() > 64 {
            let extra = cache.len() - 64;
            cache.drain(0..extra);
        }
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_whitespace)
}

fn requires_scope(method: &str) -> bool {
    !matches!(method, "runtime.status" | "runtime.stop" | "assets.status" | "cli.progress" | "model.catalog" | "model.progress")
}

fn model_switch_blocks(request: &RpcRequest) -> bool {
    PROFILE_SWITCH_RUNNING.load(Ordering::Acquire)
        && request.method == "cli.exec"
        && !is_status(&request.args)
}

fn model_switch_busy() -> (String, String) {
    (
        "model_switch_busy".to_owned(),
        "model switch in progress; semantic queries and writes are paused; inspect model.progress".to_owned(),
    )
}

fn read_request_identity(
    request: &RpcRequest,
) -> Result<std::sync::RwLockReadGuard<'static, ()>, (String, String)> {
    if matches!(request.method.as_str(), "runtime.result" | "model.control")
        || (request.method == "cli.exec" && command_name(&request.args) == Some("recall"))
        || model_switch_blocks(request)
    {
        return SCOPE_EXECUTION.try_read().map_err(|_| (
            "scope_mismatch".to_owned(), "Runtime identity is changing or unavailable".to_owned(),
        ));
    }
    SCOPE_EXECUTION.read().map_err(|_| (
        "scope_mismatch".to_owned(), "identity fence poisoned".to_owned(),
    ))
}

fn validate_request_scope(request: &RpcRequest) -> Result<(), (String, String)> {
    if !requires_scope(&request.method) { return Ok(()); }
    if request.scope.is_none() {
        return Err(("scope_required".to_owned(), "business requests require scope".to_owned()));
    }
    let _scope_held = read_request_identity(request)?;
    let gate = shared_exclusive();
    let skips_gate = matches!(request.method.as_str(), "runtime.result" | "model.control");
    let held = if skips_gate {
        None
    } else {
        gate.acquire_foreground_unless(|| model_switch_blocks(request))
    };
    let _fence = ADMISSIONS.lock().map_err(|_| (
        "admission_lock_poisoned".to_owned(), "runtime admission lock is poisoned".to_owned(),
    ))?;
    validate_scope(request.scope.as_ref(), &request_current_scope(request)?)?;
    if !skips_gate && held.is_none() {
        return Err(model_switch_busy());
    }
    Ok(())
}

fn validate_business_config() -> Result<(), String> {
    respire_app::service::validate_client_config()
        .map_err(|error| format!("invalid_config: {error:#}"))
}

fn request_current_scope(request: &RpcRequest) -> Result<RuntimeRequestScope, (String, String)> {
    // Result lookup and cancellation keep their identity fence even with broken config.
    if !core_config_method(&request.method) && !matches!(request.method.as_str(), "runtime.result" | "model.control") {
        validate_business_config().map_err(|error| ("invalid_config".to_owned(), error))?;
    }
    Ok(current_scope())
}

fn current_scope() -> RuntimeRequestScope {
    RuntimeRequestScope {
        account_id: account_identity(),
        instance_id: process_identity().instance_id,
        generation: crate::rpc_worker::wire_generation(),
    }
}

fn validate_scope(
    scope: Option<&RuntimeRequestScope>,
    current: &RuntimeRequestScope,
) -> Result<(), (String, String)> {
    let Some(scope) = scope else {
        return Err(("scope_required".to_owned(), "business requests require scope".to_owned()));
    };
    if scope.account_id.trim().is_empty()
        || scope.instance_id.trim().is_empty()
        || scope.generation == 0
        || current.account_id.trim().is_empty()
        || current.instance_id.trim().is_empty()
        || current.generation == 0
        || scope != current
    {
        return Err(("scope_mismatch".to_owned(), "request scope does not match this Runtime identity".to_owned()));
    }
    Ok(())
}

fn operation_scope(scope: &RuntimeRequestScope) -> String {
    format!("{}:{}:{}:{}:{}", scope.account_id.len(), scope.account_id,
        scope.instance_id.len(), scope.instance_id, scope.generation)
}

fn operation_identity(request: &RpcRequest) -> Result<OperationIdentity, (String, String)> {
    let scope = request.scope.as_ref().ok_or_else(|| (
        "scope_required".to_owned(), "business requests require scope".to_owned(),
    ))?;
    operation_identity_for_scope(request, scope)
}

// Legacy result IDs remain valid across a legitimate in-process generation
// transition, but never cross an account or process instance. Modern keys retain
// their existing generation fencing and lifetime idempotency policy.
// The writer already owns the shared gate; other executors take it before
// this check, then retain the captured generation in their thread context.
fn validate_job_scope(scope: &RuntimeRequestScope) -> Result<(), String> {
    // Reads can write querylog/statistics, so queued reads have no maintenance exemption.
    if business_paused() {
        return Err("runtime_maintenance: runtime maintenance rejected queued execution".to_owned());
    }
    validate_business_config()?;
    validate_scope(Some(scope), &current_scope()).map_err(|(code, _)| code)?;
    respire_app::service::ensure_runtime_profile().map_err(|error| format!("scope_mismatch: {error:#}"))?;
    Ok(())
}

fn capture_scoped(scope: &RuntimeRequestScope, args: Vec<String>) -> CommandResult {
    let _scope_held = SCOPE_EXECUTION.read().map_err(|_| "scope_mismatch: identity fence poisoned".to_owned())?;
    {
        let gate = shared_exclusive();
        let _held = gate.acquire(true);
        validate_job_scope(scope)?;
    }
    capture_scoped_locked(scope, args)
}

// Called with the scope and exclusive execution fences held, before any command effects.
fn readonly_sync_reset_rejection_locked(
    scope: &RuntimeRequestScope,
    args: &[String],
) -> Option<CommandResult> {
    if command_name(args) != Some("sync-reset") {
        return None;
    }
    let parsed = crate::cli_def::Cli::try_parse_from(
        std::iter::once("rsrs".to_owned()).chain(args.iter().cloned()),
    ).ok()?;
    if !matches!(parsed.command, Some(crate::cli_def::Command::SyncReset)) {
        return None;
    }
    if let Err(error) = validate_job_scope(scope) {
        return Some(Err(error));
    }
    let error = respire_app::service::ensure_writable().err()?;
    let (envelope, exit) = crate::output_emit::command_failure(&error);
    Some(Ok(crate::output_state::Captured { exit, envelope }))
}

std::thread_local! {
    static ACTIVE_EXECUTION_SCOPE: std::cell::RefCell<Option<RuntimeRequestScope>> =
        const { std::cell::RefCell::new(None) };
}

// Only execution that passed the queue fence may finish under its original scope.
struct ActiveExecutionContext {
    previous_scope: Option<RuntimeRequestScope>,
    previous_sync: Option<(usize, i64)>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ActiveExecutionContext {
    fn enter(scope: &RuntimeRequestScope, generation: usize) -> Self {
        let previous_scope = ACTIVE_EXECUTION_SCOPE.with(|slot| slot.replace(Some(scope.clone())));
        let previous_sync = SYNC_CONTEXT.with(|context| {
            let previous = context.get();
            let boundary = previous.filter(|(epoch, _)| *epoch == generation)
                .map_or(-1, |(_, boundary)| boundary);
            context.set(Some((generation, boundary)));
            previous
        });
        Self { previous_scope, previous_sync, _thread: std::marker::PhantomData }
    }
}

impl Drop for ActiveExecutionContext {
    fn drop(&mut self) {
        SYNC_CONTEXT.with(|context| context.set(self.previous_sync));
        ACTIVE_EXECUTION_SCOPE.with(|slot| slot.replace(self.previous_scope.take()));
    }
}

pub(crate) fn active_execution_retained(generation: usize) -> Result<bool> {
    let scope = ACTIVE_EXECUTION_SCOPE.with(|slot| slot.borrow().clone());
    let Some(scope) = scope else { return Ok(false); };
    validate_scope(Some(&scope), &current_scope())
        .map_err(|(code, message)| anyhow::anyhow!("{code}: {message}"))?;
    anyhow::ensure!(scope_generation(&scope).map_err(anyhow::Error::msg)? == generation,
        "sync context changed; pending work kept locally");
    Ok(!STOP_FORCE_TRIGGERED.load(Ordering::Acquire))
}

fn capture_scoped_locked(scope: &RuntimeRequestScope, args: Vec<String>) -> CommandResult {
    validate_job_scope(scope)?;
    let generation = scope_generation(scope)?;
    let _active = ActiveExecutionContext::enter(scope, generation);
    Ok(crate::host_hooks::capture_run(args))
}

fn scope_generation(scope: &RuntimeRequestScope) -> Result<usize, String> {
    scope.generation.checked_sub(1)
        .and_then(|generation| usize::try_from(generation).ok())
        .ok_or_else(|| "scope_mismatch".to_owned())
}

fn scoped_operation_key(scope: &str, id: &str) -> String {
    format!("{}:{scope}{id}", scope.len())
}

fn operation_identity_for_scope(
    request: &RpcRequest,
    scope: &RuntimeRequestScope,
) -> Result<OperationIdentity, (String, String)> {
    let scope_key = operation_scope(scope);
    let operation_id = request.operation_id.as_deref().unwrap_or(&request.id);
    let idempotency_key = request.idempotency_key.as_deref().unwrap_or(operation_id);
    for (value, code) in [
        (operation_id, "invalid_operation_id"),
        (idempotency_key, "invalid_idempotency_key"),
    ] {
        if !valid_identifier(value) {
            return Err((
                code.to_owned(),
                "operation identifiers must contain 1 to 256 non-space characters".to_owned(),
            ));
        }
    }
    if request
        .parent_operation_id
        .as_deref()
        .is_some_and(|parent| !valid_identifier(parent))
    {
        return Err((
            "invalid_parent_operation_id".to_owned(),
            "parent operation ID is invalid".to_owned(),
        ));
    }
    let mut args = request
        .args
        .iter()
        .map(|arg| Value::String(arg.clone()))
        .collect::<Vec<_>>();
    if matches!(request.method.as_str(), "model.configure" | "model.switch") {
        if let Some(first) = args.first_mut() {
            *first = serde_json::from_str(&request.args[0]).map_err(|_| {
                (
                    "invalid_arguments".to_owned(),
                    "model request requires valid configuration JSON".to_owned(),
                )
            })?;
        }
    }
    let encoded = serde_json::to_vec(&json!({
        "protocol": request.v, "method": request.method, "args": args,
        "parent_operation_id": request.parent_operation_id,
        "lifecycle": request.lifecycle, "scope": scope,
    }))
    .map_err(|_| {
        (
            "fingerprint_failed".to_owned(),
            "failed to normalize request fingerprint".to_owned(),
        )
    })?;
    Ok(OperationIdentity {
        scope: scope.clone(),
        operation_id: operation_id.to_owned(),
        storage_id: scoped_operation_key(&scope_key, operation_id),
        storage_key: scoped_operation_key(&scope_key, idempotency_key),
        fingerprint: format!("{:x}", Sha256::digest(encoded)),
    })
}

fn admit(
    request: &RpcRequest,
    operation: &OperationIdentity,
) -> Result<Admission, (String, String)> {
    let _scope_held = read_request_identity(request)?;
    let gate = shared_exclusive();
    // Legacy progress must observe the command while it holds the execution gate.
    let skips_gate = matches!(request.method.as_str(), "model.control" | "cli.progress");
    let held = if skips_gate {
        None
    } else {
        gate.acquire_foreground_unless(|| model_switch_blocks(request))
    };
    let mut admitted = ADMISSIONS.lock().map_err(|_| (
        "admission_lock_poisoned".to_owned(), "runtime admission lock is poisoned".to_owned(),
    ))?;
    let current = request_current_scope(request)?;
    if !skips_gate && held.is_none() {
        validate_scope(request.scope.as_ref(), &current)?;
        return Err(model_switch_busy());
    }
    admit_locked(request, operation, &current, &mut admitted)
}

#[cfg(test)]
fn admit_scoped(
    request: &RpcRequest,
    operation: &OperationIdentity,
    current: &RuntimeRequestScope,
) -> Result<Admission, (String, String)> {
    let mut admitted = ADMISSIONS.lock().map_err(|_| (
        "admission_lock_poisoned".to_owned(), "runtime admission lock is poisoned".to_owned(),
    ))?;
    admit_locked(request, operation, current, &mut admitted)
}

fn admit_locked(
    request: &RpcRequest,
    operation: &OperationIdentity,
    current: &RuntimeRequestScope,
    admitted: &mut Vec<Admitted>,
) -> Result<Admission, (String, String)> {
    // Caller holds the admission/stop fence before reading the current identity.
    validate_scope(request.scope.as_ref(), current)?;
    validate_scope(Some(&operation.scope), current)?;
    let lock_error = || {
        (
            "idempotency_lock_poisoned".to_owned(),
            "runtime idempotency lock is poisoned".to_owned(),
        )
    };
    let mut idempotency = IDEMPOTENCY.lock().map_err(|_| lock_error())?;
    let mut operation_keys = OPERATION_KEYS.lock().map_err(|_| lock_error())?;
    let mut inflight = INFLIGHT.lock().map_err(|_| lock_error())?;
    let cache = CACHE.lock().map_err(|_| lock_error())?;
    if let Some(previous_key) = operation_keys.get(&operation.storage_id) {
        if *previous_key != operation.storage_key {
            return Err((
                "idempotency_conflict".to_owned(),
                "operation ID is bound to a different idempotency key".to_owned(),
            ));
        }
    }
    if let Some(record) = idempotency.get(&operation.storage_key) {
        if record.fingerprint != operation.fingerprint
            || record.operation_id != operation.storage_id
        {
            return Err((
                "idempotency_conflict".to_owned(),
                "idempotency key is bound to a different request".to_owned(),
            ));
        }
        if let Some(mut response) = cache
            .iter()
            .find(|entry| entry.operation_id == operation.storage_id)
            .map(|entry| entry.response.clone())
        {
            response.id = request.id.clone();
            return Ok(Admission::Replay(response));
        }
        return Ok(if inflight.contains(&operation.storage_id) {
            Admission::InProgress
        } else {
            Admission::Unknown
        });
    }
    if (request.method != "model.control" && !core_config_method(&request.method) && business_paused())
        || (core_config_method(&request.method) && STOPPING.load(Ordering::Acquire)) {
        return Err((
            "runtime_maintenance".to_owned(),
            "runtime is draining and rejected new business".to_owned(),
        ));
    }
    // Retain admission tombstones until exit; evicting them could replay a committed write.
    idempotency.insert(
        operation.storage_key.clone(),
        IdempotencyRecord {
            operation_id: operation.storage_id.clone(),
            fingerprint: operation.fingerprint.clone(),
        },
    );
    operation_keys.insert(operation.storage_id.clone(), operation.storage_key.clone());
    inflight.insert(operation.storage_id.clone());
    admitted.push(Admitted {
        operation_id: operation.operation_id.clone(),
        kind: "rpc",
    });
    Ok(Admission::New(AdmissionGuard(operation.operation_id.clone())))
}

/// Only a refused connection permits startup; authentication and access failures
/// leave the existing service untouched.
pub fn probe_runtime() -> Result<Option<crate::net_rpc::Health>> {
    match crate::net_rpc::health() {
        Ok(health) => Ok(Some(health)),
        Err(error)
            if matches!(
                error.downcast_ref::<crate::runtime_error::RuntimeError>(),
                Some(crate::runtime_error::RuntimeError::Unavailable)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rpc_serve/tests.rs")); }
