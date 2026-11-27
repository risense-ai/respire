use super::*;

#[test]
fn client_sends_existing_token_on_initial_health_and_rpc_requests(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("runtime"))?;
    fs::write(dir.path().join("runtime/token"), "synthetic-upgrade-token")?;
    let server = loopback_http::Server::bind()?;
    let port = server.addr().port();
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let health_body = fixture_health(dir.path());
    let worker = thread::spawn(move || -> Result<Vec<String>, String> {
        let mut paths = Vec::new();
        while !finished.load(Ordering::Acquire) {
            let Some(request) = server
                .recv_timeout(Duration::from_millis(100))
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            if request.header("Authorization") != Some("Bearer synthetic-upgrade-token") {
                return Err("initial request omitted the existing runtime token".into());
            }
            paths.push(request.url().to_owned());
            if request.url() == "/v1/health" {
                use std::io::Write;
                request.into_stream().write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ).map_err(|error| error.to_string())?;
                continue;
            }
            let response = if request.url() == "/api/health" {
                health_body.clone()
            } else {
                let body: serde_json::Value = serde_json::from_slice(request.body())
                    .map_err(|error| error.to_string())?;
                if body["method"] == "cli.exec" {
                    check_fixture_operation(&body)?;
                }
                serde_json::json!({"v":1,"id":body["id"],"ok":true,"exit":0,"bin":"http-fixture",
                    "operation_id":body["operation_id"],"idempotency_key":body["idempotency_key"],"scope":body["scope"],
                    "envelope":{"command":"status","status":"ok","summary":{},"items":[],"actions":[],"errors":[],"details":null,"related":[]}})
            };
            request
                .reply(response.to_string())
                .map_err(|error| error.to_string())?;
        }
        Ok(paths)
    });
    let output = Command::new(bin())
        .args(["--client-only", "status", "--json"])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env_remove("RSRS_RPC_TOKEN")
        .output()?;
    let health = Command::new(bin())
        .args(["--client-only", "--runtime-internal", "--status", "--json"])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env_remove("RSRS_RPC_TOKEN")
        .output()?;
    done.store(true, Ordering::Release);
    let paths = worker.join().map_err(|_| "HTTP fixture panicked")??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        health.status.success(),
        "{}",
        String::from_utf8_lossy(&health.stdout)
    );
    assert!(paths.windows(2).any(|pair| pair == ["/v1/health", "/api/health"]));
    assert_eq!(paths.iter().filter(|path| *path == "/v1/health").count(), 1);
    assert!(paths.iter().any(|path| path == "/api/rpc"));
    Ok(())
}

/// Legacy display envelopes still normalize under the current scoped transport.
#[test]
fn client_normalizes_legacy_failure_envelopes_and_preserves_remote_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    for (status, exit, reason, errors) in [
        ("fail", 1, "legacy_missing", serde_json::json!([])),
        (
            "fail",
            2,
            "invalid_input",
            serde_json::json!(["original remote diagnostic"]),
        ),
        ("pending", 2, "decision_required", serde_json::json!([])),
    ] {
        let dir = tempfile::tempdir()?;
        let server = loopback_http::Server::bind()?;
        let port = server
            .addr()
            .port();
        let original_errors = errors.clone();
        let health_body = fixture_health(dir.path());
        let worker = thread::spawn(move || -> Result<(), String> {
            let request = loop {
                let request = server
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|error| error.to_string())?
                    .ok_or("legacy fixture received no request")?;
                if request.url() != "/api/health" {
                    break request;
                }
                request.reply(health_body.to_string())
                    .map_err(|error| error.to_string())?;
            };
            if request.url() != "/api/rpc" {
                return Err("unexpected legacy fixture request".into());
            }
            let body: serde_json::Value =
                serde_json::from_slice(request.body()).map_err(|error| error.to_string())?;
            if body["method"] != "cli.exec" {
                return Err("unexpected legacy fixture method".into());
            }
            check_fixture_operation(&body)?;
            let response = serde_json::json!({"v":1,"id":body["id"],"ok":exit==0,"exit":exit,"bin":"http-fixture",
                "operation_id":body["operation_id"],"idempotency_key":body["idempotency_key"],"scope":body["scope"],
                "envelope":{"command":"show","status":status,"summary":{"reason":reason},
                    "items":[{"name":"entry","status":"fail","value":"not found"}],
                    "actions":[],"errors":errors,"details":{"error_type":"legacy_fixture"},"related":[]}});
            request
                .reply(response.to_string())
                .map_err(|error| error.to_string())
        });
        let output = Command::new(bin())
            .args(["--client-only", "show", "synthetic-missing", "--json"])
            .env("RSRS_DATA_DIR", dir.path())
            .env("RSRS_RPC_PORT", port.to_string())
            .env_remove("RSRS_RPC_TOKEN")
            .output()?;
        worker.join().map_err(|_| "legacy fixture panicked")??;
        assert_eq!(output.status.code(), Some(exit));
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(envelope["status"], status);
        assert_eq!(envelope["summary"]["reason"], reason);
        assert_eq!(envelope["details"]["error_type"], "legacy_fixture");
        if status == "fail" && original_errors == serde_json::json!([]) {
            assert_eq!(envelope["errors"], serde_json::json!(["entry: not found"]));
        } else {
            assert_eq!(envelope["errors"], original_errors);
        }
    }
    Ok(())
}

