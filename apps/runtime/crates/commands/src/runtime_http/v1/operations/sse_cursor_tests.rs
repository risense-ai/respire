use super::{BOUND, TOKEN, WAIT};
use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    extract::ConnectInfo,
    http::{header, Request, StatusCode},
    response::Response,
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn open(app: &Router, operation: &str, token: &str, ids: &[&str]) -> Result<Response> {
    let mut request = Request::get(format!("{operation}/events"))
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    for id in ids {
        request = request.header("last-event-id", *id);
    }
    let mut request = request.body(Body::empty())?;
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::network::Peer(BOUND)));
    Ok(tokio::time::timeout(WAIT, app.clone().oneshot(request)).await??)
}

async fn expect_stream(app: &Router, operation: &str, token: &str, ids: &[&str]) -> Result<Response> {
    let response = open(app, operation, token, ids).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    Ok(response)
}

pub(super) async fn events(app: &Router, operation: &str, token: &str) -> Result<Response> {
    expect_stream(app, operation, token, &[]).await
}

fn revision(frame: &str) -> Result<u64> {
    Ok(frame
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .context("SSE revision")?
        .parse()?)
}

pub(super) async fn check_reconnect(
    app: &Router,
    operation: &str,
    progress: &[u8],
    terminal: &[u8],
    status: &Value,
) -> Result<()> {
    let progress = std::str::from_utf8(progress)?;
    let terminal = std::str::from_utf8(terminal)?;
    let progress_id = revision(progress)?;
    let terminal_id = revision(terminal)?;
    assert!(progress_id < terminal_id);
    assert!(terminal.lines().any(|line| line == "event: cancelled"));
    let data = terminal
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .context("SSE data")?;
    assert_eq!(
        serde_json::from_str::<Value>(data)?,
        json!({"event":"cancelled","data":status})
    );

    let progress_id = progress_id.to_string();
    let terminal_id_text = terminal_id.to_string();
    let replay = expect_stream(app, operation, TOKEN, &[&progress_id]).await?;
    let body = tokio::time::timeout(WAIT, to_bytes(replay.into_body(), 8192)).await??;
    assert_eq!(&body[..], terminal.as_bytes());

    let caught_up = expect_stream(app, operation, TOKEN, &[&terminal_id_text]).await?;
    let body = tokio::time::timeout(WAIT, to_bytes(caught_up.into_body(), 8192)).await??;
    assert!(body.is_empty());

    let future_id = (terminal_id + 1).to_string();
    for ids in [
        vec!["not-a-revision"],
        vec![progress_id.as_str(), terminal_id_text.as_str()],
        vec![future_id.as_str()],
    ] {
        let response = open(app, operation, TOKEN, &ids).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let body = tokio::time::timeout(WAIT, to_bytes(response.into_body(), 8192)).await??;
        let problem: Value = serde_json::from_slice(&body)?;
        assert_eq!(problem["status"], 400);
        assert_eq!(problem["code"], "invalid_last_event_id");
    }
    Ok(())
}
