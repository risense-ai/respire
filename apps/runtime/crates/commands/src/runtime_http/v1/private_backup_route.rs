//! Native-only backup restore confirmation route outside the public v1 contract.
use super::{
    operations::backup_import::{confirm_local_restore, LocalRestoreConfirmation},
    owner_control, owner_request,
    problem::{Problem, V1Result},
};
use axum::{
    body,
    extract::ConnectInfo,
    extract::Request,
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde::Deserialize;
use std::{net::SocketAddr, time::Duration};
use uuid::Uuid;

const PATH: &str = "/api/runtime/private-backup/confirm";
const PROOF: &str = "x-respire-backup-proof";
const BODY_LIMIT: usize = 1024;

pub(super) fn routes(bound: SocketAddr) -> Router {
    Router::new().route(
        PATH,
        post(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| confirm(request, peer, bound),
        ),
    )
}

async fn confirm(request: Request, peer: SocketAddr, bound: SocketAddr) -> Response {
    let outcome = async {
        let (parts, body) = request.into_parts();
        let proof = verify_headers(&parts, peer, bound)?;
        let bytes = tokio::time::timeout(Duration::from_secs(1), body::to_bytes(body, BODY_LIMIT))
            .await
            .map_err(|_| bad_request())?
            .map_err(|_| bad_request())?;
        let (operation_id, archive_sha256, target_sha256) = decode(&bytes)?;
        let confirmed_owner =
            owner_control::authorize_backup_restore(operation_id, archive_sha256, target_sha256, proof)
                .await?;
        confirm_local_restore(LocalRestoreConfirmation {
            operation_id,
            archive_sha256,
            target_sha256,
            confirmed_owner,
        })
        .await?;
        let payload = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id.to_string(),
            "status": "completed",
        }))
        .map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backup_receipt_unavailable",
                "Backup receipt unavailable",
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

pub(super) fn verify_headers(parts: &Parts, peer: SocketAddr, bound: SocketAddr) -> V1Result<String> {
    if parts.uri.query().is_some()
        || parts.headers.contains_key(header::COOKIE)
        || parts.headers.contains_key(header::ORIGIN)
    {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "native_backup_control_required",
            "Native backup control required",
        ));
    }
    owner_request::admit_selected(parts, peer, bound)?;
    let proof = owner_request::single(&parts.headers, PROOF)
        .ok()
        .flatten()
        .filter(|value| value.len() == 149 && value.starts_with("rhb1_"))
        .ok_or_else(|| Problem::new(StatusCode::UNAUTHORIZED, "unauthorized", "Authentication required"))?;
    Ok(proof.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    schema_version: u8,
    operation_id: String,
    archive_sha256: String,
    target_sha256: String,
}

fn decode_digest(raw: &str) -> V1Result<[u8; 32]> {
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(bad_request());
    }
    let mut out = [0_u8; 32];
    hex::decode_to_slice(raw, &mut out).map_err(|_| bad_request())?;
    Ok(out)
}

fn decode(bytes: &[u8]) -> V1Result<(Uuid, [u8; 32], [u8; 32])> {
    let frame: RequestFrame = serde_json::from_slice(bytes).map_err(|_| bad_request())?;
    if frame.schema_version != 1 {
        return Err(bad_request());
    }
    let operation_id = Uuid::parse_str(&frame.operation_id)
        .ok()
        .filter(|id| id.get_version_num() == 4 && id.to_string() == frame.operation_id)
        .ok_or_else(bad_request)?;
    let archive_sha256 = decode_digest(&frame.archive_sha256)?;
    let target_sha256 = decode_digest(&frame.target_sha256)?;
    Ok((operation_id, archive_sha256, target_sha256))
}

fn bad_request() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_backup_confirmation",
        "Invalid backup confirmation request",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_accepts_only_exact_confirmation_fields() {
        let id = Uuid::new_v4().to_string();
        let digest = "a".repeat(64);
        let target = "b".repeat(64);
        let valid = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "operation_id": id,
            "archive_sha256": digest,
            "target_sha256": target,
        }))
        .unwrap();
        assert!(decode(&valid).is_ok());
        for bad in [
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "operation_id": id, "archive_sha256": digest, "target_sha256": target,
            })).unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "operation_id": id, "archive_sha256": digest, "target_sha256": target, "extra": true,
            })).unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "operation_id": "not-a-uuid", "archive_sha256": digest, "target_sha256": target,
            })).unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "operation_id": id, "archive_sha256": "A".repeat(64), "target_sha256": target,
            })).unwrap(),
        ] {
            assert!(decode(&bad).is_err());
        }
    }

    #[test]
    fn ordinary_bearer_or_other_proofs_cannot_confirm_backup() {
        let address: SocketAddr = "127.0.0.1:19000".parse().unwrap();
        for (name, value) in [
            ("authorization", "Bearer ordinary"),
            ("x-respire-handshake-proof", "rhp1_ordinary"),
            ("x-respire-model-proof", "rhm1_ordinary"),
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
