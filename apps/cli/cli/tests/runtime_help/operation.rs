use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{io::Write, process::Command, time::Duration};

#[path = "../../../test-support/loopback_http.rs"]
mod loopback_http;

type Reply = (&'static str, &'static str, &'static str, String);
const ID: &str = "eb20bd22-8aa3-4e5d-bb36-c8db4d91dcea";

fn invoke(script: Vec<Reply>) -> Result<std::process::Output> {
    invoke_operation(script, true)
}

fn invoke_operation(script: Vec<Reply>, follow: bool) -> Result<std::process::Output> {
    let dir = tempfile::tempdir()?;
    let server = loopback_http::Server::bind()?;
    let port = server.addr().port();
    let worker = std::thread::spawn(move || -> Result<()> {
        for (path, status, mime, body) in script {
            let req = server
                .recv_timeout(Duration::from_secs(5))?
                .context("expected HTTP request")?;
            assert_eq!(req.url(), path);
            assert_eq!(req.header("Authorization"), Some("Bearer fixture"));
            let mut stream = req.into_stream();
            if mime == "chunked-json" {
                write!(stream, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",body.len())?;
                continue;
            }
            if mime == "slow-json" {
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n")?;
                for _ in 0..70 {
                    if stream.write_all(b" ").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
            if mime == "keepalive" {
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")?;
                let started = std::time::Instant::now();
                while started.elapsed() < Duration::from_secs(35) {
                    if stream.write_all(b": heartbeat\n\n").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
        }
        Ok(())
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsrs"));
    cmd.args(["--client-only", "--json"])
        .env("RSRS_DATA_DIR", dir.path())
        .env("RSRS_RPC_PORT", port.to_string())
        .env("RSRS_RPC_TOKEN", "fixture");
    cmd.args(["runtime", "operation", ID]);
    if follow {
        cmd.arg("--follow");
    }
    let output = cmd.output()?;
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
    Ok(output)
}

const HEALTH: &str = "/v1/health";
const SNAPSHOT: &str = "/v1/operations/eb20bd22-8aa3-4e5d-bb36-c8db4d91dcea";
const EVENTS: &str = "/v1/operations/eb20bd22-8aa3-4e5d-bb36-c8db4d91dcea/events";
const JSON: &str = "application/json";

fn health() -> String {
    json!({"status":"ready","instance_id":"fixture","capabilities":["sse.operations.v1"]})
        .to_string()
}

fn snapshot(id: &str, state: &str) -> Value {
    json!({"operation_id":id,"kind":"model_switch","state":state})
}

fn snapshot_only_health() -> Reply {
    (
        HEALTH,
        "200 OK",
        JSON,
        json!({"status":"ready","instance_id":"fixture","capabilities":["openapi.v1"]}).to_string(),
    )
}

#[test]
fn snapshots_can_use_openapi_without_sse() -> Result<()> {
    let output = invoke_operation(
        vec![
            snapshot_only_health(),
            running_snapshot(),
            snapshot_only_health(),
        ],
        false,
    )?;
    assert!(output.status.success(), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(envelope["command"], "runtime.operation");
    assert_eq!(envelope["summary"], snapshot(ID, "running"));
    Ok(())
}

#[test]
fn following_requires_an_advertised_event_stream() -> Result<()> {
    failed_diagnostic(
        vec![snapshot_only_health()],
        "Runtime does not advertise sse.operations.v1",
    )
}

#[test]
fn unrelated_capabilities_do_not_enable_operation_snapshots() -> Result<()> {
    let health = json!({"status":"ready","instance_id":"fixture","capabilities":["backups.v1"]});
    let output = invoke_operation(vec![(HEALTH, "200 OK", JSON, health.to_string())], false)?;
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("Runtime does not advertise OpenAPI operation snapshots"));
    Ok(())
}

#[test]
fn operation_recovers_snapshot_before_resubscribing() -> Result<()> {
    let end = json!({"event":"completed","data":snapshot(ID,"completed")});
    let wire = format!(": heartbeat\r\n\r\nevent: completed\r\ndata: {end}\r\n\r\n");
    let output = invoke(vec![
        (HEALTH, "200 OK", JSON, health()),
        (
            SNAPSHOT,
            "200 OK",
            JSON,
            snapshot(ID, "running").to_string(),
        ),
        (HEALTH, "200 OK", JSON, health()),
        (EVENTS, "200 OK", "text/event-stream", String::new()),
        (HEALTH, "200 OK", JSON, health()),
        (
            SNAPSHOT,
            "200 OK",
            JSON,
            snapshot(ID, "running").to_string(),
        ),
        (HEALTH, "200 OK", JSON, health()),
        (EVENTS, "200 OK", "text/event-stream", wire),
        (HEALTH, "200 OK", JSON, health()),
    ])?;
    assert!(output.status.success(), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(envelope["command"], "runtime.operation");
    assert_eq!(envelope["summary"], end["data"]);
    Ok(())
}

fn running_snapshot() -> Reply {
    (
        SNAPSHOT,
        "200 OK",
        JSON,
        snapshot(ID, "running").to_string(),
    )
}

fn ready() -> Reply {
    (HEALTH, "200 OK", JSON, health())
}

fn events(body: String) -> Reply {
    (EVENTS, "200 OK", "text/event-stream", body)
}

fn failed_diagnostic(script: Vec<Reply>, diagnostic: &str) -> Result<()> {
    let output = invoke(script)?;
    assert!(!output.status.success(), "{output:?}");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains(diagnostic), "{printed}");
    assert!(!printed.contains("model_switch"), "{printed}");
    Ok(())
}

#[test]
fn operation_rejects_oversized_sse_line_and_accumulated_data() -> Result<()> {
    // The second frame has individually bounded lines whose joined data is too large.
    for (wire, diagnostic) in [
        (
            format!(": {}\n\n", " ".repeat(64 * 1024)),
            "Runtime event line exceeds 64 KiB",
        ),
        (
            format!(
                "data: {}\ndata: {}\n\n",
                " ".repeat(32768),
                " ".repeat(32768)
            ),
            "Runtime event exceeds 64 KiB",
        ),
    ] {
        failed_diagnostic(
            vec![ready(), running_snapshot(), ready(), events(wire)],
            diagnostic,
        )?;
    }
    Ok(())
}

#[test]
fn operation_rejects_malformed_or_mismatched_sse_events() -> Result<()> {
    let data = snapshot(ID, "completed");
    let foreign = snapshot("fc3387f5-aa39-44cc-8770-a57cabdc3789", "completed");
    for (wire, diagnostic) in [
        (
            "event: completed\ndata: {\n\n".into(),
            "EOF while parsing an object",
        ),
        (
            format!(
                "event: completed\ndata: {}\n\n",
                json!({"event":"completed","data":foreign})
            ),
            "Runtime event identity or state mismatch",
        ),
        (
            format!(
                "event: progress\ndata: {}\n\n",
                json!({"event":"completed","data":data})
            ),
            "Runtime event identity or state mismatch",
        ),
        (
            format!(
                "event: completed\ndata: {}\n\n",
                json!({"event":"progress","data":data})
            ),
            "Runtime event identity or state mismatch",
        ),
    ] {
        failed_diagnostic(
            vec![ready(), running_snapshot(), ready(), events(wire)],
            diagnostic,
        )?;
    }
    Ok(())
}

#[test]
fn operation_discards_truncated_sse_frame_and_recovers_snapshot() -> Result<()> {
    let truncated = json!({"event":"completed","data":snapshot(ID,"completed")});
    // The terminal payload is valid JSON, but EOF without a blank line does not dispatch it.
    let wire = format!("event: completed\ndata: {truncated}\n");
    let recovered = snapshot(ID, "failed");
    let output = invoke(vec![
        ready(),
        running_snapshot(),
        ready(),
        events(wire),
        ready(),
        (SNAPSHOT, "200 OK", JSON, recovered.to_string()),
        ready(),
    ])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(envelope["command"], "runtime.operation");
    assert_eq!(envelope["summary"], recovered);
    Ok(())
}

#[test]
fn operation_stops_after_repeated_sse_disconnects() -> Result<()> {
    failed_diagnostic(
        vec![
            ready(),
            running_snapshot(),
            ready(),
            events(String::new()),
            ready(),
            running_snapshot(),
            ready(),
            events(String::new()),
        ],
        "Runtime event stream disconnected repeatedly",
    )
}

#[test]
fn unsupported_runtime_and_foreign_operation_fail_closed() -> Result<()> {
    for (script, diagnostic) in [
        (
            vec![(HEALTH, "404 Not Found", JSON, "{}".into())],
            "legacy Runtime",
        ),
        (
            vec![(
                HEALTH,
                "200 OK",
                JSON,
                health().replace("sse.operations.v1", "openapi.v1"),
            )],
            "does not advertise",
        ),
        (
            vec![
                (HEALTH, "200 OK", JSON, health()),
                (
                    SNAPSHOT,
                    "200 OK",
                    JSON,
                    snapshot("fc3387f5-aa39-44cc-8770-a57cabdc3789", "completed").to_string(),
                ),
            ],
            "different operation",
        ),
    ] {
        let output = invoke(script)?;
        assert!(!output.status.success());
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(printed.contains(diagnostic), "{printed}");
    }
    Ok(())
}

#[test]
fn rejects_replacement_and_bounded_json_overflow() -> Result<()> {
    let h = (HEALTH, "200 OK", JSON, health());
    let running = (
        SNAPSHOT,
        "200 OK",
        JSON,
        snapshot(ID, "running").to_string(),
    );
    let replacement = (
        HEALTH,
        "200 OK",
        JSON,
        health().replace("fixture", "replacement"),
    );
    for (script, diagnostic) in [
        (
            vec![h.clone(), running.clone(), replacement.clone()],
            "instance changed",
        ),
        (
            vec![
                h.clone(),
                running.clone(),
                h.clone(),
                (EVENTS, "200 OK", "text/event-stream", String::new()),
                replacement,
            ],
            "instance changed",
        ),
        (
            vec![(HEALTH, "200 OK", JSON, " ".repeat(65537))],
            "exceeds 64 KiB",
        ),
        (
            vec![h, (SNAPSHOT, "200 OK", "chunked-json", " ".repeat(65537))],
            "exceeds 64 KiB",
        ),
    ] {
        let out = invoke(script)?;
        assert!(!out.status.success());
        assert!(
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .contains(diagnostic),
            "{out:?}"
        );
    }
    Ok(())
}

#[test]
fn heartbeats_cannot_extend_observation_deadline() -> Result<()> {
    let h = (HEALTH, "200 OK", JSON, health());
    let output = invoke(vec![
        h.clone(),
        (
            SNAPSHOT,
            "200 OK",
            JSON,
            snapshot(ID, "running").to_string(),
        ),
        h,
        (EVENTS, "200 OK", "keepalive", String::new()),
    ])?;
    assert!(!output.status.success());
    assert!(
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .contains("observation deadline exceeded"),
        "{output:?}"
    );
    Ok(())
}

#[test]
fn trickled_snapshot_expires_within_json_request_budget() -> Result<()> {
    let start = std::time::Instant::now();
    let output = invoke(vec![
        (HEALTH, "200 OK", JSON, health()),
        (SNAPSHOT, "200 OK", "slow-json", String::new()),
    ])?;
    assert!(!output.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!diagnostic.contains("observation deadline"), "{diagnostic}");
    assert!(start.elapsed() < Duration::from_secs(7), "{diagnostic}");
    Ok(())
}

#[test]
fn terminal_event_from_replaced_instance_is_discarded() -> Result<()> {
    let h = (HEALTH, "200 OK", JSON, health());
    let end = json!({"event":"completed","data":snapshot(ID,"completed")});
    let wire = format!("event: completed\ndata: {end}\n\n");
    let output = invoke(vec![
        h.clone(),
        (
            SNAPSHOT,
            "200 OK",
            JSON,
            snapshot(ID, "running").to_string(),
        ),
        h,
        (EVENTS, "200 OK", "text/event-stream", wire),
        (
            HEALTH,
            "200 OK",
            JSON,
            health().replace("fixture", "replacement"),
        ),
    ])?;
    assert!(!output.status.success());
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains("Runtime instance changed"), "{printed}");
    assert!(!printed.contains("model_switch"), "{printed}");
    Ok(())
}
