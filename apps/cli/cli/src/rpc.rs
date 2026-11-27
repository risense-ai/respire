//! Local authenticated runtime transport (client face).
//!
//! Commands connect only to the independent Runtime over loopback HTTP and
//! auto-start it through the lifecycle coordinator. No in-process dispatch.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::output::{ResultEnvelope, Status as OutputStatus};

mod api;

const START_POLLS: usize = 100;
const START_WAIT: Duration = Duration::from_millis(150);

pub(crate) type RpcResponse = respire_protocol::RpcResponse<ResultEnvelope>;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeFlags {
    pub port: Option<u16>,
    pub host: String,
    pub status: bool,
    pub stop: bool,
}

pub fn call_from_argv() -> Result<()> {
    call_from_argv_with_start(true)
}

/// Poll the existing loopback runtime without lifecycle or recovery writes.
pub fn call_existing_from_argv() -> Result<()> {
    call_from_argv_with_start(false)
}

fn call_from_argv_with_start(auto_start: bool) -> Result<()> {
    let mut args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--direct" && arg != "--client-only")
        .collect();
    let json = args.iter().any(|arg| arg == "--json")
        || respire::env::var("RSRS_JSON").is_ok_and(|v| v == "1" || v == "true");
    crate::set_json_mode(json);
    if let Some(id) = crate::progress::remote_id() {
        let insertion = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        let mut progress_args = vec!["--progress-id".to_owned(), id.clone()];
        if command_name(&args) == Some("model") && !args.iter().any(|arg| arg == "--model-task-id")
        {
            progress_args.extend(["--model-task-id".to_owned(), id]);
        }
        args.splice(insertion..insertion, progress_args);
    }
    let response = call_method("cli.exec", args, auto_start)?;
    render_response(response, json)
}

pub fn runtime_is_up() -> bool {
    call_method("runtime.status", Vec::new(), false).is_ok()
}

pub(crate) fn foreground_progress(id: &str) -> Result<Value> {
    let response = call_method("cli.progress", vec![id.to_owned()], false)?;
    anyhow::ensure!(response.ok, "runtime progress unavailable");
    Ok(response
        .envelope
        .map(|envelope| envelope.summary)
        .unwrap_or(Value::Null))
}

/// Run one command through the resident runtime and return its JSON envelope.
/// Starts the runtime if it is not up yet. The envelope is returned even when
/// the command status is fail, so a dashboard can show the failing checks.
pub fn query_json(args: Vec<String>) -> Result<serde_json::Value> {
    query_json_with_start(args, true)
}

/// TUI polling must not start or replace a runtime while a host action is running.
pub(crate) fn query_existing_json(args: Vec<String>) -> Result<serde_json::Value> {
    query_json_with_start(args, false)
}

fn query_json_with_start(args: Vec<String>, auto_start: bool) -> Result<serde_json::Value> {
    let (envelope, _) = query_command(args, auto_start)?;
    serde_json::to_value(envelope).context("runtime envelope could not be encoded")
}

pub(crate) fn query_command(args: Vec<String>, auto_start: bool) -> Result<(ResultEnvelope, String)> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("--json".to_owned());
    full.extend(args);
    let response = call_method("cli.exec", full, auto_start)?;
    if let Some(mut envelope) = response.envelope {
        envelope.ensure_failure_diagnostics();
        return Ok((envelope, response.bin));
    }
    let err = response
        .error
        .unwrap_or_else(|| "runtime returned an empty response".to_owned());
    bail!("{err}")
}

/// Pid, web URL and binary version of the resident runtime, if it answers.
pub fn runtime_brief() -> Option<(u32, String, String)> {
    let response = call_method("runtime.status", Vec::new(), false).ok()?;
    Some((
        response.pid.unwrap_or(0),
        response.web_url.unwrap_or_default(),
        response.bin,
    ))
}

mod profile;
pub use profile::{change_host_assets, change_profile, change_profile_with_rollback, change_profile_with_verification,
    recover_interrupted_login, recover_legacy_login_before_account_control, stop_if_running, verify_open_store, verify_snapshot_store};
pub(crate) use profile::restart_runtime;
#[cfg(test)]
use profile::{needs_legacy_login_recovery, LoginRecoveryCaller};

