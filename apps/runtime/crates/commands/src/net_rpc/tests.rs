use super::*;

#[test]
fn darwin_readiness_keeps_the_image_separate_from_arguments() -> Result<()> {
    let birth = "Sun Nov  8 01:02:03 2026";
    let image = "/private/var/folders/long path/rsrs-runtime";
    for arguments in ["--runtime-internal --runtime-background-internal --port 15169",
        "serve --data-dir /private/var/folders/profile with spaces"] {
        let identity = darwin_runtime_identity(&format!("{birth}  {image} {arguments}\n"), &format!("{image}\n"))?;
        assert_eq!(identity.executable, image);
        assert_eq!(identity.started_at, birth);
    }
    for (command, claimed) in [
        (format!("{image} --runtime-internal"), "/foreign/rsrs-runtime"),
        (format!("{image}-other --runtime-internal"), image),
        (format!("{image} --version"), image),
    ] {
        assert!(darwin_runtime_identity(&format!("{birth}  {command}"), claimed).is_err());
    }
    assert!(darwin_runtime_identity("short", image).is_err());
    assert!(darwin_runtime_identity(&format!("{birth}  {image} serve"), "rsrs-runtime").is_err());
    Ok(())
}


#[test]
fn foreground_identity_accepts_standalone_serving_modes_and_option_values() {
    for args in [
        vec!["/tmp/rsrs-runtime", "serve", "--port", "15169"],
        vec!["/tmp/path with spaces/rsrs-runtime", "--json", "serve"],
        vec!["/tmp/rsrs-runtime"],
        vec!["/tmp/rsrs-runtime", "--json", "runtime", "start"],
        vec!["/tmp/rsrs-runtime", "--port", "15169", "serve"],
        vec!["/tmp/rsrs-runtime", "serve", "--data-dir", "--status"],
        vec![
            "/tmp/rsrs-runtime",
            "serve",
            "--data-dir",
            "/tmp/profile",
            "--supervised",
        ],
        vec![
            "/tmp/rsrs-runtime",
            "serve",
            "--data-dir",
            "/tmp/profile",
            "--host",
            "127.0.0.1",
            "--port",
            "15169",
        ],
        vec!["/tmp/rsrs", "runtime", "run"],
        vec!["/tmp/rsrs", "--runtime-internal"],
        vec!["/tmp/rsrs", "web", "--internal"],
    ] {
        assert!(
            runtime_role_arguments(Path::new(args[0]), &args),
            "{args:?}"
        );
    }
}

