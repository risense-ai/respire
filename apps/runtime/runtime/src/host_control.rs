//! One-shot offline control, dispatched before serving or opening a business store.
use std::io::{Read, Write};
use std::time::Duration;
use std::sync::{Arc, Mutex, mpsc};

use anyhow::{Context, Result};
use respire_commands::output::{Item, ResultEnvelope, Status};
use respire_protocol::{
    HostControlLaunchGrant, HostControlOperation, HostControlParams, RpcRequest, RpcResponse,
    HOST_CONTROL_MAX_FRAME_BYTES, HOST_CONTROL_SCHEMA_V,
    HostAuthKeygenParams, HostAuthOutcome, HOST_AUTH_SCHEMA_V,
    HOST_AUTH_KEYGEN_METHOD, HOST_AUTH_RESULT_METHOD,
    HostModelParams, HOST_MODEL_SCHEMA_V,
};

const PROTOCOL_V: u32 = respire_commands::rpc_worker::PROTOCOL_V;

enum ControlEvent {
    Finished(RpcResponse<ResultEnvelope>),
    ParentRevoked,
}

mod cold_keygen;

pub(crate) fn run() -> Result<i32> {
    run_mode(false)
}

pub(crate) fn run_cold_keygen() -> Result<i32> {
    run_mode(true)
}

fn run_mode(cold: bool) -> Result<i32> {
    let progress = Arc::new(Mutex::new(response("")));
    let worker_progress = Arc::clone(&progress);
    let (sent, received) = mpsc::sync_channel(1);
    std::thread::Builder::new().name("control-operation".into()).spawn(move || {
        let response = execute(&worker_progress, &sent, cold);
        let _ = sent.send(ControlEvent::Finished(response));
    })?;
    // Operation lifetime follows the host authorization, not elapsed wall time.
    let response = match received.recv() {
        Ok(ControlEvent::Finished(response)) => response,
        Ok(ControlEvent::ParentRevoked) | Err(_) => {
            let mut response = progress.lock().map_err(|_| anyhow::anyhow!("control progress unavailable"))?.clone();
            response.code = Some("control_outcome_unknown".into());
            response.error = Some("control parent authorization ended or worker disconnected; query the same operation ID before retrying".into());
            response.envelope = Some(ResultEnvelope::new(
                response.envelope.as_ref().map(|envelope| envelope.command.as_str())
                    .unwrap_or(if response.operation_id.is_some() { "keygen" } else { "memory-revision" }),
                Status::Fail, serde_json::json!({"outcome":"unknown"}), vec![
                    Item::new("control_ready", Status::Skip, "unknown"),
                    Item::new("store_ready", Status::Skip, "false"),
                ],
            ));
            response
        }
    };
    // Writes are journaled before effects; revocation never claims rollback.
    // Bound only delivery of the completed response if the host stops reading.
    std::thread::Builder::new().name("control-output-deadline".into()).spawn(|| {
        std::thread::sleep(Duration::from_secs(2));
        std::process::exit(2);
    })?;
    let bytes = serde_json::to_vec(&response)?;
    anyhow::ensure!(bytes.len() <= HOST_CONTROL_MAX_FRAME_BYTES, "host control response too large");
    let mut output = std::io::stdout().lock();
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(response.exit)
}

fn execute(progress: &Mutex<RpcResponse<ResultEnvelope>>, events: &mpsc::SyncSender<ControlEvent>, cold: bool) -> RpcResponse<ResultEnvelope> {
    let mut response = response("");
    let mut control_ready = false;
    let result = if cold {
        cold_keygen::execute(&mut response, &mut control_ready, progress, events)
    } else {
        receive_and_execute(&mut response, &mut control_ready, progress, events)
    };
    if let Err((code, message)) = result {
        response.code = Some(code.to_owned());
        let diagnostic = response.error.take().unwrap_or_else(|| message.to_owned());
        response.error = Some(diagnostic.clone());
        let mut envelope = ResultEnvelope::new(
            response.envelope.as_ref().map(|envelope| envelope.command.as_str())
                .unwrap_or(if response.operation_id.is_some() { "keygen" } else { "memory-revision" }),
            Status::Fail, serde_json::json!({}), Vec::new(),
        );
        envelope.errors.push(diagnostic);
        response.envelope = Some(envelope);
    }
    if let Some(envelope) = &mut response.envelope {
        envelope.items.push(Item::new("control_ready", if control_ready { Status::Ok } else { Status::Fail }, control_ready.to_string()));
        envelope.items.push(Item::new("store_ready", Status::Skip, "false"));
    }
    response
}

type ControlFailure = (&'static str, &'static str);

mod dispatch;
mod offline_setting;
use dispatch::receive_and_execute;


fn watch_auth_parent(events: mpsc::SyncSender<ControlEvent>) -> std::io::Result<()> {
    respire_app::credential_io::watch_parent(move || {
        let _ = events.send(ControlEvent::ParentRevoked);
    })
}

pub(crate) fn watch_control_parent() -> std::io::Result<()> {
    respire_app::credential_io::watch_parent(|| std::process::exit(2))
}