pub fn runtime_entry(flags: RuntimeFlags) -> Result<()> {
    if flags.status {
        return match call_method("runtime.status", Vec::new(), false) {
            Ok(response) => {
                if crate::json_mode() {
                    return crate::emit_result(ResultEnvelope::new(
                        "runtime",
                        OutputStatus::Ok,
                        json!({"state":"up","pid":response.pid,"url":response.web_url,"data_dir":response.data_dir}),
                        Vec::new(),
                    ));
                }
                println!(
                    "runtime=up pid={} url={} data_dir={}",
                    response.pid.unwrap_or(0),
                    response.web_url.as_deref().unwrap_or(""),
                    response.data_dir.as_deref().unwrap_or("")
                );
                Ok(())
            }
            Err(error) => {
                if crate::json_mode() {
                    let mut envelope = ResultEnvelope::new(
                        "runtime",
                        OutputStatus::Warn,
                        json!({"state":"down"}),
                        Vec::new(),
                    );
                    envelope.errors.push(format!("{error:#}"));
                    return crate::emit_result(envelope);
                }
                eprintln!("{error:#}");
                crate::set_exit_code(2);
                Ok(())
            }
        };
    }
    if flags.stop {
        stop_if_running()?;
        if crate::json_mode() {
            return crate::emit_result(ResultEnvelope::new(
                "runtime",
                OutputStatus::Ok,
                json!({"state":"stopped"}),
                Vec::new(),
            ));
        }
        println!("runtime=stopped");
        return Ok(());
    }
    crate::runtime_policy::require_host("runtime startup")?;
    let executable = resolve_runtime_executable()?;
    let mut arguments: Vec<std::ffi::OsString> = vec![
        "serve".into(),
        "--data-dir".into(),
        respire_client_host::paths::main_data_dir().into_os_string(),
        "--host".into(),
        flags.host.into(),
    ];
    if let Some(port) = flags.port {
        arguments.push("--port".into());
        arguments.push(port.to_string().into());
    }
    let managed_id = crate::autostart::managed_id();
    if managed_id.is_some() {
        arguments.push("--supervised".into());
    }
    let (mut child, _instance_id) =
        crate::autostart::spawn_authorized_runtime(&executable, &arguments, managed_id)
            .context("failed to start host runtime")?;
    // The forwarding host stays alive for the serving process's whole lifetime
    // and passes its exit code through unchanged.
    let status = child
        .wait()
        .context("failed to wait for the host runtime")?;
    crate::set_exit_code(status.code().unwrap_or(1));
    Ok(())
}

/// Detect an explicit CLI selection so callers can reject it, never authorize it.
pub(crate) fn explicit_cli_runtime(executable: &std::path::Path) -> Result<bool> {
    if let Some(selected) = crate::embedding::selection() {
        anyhow::ensure!(crate::mcp::same_path(executable, selected.verify()?.path()),
            "embedded host selected a different Runtime image");
        return Ok(false);
    }
    let Some(selected) = respire::env::var("RSRS_RUNTIME_BIN")
        .ok()
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty())
    else {
        return Ok(false);
    };
    Ok(crate::mcp::same_path(executable, &selected)
        && crate::mcp::same_path(executable, &std::env::current_exe()?))
}

/// Resolution and installation failures stay errors, including typed repair errors.
/// Neither detached, foreground nor supervised startup synthesizes a shim plan.
pub(crate) fn resolve_runtime_executable() -> Result<PathBuf> {
    let executable = crate::runtime_fetch::resolve_runtime_bin()?;
    anyhow::ensure!(
        crate::embedding::selection().is_some()
            || !crate::mcp::same_path(&executable, &std::env::current_exe()?),
        "CLI image cannot substitute for the independent Runtime; repair the selected Runtime"
    );
    Ok(executable)
}

fn render_response(response: RpcResponse, json: bool) -> Result<()> {
    if let Some(mut envelope) = response.envelope {
        envelope.ensure_failure_diagnostics();
        crate::progress::finish();
        if json || envelope.summary["skipped"] != true || envelope.summary["mode"] != "off" {
            println!("{}", envelope.render(json)?);
        }
        crate::mark_emitted();
        crate::set_exit_code(response.exit);
        return Ok(());
    }
    bail!(
        "{}",
        response
            .error
            .unwrap_or_else(|| "runtime returned an empty response".to_owned())
    )
}