#[test]
fn foreground_identity_rejects_conflicting_or_incomplete_standalone_arguments() {
    for args in [
        vec!["/tmp/rsrs", "serve"],
        vec!["/tmp/unrelated", "serve"],
        vec!["/tmp/rsrs-runtime", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--stop"],
        vec!["/tmp/rsrs-runtime", "serve", "--version"],
        vec!["/tmp/rsrs-runtime", "runtime", "start", "--stop"],
        vec!["/tmp/rsrs-runtime", "runtime", "status"],
        vec!["/tmp/rsrs-runtime", "runtime"],
        vec!["/tmp/rsrs-runtime", "serve", "--data-dir"],
        vec!["/tmp/rsrs-runtime", "serve", "--host"],
        vec!["/tmp/rsrs-runtime", "serve", "--host", ""],
        vec!["/tmp/rsrs-runtime", "serve", "--port"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "0"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "65536"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--direct"],
        vec!["/tmp/rsrs-runtime", "serve", "--core-worker-internal"],
        vec!["/tmp/rsrs-runtime", "serve", "--runtime-internal"],
        vec!["/tmp/rsrs-runtime", "--runtime-internal"],
        vec!["/tmp/rsrs-runtime", "web", "--internal"],
        vec!["/tmp/rsrs-runtime", "--status", "--data-dir", "serve"],
        vec!["/tmp/rsrs-runtime", "serve", "unknown"],
    ] {
        assert!(
            !runtime_role_arguments(Path::new(args[0]), &args),
            "{args:?}"
        );
    }
}

#[test]
fn background_identity_requires_the_exact_internal_launcher_shape() {
    for image in ["/tmp/rsrs-runtime", "/tmp/rsrs-runtime (deleted)"] {
        assert!(runtime_role_arguments(Path::new(image),
            &["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169"]));
    }
    for args in [
        vec!["rsrs-runtime", "--runtime-background-internal"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "0"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169", "--status"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--data-dir", "15169"],
    ] {
        assert!(!runtime_role_arguments(Path::new("/tmp/rsrs-runtime"), &args));
    }
    assert!(!runtime_role_arguments(Path::new("/tmp/foreign"),
        &["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169"]));
}

#[test]
fn runtime_role_uses_the_executable_not_argv_zero() {
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/foreign"),
        &["rsrs", "--runtime-internal"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime"),
        &["rsrs", "--runtime-internal"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime"),
        &["rsrs", "serve"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime (deleted)"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs (deleted)"),
        &["rsrs", "runtime", "run"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/foreign (deleted)"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime (deleted) suffix"),
        &["rsrs-runtime", "serve"]
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn procfs_arguments_preserve_empty_values_and_reject_malformed_input() -> Result<()> {
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0serve\0")?,
        ["rsrs-runtime", "serve"]
    );
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0--host\0\0")?,
        ["rsrs-runtime", "--host", ""]
    );
    assert!(!runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0--host\0\0")?
    ));
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0serve\0--data-dir\0\0")?,
        ["rsrs-runtime", "serve", "--data-dir", ""]
    );
    assert!(runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0serve\0--data-dir\0\0")?
    ));
    assert!(!runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0serve\0\0")?
    ));
    assert!(linux_process_arguments(b"").is_err());
    assert!(linux_process_arguments(b"rsrs-runtime\0serve").is_err());
    assert!(linux_process_arguments(b"rsrs-runtime\0\xff\0").is_err());
    Ok(())
}
fn valid_timeout_args(args: &[&str]) -> Vec<String> {
    use clap::Parser;
    crate::cli_def::Cli::try_parse_from(std::iter::once("rsrs").chain(args.iter().copied()))
        .expect("timeout regression must exercise valid CLI argv");
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn rpc_timeout_recognizes_commands_after_global_values_and_equals_forms() {
    for args in [
        vec!["sync"],
        vec!["--json", "--progress-id", "request-one", "sync"],
        vec!["--progress-id=import", "--model-task-id=task-one", "sync"],
        vec!["--model-task-id", "task-one", "import", "synthetic.json"],
        vec!["import", "synthetic.json"],
        vec!["reembed"],
        vec!["--json", "--progress-id", "model", "model", "activate", "m3"],
        vec!["model", "--model-task-id", "install", "activate", "m3"],
    ] {
        let parsed = valid_timeout_args(&args);
        assert_eq!(
            rpc_timeout("cli.exec", &parsed, None),
            Duration::from_secs(30 * 60),
            "{args:?}"
        );
    }
}

#[test]
fn rpc_timeout_does_not_treat_tracking_values_or_other_methods_as_commands() {
    for args in [
        vec!["--progress-id", "sync", "list"],
        vec!["--model-task-id", "import", "status"],
        vec!["model", "--progress-id", "install", "engine"],
        vec!["show", "synthetic-id"],
    ] {
        let parsed = valid_timeout_args(&args);
        assert_eq!(
            rpc_timeout("cli.exec", &parsed, None),
            HTTP_TIMEOUT,
            "{args:?}"
        );
    }
    let long = valid_timeout_args(&["--progress-id", "request-one", "sync"]);
    assert_eq!(
        rpc_timeout("cli.progress", &long, None),
        Duration::from_secs(1)
    );
    assert_eq!(
        rpc_timeout("model.control", &long, None),
        Duration::from_secs(3)
    );
    assert_eq!(
        rpc_timeout("runtime.result", &["sync".into()], None),
        HTTP_TIMEOUT
    );
    assert_eq!(
        rpc_timeout("model.control", &long, Some(Duration::from_millis(7))),
        Duration::from_millis(7)
    );
}

#[test]
fn response_after_client_deadline_reports_unknown_and_can_be_inspected_without_replay(
) -> Result<()> {
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = tempfile::tempdir()?;
    let server =
        tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .context("missing test address")?
        .port();
    let previous: Vec<_> = ["RSRS_RPC_PORT", "RSRS_DATA_DIR", "RSRS_RPC_TOKEN"]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
    std::env::set_var("RSRS_RPC_PORT", port.to_string());
    std::env::set_var("RSRS_DATA_DIR", directory.path());
    std::env::remove_var("RSRS_RPC_TOKEN");
    let test_root = directory.path().to_path_buf();
    let actor = std::thread::spawn(move || -> Result<String> {
        fn receive_rpc(
            server: &tiny_http::Server,
            root: &std::path::Path,
        ) -> Result<tiny_http::Request> {
            loop {
                let request = server
                    .recv_timeout(Duration::from_secs(2))?
                    .context("RPC never arrived")?;
                if request.url() == "/api/health" {
                    request.respond(tiny_http::Response::from_string(json!({
                        "server":"respire", "bin":"fixture", "pid":4321, "v":1,
                        "installation_root":root, "account_id":"account-1", "instance_id":"instance-1", "generation":7,
                    }).to_string()))?;
                } else {
                    return Ok(request);
                }
            }
        }
        let mut request = receive_rpc(&server, &test_root)?;
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body)?;
        let write: Value = serde_json::from_str(&body)?;
        anyhow::ensure!(write["method"] == "cli.exec", "unexpected first request");
        anyhow::ensure!(
            write["id"].is_string()
                && write["operation_id"].is_string()
                && write["id"] != write["operation_id"],
            "request and operation identities must be distinct"
        );
        let id = write["id"].as_str().context("missing write ID")?.to_owned();
        let operation_id = write["operation_id"]
            .as_str()
            .context("missing operation ID")?
            .to_owned();
        std::thread::sleep(Duration::from_millis(150));
        let _ = request.respond(tiny_http::Response::from_string(
            json!({"v":1,"id":id,"ok":true}).to_string(),
        ));
        let mut inspect = receive_rpc(&server, &test_root)?;
        body.clear();
        inspect.as_reader().read_to_string(&mut body)?;
        let query: Value = serde_json::from_str(&body)?;
        anyhow::ensure!(
            write["scope"]
                == json!({"account_id":"account-1","instance_id":"instance-1","generation":7})
                && query["scope"] == write["scope"],
            "request scope changed or was omitted"
        );
        anyhow::ensure!(
            query["method"] == "runtime.result" && query["args"][0] == operation_id,
            "client replayed or inspected the wrong write"
        );
        inspect.respond(tiny_http::Response::from_string(json!({"v":1,"id":query["id"],"ok":true,"envelope":{"summary":{"state":"completed","request_id":query["id"],"operation_id":operation_id,"response":{"ok":true,"committed":true,"id":id}}}}).to_string()))?;
        Ok(operation_id)
    });
    let result = (|| -> Result<()> {
        let error = rpc_method_with_timeout(
            "cli.exec",
            vec!["remember".into(), "synthetic write".into()],
            Some(Duration::from_millis(50)),
        )
        .err()
        .context("delayed write unexpectedly completed within deadline")?;
        let id = match error.downcast_ref::<RuntimeError>() {
            Some(RuntimeError::OutcomeUnknown { operation_id, .. }) => operation_id.clone(),
            _ => anyhow::bail!(
                "ambiguous transport failure was not marked outcome_unknown: {error}"
            ),
        };
        let inspected = rpc_method_with_timeout(
            "runtime.result",
            vec![id.clone()],
            Some(Duration::from_secs(2)),
        )?;
        anyhow::ensure!(
            inspected["envelope"]["summary"]["state"] == "completed",
            "inspection failed: {inspected}"
        );
        let seen = actor
            .join()
            .map_err(|_| anyhow::anyhow!("test transport actor panicked"))??;
        anyhow::ensure!(seen == id, "operation ID changed");
        Ok(())
    })();
    for (key, value) in previous {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    result
}

#[cfg(target_os = "linux")]
#[test]
fn procfs_liveness_keeps_pid_and_state_checks_strict() -> Result<()> {
    assert!(linux_stat_is_running("123 (rsrs) S 1 2 3", 123)?);
    assert!(linux_stat_is_running(
        "123 (name with ) parentheses) R 1 2 3",
        123
    )?);
    assert!(!linux_stat_is_running("123 (rsrs) Z 1 2 3", 123)?);
    for invalid in [
        "124 (rsrs) S 1",
        "123 rsrs S",
        "123 (rsrs)",
        "123 (rsrs) ? 1",
    ] {
        assert!(linux_stat_is_running(invalid, 123).is_err());
    }
    assert!(process_is_running(std::process::id())?);
    assert!(process_is_running(0).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn exited_child_is_not_a_running_library_holder() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()?;
    let exited = wait_until_exited(child.id());
    child.wait()?;
    exited
}

#[test]
fn port_is_open_sees_local_listener() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    if !port_is_open_on(port) {
        return Err(format!("listener on {port} should be open"));
    }
    #[cfg(windows)]
    {
        let localized = b"\r\n\xbb\xee\xb6\xaf\xc1\xac\xbd\xd3\r\n  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING  1234\r\n  TCP  [::1]:43211  [::]:0  TIME_WAIT  0\r\n";
        if windows_listener_pid_from_netstat(localized, 43210).map_err(|err| err.to_string())? != Some(1234) {
            return Err("localized headings hid a valid TCP listener".into());
        }
        if windows_listener_pid_from_netstat(localized, 43211).map_err(|err| err.to_string())?.is_some() {
            return Err("TIME_WAIT was treated as a listener".into());
        }
        for invalid in [
            &b"  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING  invalid\r\n"[..],
            &b"  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING  \xff\r\n"[..],
            &b"  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING\r\n"[..],
            &b"  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING  0\r\n"[..],
            &b"  TCP  127.0.0.1:43210  0.0.0.0:0  LISTENING  1234\r\n  TCP  127.0.0.1:43211  0.0.0.0:0  LISTENING  invalid\r\n"[..],
        ] {
            match windows_listener_pid_from_netstat(invalid, 43210) {
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {},
                other => return Err(format!("invalid TCP row was not rejected: {other:?}")),
            }
        }
        let owner = windows_listener_pid_on(port).map_err(|err| err.to_string())?;
        if owner != Some(std::process::id()) {
            return Err(format!("listener on {port} has unexpected owner: {owner:?}"));
        }
        drop(listener);
        let owner = windows_listener_pid_on(port).map_err(|err| err.to_string())?;
        if owner.is_some() {
            return Err(format!("closed listener on {port} still has an owner: {owner:?}"));
        }
    }
    Ok(())
}

#[test]
fn default_port_is_15169() -> Result<(), String> {
    if DEFAULT_RPC_PORT != 15169 {
        return Err(format!("port {}", DEFAULT_RPC_PORT));
    }
    Ok(())
}

#[test]
fn rpc_port_reads_env() -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = respire_app::env::var("RSRS_RPC_PORT").ok();
    std::env::set_var("RSRS_RPC_PORT", "26111");
    let got = rpc_port();
    match prev {
        Some(v) => std::env::set_var("RSRS_RPC_PORT", v),
        None => std::env::remove_var("RSRS_RPC_PORT"),
    }
    if got != 26111 {
        return Err(format!("got {got}"));
    }
    Ok(())
}