#[test]
fn client_only_rejects_stalled_inference_before_submitting_a_write(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let server = loopback_http::Server::bind()?;
    let port = server.addr().port();
    let worker = thread::spawn(move || -> Result<String, String> {
        let request = server
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| error.to_string())?
            .ok_or("client did not inspect health")?;
        let path = request.url().to_owned();
        let body = serde_json::json!({"server":"respire","bin":env!("CARGO_PKG_VERSION"),"pid":1,"v":1,
            "inference":{"host_recovery_required":true}});
        request
            .reply(body.to_string())
            .map_err(|error| error.to_string())?;
        Ok(path)
    });
    let output = Command::new(bin())
        .args([
            "--client-only",
            "remember",
            "synthetic pending write",
            "--force",
            "--json",
        ])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env_remove("RSRS_RPC_TOKEN")
        .output()?;
    assert_eq!(
        worker.join().map_err(|_| "HTTP fixture panicked")??,
        "/api/health"
    );
    assert!(!output.status.success());
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(message.contains("request was not submitted"), "{message}");
    assert!(
        !dir.path().join("onememory.db").exists(),
        "rejected client created a local store"
    );
    Ok(())
}

#[test]
fn model_commands_negotiate_v1_and_only_use_legacy_on_unavailable_routes(
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{json, Value};
    use std::io::Write;
    use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

    for (action, capabilities, status, expected_legacy) in [
        ("catalog", true, 200, false),
        ("switch", true, 202, false),
        ("catalog", true, 404, true),
        ("switch", true, 404, true),
        ("catalog", false, 404, true),
        ("switch", true, 409, false),
    ] {
        let dir = tempfile::tempdir()?;
        let server = loopback_http::Server::bind()?;
        let port = server.addr().port();
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        let owner = fixture_health(dir.path());
        let worker = thread::spawn(move || -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
            let mut paths = Vec::new();
            while !finished.load(Ordering::Acquire) {
                let Some(request) = server.recv_timeout(Duration::from_millis(100))? else { continue };
                let path = request.url().to_owned();
                paths.push(path.clone());
                let (code, body) = match path.as_str() {
                    "/api/health" => (200, owner.clone()),
                    "/v1/health" => (200, json!({"status":"ready","instance_id":"fixture-instance",
                        "capabilities":if capabilities { vec!["openapi.v1"] } else { vec![] }})),
                    "/v1/models/catalog" if action == "catalog" => (status, json!({"active_model_id":"m3","models":[]})),
                    "/v1/models/switch" if action == "switch" => {
                        assert_eq!(serde_json::from_slice::<Value>(request.body())?,
                            json!({"model_id":"m3","engine":"cpu","variant":"q4"}));
                        (status, json!({"operation_id":"switch-operation","status":"pending"}))
                    }
                    "/api/rpc" => {
                        let body: Value = serde_json::from_slice(request.body())?;
                        assert_eq!(body["method"], format!("model.{action}"));
                        if action == "switch" { check_fixture_operation(&body)?; }
                        (200, json!({"v":1,"id":body["id"],"ok":true,"exit":0,"bin":"fixture",
                            "operation_id":body["operation_id"],"scope":body["scope"],"idempotency_key":body["idempotency_key"],
                            "envelope":{"command":body["method"],"status":"ok","summary":{"legacy":true},
                            "items":[],"actions":[],"errors":[],"details":null}}))
                    }
                    _ => return Err(format!("unexpected model request {path}").into()),
                };
                let body = body.to_string();
                write!(request.into_stream(), "HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
            }
            Ok(paths)
        });
        let mut command = Command::new(bin());
        command.args(["--client-only", "--json", "model", action]);
        if action == "switch" { command.args(["m3", "--variant", "q4"]); }
        let output = command.env("RSRS_DATA_DIR", dir.path())
            .env("RSRS_RPC_PORT", port.to_string()).env_remove("RSRS_RPC_TOKEN").output();
        done.store(true, Ordering::Release);
        let paths = worker.join().map_err(|_| "model fixture panicked")?.map_err(|error| error.to_string())?;
        assert_eq!(paths.iter().filter(|path| *path == "/api/rpc").count(), usize::from(expected_legacy));
        assert_eq!(paths.iter().filter(|path| path.starts_with("/v1/models/")).count(), usize::from(capabilities));
        let output = output?;
        let envelope: Value = serde_json::from_slice(&output.stdout)?;
        if expected_legacy {
            assert_eq!(envelope["summary"]["legacy"], true);
        } else if status == 409 {
            assert!(!output.status.success());
        } else if action == "catalog" {
            assert!(output.status.success());
            assert_eq!(envelope["summary"]["active_model_id"], "m3");
        } else {
            assert_eq!(envelope["status"], "pending");
            assert_eq!(envelope["summary"]["id"], "switch-operation");
        }
    }
    Ok(())
}
