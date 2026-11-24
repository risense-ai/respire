use super::*;

/// Wire the host services consumed by the shared worker/engine modules; the
/// shared code calls them through `respire_commands::host_hooks`. Installation
/// is idempotent (first wiring wins).
pub(super) fn install_cli_host_hooks() -> bool {
    let installed =
        respire_commands::host_hooks::install(respire_commands::host_hooks::HostHooks {
            background_paused: Some(|| autostart::background_paused()),
            check_direct_access: Some(|| autostart::check_direct_access()),
            note_credential_access: Some(|available| autostart::note_credential_access(available)),
            context_changed: None,
            call_runtime_method: Some(rpc::call_method_outcome),
            update_hint: Some(respire::update_check::hint),
            update_enabled: Some(respire::update_check::enabled),
            update_check_run: Some(|force| {
                respire::update_check::check(force).map(|status| {
                    respire_commands::host_hooks::UpdateStatus {
                        current: status.current,
                        latest: status.latest,
                        outdated: status.outdated,
                        cached: status.cached,
                    }
                })
            }),
            materialize_bin: Some(mcp::materialize_bin),
            foreground_progress: Some(rpc::foreground_progress),
            runtime_brief: Some(rpc::runtime_brief),
            host_command: Some(host_command),
            extract_tui_run: Some(run_extract_tui),
            runtime_is_up: Some(rpc::runtime_is_up),
            change_profile: Some(|change| rpc::change_profile(|| change())),
            managed_id: Some(|| autostart::managed_id()),
            verify_managed_boot: Some(|| autostart::verify_managed_boot()),
            record_exit: Some(|success| autostart::record_exit(success)),
            credential_state: Some(|| autostart::credential_state()),
            capture_run: Some(capture_run),
            mcp_http_response: None,
        });
    installed
}

pub(super) fn command_failure(error: &anyhow::Error) -> (ResultEnvelope, i32) {
    let (mut envelope, code) = respire_commands::output_emit::command_failure(error);
    if let Some(original) = error.downcast_ref::<autostart::CoreConfigUnknown>() {
        envelope.summary = serde_json::json!({"reason":"agent_config_outcome_unknown", "core_application":{"state":"unknown"}});
        envelope.details = original.details.clone();
        envelope.details["error_type"] = serde_json::json!("outcome_unknown");
        envelope.errors = vec!["Core configuration outcome is unknown; query the original operation without replaying".into()];
    } else if let Some(original) = error.downcast_ref::<autostart::AuthOutcomeUnknown>() {
        envelope.summary = serde_json::json!({"reason":"auth_outcome_unknown"});
        envelope.details = serde_json::json!({
            "error_type":"outcome_unknown", "operation_id":original.operation_id,
            "force":original.force, "result_command":"runtime auth-result", "replayed":false,
        });
    } else if let Some(original) = error.downcast_ref::<autostart::IdentityWriteRecoveryRequired>()
    {
        envelope.summary = serde_json::json!({"reason":"identity_write_recovery_required"});
        envelope.details = serde_json::json!({
            "error_type":"identity_write_recovery_required", "operation_id":original.operation_id,
            "idempotency_key":original.key, "source_profile":original.profile,
            "operation":original.operation, "query_only":original.query_only, "replayed":false,
            "result_command":"repeat the same logout/session revoke command",
        });
    } else if let Some(original) = error.downcast_ref::<autostart::KeyRecoveryRequired>() {
        envelope.summary = serde_json::json!({"reason":"key_outcome_unknown"});
        envelope.details = serde_json::json!({
            "error_type":"outcome_unknown", "operation_id":original.operation_id,
            "source_profile":original.source_profile, "result_command":"runtime key-result",
            "query_only":true, "replayed":false,
        });
    } else if let Some(original) =
        error.downcast_ref::<respire_host_control::AccountHandoffPending>()
    {
        let mut result_args = vec![
            "--operation-id".to_owned(),
            original.operation_id.clone(),
            "--source-profile".to_owned(),
            original.source_profile.to_string_lossy().into_owned(),
        ];
        if original.register {
            result_args.push("--register".to_owned());
        }
        envelope.summary = serde_json::json!({"reason":"account_handoff_pending"});
        envelope.details = serde_json::json!({
            "error_type":"handoff_pending", "operation_id":original.operation_id,
            "source_profile":original.source_profile, "register":original.register,
            "committed":true, "result_command":"runtime account-result", "result_args":result_args,
            "query_only":true, "replayed":false,
        });
    } else if let Some(original) = error.downcast_ref::<autostart::AccountOutcomeUnknown>() {
        envelope.summary = serde_json::json!({"reason":"account_outcome_unknown"});
        envelope.details = serde_json::json!({
            "error_type":"outcome_unknown", "operation_id":original.operation_id,
            "source_profile":original.source_profile, "register":original.register,
            "result_command":"runtime account-result", "replayed":false,
        });
    }
    (envelope, code)
}

