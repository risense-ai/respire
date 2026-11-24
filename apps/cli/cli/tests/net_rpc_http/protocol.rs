use super::*;

#[test]
fn health_without_token_reports_bin() -> Result<(), String> {
    let rt = start_internal_runtime()?;
    let url = format!("http://127.0.0.1:{}/api/health", rt.port);
    if rt.dir.path().join("runtime").join("token").exists() {
        return Err("loopback runtime created a token file".into());
    }
    let ok = ureq::get(&url).call().map_err(|err| err.to_string())?;
    let body: serde_json::Value = ok.into_json().map_err(|err| err.to_string())?;
    if body["server"] != "respire" {
        return Err(format!("server {body}"));
    }
    rt.check_health(&body);
    if body["pid"].as_u64().unwrap_or(0) == 0 {
        return Err(format!("pid {body}"));
    }
    for path in ["/api/health", "/mcp", "/sse"] {
        for (header, value, error) in [
            ("Host", "example.invalid", "forbidden host"),
            ("Origin", "https://example.invalid", "forbidden origin"),
        ] {
            match ureq::get(&format!("http://127.0.0.1:{}{path}", rt.port))
                .set(header, value)
                .call()
            {
                Err(ureq::Error::Status(403, response)) => {
                    let denied: serde_json::Value =
                        response.into_json().map_err(|err| err.to_string())?;
                    if denied["error"] != error {
                        return Err(format!("unexpected {path} {header} rejection: {denied}"));
                    }
                }
                other => return Err(format!("expected 403 for {path} {header}, got {other:?}")),
            }
        }
    }
    let local = ureq::get(&url)
        .set("Host", &format!("localhost:{}", rt.port))
        .set("Authorization", "Bearer invalid-local-token")
        .call()
        .map_err(|err| err.to_string())?;
    if local.status() != 200 {
        return Err("loopback alias or ignored local token was rejected".into());
    }
    let endpoint = ureq::get(&format!("http://127.0.0.1:{}/mcp", rt.port))
        .set("Host", &format!("localhost:{}", rt.port))
        .call()
        .map_err(|err| err.to_string())?
        .into_string()
        .map_err(|err| err.to_string())?;
    if !endpoint.contains(&format!("http://127.0.0.1:{}/mcp", rt.port)) {
        return Err("MCP endpoint did not use the actual bound address".into());
    }
    let _ = rt.stderr_path.as_path();
    let _ = rt.dir.path();
    Ok(())
}

#[test]
fn rpc_cli_exec_status_returns_envelope() -> Result<(), String> {
    let rt = start_scoped_runtime()?;
    let url = format!("http://127.0.0.1:{}/api/rpc", rt.port);
    for code in ["scope_required", "scope_mismatch"] {
        let mut invalid = rt.rpc_body(code, &["--json", "status"]);
        if code == "scope_required" {
            invalid["scope"] = serde_json::Value::Null;
        } else {
            invalid["scope"]["instance_id"] = serde_json::json!("foreign-runtime-instance");
        }
        let rejected: serde_json::Value = ureq::post(&url)
            .send_json(invalid.clone()).map_err(|err| err.to_string())?
            .into_json().map_err(|err| err.to_string())?;
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert_eq!(rejected["code"], code, "{rejected}");
        assert_eq!(rejected["id"], invalid["id"]);
        assert_eq!(rejected["pid"], rt.identity["pid"]);
    }
    let body = rt.rpc_body("ci-status", &["--json", "status"]);
    let resp = ureq::post(&format!("http://127.0.0.1:{}/api/rpc", rt.port))
        .send_json(body.clone()).map_err(|err| err.to_string())?;
    let parsed: serde_json::Value = resp.into_json().map_err(|err| err.to_string())?;
    if parsed["ok"] != true {
        return Err(format!("rpc {parsed}"));
    }
    assert_eq!(parsed["id"], body["id"]);
    assert_eq!(parsed["operation_id"], body["operation_id"]);
    assert_eq!(parsed["idempotency_key"], body["idempotency_key"]);
    assert_eq!(parsed["scope"], body["scope"]);
    if parsed["envelope"]["command"] != "status" {
        return Err(format!("envelope {parsed}"));
    }
    // Existing host credentials remain usable; the new runtime ignores the header.
    fs::write(
        rt.dir.path().join("runtime").join("token"),
        "synthetic-old-runtime-token",
    )
    .map_err(|err| err.to_string())?;
    let output = Command::new(bin())
        .args(["--client-only", "status", "--json"])
        .env("RSRS_DATA_DIR", rt.dir.path())
        .env("RSRS_RPC_PORT", rt.port.to_string())
        .env_remove("RSRS_RPC_TOKEN")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "client-only status failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    if envelope["command"] != "status" {
        return Err(format!("client-only envelope {envelope}"));
    }
    Ok(())
}

