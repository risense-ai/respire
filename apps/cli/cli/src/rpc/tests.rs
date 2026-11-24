use super::*;
use std::io::Read;

fn test_response(id: &str) -> RpcResponse {
    RpcResponse {
        v: PROTOCOL_V,
        id: id.to_owned(),
        operation_id: None,
        scope: None,
        idempotency_key: None,
        ok: true,
        exit: 0,
        bin: "rsrs-runtime".into(),
        envelope: None,
        error: None,
        code: None,
        web_url: None,
        pid: None,
        data_dir: None,
    }
}

struct TestEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct OfficialRuntime {
    child: std::process::Child,
    port: u16,
    _environment: TestEnvironment,
    _root: tempfile::TempDir,
}

impl Drop for OfficialRuntime {
    fn drop(&mut self) {
        // Reap only this fixture's owned child, including failed assertions.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl OfficialRuntime {
    fn start() -> Result<Self> {
        // Same official managed-install fixture as cli/tests/support/mod.rs.
        // The override locates it; it is removed before exercising product code.
        let binary = PathBuf::from(std::env::var_os("RSRS_RUNTIME_BIN")
            .context("official managed Runtime fixture required")?).canonicalize()?;
        anyhow::ensure!(binary != std::env::current_exe()?.canonicalize()?,
            "test executable cannot substitute for the independent Runtime");
        let version_dir = binary.parent().context("fixture version directory missing")?;
        let source_root = version_dir.parent().and_then(std::path::Path::parent)
            .context("fixture installation root missing")?;
        let receipt = std::fs::read(source_root.join("runtime.json"))?;
        let selected: Value = serde_json::from_slice(&receipt)?;
        let version = crate::runtime_fetch::pinned()?.required.clone();
        anyhow::ensure!(selected["version"] == version
            && selected["release"]["metadata_json"].is_string()
            && source_root.join("runtime").join(&version) == version_dir,
            "fixture must be the official pinned managed installation");
        let root = tempfile::tempdir()?;
        let mut pending = vec![(version_dir.to_path_buf(), root.path().join("bin/runtime").join(&version))];
        while let Some((source, target)) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&source)?;
            anyhow::ensure!(!metadata.file_type().is_symlink(), "fixture contains a link");
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                anyhow::ensure!(metadata.file_attributes() & 0x400 == 0,
                    "fixture contains a reparse point");
            }
            if metadata.is_dir() {
                std::fs::create_dir_all(&target)?;
                for entry in std::fs::read_dir(source)? {
                    let entry = entry?;
                    pending.push((entry.path(), target.join(entry.file_name())));
                }
            } else {
                anyhow::ensure!(metadata.is_file(), "fixture contains a non-file");
                std::fs::copy(source, target)?;
            }
        }
        std::fs::write(root.path().join("bin/runtime.json"), receipt)?;
        std::fs::write(root.path().join("client.json"), b"{\"service_mode\":\"normal\"}")?;
        let keys = ["RSRS_DATA_DIR", "RSRS_BIN_DIR", "RSRS_RPC_PORT", "RSRS_CORE_TEST_MODE",
            "RSRS_NO_AUTOSYNC", "RSRS_RUNTIME_BIN", "ONEMEMORY_RUNTIME_BIN", "RESPIRE_RUNTIME_BIN",
            "RSRS_RPC_TOKEN", "RSRS_NO_AUTOSTART", "RSRS_CLIENT_ONLY"];
        let environment = TestEnvironment(keys.iter()
            .map(|key| (*key, std::env::var_os(key))).collect());
        for key in &keys { std::env::remove_var(key); }
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        std::env::set_var("RSRS_DATA_DIR", root.path());
        std::env::set_var("RSRS_BIN_DIR", root.path().join("bin"));
        std::env::set_var("RSRS_RPC_PORT", port.to_string());
        std::env::set_var("RSRS_NO_AUTOSYNC", "1");
        let executable = crate::runtime_fetch::managed_runtime_path()?
            .context("copied official Runtime fixture is not installed")?;
        let _takeover = crate::runtime_policy::takeover_lock()?;
        crate::autostart::authorize_detached_boot_locked()?;
        let arguments = vec!["serve".into(), "--data-dir".into(),
            root.path().as_os_str().to_owned(), "--port".into(), port.to_string().into()];
        let (child, instance_id) = crate::autostart::spawn_authorized_runtime(
            &executable, &arguments, None)?;
        let mut runtime = Self { child, port, _environment: environment, _root: root };
        let pid = runtime.child.id();
        wait_daemon_ready(&mut runtime.child, pid, &executable, &instance_id,
            respire_host_control::lifecycle_io::AccountRuntimeExpectation::Startup)?;
        let health = crate::net_rpc::health()?;
        anyhow::ensure!(!health.store_ready && health.credential_state == "required",
            "transport fixture must remain keyless and locked");
        Ok(runtime)
    }

    fn request(&self, method: &str, args: Vec<&str>) -> Result<Value> {
        let scope = crate::net_rpc::request_scope(&crate::net_rpc::health()?)?;
        let operation = uuid::Uuid::new_v4().to_string();
        Ok(json!({"v":PROTOCOL_V,"id":uuid::Uuid::new_v4().to_string(),
            "scope":scope,"operation_id":operation,"idempotency_key":operation,
            "method":method,"args":args}))
    }

    fn post(&self, request: &Value) -> Result<Value> {
        Ok(ureq::post(&format!("http://127.0.0.1:{}/api/rpc", self.port))
            .timeout(Duration::from_secs(15)).send_json(request.clone())?.into_json()?)
    }
}