/// Execute the process command line through the shared CLI host frontend.
/// The embedding binary must dispatch private Runtime entry points first.
pub fn run_frontend() -> i32 {
    if !install_cli_host_hooks() {
        eprintln!("host hooks were installed twice; keeping the first wiring");
    }
    // The command enum and the ONNX session are large. Debug builds on Windows
    // give the main thread a 1MB stack, which overflows inside clap. Do the work
    // on a thread with room.
    let handle = match std::thread::Builder::new()
        .name("rsrs".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(main_body)
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("failed to start the command thread: {error}");
            return 1;
        }
    };
    match handle.join() {
        Ok(exit) => exit,
        Err(payload) => {
            eprintln!("command thread panicked: {}", panic_message(payload));
            1
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "command thread panicked without a string payload".to_owned()
}

fn main_body() -> i32 {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().is_some_and(|arg| arg == "--runtime-plugin-host-internal") {
        return match crate::autostart::run_detached_plugin_host(arguments) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Runtime host plugin channel failed: {error:#}");
                1
            }
        };
    }
    #[cfg(feature = "client2-host-prototype")]
    if std::env::args_os().nth(1).is_some_and(|arg| {
        arg == "--client2-host-supervisor-internal" || arg == "--client2-host-internal"
    }) {
        if let Err(error) = client2_host::reject_startup(std::env::args_os().skip(2)) {
            eprintln!("Client2 startup diagnostic delivery failed: {error}");
        }
        return 1;
    }
    let roots_requested = std::env::args_os().any(|arg| arg == "--roots");
    if !roots_requested && std::env::args_os().any(|arg| arg == "--host-rpc-stdio") {
        set_json_mode(true);
        if std::env::args_os().any(|arg| arg == "--client-only") {
            std::env::set_var("RSRS_CLIENT_ONLY", "1");
        }
        return match respire_client_host::roots::validate_client_environment()
            .and_then(|()| host_rpc::serve())
        {
            Ok(code) => code,
            Err(error) => {
                let (envelope, code) = command_failure(&error);
                if let Err(output_error) = emit_result(envelope) {
                    eprintln!("host RPC reply failed: {output_error}");
                }
                code
            }
        };
    }
    set_json_mode(
        std::env::args_os().any(|arg| arg == "--json")
            || respire::env::var("RSRS_JSON").is_ok_and(|value| value == "1" || value == "true"),
    );
    if std::env::args_os().any(|arg| arg == "--client-only") {
        std::env::set_var("RSRS_CLIENT_ONLY", "1");
    }
    // Parse args, then run the command, so a Windows debug stack does not hold both at once.
    // The hidden runtime entry and `--direct` are peeled off before clap: the command enum is large enough that
    // extra derived fields overflow the debug main thread.
    let result = match preprocess_args() {
        Preparsed::Roots { data_dir } => {
            if let Some(dir) = data_dir {
                std::env::remove_var("RSRS_DEFAULT_DATA_DIR");
                std::env::set_var("RSRS_DATA_DIR", dir);
            }
            respire_client_host::roots::resolved_roots().and_then(|roots| {
                emit_result(ResultEnvelope::new(
                    "roots",
                    OutputStatus::Ok,
                    serde_json::to_value(roots)?,
                    Vec::new(),
                ))
            })
        }
        Preparsed::Version { json } => {
            set_json_mode(json);
            crate::app_version::emit(json)
        }
        Preparsed::RuntimeHelp { json } => {
            set_json_mode(json);
            emit_runtime_help()
        }
        Preparsed::Runtime { flags, json } => {
            set_json_mode(json);
            prepare_command_runtime().and_then(|()| rpc::runtime_entry(flags))
        }
        Preparsed::Invalid(error) => {
            Err(respire_client_host::input_error::InputError(error).into())
        }
        Preparsed::Cli { direct, args } => {
            DIRECT_MODE.store(direct, Ordering::Relaxed);
            match Cli::try_parse_from(std::iter::once(std::ffi::OsString::from("rsrs")).chain(args))
            {
                Ok(cli)
                    if matches!(
                        cli.command,
                        Some(
                            Command::AgentConfig { set: None }
                                | Command::MemoryRevision
                                | Command::Capabilities
                                | Command::Runtime { .. }
                                | Command::Doctor { .. }
                                | Command::UpdateCheck { .. }
                        )
                    ) =>
                {
                    run(cli)
                }
                Ok(cli) => prepare_command_runtime().and_then(|()| run(cli)),
                Err(error) if error.exit_code() == 0 => error.print().map_err(Into::into),
                Err(error) => {
                    let code = error.exit_code();
                    emit_result(parse_failure(&error)).map(|()| set_exit_code(code))
                }
            }
        }
    };
    if let Err(error) = result {
        let (failure, code) = command_failure(&error);
        // Persist only the fixed event: error chains can contain OAuth codes
        // or user input. Keep the interactive stderr/envelope unchanged.
        respire_logging::error(format_args!("command failed"));
        if output_emitted() {
            eprintln!("ERROR: {error:#}");
        } else {
            if let Err(render_error) = emit_result(failure) {
                eprintln!("ERROR: {error:#}; output_error: {render_error:#}");
            }
        }
        return code;
    }
    let exit_code = exit_code();
    if exit_code != 0 {
        return exit_code;
    }
    0
}