pub(crate) fn call_method(
    method: &str,
    args: Vec<String>,
    auto_start: bool,
) -> Result<RpcResponse> {
    let plugins = if method == "cli.exec"
        && matches!(command_name(&args), Some("remember" | "recall" | "forget" | "sync"))
    {
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(std::iter::once("rsrs".to_owned()).chain(args.clone()))?;
        let (_, profile) = respire_client_host::config::control_roots()?;
        let config = respire_commands::hooks::read_config_at(&profile.join("plugins.json"))?;
        cli.command.map(|command| (command, config))
    } else {
        None
    };
    let auto = auto_start && !crate::net_rpc::no_autostart();
    // Validate/start the runtime for this data directory before submitting work.
    // Once submitted, a disconnect must not cause a mutating command to be replayed.
    if auto {
        ensure_daemon()?;
    } else if method == "cli.exec" && needs_inference(&args) && crate::runtime_policy::client_only()
    {
        anyhow::ensure!(!crate::net_rpc::health()?.inference_stalled(),
            "inference is unresponsive after cancellation; run rsrs from the host terminal to recover the runtime; this request was not submitted");
    }
    if let Some((command, config)) = plugins.as_ref().filter(|(_, config)| !config.hooks.is_empty()) {
        // Old runtimes execute plugins themselves. Reject them before submitting
        // business work so a host plugin cannot run twice.
        let capabilities = http_roundtrip("cli.exec", &["--json".into(), "capabilities".into()])?;
        anyhow::ensure!(compatible(&capabilities) && capabilities.ok
            && capabilities.envelope.as_ref().is_some_and(|envelope| envelope.summary["host_plugin_events"].is_array()),
            "runtime does not support host plugin events; command was not submitted");
        return run_plugin_hooks(command, config, || {
            let response = http_roundtrip(method, &args)?;
            anyhow::ensure!(compatible(&response), "local runtime protocol/version mismatch");
            Ok(response)
        });
    }
    let response = http_roundtrip(method, &args)?;
    if !compatible(&response) {
        bail!("local runtime protocol/version mismatch");
    }
    Ok(response)
}