#[test]
fn origin_empty_is_cli() -> Result<(), String> {
    if !origin_ok("", "http://127.0.0.1:15169") {
        return Err("empty origin should pass".into());
    }
    if origin_ok("https://evil.example", "http://127.0.0.1:15169") {
        return Err("evil origin should fail".into());
    }
    if !origin_ok("http://127.0.0.1:15169", "http://127.0.0.1:15169") {
        return Err("same origin should pass".into());
    }
    if !origin_ok("http://localhost:29123", "http://127.0.0.1:29123") {
        return Err("localhost must use the actual bound port".into());
    }
    if origin_ok("http://localhost:15169", "http://127.0.0.1:29123") {
        return Err("default-port origin must not match a different listener".into());
    }
    if !origin_ok("http://127.0.0.1", "http://127.0.0.1:80")
        || !origin_ok("http://localhost", "http://127.0.0.1:80")
        || !origin_ok("http://[::1]", "http://[::1]:80")
    {
        return Err("HTTP default-port origins must use their canonical form".into());
    }
    if origin_ok("http://localhost", "http://127.0.0.1:29123")
        || origin_ok("http://evil.example", "http://127.0.0.1:80")
    {
        return Err("canonical origins must not exempt other ports or hosts".into());
    }
    Ok(())
}

