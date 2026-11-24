//! Frozen legacy HTTP transport for the existing command dispatcher.
use super::{response, HttpRequest, MAX_BODY};
use axum::{body::Bytes, response::Response};
use serde_json::json;
use std::io::Read;

pub(super) fn matches(method: &tiny_http::Method, path: &str) -> bool {
    method == &tiny_http::Method::Post && matches!(path, "/rpc" | "/api/rpc")
}

pub(super) fn synchronous(req: &mut HttpRequest) -> (u16, &'static str, Vec<u8>) {
    let mut body = String::new();
    if req
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_string(&mut body)
        .is_err()
    {
        return (
            400,
            "application/json; charset=utf-8",
            serde_json::json!({"error":"request body unreadable"})
                .to_string()
                .into_bytes(),
        );
    }
    if body.len() as u64 > MAX_BODY {
        return (
            413,
            "application/json; charset=utf-8",
            json!({"error":"request body exceeds runtime frame limit"})
                .to_string()
                .into_bytes(),
        );
    }
    match crate::rpc_serve::handle_http_rpc(body.as_bytes()) {
        Ok(response) => (
            200,
            "application/json; charset=utf-8",
            serde_json::to_vec(&response).unwrap_or_default(),
        ),
        Err(error) => (
            400,
            "application/json; charset=utf-8",
            serde_json::json!({"error": error.to_string()})
                .to_string()
                .into_bytes(),
        ),
    }
}

pub(super) async fn deferred(bytes: Bytes) -> Response {
    let (send, receive) = tokio::sync::oneshot::channel();
    // The completion callback only sends an in-memory result. Hyper owns
    // response ordering and all socket backpressure in its async task.
    // Admission may wait for a business gate; never block a network worker.
    match tokio::task::spawn_blocking(move || {
        crate::rpc_serve::handle_http_rpc_deferred(&bytes, move |value| {
            let _ = send.send(value);
        })
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return response(
                400,
                "application/json",
                json!({"error": error.to_string()}).to_string().into_bytes(),
            )
        }
        Err(_) => {
            return response(
                503,
                "application/json",
                b"{\"error\":\"runtime admission unavailable\"}".to_vec(),
            )
        }
    }
    match receive.await {
        Ok(value) => response(
            200,
            "application/json; charset=utf-8",
            serde_json::to_vec(&value).unwrap_or_default(),
        ),
        Err(_) => response(
            503,
            "application/json",
            b"{\"error\":\"runtime reply unavailable\"}".to_vec(),
        ),
    }
}