/// Normal command bootstrap, intentionally omitted by the read-only revision probe.
pub(super) fn prepare_command_runtime() -> Result<()> {
    respire_client_host::roots::validate_client_environment()?;
    if !runtime_policy::client_only() {
        respire_client_host::paths::ensure_default_home()
            .map_err(|error| anyhow!("default profile migration failed: {error:#}"))?;
        // File logging creates its parent directory even before an event. A
        // client-only command must leave profiles untouched; its existing
        // stderr/envelope and default panic diagnostics remain available.
        install_error_logging();
    }
    Ok(())
}

/// Point the error log at the resolved data root. The sink is best effort:
/// a failure here keeps every record call a no-op and stderr stays the only
/// diagnostic surface, so the result is deliberately ignored.
pub(super) fn install_error_logging() {
    let log_path = respire_client_host::paths::main_data_dir()
        .join("logs")
        .join("cli.log");
    let _ = respire_logging::init(respire_logging::LoggingConfig::new(log_path));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| location.to_string())
            .unwrap_or_else(|| "unknown".to_owned());
        respire_logging::log_panic(info.payload(), &location);
        previous(info);
    }));
}

enum Preparsed {
    Roots {
        data_dir: Option<String>,
    },
    RuntimeHelp {
        json: bool,
    },
    Version {
        json: bool,
    },
    Runtime {
        flags: rpc::RuntimeFlags,
        json: bool,
    },
    Invalid(String),
    Cli {
        direct: bool,
        args: Vec<std::ffi::OsString>,
    },
}

