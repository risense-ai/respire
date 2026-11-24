//! Local launch-grant producer. The inherited token identifies one private grant.
use super::super::{
    lifecycle, private_write, read_registration, recovery::account_identity_source, Registration,
};
use anyhow::{Context, Result};

#[path = "shared_host.rs"]
pub(super) mod shared_host;

#[path = "control.rs"]
pub(crate) mod control;
#[path = "identity_write_control.rs"]
pub(crate) mod identity_write_control;
#[path = "space_control.rs"]
pub(crate) mod space_control;
#[path = "profile_maintenance.rs"]
pub(crate) mod profile_maintenance;
#[path = "secret_control.rs"]
pub(crate) mod secret_control;
#[path = "key_control.rs"]
pub(crate) mod key_control;
#[path = "migration_control.rs"]
pub(crate) mod migration_control;
#[path = "metadata_control.rs"]
pub(crate) mod metadata_control;
#[path = "model_control.rs"]
pub(crate) mod model_control;
#[path = "model_maintenance.rs"]
pub(crate) mod model_maintenance;
#[path = "config_control.rs"]
pub(crate) mod config_control;
#[path = "agent_config_control.rs"]
pub(crate) mod agent_config_control;
use respire_commands::output::{ResultEnvelope, Status};
use respire_protocol::{
    ControlBinding, HostControlLaunchGrant, HostControlOperation, HostControlParams, RpcRequest,
    RpcResponse, HOST_CONTROL_MAX_FRAME_BYTES, HOST_CONTROL_SCHEMA_V,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) fn spawn(
    binary: &Path,
    arguments: &[OsString],
    managed_id: Option<&str>,
) -> Result<(Child, String)> {
    let (child, _, nonce) = spawn_serving(binary, arguments, managed_id, false)?;
    Ok((child, nonce))
}

pub(crate) fn spawn_detached_runtime(
    binary: &Path,
    arguments: &[OsString],
) -> Result<(Child, u32, String)> {
    spawn_serving(binary, arguments, None, true)
}

fn spawn_serving(
    binary: &Path,
    arguments: &[OsString],
    managed_id: Option<&str>,
    detached: bool,
) -> Result<(Child, u32, String)> {
    let binary = binary.canonicalize()?;
    let (root, profile) = prepare_launch_roots()?;
    let bridge = if detached {
        let plugins = respire_commands::hooks::read_config_at(&profile.join("plugins.json"))?;
        if plugins.hooks.get("post-sync").is_some_and(|hooks| !hooks.is_empty()) {
            let source = std::env::current_exe()?;
            let directory = root.join("runtime/plugin-host").join(hash(&fs::read(&source)?));
            let destination = directory.join(crate::mcp::stable_bin_path().file_name()
                .context("CLI executable name missing")?);
            crate::mcp::install_binary(&source, &destination).map_err(anyhow::Error::msg)?;
            Some(destination)
        } else {
            None
        }
    } else {
        None
    };
    let (grant, nonce) = prepare_grant(
        &binary,
        &root,
        &profile,
        managed_id,
        launch_port(arguments)?,
    )?;
    let path = root
        .join("runtime")
        .join("autostart")
        .join("launch")
        .join(format!("launch-{nonce}.json"));
    private_write(&path, &serde_json::to_vec(&grant)?, None)?;
    let token = format!("launch-v1:{nonce}");
    let spawned = match bridge {
        Some(executable) => spawn_detached_plugin_host(&executable, &binary, arguments, &token, &profile),
        None => {
            let child = if detached {
                respire_spawn::spawn_binary_with_boot_token(&binary, arguments, Some(&token))
                    .map_err(Into::into)
            } else {
                spawn_with_plugin_host(&binary, arguments, &token, &profile)
            };
            child.map(|child| {
                let pid = child.id();
                (child, pid)
            })
        }
    };
    match spawned {
        Ok((child, pid)) => Ok((child, pid, nonce)),
        Err(error) => {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {}
                Err(cleanup) => return Err(cleanup).context(error).context("unused launch grant cleanup failed"),
            }
            Err(error).context("authorized Runtime could not be spawned")
        }
    }
}

fn spawn_with_plugin_host(
    binary: &Path,
    arguments: &[OsString],
    token: &str,
    profile: &Path,
) -> Result<Child> {
    let mut command = Command::new(binary);
    // Stdout carries framed plugin events; suppress Runtime's human startup banner.
    command.arg("--json").args(arguments).env("RSRS_RUNTIME_BOOT_TOKEN", token)
        .env("RSRS_HOST_PLUGIN_CHANNEL", "1").env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let mut child = respire_spawn::spawn_owned_runtime(&mut command)?;
    if let Err(error) = start_plugin_host(&mut child, profile.to_path_buf()) {
        if child.try_wait()?.is_none() {
            child.kill().context("stop Runtime after plugin channel initialization failure")?;
        }
        child.wait().context("reap Runtime after plugin channel initialization failure")?;
        return Err(error);
    }
    Ok(child)
}