#[test]
fn diary_inference_routing_honours_modes_and_literal_query_arguments() -> Result<()> {
    use clap::Parser;
    for (args, inference) in [
        (vec!["diary-search", "q"], true),
        (vec!["diary-search", "q", "--mode", "lexical"], false),
        (vec!["diary-search", "--mode=lexical", "q"], false),
        (
            vec!["diary-search", "--mode", "hybrid", "--", "--mode=lexical"],
            true,
        ),
        (vec!["diary-search", "--", "--mode=lexical"], true),
        (
            vec!["diary-search", "--mode", "lexical", "--", "--mode=hybrid"],
            false,
        ),
        (
            vec!["diary-fragment", "--neighbors", "1", "--", "s", "f"],
            false,
        ),
    ] {
        let arguments: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(needs_inference(&arguments), inference, "{arguments:?}");
        let parsed = crate::Cli::try_parse_from(std::iter::once("rsrs").chain(args))?;
        if let Some(crate::Command::DiarySearch { query, mode, .. }) = parsed.command {
            if query == "--mode=lexical" {
                assert_eq!(mode, "hybrid");
            }
            if query == "--mode=hybrid" {
                assert_eq!(mode, "lexical");
            }
        }
    }
    Ok(())
}

#[test]
fn shutdown_requires_a_matching_final_receipt() -> Result<()> {
    let root = std::path::Path::new(env!("RSRS_CONTRACT_FIXTURES"))
        .join("fixtures");
    let fixture: Value =
        serde_json::from_slice(&std::fs::read(root.join("positive/health-identity.json"))?)?;
    let mut identity = fixture["payload"].clone();
    identity["server"] = json!("respire");
    identity["bin"] = json!("rsrs-runtime");
    identity["v"] = json!(1);
    let health: crate::net_rpc::Health = serde_json::from_value(identity)?;
    let fixture: Value = serde_json::from_slice(&std::fs::read(
        root.join("positive/stop-final-drained.json"),
    )?)?;
    let receipt = fixture["payload"].clone();
    let operation = receipt["operation_id"]
        .as_str()
        .context("fixture stop operation")?;
    crate::net_rpc::validate_final_stop_receipt(&health, Some(operation), &receipt)?;
    for (key, invalid) in [
        ("schema_version", json!(2)),
        ("protocol_version", json!(2)),
        ("operation_id", json!("other-operation")),
        ("installation_id", json!("other-installation")),
        ("instance_id", json!("other-instance")),
        ("pid", json!(999)),
        ("process_start_id", json!("reused-pid")),
        ("image_path", json!("other-runtime")),
        ("image_sha256", json!("other-digest")),
        ("runtime_version", json!("other-version")),
        ("account_id", json!("other-account")),
        ("generation", json!(8)),
        ("state", json!("forced_timeout")),
        ("unfinished", Value::Null),
    ] {
        let mut invalid_receipt = receipt.clone();
        invalid_receipt[key] = invalid;
        assert!(
            crate::net_rpc::validate_final_stop_receipt(
                &health,
                Some(operation),
                &invalid_receipt
            )
            .is_err(),
            "accepted mismatched {key}"
        );
    }
    let incomplete: Value = serde_json::from_slice(&std::fs::read(
        root.join("negative/stop-final-unfinished.json"),
    )?)?;
    assert!(crate::net_rpc::validate_final_stop_receipt(
        &health,
        Some(operation),
        &incomplete["payload"]
    )
    .is_err());
    Ok(())
}

