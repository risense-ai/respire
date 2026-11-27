use super::*;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn inspect_runtime(
    status: u16,
    discovery: Value,
) -> Result<(Value, Vec<String>), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("runtime"))?;
    fs::write(dir.path().join("runtime/token"), "discovery-fixture-token")?;
    let server = loopback_http::Server::bind()?;
    let port = server.addr().port();
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let mut owner = fixture_health(dir.path());
    owner["url"] = json!("http://127.0.0.1:15170");
    owner["data_dir"] = json!("fixture-profile");
    let worker = thread::spawn(move || -> Result<Vec<String>, String> {
        let mut paths = Vec::new();
        while !finished.load(Ordering::Acquire) {
            let Some(request) = server
                .recv_timeout(Duration::from_millis(100))
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            if request.header("Authorization") != Some("Bearer discovery-fixture-token") {
                return Err("discovery or owner request omitted the existing bearer".into());
            }
            let path = request.url().to_owned();
            paths.push(path.clone());
            if path == "/v1/health" {
                use std::io::Write;
                let body = discovery.to_string();
                write!(request.into_stream(),
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                    .map_err(|error| error.to_string())?;
            } else if path == "/api/health" {
                request
                    .reply(owner.to_string())
                    .map_err(|error| error.to_string())?;
            } else {
                return Err(format!("unexpected discovery request: {path}"));
            }
        }
        Ok(paths)
    });
    let output = Command::new(bin())
        .args(["--client-only", "--runtime-internal", "--status", "--json"])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env_remove("RSRS_RPC_TOKEN")
        .output();
    done.store(true, Ordering::Release);
    let paths = worker.join().map_err(|_| "discovery fixture panicked")??;
    let output = output?;
    let envelope: Value = serde_json::from_slice(&output.stdout)?;
    let expected_exit = if envelope["status"] == "ok" { 0 } else { 2 };
    assert_eq!(output.status.code(), Some(expected_exit), "{envelope}");
    Ok((envelope, paths))
}

#[test]
fn runtime_discovery_preserves_the_exact_legacy_status_envelope(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut baseline = None;
    for (status, capabilities) in [
        (404, json!([])),
        (200, json!([])),
        (200, json!(["unrelated.capability"])),
        (200, json!(["openapi.v1"])),
    ] {
        let (envelope, paths) = inspect_runtime(
            status,
            json!({
                "status":"ready", "instance_id":"fixture-instance", "capabilities":capabilities
            }),
        )?;
        assert_eq!(paths, ["/v1/health", "/api/health"]);
        assert_eq!(envelope["command"], "runtime");
        assert_eq!(envelope["status"], "ok");
        assert_eq!(
            envelope["summary"],
            json!({"state":"up", "pid":1,
            "url":"http://127.0.0.1:15170", "data_dir":"fixture-profile"})
        );
        if let Some(expected) = baseline.as_ref() {
            assert_eq!(
                &envelope, expected,
                "negotiation changed the CLI output envelope"
            );
        } else {
            baseline = Some(envelope);
        }
    }
    Ok(())
}

#[test]
fn runtime_discovery_does_not_fall_back_after_auth_or_transport_errors(
) -> Result<(), Box<dyn std::error::Error>> {
    for status in [401, 403, 503, 302] {
        let (envelope, paths) = inspect_runtime(status, json!({}))?;
        assert_eq!(paths, ["/v1/health"]);
        assert_eq!(envelope["status"], "warn");
        assert_eq!(envelope["summary"], json!({"state":"down"}));
        assert!(!envelope["errors"]
            .as_array()
            .ok_or("errors missing")?
            .is_empty());
    }
    Ok(())
}

#[test]
fn runtime_discovery_rejects_instance_changes_and_malformed_health(
) -> Result<(), Box<dyn std::error::Error>> {
    for (body, expected_paths) in [
        (
            json!({"status":"ready", "instance_id":"replacement", "capabilities":["openapi.v1"]}),
            vec!["/v1/health", "/api/health"],
        ),
        (
            json!({"status":"ready", "instance_id":"", "capabilities":["openapi.v1"]}),
            vec!["/v1/health", "/api/health"],
        ),
        (json!({}), vec!["/v1/health"]),
    ] {
        let (envelope, paths) = inspect_runtime(200, body)?;
        assert_eq!(paths, expected_paths);
        assert_eq!(envelope["status"], "warn");
        assert_eq!(envelope["summary"], json!({"state":"down"}));
    }
    Ok(())
}