fn spawn_detached_plugin_host(
    executable: &Path,
    binary: &Path,
    arguments: &[OsString],
    token: &str,
    profile: &Path,
) -> Result<(Child, u32)> {
    let diagnostics = executable.parent().context("plugin host directory missing")?.join("plugin-host.log");
    let log = fs::OpenOptions::new().create(true).append(true).open(&diagnostics)?;
    let mut command = Command::new(executable);
    command.arg("--runtime-plugin-host-internal").arg(profile).arg(binary).args(arguments)
        .env("RSRS_RUNTIME_BOOT_TOKEN", token)
        .env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN")
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(log);
    let mut helper = respire_spawn::spawn_detached_command(&mut command).context("start background plugin host")?;
    let mut pid = [0; 4];
    if let Err(error) = helper.stdout.take().context("plugin host readiness pipe missing")?.read_exact(&mut pid) {
        if helper.try_wait()?.is_none() {
            helper.kill().context("stop plugin host after failed readiness")?;
        }
        helper.wait().context("reap plugin host after failed readiness")?;
        return Err(error).with_context(|| format!("background plugin host did not start Runtime; see {}", diagnostics.display()));
    }
    Ok((helper, u32::from_le_bytes(pid)))
}

pub(crate) fn run_detached_plugin_host(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let profile = args.next().context("Runtime plugin profile missing")?;
    let binary = args.next().context("Runtime plugin binary missing")?;
    let token = std::env::var("RSRS_RUNTIME_BOOT_TOKEN").context("Runtime launch grant missing")?;
    let arguments: Vec<_> = args.collect();
    let mut child = spawn_with_plugin_host(Path::new(&binary), &arguments, &token, Path::new(&profile))?;
    let mut output = std::io::stdout().lock();
    output.write_all(&child.id().to_le_bytes())?;
    output.flush()?;
    drop(output);
    let status = child.wait().context("reap Runtime owned by background plugin host")?;
    anyhow::ensure!(status.success(), "Runtime owned by background plugin host exited with {status}");
    Ok(())
}

fn start_plugin_host(child: &mut Child, root: std::path::PathBuf) -> Result<()> {
    let input = child.stdin.take().context("Runtime host plugin input is missing")?;
    let output = child.stdout.take().context("Runtime host plugin output is missing")?;
    std::thread::Builder::new().name("runtime-plugin-host".into()).spawn(move || {
        if let Err(error) = serve_plugin_events(output, input, &root.join("plugins.json")) {
            // Dropping the writer revokes the Runtime parent channel. Never replay an event.
            eprintln!("Runtime host plugin channel failed: {error:#}");
            respire_logging::error(format_args!("Runtime host plugin channel failed: {error:#}"));
        }
    }).context("start Runtime host plugin reader")?;
    Ok(())
}

fn execute_plugin_event(bytes: &[u8], config: &Path) -> Result<respire_protocol::host_plugin::HostPluginEventAck> {
    use respire_protocol::host_plugin::*;
    let request: HostPluginEventRequest = serde_json::from_slice(bytes).context("invalid Runtime host plugin event")?;
    anyhow::ensure!(request.plugin_event == HOST_PLUGIN_SCHEMA_V
        && uuid::Uuid::parse_str(&request.id).is_ok(), "invalid Runtime host plugin event identity");
    let result = (|| -> Result<HostPluginEventResult> {
        let event = match request.event { HostPluginEvent::PostSync => respire_commands::hooks::HookEvent::PostSync };
        let verdict = respire_commands::hooks::fire_at(config, event, serde_json::to_value(request.data)?)?;
        if verdict.failures.is_empty() {
            Ok(HostPluginEventResult::Completed { warnings: verdict.warnings })
        } else {
            Ok(HostPluginEventResult::Failed { error: verdict.failures.join("; ") })
        }
    })().unwrap_or_else(|error| HostPluginEventResult::Failed { error: format!("{error:#}") });
    match &result {
        HostPluginEventResult::Failed { error } => {
            eprintln!("Runtime post-sync plugin failed: {error}");
            respire_logging::error(format_args!("Runtime post-sync plugin failed: {error}"));
        },
        HostPluginEventResult::Completed { warnings } => {
            for warning in warnings { eprintln!("Runtime post-sync plugin warning: {warning}"); }
        },
    }
    Ok(HostPluginEventAck { plugin_event_ack: HOST_PLUGIN_SCHEMA_V, id: request.id, result })
}

fn serve_plugin_events(mut output: impl Read, mut input: impl Write, config: &Path) -> Result<()> {
    loop {
        let mut length = [0_u8; 4];
        // EOF between frames is the Runtime's normal exit. Partial frames are errors.
        match output.read(&mut length[..1])? {
            0 => return Ok(()),
            1 => {},
            _ => unreachable!("one-byte read cannot return more than one byte"),
        }
        output.read_exact(&mut length[1..])?;
        let length = u32::from_be_bytes(length) as usize;
        anyhow::ensure!(length > 0 && length <= respire_protocol::HOST_CONTROL_MAX_FRAME_BYTES,
            "invalid Runtime host plugin event frame size");
        let mut bytes = vec![0_u8; length];
        output.read_exact(&mut bytes)?;
        let acknowledgement = execute_plugin_event(&bytes, config)?;
        let bytes = serde_json::to_vec(&acknowledgement)?;
        anyhow::ensure!(bytes.len() <= respire_protocol::HOST_CONTROL_MAX_FRAME_BYTES,
            "Runtime host plugin acknowledgement exceeds protocol limit");
        input.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
        input.write_all(&bytes)?;
        input.flush()?;
    }
}

include!("launch_authorization/grants.rs");

include!("launch_authorization/exchange.rs");

#[cfg(test)]
#[path = "launch_authorization/tests.rs"]
mod tests;