/// The pre hook runs before submission; post receipts describe actual core events,
/// including commits followed by a later command failure. Never replay the RPC.
fn run_plugin_hooks(
    command: &crate::Command,
    config: &respire_commands::hooks::HooksConfig,
    submit: impl FnOnce() -> Result<RpcResponse>,
) -> Result<RpcResponse> {
    use respire_commands::hooks::{self, HookEvent};
    if let crate::Command::Remember { content, title, importance, r#type, project, .. } = command {
        let verdict = hooks::fire_config(config, HookEvent::PreRemember,
            hooks::remember_payload(title, importance, r#type.as_deref().unwrap_or("context"), project, content));
        for warning in verdict.warnings {
            eprintln!("WARN {warning}");
        }
        anyhow::ensure!(!verdict.blocked, "{}; write was not submitted", verdict.reason);
    }
    let mut response = submit()?;
    let Some(envelope) = response.envelope.as_mut() else {
        // Admission failures have no business events. A successful response must
        // carry its event receipts, otherwise hook delivery is unknown.
        anyhow::ensure!(!response.ok, "runtime omitted plugin event receipts; do not retry the command");
        return Ok(response);
    };
    let events = envelope.summary.get("host_plugin_events")
        .and_then(Value::as_array)
        .context("runtime omitted plugin event receipts; do not retry the command")?;
    let expected = match command {
        crate::Command::Remember { .. } => HookEvent::PostRemember,
        crate::Command::Recall { .. } => HookEvent::PostRecall,
        crate::Command::Forget { .. } => HookEvent::PostForget,
        crate::Command::Sync => HookEvent::PostSync,
        _ => bail!("unsupported plugin command"),
    };
    // Validate the full receipt before launching any shell commands.
    for receipt in events {
        anyhow::ensure!(receipt["event"].as_str() == Some(expected.as_str()) && receipt["data"].is_object(),
            "runtime returned invalid plugin event receipts; do not retry the command");
    }
    for receipt in events {
        let verdict = hooks::fire_config(config, expected, receipt["data"].clone());
        for warning in verdict.warnings {
            eprintln!("WARN {warning}");
            envelope.errors.push(warning);
            if matches!(envelope.status, OutputStatus::Ok | OutputStatus::Skip) {
                envelope.status = OutputStatus::Warn;
                response.exit = respire_commands::output_state::status_exit_code(envelope.status);
            }
        }
    }
    Ok(response)
}

pub(crate) fn call_method_outcome(
    method: &str,
    args: Vec<String>,
    auto_start: bool,
) -> Result<respire_commands::host_hooks::MethodOutcome> {
    call_method(method, args, auto_start).map(|response| {
        respire_commands::host_hooks::MethodOutcome {
            ok: response.ok,
            error: response.error,
            envelope: response.envelope,
        }
    })
}

pub(crate) fn model_control(task_id: &str, cancel: bool) -> Result<Value> {
    let (method, args) = if cancel {
        ("model.control", vec![task_id.to_owned(), "true".to_owned()])
    } else {
        ("model.progress", Vec::new())
    };
    let response = call_method(method, args, false)?;
    anyhow::ensure!(response.ok, "{}", response.error.unwrap_or_default());
    let summary = response
        .envelope
        .map(|envelope| envelope.summary)
        .ok_or_else(|| anyhow::anyhow!("runtime did not return model progress"))?;
    let progress = if cancel {
        summary
    } else {
        summary["operation"].clone()
    };
    model_progress_for_task(progress, task_id, cancel)
}

fn model_progress_for_task(progress: Value, task_id: &str, cancel: bool) -> Result<Value> {
    anyhow::ensure!(
        progress.get("active").and_then(Value::as_bool).is_some(),
        "runtime returned invalid model progress"
    );
    if progress["active"] == true && progress["id"].as_str() != Some(task_id) {
        // Keep the stale contract without exposing a different task's progress.
        anyhow::ensure!(
            !cancel || progress["stale"] == true,
            "runtime returned a foreign model task"
        );
        return Ok(serde_json::json!({"active":true,"stale":true}));
    }
    Ok(progress)
}

pub(crate) fn prepare_model(mirror: &str) -> Result<()> {
    let response = call_method("model.prepare", vec![mirror.to_owned()], false)?;
    anyhow::ensure!(response.ok, "{}", response.error.unwrap_or_default());
    Ok(())
}

pub(crate) fn profile_request(method: &str, args: Vec<String>) -> Result<Value> {
    let response = call_method(method, args, true)?;
    anyhow::ensure!(
        response.ok,
        "{}",
        response
            .error
            .unwrap_or_else(|| "model profile request failed".to_owned())
    );
    Ok(serde_json::to_value(
        response
            .envelope
            .context("model profile response missing")?,
    )?)
}

fn http_roundtrip(method: &str, args: &[String]) -> Result<RpcResponse> {
    match method {
        "runtime.status" => {
            let health = api::health()?;
            Ok(RpcResponse {
                v: health.v,
                id: "health".into(),
                operation_id: None,
                scope: None,
                idempotency_key: None,
                ok: true,
                exit: 0,
                bin: health.bin,
                envelope: None,
                error: None,
                code: None,
                web_url: Some(health.url),
                pid: Some(health.pid),
                data_dir: Some(health.data_dir),
            })
        }
        "runtime.stop" => {
            crate::net_rpc::request_stop()?;
            Ok(RpcResponse {
                v: PROTOCOL_V,
                id: "stop".into(),
                operation_id: None,
                scope: None,
                idempotency_key: None,
                ok: true,
                exit: 0,
                bin: env!("CARGO_PKG_VERSION").to_owned(),
                envelope: None,
                error: None,
                code: None,
                web_url: None,
                pid: None,
                data_dir: None,
            })
        }
        "cli.exec" | "cli.progress" | "model.control" | "model.prepare" | "index.prepare"
        | "model.catalog" | "model.configure" | "model.switch" | "model.resume"
        | "model.progress" => {
            let parsed = if method == "cli.exec" {
                crate::net_rpc::rpc_exec(args.to_vec())?
            } else {
                crate::net_rpc::rpc_method(method, args.to_vec())?
            };
            serde_json::from_value(parsed).context("runtime rpc response shape mismatch")
        }
        other => bail!("unknown runtime method {other}"),
    }
}

fn compatible(response: &RpcResponse) -> bool {
    response.v == PROTOCOL_V && response.code.as_deref() != Some("protocol_version_mismatch")
}

pub(crate) use respire_commands::rpc_client::{
    command_name, command_words, diary_lexical_mode, is_context_query, is_status, needs_inference,
    GLOBAL_VALUE_FLAGS, PROTOCOL_V,
};

mod owner;
pub(crate) use owner::{drain_occupant, drain_occupant_for_restart, endpoint_path, ensure_daemon,
    ensure_daemon_locked, ensure_daemon_locked_with_expectation, force_stop_for_reset, probe_runtime,
    runtime_acceptable, stop_occupant};
#[cfg(test)]
use owner::{endpoint_pid, reusable_business_runtime, wait_daemon_ready};

#[cfg(test)]
mod runtime_resolution_tests;

#[cfg(test)]
mod tests;