pub(crate) fn validate_auth_request(request: &RpcRequest) -> std::result::Result<HostAuthKeygenParams, ControlFailure> {
    if request.v != PROTOCOL_V { return Err(("protocol_version_mismatch", "protocol_version_mismatch")); }
    let uuid = |value: &str| uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value);
    if !uuid(&request.id) || !request.operation_id.as_deref().is_some_and(uuid)
        || request.idempotency_key.as_ref().is_none_or(|key| key.is_empty() || key.len() > 128
            || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)))
        || ![HOST_AUTH_KEYGEN_METHOD, HOST_AUTH_RESULT_METHOD].contains(&request.method.as_str())
        || request.args.len() != 1 || request.scope.is_some() || request.parent_operation_id.is_some()
        || request.queue_wait_ms.is_some() || request.lifecycle.is_some() {
        return Err(("invalid_request", "unsupported auth control RPC shape"));
    }
    let params: HostAuthKeygenParams = serde_json::from_str(&request.args[0])
        .map_err(|_| ("invalid_request", "invalid auth control params"))?;
    if params.schema_version != HOST_AUTH_SCHEMA_V {
        return Err(("invalid_request", "unsupported auth control schema"));
    }
    Ok(params)
}

pub(crate) fn validate_request(
    request: &RpcRequest,
) -> std::result::Result<(HostControlParams, Option<HostModelParams>), ControlFailure> {
    if request.v != PROTOCOL_V {
        return Err(("protocol_version_mismatch", "protocol_version_mismatch"));
    }
    if !uuid::Uuid::parse_str(&request.id)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == request.id)
        || ![HostControlOperation::MemoryRevision.method(), HostControlOperation::ModelInstallPlan.method(),
            HostControlOperation::ModelInstallVerify.method()]
            .contains(&request.method.as_str())
        || request.args.len() != 1
        || request.scope.is_some()
        || request.operation_id.is_some()
        || request.idempotency_key.is_some()
        || request.parent_operation_id.is_some()
        || request.queue_wait_ms.is_some()
        || request.lifecycle.is_some()
    {
        return Err(("invalid_request", "unsupported host control RPC shape"));
    }
    if request.method != HostControlOperation::MemoryRevision.method() {
        let params: HostModelParams = serde_json::from_str(&request.args[0])
            .map_err(|_| ("invalid_request", "invalid model preparation params"))?;
        let verify = request.method == HostControlOperation::ModelInstallVerify.method();
        if params.schema_version != HOST_MODEL_SCHEMA_V
            || if verify {
                !params.selection_sha256.as_deref().is_some_and(|digest| digest.len() == 64
                    && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            } else { params.selection_sha256.is_some() }
        {
            return Err(("invalid_request", "invalid model preparation schema or selection digest"));
        }
        return Ok((HostControlParams { schema_version: HOST_CONTROL_SCHEMA_V,
            binding: params.binding.clone() }, Some(params)));
    }
    let params: HostControlParams = serde_json::from_str(&request.args[0])
        .map_err(|_| ("invalid_request", "invalid host control params"))?;
    if params.schema_version != HOST_CONTROL_SCHEMA_V {
        return Err(("invalid_request", "unsupported host control schema"));
    }
    Ok((params, None))
}

fn read_frame<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut input = std::io::stdin().lock();
    let mut length = [0u8; 4];
    input.read_exact(&mut length).context("missing host control frame")?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(length > 0 && length <= HOST_CONTROL_MAX_FRAME_BYTES, "invalid host control frame size");
    let mut bytes = vec![0u8; length];
    input.read_exact(&mut bytes).context("incomplete host control frame")?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid host control JSON"))
}

fn response(id: &str) -> RpcResponse<ResultEnvelope> {
    RpcResponse {
        v: PROTOCOL_V,
        id: id.to_owned(),
        operation_id: None,
        scope: None,
        idempotency_key: None,
        ok: false,
        exit: 2,
        bin: "rsrs-runtime".to_owned(),
        envelope: None,
        error: None,
        code: None,
        web_url: None,
        pid: Some(std::process::id()),
        data_dir: None,
    }
}

#[cfg(test)]
mod keygen_handoff_request_tests {
    use super::*;

    #[test]
    fn existing_result_request_has_original_operation_and_no_claimed_authority() -> Result<()> {
        let wire = serde_json::json!({
            "v":PROTOCOL_V,"id":"00000000-0000-4000-8000-000000000001",
            "method":HOST_AUTH_RESULT_METHOD,
            "operation_id":"00000000-0000-4000-8000-000000000002","idempotency_key":"fixture-original",
            "args":[serde_json::json!({"schema_version":HOST_AUTH_SCHEMA_V,"force":false,
                "binding":{"installation_root":"/fixture","profile":"/fixture",
                    "launch_id":"fixture-launch","expected_account_id":null},
                "identity":{"kind":"unbound","operation_id":"00000000-0000-4000-8000-000000000002",
                    "session_sha256":"a".repeat(64)}}).to_string()]
        });
        let request: RpcRequest = serde_json::from_value(wire.clone())?;
        assert!(validate_auth_request(&request).is_ok());
        let mut removed: RpcRequest = serde_json::from_value(wire.clone())?;
        removed.method = "control.auth.keygen_complete".into();
        assert!(validate_auth_request(&removed).is_err());
        for field in ["operation_id", "idempotency_key"] {
            let mut missing = wire.clone();
            missing[field] = serde_json::Value::Null;
            let request: RpcRequest = serde_json::from_value(missing)?;
            assert!(validate_auth_request(&request).is_err());
        }
        let mut params: serde_json::Value = serde_json::from_str(&request.args[0])?;
        params["verified"] = serde_json::json!(true);
        let mut request = request;
        request.args = vec![params.to_string()];
        assert!(validate_auth_request(&request).is_err());
        Ok(())
    }
}
