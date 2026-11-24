//! Native model control is intentionally outside the public v1 contract.
use super::{
    owner_control::model_work,
    owner_request,
    problem::{Problem, V1Result},
};
use axum::{
    body,
    extract::{ConnectInfo, Request},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde::Deserialize;
use std::{net::SocketAddr, time::Duration};

const PATH: &str = "/api/runtime/private-model/remove";
const PROOF: &str = "x-respire-model-proof";
const BODY_LIMIT: usize = 512;

pub(super) fn routes(bound: SocketAddr) -> Router {
    Router::new().route(
        PATH,
        post(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| { serve(request, peer, bound) },
        ),
    )
}

async fn serve(request: Request, peer: SocketAddr, bound: SocketAddr) -> Response {
    let outcome = async {
        let (parts, body) = request.into_parts();
        let proof = verify_headers(&parts, peer, bound)?;
        let bytes = tokio::time::timeout(Duration::from_secs(1), body::to_bytes(body, BODY_LIMIT))
            .await
            .map_err(|_| bad_request())?
            .map_err(|_| bad_request())?;
        let frame = decode(&bytes)?;
        let receipt = model_work::admit_remove(frame.model_id, proof).await?;
        let payload = serde_json::to_vec(&receipt).map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_receipt_unavailable",
                "Model receipt unavailable",
            )
        })?;
        Ok::<_, Problem>(
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                payload,
            )
                .into_response(),
        )
    }
    .await;
    outcome.into_response()
}

pub(super) fn verify_headers(
    parts: &Parts,
    peer: SocketAddr,
    bound: SocketAddr,
) -> V1Result<String> {
    // Prohibit browser-originated or ambient-cookie native control. A normal
    // HTTP bearer alone cannot redeem an operation.
    if parts.uri.query().is_some()
        || parts.headers.contains_key(header::COOKIE)
        || parts.headers.contains_key(header::ORIGIN)
    {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "native_model_control_required",
            "Native model control required",
        ));
    }
    owner_request::admit_selected(parts, peer, bound)?;
    let proof = owner_request::single(&parts.headers, PROOF)
        .ok()
        .flatten()
        .filter(|value| value.len() == 149 && value.starts_with("rhm1_"))
        .ok_or_else(|| {
            Problem::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication required",
            )
        })?;
    Ok(proof.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    schema_version: u8,
    model_id: String,
}

fn decode(bytes: &[u8]) -> V1Result<RequestFrame> {
    let frame: RequestFrame = serde_json::from_slice(bytes).map_err(|_| bad_request())?;
    if frame.schema_version != 1 {
        return Err(bad_request());
    }
    Ok(frame)
}

pub(super) fn bad_request() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_model_request",
        "Invalid model request",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_accepts_only_model_id_and_explicit_version() {
        assert!(decode(br#"{"schema_version":1,"model_id":"m3"}"#).is_ok());
        for invalid in [
            br#"{"schema_version":2,"model_id":"m3"}"#.as_slice(),
            br#"{"schema_version":1,"model_id":"m3","path":"untrusted"}"#,
            br#"{"schema_version":1,"model_id":"m3","account":"untrusted"}"#,
            br#"{"model_id":"m3"}"#,
        ] {
            assert!(decode(invalid).is_err());
        }
    }

    #[test]
    fn ordinary_proof_or_bearer_alone_cannot_admit_native_model() {
        let address: SocketAddr = "127.0.0.1:19000".parse().unwrap();
        for (name, value) in [
            ("authorization", "Bearer ordinary"),
            ("x-respire-handshake-proof", "rhp1_ordinary"),
            ("cookie", "ambient=1"),
            ("origin", "http://127.0.0.1:19000"),
        ] {
            let (parts, _) = axum::http::Request::builder()
                .method("POST")
                .uri(PATH)
                .header("host", "127.0.0.1:19000")
                .header(name, value)
                .body(())
                .unwrap()
                .into_parts();
            assert!(verify_headers(&parts, address, address).is_err());
        }
    }
}