#[test]
fn routing_value_flags_match_the_actual_cli_contract() {
    use clap::CommandFactory;
    let mut command = crate::Cli::command();
    command.build();
    let actual: std::collections::BTreeSet<_> = command
        .get_arguments()
        .filter(|arg| arg.is_global_set() && arg.get_action().takes_values())
        .filter_map(|arg| arg.get_long().map(|name| format!("--{name}")))
        .collect();
    let expected = GLOBAL_VALUE_FLAGS
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect();
    assert_eq!(
        actual, expected,
        "new valued globals must update the shared argv classifier"
    );
}

#[test]
fn command_names_ignore_flags() {
    assert_eq!(command_name(&[]), None);
    assert_eq!(
        command_name(&["--json".into(), "status".into()]).as_deref(),
        Some("status")
    );
    assert!(is_status(&["--json".into(), "status".into()]));
    assert!(!is_status(&["remember".into()]));
    let tracked = [
        "--json",
        "--progress-id",
        "request-one",
        "--model-task-id",
        "task-one",
        "remember",
        "body",
    ]
    .map(String::from);
    assert_eq!(command_name(&tracked), Some("remember"));
    assert!(needs_inference(&tracked));
    assert!(!needs_inference(&["status".into()]));
}

#[test]
fn compatible_requires_version_and_bin() -> Result<()> {
    use clap::Parser;
    let mut response = test_response("s");
    assert!(compatible(&response));
    response.v = 99;
    assert!(!compatible(&response));
    response.v = PROTOCOL_V;
    response.bin = "other".into();
    assert!(compatible(&response));
    response.code = Some("protocol_version_mismatch".into());
    assert!(!compatible(&response));

    let command = crate::Cli::try_parse_from(["rsrs", "remember", "private content"])?
        .command.context("remember command missing")?;
    let veto = if cfg!(windows) {
        "echo {\"verdict\":\"block\",\"reason\":\"private data\"}"
    } else {
        "echo '{\"verdict\":\"block\",\"reason\":\"private data\"}'"
    };
    let config = serde_json::from_value(json!({"hooks": {"pre-remember": [
        {"cmd": veto, "on_error": "skip"}
    ]}}))?;
    let blocked = run_plugin_hooks(&command, &config, || panic!("vetoed write reached Runtime"));
    assert!(blocked.err().context("pre hook did not veto")?.to_string().contains("private data"));
    let config = serde_json::from_value(json!({"hooks": {"pre-remember": [
        {"cmd": "exit 1", "on_error": "block"}
    ]}}))?;
    assert!(run_plugin_hooks(&command, &config, || panic!("failed pre hook reached Runtime")).is_err());

    let config = serde_json::from_value(json!({"hooks": {"post-remember": [
        {"cmd": veto}, {"cmd": "exit 1", "on_error": "warn"}
    ]}}))?;
    for failed_after_commit in [false, true] {
        let mut response = test_response("committed");
        response.ok = !failed_after_commit;
        response.exit = if failed_after_commit { 1 } else { 0 };
        response.envelope = Some(ResultEnvelope::new("remember",
            if failed_after_commit { OutputStatus::Fail } else { OutputStatus::Ok },
            json!({"host_plugin_events": [{"event": "post-remember", "data": {"id": "committed"}}]}), Vec::new()));
        let result = run_plugin_hooks(&command, &config, || Ok(response))?;
        assert_eq!(result.ok, !failed_after_commit);
        assert_eq!(result.exit, if failed_after_commit { 1 } else { 2 });
        let envelope = result.envelope.context("missing command result")?;
        assert_eq!(envelope.errors.len(), 2, "a post veto must not suppress later hooks");
        assert_eq!(envelope.summary["host_plugin_events"][0]["data"]["id"], "committed");
    }
    assert!(run_plugin_hooks(&command, &config, || Ok(test_response("missing-receipts"))).is_err());
    Ok(())
}

