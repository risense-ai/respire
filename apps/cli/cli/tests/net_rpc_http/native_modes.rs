use super::*;

#[test]
fn runtime_uses_only_tcp_and_preserves_owner() -> Result<(), String> {
    let mut rt = start_runtime("normal", true, false)?;
    let health = || -> Result<serde_json::Value, String> {
        ureq::get(&format!("http://127.0.0.1:{}/api/health", rt.port))
            .timeout(Duration::from_secs(3))
            .call()
            .map_err(|err| err.to_string())?
            .into_json()
            .map_err(|err| err.to_string())
    };
    let owner = health()?["pid"].as_u64().ok_or("runtime pid missing")?;
    assert_eq!(owner, u64::from(rt.child.id()));
    assert_eq!(
        fs::read(rt.dir.path().join("runtime/rpc.sock/owned-marker"))
            .map_err(|err| err.to_string())?,
        b"preserve"
    );
    // Starting another host for the same profile must retain the TCP owner.
    let mut duplicate = Command::new(runtime_bin()?)
        .args([
            "serve",
            "--port",
            &rt.port.to_string(),
        ])
        .env("RSRS_DATA_DIR", rt.dir.path())
        .env("RSRS_RPC_PORT", rt.port.to_string())
        .env("RSRS_CORE_TEST_MODE", "1")
        .env("RSRS_NO_AUTOSYNC", "1")
        .env_remove("RSRS_CLIENT_ONLY")
        .env_remove("RSRS_NO_AUTOSTART")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = duplicate.try_wait().map_err(|err| err.to_string())? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = duplicate.kill();
            let _ = duplicate.wait();
            return Err("duplicate runtime did not return to its existing owner".into());
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert!(status.success());
    assert_eq!(health()?["pid"].as_u64(), Some(owner));
    // A different port must not bypass installation ownership. In particular,
    // do not publish a healthy second host whose dispatcher is still locked.
    let endpoint =
        fs::read(rt.dir.path().join("runtime/endpoint.json")).map_err(|err| err.to_string())?;
    let original_config =
        fs::read(rt.dir.path().join("client.json")).map_err(|err| err.to_string())?;
    for alternate_profile in [false, true] {
        if alternate_profile {
            // The endpoint belongs to the installation root, while its library
            // lock belongs to the selected account. Both ownership scopes matter.
            let alternate = rt.dir.path().join("accounts/other");
            fs::create_dir_all(&alternate).map_err(|err| err.to_string())?;
            let mut config: serde_json::Value =
                serde_json::from_slice(&original_config).map_err(|err| err.to_string())?;
            config["data_dir"] = serde_json::json!(alternate);
            fs::write(rt.dir.path().join("client.json"), config.to_string())
                .map_err(|err| err.to_string())?;
        }
        let other_port = pick_port()?;
        let other_stderr_path = rt.dir.path().join(format!(
            "alternate-runtime-{alternate_profile}.stderr.log"
        ));
        let other_stderr = fs::File::create(&other_stderr_path).map_err(|err| err.to_string())?;
        let read_other_stderr = || match fs::read_to_string(&other_stderr_path) {
            Ok(stderr) => stderr,
            Err(err) => format!("failed to read private stderr: {err}"),
        };
        let mut other = Command::new(runtime_bin()?)
            .args([
                "serve",
                "--port",
                &other_port.to_string(),
            ])
            .env("RSRS_DATA_DIR", rt.dir.path())
            .env("RSRS_RPC_PORT", other_port.to_string())
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_NO_AUTOSYNC", "1")
            .env_remove("RSRS_CLIENT_ONLY")
            .env_remove("RSRS_NO_AUTOSTART")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(other_stderr))
            .spawn()
            .map_err(|err| err.to_string())?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = other.try_wait().map_err(|err| err.to_string())? {
                break status;
            }
            // Windows connection refusal can block for seconds and consume the startup deadline.
            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], other_port)),
                Duration::from_millis(50),
            )
            .is_ok()
            {
                let kill = other.kill();
                let wait = other.wait();
                return Err(format!(
                    "another port became listening: alternate_profile={alternate_profile}; kill={kill:?}; wait={wait:?}; stderr={}",
                    read_other_stderr()
                ));
            }
            // Allow both 5s probes and the 15s installation-lock wait, plus margin.
            if started.elapsed() > Duration::from_secs(40) {
                if let Some(status) = other.try_wait().map_err(|err| err.to_string())? {
                    break status;
                }
                let kill = other.kill();
                let wait = other.wait();
                return Err(format!(
                    "alternate runtime startup rejection timed out after 40s: alternate_profile={alternate_profile}; kill={kill:?}; wait={wait:?}; stderr={}",
                    read_other_stderr()
                ));
            }
            thread::sleep(Duration::from_millis(25));
        };
        assert!(
            !status.success(),
            "alternate runtime exited successfully instead of rejecting startup: alternate_profile={alternate_profile}; status={status}; stderr={}",
            read_other_stderr()
        );
        assert_eq!(
            fs::read(rt.dir.path().join("runtime/endpoint.json")).map_err(|err| err.to_string())?,
            endpoint
        );
        fs::write(rt.dir.path().join("client.json"), &original_config)
            .map_err(|err| err.to_string())?;
        assert_eq!(health()?["pid"].as_u64(), Some(owner));
    }
    rt.check_health(&health()?);
    let response = rt.rpc("tcp-only-status", &["status", "--json"], Duration::from_secs(10))?;
    assert_eq!(response["ok"], true, "{response}");
    let stop = Command::new(bin())
        .args(["--runtime-internal", "--stop", "--json"])
        .env("RSRS_DATA_DIR", rt.dir.path())
        .env("RSRS_RPC_PORT", rt.port.to_string())
        .env_remove("RSRS_CLIENT_ONLY")
        .env_remove("RSRS_NO_AUTOSTART")
        .output()
        .map_err(|err| err.to_string())?;
    assert!(
        stop.status.success(),
        "{} {}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    let started = Instant::now();
    while rt
        .child
        .try_wait()
        .map_err(|err| err.to_string())?
        .is_none()
    {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("TCP runtime did not stop".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(std::net::TcpStream::connect(("127.0.0.1", rt.port)).is_err());
    assert_eq!(
        fs::read(rt.dir.path().join("runtime/rpc.sock/owned-marker"))
            .map_err(|err| err.to_string())?,
        b"preserve"
    );
    Ok(())
}

#[test]
fn device_modes_gate_rpc_and_disabled_startup_never_opens_memory() -> Result<(), String> {
    let mut rt = start_runtime("off", false, true)?;
    eprintln!("device-stage=off-rpc-probes");
    let execute = |rt: &Runtime, args: &[&str]| {
        rt.rpc(&uuid::Uuid::new_v4().to_string(), args, Duration::from_secs(20))
    };
    let host = |rt: &Runtime, args: &[&str]| -> Result<(bool, serde_json::Value), String> {
        let output = rt.host_command(args)?;
        let envelope = serde_json::from_slice(&output.stdout).map_err(|err|
            format!("host command {args:?} returned no envelope: {err}; {}", String::from_utf8_lossy(&output.stderr)))?;
        Ok((output.status.success(), envelope))
    };
    let set_mode = |rt: &mut Runtime, mode: &str| -> Result<(), String> {
        let intent = format!("workspace_mode={mode}");
        let (ok, result) = host(rt, &["--json", "agent-config", "--set", &intent])?;
        assert!(ok, "{result}");
        assert_eq!(result["command"], "agent-config");
        assert_eq!(result["summary"]["action"], "updated");
        assert_eq!(result["summary"]["key"], "workspace_mode");
        assert_eq!(result["summary"]["value"], mode);
        eprintln!("device-stage=mode-owner-refresh");
        rt.refresh_controlled_identity()?;
        eprintln!("device-stage=mode-config-read");
        let (ok, config) = host(rt, &["--json", "agent-config"])?;
        assert!(ok, "{config}");
        assert_eq!(config["summary"]["workspace_mode"], mode, "{config}");
        Ok(())
    };
    for args in [
        vec!["--json", "recall", "synthetic query"],
        vec!["--json", "remember", "synthetic memory"],
        vec!["--json", "list"],
        vec!["--json", "sync"],
    ] {
        let response = execute(&rt, &args)?;
        assert_eq!(response["ok"], false, "{response}");
        assert_eq!(response["code"], "runtime_locked", "{response}");
        let (ok, result) = host(&rt, &args)?;
        assert!(ok, "{result}");
        assert_eq!(result["summary"]["mode"], "off");
        assert_eq!(result["summary"]["skipped"], true);
        assert_eq!(result["summary"]["count"], 0);
        assert_eq!(result["items"], serde_json::json!([]));
        assert_eq!(result["details"], serde_json::json!([]));
        assert_eq!(result["errors"], serde_json::json!([]));
    }
    // Doctor always checks the generic prompt, even when no IDE is installed.
    eprintln!("device-stage=off-host-inject");
    let (ok, injected) = host(&rt, &["--json", "inject", "--id", "generic"])?;
    assert!(ok, "{injected}");
    assert_eq!(injected["command"], "inject");
    assert!(rt.dir.path().join("host/home/AGENTS.md").is_file());
    // Repairs and migrations belong to the host, not the business RPC allowlist.
    eprintln!("device-stage=off-host-doctor");
    let doctor_output = rt.host_command(&["--json", "doctor", "--fix"])?;
    let doctor: serde_json::Value = serde_json::from_slice(&doctor_output.stdout).map_err(|err|
        format!("doctor returned no envelope: {err}"))?;
    // Off diagnostics are an explicit warning (exit 2), not a failed host check.
    assert_eq!(doctor_output.status.code(), Some(2), "off doctor must exit with warning status");
    assert_eq!(doctor["command"], "doctor");
    assert_eq!(doctor["status"], "warn");
    assert_eq!(doctor["errors"], serde_json::json!([]));
    assert_eq!(doctor["summary"]["mode"], "off");
    assert_eq!(doctor["summary"]["fail"], 0);
    assert_eq!(doctor["summary"]["warn"], 1);
    assert_eq!(doctor["summary"]["skipped"], true);
    assert!(doctor["items"].as_array().is_some_and(|items| items.iter()
        .any(|item| item["name"] == "Runtime diagnostics" && item["status"] == "warn"
            && item["value"] == "Memory is off; Runtime diagnostics were not executed")));
    assert!(doctor["items"].as_array().is_some_and(|items| items.iter()
        .any(|item| item["name"] == "inject" && item["status"] == "ok"
            && item["value"] == "1/1 injected and fresh (1 detected)")));
    assert!(doctor["items"].as_array().is_some_and(|items| items.iter()
        .any(|item| item["name"] == "host repair" && item["status"] == "skip")));
    let migration = ["--json", "migrate", "--source", "/missing-synthetic-library", "--account", "fixture"];
    eprintln!("device-stage=off-host-migration");
    let (ok, rejected) = host(&rt, &migration)?;
    assert!(!ok, "invalid legacy source was accepted: {rejected}");
    assert!(rejected["errors"].as_array().is_some_and(|errors| errors.iter()
        .any(|error| error.as_str().is_some_and(|message| message.contains("invalid migration source or account")))));
    eprintln!("device-stage=off-status-rpc");
    let status = execute(&rt, &["--json", "status"])?;
    assert_eq!(status["envelope"]["summary"]["workspace"], "off", "{status}");
    assert!(status["envelope"]["summary"]["local_total"].is_null());
    assert!(!rt.dir.path().join("rsrs.db").exists());
    assert!(!rt.dir.path().join("onememory.db").exists());
    let (ok, config) = host(&rt, &["--json", "agent-config"])?;
    assert!(ok, "{config}");
    assert_eq!(config["summary"]["memory_off"], true, "{config}");
    assert_eq!(config["summary"]["workspace_mode"], "off");
    let plain = rt.host_command(&["--client-only", "recall", "synthetic query"])?;
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    assert!(plain.stdout.is_empty(), "disabled human output must be empty");
    eprintln!("device-stage=enter-readonly");
    set_mode(&mut rt, "readonly")?;
    eprintln!("device-stage=enter-normal");
    set_mode(&mut rt, "normal")?;
    eprintln!("device-stage=normal-initial-status-rpc");
    let initialized = execute(&rt, &["--json", "status"])?;
    // Changing device mode does not supply keys; this fixture owns no account.
    assert_eq!(initialized["ok"], false, "{initialized}");
    assert_eq!(initialized["exit"], 2, "{initialized}");
    assert_eq!(initialized["envelope"]["summary"]["store_ready"], false);
    assert_eq!(initialized["envelope"]["summary"]["credential_state"], "required");
    assert!(!rt.dir.path().join("rsrs.db").exists());
    eprintln!("device-stage=enter-readonly");
    set_mode(&mut rt, "readonly")?;
    let (ok, config) = host(&rt, &["--json", "agent-config"])?;
    assert!(ok, "{config}");
    assert_eq!(config["summary"]["readonly"], true, "{config}");
    for args in [
        vec!["--json", "remember", "synthetic memory"],
        vec!["--json", "sync"],
        vec!["--json", "sync-reset"],
        vec!["--json", "doctor", "--fix"],
        vec!["--json", "sync-conflicts", "--refresh"],
        vec!["--json", "sync-history", "--remote"],
        migration.to_vec(),
    ] {
        if matches!(args.get(1), Some(&"doctor" | &"migrate")) {
            let (ok, result) = host(&rt, &args)?;
            assert!(!ok, "read-only host accepted {args:?}: {result}");
            assert_ne!(result["details"]["error_type"], "host_boundary");
            let expected = if args[1] == "doctor" { "read-only" } else { "invalid migration source or account" };
            assert!(result["errors"].as_array().is_some_and(|errors| errors.iter()
                .any(|error| error.as_str().is_some_and(|message| message.contains(expected)))),
                "host rejection was not the intended guard: {result}");
        } else {
            eprintln!("device-stage=readonly-reject-write-rpc");
            let response = execute(&rt, &args)?;
            assert_eq!(response["ok"], false, "locked Runtime accepted {args:?}: {response}");
            assert_eq!(response["code"], "runtime_locked", "{response}");
        }
    }
    eprintln!("device-stage=enter-normal");
    set_mode(&mut rt, "normal")?;
    eprintln!("device-stage=normal-final-status-rpc");
    let status = execute(&rt, &["--json", "status"])?;
    assert_eq!(status["ok"], false, "{status}");
    assert_eq!(status["exit"], 2, "{status}");
    assert_eq!(status["envelope"]["summary"]["store_ready"], false);
    assert_eq!(status["envelope"]["summary"]["credential_state"], "required");
    assert_ne!(status["envelope"]["summary"]["workspace"], "off");
    assert!(!rt.dir.path().join("rsrs.db").exists());
    let (ok, config) = host(&rt, &["--json", "agent-config"])?;
    assert!(ok, "{config}");
    assert_eq!(config["summary"]["readonly"], false, "{config}");
    assert_eq!(config["summary"]["memory_off"], false);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(
        &fs::read(rt.dir.path().join("client.json")).map_err(|err| err.to_string())?
    ).map_err(|err| err.to_string())?["service_mode"], "normal");
    let cleanup = rt.host_command(&["--runtime-internal", "--stop", "--json"])?;
    assert!(cleanup.status.success(), "{}", String::from_utf8_lossy(&cleanup.stderr));
    rt.verify_stop_receipt(None)?;
    assert!(std::net::TcpStream::connect(("127.0.0.1", rt.port)).is_err());
    rt.host_control = false;
    Ok(())
}