/// Static lifecycle help is also available to sandbox clients without starting a host.
fn emit_runtime_help() -> Result<()> {
    let usage = "Usage: rsrs --runtime-internal [--no-open] [--host 127.0.0.1] [--port PORT] [--status | --stop]\n\nHost runtime: loopback HTTP only. Without --status/--stop, serve until stopped.\n  --status   Inspect the existing host without starting it\n  --stop     Ask the owned host runtime to stop\n  --no-open  Do not open a browser (accepted for compatibility)\n\nClient-only tools connect to the host; lifecycle changes require the host terminal.\nFor Windows sign-in startup, see docs/runtime.md.\n";
    if json_mode() {
        emit_result(ResultEnvelope::new(
            "runtime-help",
            OutputStatus::Ok,
            serde_json::json!({"usage":usage,"lifecycle":"host-managed","transport":"loopback-http"}),
            Vec::new(),
        ))
    } else {
        print!("{usage}");
        mark_emitted();
        Ok(())
    }
}

fn is_short_v_version(args: &[String]) -> Option<bool> {
    let mut json = false;
    let mut version = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-v" => version = true,
            _ => return None,
        }
    }
    if version {
        Some(json)
    } else {
        None
    }
}

fn preprocess_args() -> Preparsed {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let direct = args.iter().any(|arg| arg == "--direct");
    args.retain(|arg| arg != "--direct" && arg != "--client-only");
    if args.iter().any(|arg| arg == "--roots") {
        let mut data_dir = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--roots" | "--json" => {}
                "--data-dir" if data_dir.is_none() => {
                    index += 1;
                    match args
                        .get(index)
                        .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
                    {
                        Some(value) => data_dir = Some(value.clone()),
                        None => {
                            return Preparsed::Invalid(
                                "--roots requires a nonempty --data-dir".to_owned(),
                            )
                        }
                    }
                }
                _ => {
                    return Preparsed::Invalid(
                        "use --roots [--data-dir PATH] [--json] without a command".to_owned(),
                    )
                }
            }
            index += 1;
        }
        return Preparsed::Roots { data_dir };
    }
    if let Some(json) = is_short_v_version(&args) {
        return Preparsed::Version { json };
    }
    if args
        .iter()
        .find(|arg| arg.as_str() != "--json")
        .is_some_and(|arg| arg == "--runtime-internal")
    {
        if args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
            && args.iter().all(|arg| {
                matches!(
                    arg.as_str(),
                    "--runtime-internal" | "--help" | "-h" | "--json"
                )
            })
        {
            return Preparsed::RuntimeHelp {
                json: json_mode() || args.iter().any(|arg| arg == "--json"),
            };
        }
        let mut flags = rpc::RuntimeFlags {
            port: None,
            host: "127.0.0.1".to_owned(),
            status: false,
            stop: false,
        };
        let mut json = false;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--runtime-internal" | "--no-open" => {}
                "--json" => json = true,
                "--status" => flags.status = true,
                "--stop" => flags.stop = true,
                "--port" => {
                    index += 1;
                    match args
                        .get(index)
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|port| *port > 0)
                    {
                        Some(port) => flags.port = Some(port),
                        None => {
                            return Preparsed::Invalid(
                                "internal runtime requires a valid --port".to_owned(),
                            )
                        }
                    }
                }
                "--host" => {
                    index += 1;
                    match args.get(index).filter(|host| !host.is_empty()) {
                        Some(host) => flags.host = host.clone(),
                        None => {
                            return Preparsed::Invalid(
                                "internal runtime requires --host".to_owned(),
                            )
                        }
                    }
                }
                other => {
                    return Preparsed::Invalid(format!("unknown internal runtime flag: {other}"))
                }
            }
            index += 1;
        }
        if flags.status && flags.stop {
            return Preparsed::Invalid(
                "internal runtime cannot combine --status and --stop".to_owned(),
            );
        }
        return Preparsed::Runtime { flags, json };
    }
    Preparsed::Cli {
        direct,
        args: args.into_iter().map(std::ffi::OsString::from).collect(),
    }
}