#[test]
fn scoped_operation_replays_and_retains_its_result_over_real_http() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK.lock()
        .map_err(|error| anyhow::anyhow!("test environment lock poisoned: {error}"))?;
    let mut runtime = OfficialRuntime::start()?;
    let request = runtime.request("core_config.read", vec![r#"{"schema_version":1}"#])?;
    let first = runtime.post(&request)?;
    anyhow::ensure!(first["ok"] == true, "official Runtime core configuration read failed: {first}");
    let mut replay = request.clone();
    replay["id"] = json!(uuid::Uuid::new_v4().to_string());
    let repeated = runtime.post(&replay)?;
    assert_eq!(repeated["id"], replay["id"]);
    for field in ["operation_id", "idempotency_key", "scope", "ok", "exit", "envelope"] {
        assert_eq!(repeated[field], first[field], "replay changed {field}");
    }
    let operation: crate::runtime_error::OperationContext = serde_json::from_value(json!({
        "scope":request["scope"], "operation_id":request["operation_id"],
        "idempotency_key":request["idempotency_key"],
    }))?;
    let result = crate::net_rpc::rpc_result(&operation)?;
    assert_eq!(result["envelope"]["summary"]["state"], "completed");
    assert_eq!(result["envelope"]["summary"]["response"]["envelope"], first["envelope"]);
    replay["id"] = json!(uuid::Uuid::new_v4().to_string());
    replay["args"] = json!([r#"{ "schema_version": 1 }"#]);
    assert_eq!(runtime.post(&replay)?["code"], "idempotency_conflict");
    replay["id"] = json!(uuid::Uuid::new_v4().to_string());
    replay["method"] = json!("cli.exec");
    replay["args"] = json!(["--json", "status"]);
    assert_eq!(runtime.post(&replay)?["code"], "idempotency_conflict");
    // Final drain must retain the original operation binding and prove release.
    let _takeover = crate::runtime_policy::takeover_lock()?;
    let health = crate::net_rpc::health()?;
    std::thread::scope(|scope| -> Result<()> {
        let draining = scope.spawn(|| drain_occupant(&health, Duration::from_secs(30)));
        while !draining.is_finished() {
            // Reap our child so Unix liveness checks do not wait on a zombie.
            runtime.child.try_wait()?;
            std::thread::sleep(Duration::from_millis(20));
        }
        draining.join().map_err(|_| anyhow::anyhow!("fixture drain panicked"))?
    })?;
    assert!(!endpoint_path().exists());
    assert!(!crate::net_rpc::port_is_open());
    Ok(())
}

#[test]
fn http_pipeline_and_nonreading_peer_keep_runtime_available() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK.lock()
        .map_err(|error| anyhow::anyhow!("test environment lock poisoned: {error}"))?;
    let runtime = OfficialRuntime::start()?;
    let first = runtime.request("core_config.read", vec![r#"{"schema_version":1}"#])?;
    let second = runtime.request("core_config.read", vec![r#"{"schema_version":1}"#])?;
    let mut pipeline = std::net::TcpStream::connect(("127.0.0.1", runtime.port))?;
    pipeline.set_read_timeout(Some(Duration::from_secs(15)))?;
    for (body, close) in [(&first, false), (&second, true)] {
        let body = body.to_string();
        let connection = if close { "Connection: close\r\n" } else { "" };
        write!(pipeline, "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{connection}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", runtime.port, body.len())?;
    }
    // Do not read either reply until the separate control endpoint answers.
    assert_eq!(crate::net_rpc::health()?.pid, runtime.child.id());
    let mut wire = String::new();
    pipeline.read_to_string(&mut wire)?;
    let first_id = first["id"].as_str().context("first request ID missing")?;
    let second_id = second["id"].as_str().context("second request ID missing")?;
    let first_at = wire.find(first_id).context("first pipeline reply missing")?;
    let second_at = wire.find(second_id).context("second pipeline reply missing")?;
    assert!(first_at < second_at, "HTTP pipeline replies reordered");
    assert_eq!(wire.matches("HTTP/1.1 200").count(), 2, "pipeline response failed: {wire}");
    let response = call_method("cli.exec", vec!["--json".into(), "status".into()], false)?;
    assert!(!response.ok && response.exit == 2);
    let envelope = response.envelope.as_ref().context("locked status envelope missing")?;
    assert_eq!(envelope.summary["credential_state"], "required");
    assert_eq!(envelope.summary["store_ready"], false);
    assert!(response.scope.is_some() && response.operation_id.is_some()
        && response.idempotency_key == response.operation_id);
    Ok(())
}

#[test]
fn endpoint_path_follows_data_dir() -> Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let previous = respire::env::var("RSRS_DATA_DIR").ok();
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", first.path());
    let first_path = endpoint_path();
    std::env::set_var("RSRS_DATA_DIR", second.path());
    let second_path = endpoint_path();
    match previous {
        Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    if first_path == second_path {
        anyhow::bail!(
            "endpoint path stayed at {} after the data dir changed",
            first_path.display()
        );
    }
    if !first_path.starts_with(first.path()) || !second_path.starts_with(second.path()) {
        anyhow::bail!(
            "endpoint path left its data dir: {} / {}",
            first_path.display(),
            second_path.display()
        );
    }
    Ok(())
}

#[test]
fn offline_entry_and_endpoint_roundtrip() -> Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let previous = respire::env::var("RSRS_DATA_DIR").ok();
    let previous_port = respire::env::var("RSRS_RPC_PORT").ok();
    let dir = tempfile::tempdir()?;
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_RPC_PORT", "18761");
    assert!(!runtime_is_up());
    assert!(call_method("runtime.status", Vec::new(), false).is_err());
    let down = RuntimeFlags {
        port: None,
        host: "127.0.0.1".into(),
        status: true,
        stop: false,
    };
    runtime_entry(down.clone())?;
    assert_eq!(crate::exit_code(), 2);
    runtime_entry(RuntimeFlags {
        status: false,
        stop: true,
        ..down
    })?;
    std::fs::create_dir_all(endpoint_path().parent().context("endpoint parent missing")?)?;
    std::fs::write(endpoint_path(), serde_json::to_vec(&json!({"v":PROTOCOL_V,
        "pid":std::process::id(),"url":"http://127.0.0.1:9"}))?)?;
    assert_eq!(endpoint_pid(), Some(std::process::id()));
    let bare = test_response("render-none");
    assert!(render_response(bare, true).is_err());
    let mut with_body = test_response("render-body");
    with_body.envelope = Some(crate::output::ResultEnvelope::new(
        "status",
        OutputStatus::Ok,
        json!({"ok": true}),
        Vec::new(),
    ));
    render_response(with_body, true)?;
    let mut failure = test_response("render-err");
    failure.ok = false;
    failure.exit = 2;
    failure.code = Some("timeout".into());
    failure.error = Some("slow".into());
    assert!(render_response(failure, false).is_err());
    // Live startup, ownership and shutdown are exercised in a separate
    // process by net_rpc_http::runtime_uses_only_tcp_and_preserves_owner.
    match previous {
        Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    match previous_port {
        Some(value) => std::env::set_var("RSRS_RPC_PORT", value),
        None => std::env::remove_var("RSRS_RPC_PORT"),
    }
    Ok(())
}

#[test]
fn progress_queries_keep_stale_without_exposing_another_task() -> Result<()> {
    let stale = serde_json::json!({"active":true,"stale":true});
    assert_eq!(model_progress_for_task(stale.clone(), "original", true)?, stale);
    let foreign = serde_json::json!({"active":true,"id":"other","phase":"private-progress"});
    assert_eq!(model_progress_for_task(foreign.clone(), "original", false)?, stale);
    assert!(model_progress_for_task(foreign, "original", true).is_err());
    let own = serde_json::json!({"active":true,"id":"original","cancelled":true});
    assert_eq!(model_progress_for_task(own.clone(), "original", true)?, own);
    assert!(model_progress_for_task(serde_json::Value::Null, "original", false).is_err());
    Ok(())
}

#[test]
fn legacy_login_preflight_preserves_operation_owned_recovery() -> Result<()> {
    use respire_protocol::host_control::HostLoginRecoveryState;
    for caller in [LoginRecoveryCaller::OrdinaryRequest, LoginRecoveryCaller::AccountControl] {
        assert!(!needs_legacy_login_recovery(HostLoginRecoveryState::Clear, caller)?);
        assert!(needs_legacy_login_recovery(HostLoginRecoveryState::LegacyPending, caller)?);
    }
    assert!(!needs_legacy_login_recovery(
        HostLoginRecoveryState::OperationPending, LoginRecoveryCaller::AccountControl)?);
    assert!(needs_legacy_login_recovery(
        HostLoginRecoveryState::OperationPending, LoginRecoveryCaller::OrdinaryRequest).is_err());
    Ok(())
}