#[test]
fn parallel_status_requests_all_complete() -> Result<(), String> {
    let rt = start_internal_runtime()?;
    let url = format!("http://127.0.0.1:{}/api/rpc", rt.port);
    let mut handles = Vec::new();
    for index in 0..8 {
        let url = url.clone();
        let body = rt.rpc_body(&format!("queue-{index}"), &["--json", "status"]);
        handles.push(thread::spawn(move || {

            let resp = ureq::post(&url)
                .timeout(Duration::from_secs(20))
                .send_json(body.clone())
                .map_err(|err| err.to_string())?;
            let parsed: serde_json::Value = resp.into_json().map_err(|err| err.to_string())?;
            for field in ["id", "operation_id", "idempotency_key", "scope"] {
                assert_eq!(parsed[field], body[field], "unbound parallel reply: {field}");
            }
            if parsed["ok"] != true {
                return Err(format!("rpc {parsed}"));
            }
            if parsed["envelope"]["command"] != "status" {
                return Err(format!("envelope {parsed}"));
            }
            Ok(())
        }));
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| "status worker panicked".to_string())??;
    }
    Ok(())
}

#[test]
fn stop_closes_listen_port() -> Result<(), String> {
    let mut rt = start_internal_runtime()?;
    let url = format!("http://127.0.0.1:{}/api/runtime/stop", rt.port);
    let mut foreign = serde_json::json!({"installation_id":rt.identity["installation_id"],
        "account_id":rt.identity["account_id"],"instance_id":rt.identity["instance_id"],
        "generation":rt.identity["generation"]});
    foreign["instance_id"] = serde_json::json!("foreign-runtime-instance");
    for invalid in [serde_json::json!({}), foreign] {
        match ureq::post(&url).set("X-Respire-Lifecycle", "coordinator-v1").send_json(invalid) {
            Err(ureq::Error::Status(409, response)) => {
                let rejected: serde_json::Value = response.into_json().map_err(|err| err.to_string())?;
                assert_eq!(rejected["error"], "stop_identity_mismatch");
            }
            other => return Err(format!("unbound stop was not rejected: {other:?}")),
        }
        let health: serde_json::Value = ureq::get(&format!("http://127.0.0.1:{}/api/health", rt.port))
            .call().map_err(|err| err.to_string())?.into_json().map_err(|err| err.to_string())?;
        rt.check_health(&health);
        assert!(rt.child.try_wait().map_err(|err| err.to_string())?.is_none());
    }
    let operation = rt.stop()?;
    let start = Instant::now();
    let addr = format!("127.0.0.1:{}", rt.port);
    while start.elapsed() < Duration::from_secs(12) {
        if rt.child.try_wait().map_err(|err| err.to_string())?.is_some()
            && std::net::TcpStream::connect(&addr).is_err() {
            return rt.verify_stop_receipt(Some(&operation));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("runtime process or listen port remained after stop".into())
}

#[test]
fn incomplete_control_body_does_not_block_health_rpc_or_stop(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut rt = start_internal_runtime()?;
    let mut incomplete = std::net::TcpStream::connect(("127.0.0.1", rt.port))?;
    write!(
        incomplete,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 4096\r\n\r\nx",
        rt.port
    )?;
    thread::sleep(Duration::from_millis(100));
    let base = format!("http://127.0.0.1:{}", rt.port);
    let health: serde_json::Value = ureq::get(&format!("{base}/api/health"))
        .timeout(Duration::from_secs(2))
        .call()?
        .into_json()?;
    rt.check_health(&health);
    let reply = rt.rpc("incomplete-body-next", &["--json", "status"], Duration::from_secs(2))?;
    assert_eq!(reply["ok"], true, "{reply}");
    let operation = rt.stop()?;
    let until = Instant::now() + Duration::from_secs(12);
    while rt.child.try_wait()?.is_none() {
        assert!(
            Instant::now() < until,
            "owned runtime did not stop with incomplete peer connected"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(std::net::TcpStream::connect(("127.0.0.1", rt.port)).is_err());
    rt.verify_stop_receipt(Some(&operation))?;
    Ok(())
}

#[test]
fn non_reading_peer_and_pipelined_responses_do_not_block_other_connections(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    let rt = start_internal_runtime()?;
    let mut stalled = std::net::TcpStream::connect(("127.0.0.1", rt.port))?;
    stalled.set_write_timeout(Some(Duration::from_secs(5)))?;
    let body = serde_json::json!({"v":1,"id":"non-reading-peer","method":"x".repeat(8*1024*1024),"args":[]}).to_string();
    write!(stalled, "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", rt.port, body.len(), body)?;
    // Queue a control request behind the large response on the same connection.
    write!(
        stalled,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        rt.port
    )?;
    thread::sleep(Duration::from_millis(250));
    let base = format!("http://127.0.0.1:{}", rt.port);
    for _ in 0..3 {
        let health: serde_json::Value = ureq::get(&format!("{base}/api/health"))
            .timeout(Duration::from_secs(2))
            .call()?
            .into_json()?;
        rt.check_health(&health);
    }
    let reply = rt.rpc("non-reading-next", &["--json", "status"], Duration::from_secs(2))?;
    assert_eq!(reply["ok"], true, "{reply}");
    drop(stalled);
    let mut pipeline = std::net::TcpStream::connect(("127.0.0.1", rt.port))?;
    pipeline.set_read_timeout(Some(Duration::from_secs(5)))?;
    for (id, deadline) in [("pipeline-a", None), ("pipeline-b", Some(0u64))] {
        let mut body = rt.rpc_body(id, &["--json", "status"]);
        if let Some(deadline) = deadline {
            body["queue_wait_ms"] = serde_json::json!(deadline);
        }
        let body = body.to_string();
        write!(pipeline, "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", rt.port, body.len(), body)?;
    }
    write!(
        pipeline,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        rt.port
    )?;
    let mut wire = String::new();
    pipeline.read_to_string(&mut wire)?;
    let first = wire
        .find("\"id\":\"pipeline-a\"")
        .ok_or("first pipeline reply missing")?;
    let second = wire
        .find("\"id\":\"pipeline-b\"")
        .ok_or("second pipeline reply missing")?;
    assert!(first < second, "responses were reordered");
    assert!(
        wire.contains("command was not started"),
        "zero-deadline pipeline write/read was executed: {wire}"
    );
    assert!(
        wire[second..].contains("\"server\":\"respire\""),
        "pipelined health reply missing"
    );
    Ok(())
}

#[test]
fn rpc_memory_revision_is_read_only_and_model_free() -> Result<(), String> {
    let rt = start_scoped_runtime()?;
    support::install_runtime_fixture(rt.dir.path()).map_err(|err| format!("{err:#}"))?;
    let mut baseline = None;
    for args in [
        vec!["memory-revision", "--json"],
        vec!["--client-only", "memory-revision", "--json"],
        vec!["memory-revision"],
    ] {
        let output = Command::new(bin())
            .args(args)
            .env("RSRS_DATA_DIR", rt.dir.path())
            .env("RSRS_RPC_PORT", rt.port.to_string())
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_CLIENT_ONLY")
            .env_remove("RSRS_RUNTIME_BIN")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env("RSRS_JSON", "1")
            .output()
            .map_err(|err| err.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "revision failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
        assert_eq!(envelope["command"], "memory-revision");
        assert_eq!(envelope["status"], "ok");
        assert_eq!(
            envelope["summary"]["profile"],
            rt.dir.path().to_string_lossy().as_ref()
        );
        assert_eq!(
            envelope["summary"]["revision"].as_str().unwrap_or("").len(),
            32
        );
        if let Some(expected) = &baseline {
            assert_eq!(&envelope["summary"], expected);
        } else {
            baseline = Some(envelope["summary"].clone());
        }
    }
    assert!(!rt.dir.path().join("session.json").exists());
    assert!(!rt.dir.path().join("model").exists());
    Ok(())
}

#[test]
fn rpc_memory_revision_does_not_initialize_after_off_start(
) -> Result<(), Box<dyn std::error::Error>> {
    let rt = start_internal_runtime_with_mode("off")?;
    let db = rt.dir.path().join("rsrs.db");
    assert!(!db.exists());
    fs::write(
        rt.dir.path().join("client.json"),
        r#"{"service_mode":"normal"}"#,
    )?;
    for index in 0..3 {
        let reply = rt.rpc(&format!("revision-off-start-{index}"), &["--json", "memory-revision"], Duration::from_secs(3))?;
        assert_eq!(
            reply["ok"], false,
            "probe created an absent library: {reply}"
        );
        assert!(!db.exists());
        assert!(!rt.dir.path().join("onememory.db").exists());
        assert!(!rt.dir.path().join("session.json").exists());
    }
    Ok(())
}

#[test]
fn rpc_memory_revision_does_not_schedule_background_work() -> Result<(), Box<dyn std::error::Error>>
{
    let rt = start_internal_runtime()?;
    let base = format!("http://127.0.0.1:{}", rt.port);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let health: serde_json::Value = ureq::get(&format!("{base}/api/health"))
            .call()?
            .into_json()?;
        if health["retrieval_index"]["state"] == "ready"
            && health["retrieval_index"]["scheduled"] == false
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "initial empty index did not settle: {health}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    // A later unsolicited index kick would now fail on the deliberate ambiguity.
    // This synthetic file is never opened as a database and contains no user data.
    fs::write(
        rt.dir.path().join("onememory.db"),
        b"synthetic ambiguous path",
    )?;
    for index in 0..3 {
        let reply = rt.rpc(&format!("revision-no-index-{index}"), &["--json", "memory-revision"], Duration::from_secs(3))?;
        assert_eq!(reply["ok"], false);
        thread::sleep(Duration::from_millis(100));
        let health: serde_json::Value = ureq::get(&format!("{base}/api/health"))
            .call()?
            .into_json()?;
        assert_eq!(
            health["retrieval_index"]["state"], "ready",
            "probe scheduled indexing: {health}"
        );
        assert_eq!(health["retrieval_index"]["scheduled"], false);
        assert_eq!(health["retrieval_index"]["running"], false);
        assert!(!rt.dir.path().join("session.json").exists());
    }
    Ok(())
}