#[test]
fn health_shape() -> Result<(), String> {
    let ours = json!({"server":"respire","bin":"1.0.6-dev.2","pid":1,"v":1,"exe":""});
    if !is_our_health(&ours) {
        return Err("ours".into());
    }
    if is_our_health(&json!({"server":"nginx"})) {
        return Err("nginx".into());
    }
    Ok(())
}

#[test]
fn version_newer_dev() -> Result<(), String> {
    if version_cmp("1.0.6-dev.2", "1.0.6-dev.1") != std::cmp::Ordering::Greater {
        return Err("dev.2 should be newer".into());
    }
    if version_cmp("1.0.6-dev.1", "1.0.6-dev.1") != std::cmp::Ordering::Equal {
        return Err("equal".into());
    }
    Ok(())
}

#[test]
fn token_env_beats_file() -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let prev_data = respire_app::env::var("RSRS_DATA_DIR").ok();
    let prev_tok = respire_app::env::var("RSRS_RPC_TOKEN").ok();
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_RPC_TOKEN", "from-env");
    let got = load_token();
    match prev_data {
        Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    match prev_tok {
        Some(v) => std::env::set_var("RSRS_RPC_TOKEN", v),
        None => std::env::remove_var("RSRS_RPC_TOKEN"),
    }
    if got.as_deref() != Some("from-env") {
        return Err(format!("{got:?}"));
    }
    Ok(())
}

#[test]
fn persist_and_load_token_file() -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let prev_data = respire_app::env::var("RSRS_DATA_DIR").ok();
    let prev_tok = respire_app::env::var("RSRS_RPC_TOKEN").ok();
    std::env::remove_var("RSRS_RPC_TOKEN");
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    persist_token("file-token").map_err(|e| e.to_string())?;
    let got = load_token();
    match prev_data {
        Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    match prev_tok {
        Some(v) => std::env::set_var("RSRS_RPC_TOKEN", v),
        None => std::env::remove_var("RSRS_RPC_TOKEN"),
    }
    if got.as_deref() != Some("file-token") {
        return Err(format!("{got:?}"));
    }
    Ok(())
}
