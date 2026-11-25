//! REST redemption logic. Production routing requires independently verified native owner state.
use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use std::{net::SocketAddr, time::Instant};

use super::{
    owner_request,
    owner_scope::{ActionSet, OwnerAction},
    problem::{Problem, V1Result},
};

#[cfg(test)]
use super::owner_scope::{Denied, OwnerBinding, OwnerLeaseRegistry};
#[cfg(test)]
use axum::http::request::Parts;

pub(super) fn routes(bound: SocketAddr) -> axum::Router {
    use super::registration::{bind, RouteSpec, Verb};
    use axum::extract::{ConnectInfo, Request};

    bind(
        axum::Router::new(),
        RouteSpec::new(Verb::Post, "/v1/auth/handshake", "exchangePrivateOwnerProof"),
        move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
              request: Request| serve(request, peer, bound),
    )
    .method_not_allowed_fallback(super::method_not_allowed)
}

async fn serve(request: axum::extract::Request, peer: SocketAddr, bound: SocketAddr) -> Response {
    use super::owner_control::{bounded_blocking, unavailable, wall_ms, with_active};

    let deadline = Instant::now() + HANDSHAKE_BODY_TIMEOUT;
    let (parts, body) = request.into_parts();
    if let Err(problem) = owner_request::admit_selected(&parts, peer, bound) {
        return problem.into_response();
    }
    let candidate = parts
        .headers
        .get(PROOF)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let screened = bounded_blocking(move || {
        with_active(|owner, live| {
            if !owner.matches(live) {
                return Err(unavailable());
            }
            let proof = candidate.ok_or_else(unauthorized)?;
            let (wall, _) = fenced_sample(deadline, Instant::now, wall_ms)?;
            owner
                .screen(live, &proof, wall)
                .map_err(|_| unauthorized())?;
            Ok(proof)
        })
    })
    .await;
    let proof = match screened {
        Ok(proof) => proof,
        Err(problem) => return problem.into_response(),
    };
    let bytes = match collect_body_before(body, deadline).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let result = bounded_blocking(move || {
        let (input, requested) = decode(&bytes)?;
        let lease = with_active(|owner, live| {
            let (wall, _) = fenced_sample(deadline, Instant::now, wall_ms)?;
            owner
                .exchange(live, &proof, requested, wall, || {
                    fenced_sample(deadline, Instant::now, wall_ms).ok()
                })
                .map_err(|_| unauthorized())
        })?;
        Ok::<_, Problem>(render(input.scopes, lease))
    })
    .await;
    match result {
        Ok(response) => response,
        Err(problem) => problem.into_response(),
    }
}

const PROOF: &str = "x-respire-handshake-proof";
const MAX_HANDSHAKE_BODY: usize = 1024;
const HANDSHAKE_BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
pub(super) fn exchange(
    parts: &Parts,
    peer: SocketAddr,
    bound: SocketAddr,
    bytes: &[u8],
    registry: &mut OwnerLeaseRegistry,
    verified_live: Option<&OwnerBinding>,
    now: Instant,
) -> V1Result<Response> {
    let proof = admitted_proof(parts, peer, bound)?;
    let (input, requested) = decode(bytes)?;
    let lease = registry
        .exchange(verified_live, proof, requested, now)
        .map_err(|reason| match reason {
            Denied::Busy => Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_unavailable",
                "Runtime unavailable",
            ),
            _ => unauthorized(),
        })?;
    Ok(render(input.scopes, lease))
}

fn decode(bytes: &[u8]) -> V1Result<(Input, ActionSet)> {
    // Scope data must be the only field; never parse credentials from JSON.
    if bytes.len() > MAX_HANDSHAKE_BODY {
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_body_limit",
            "Request body exceeds handshake limit",
        ));
    }
    let input: Input = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let actions: Vec<_> = input
        .scopes
        .iter()
        .map(|item| role(item))
        .collect::<Option<_>>()
        .ok_or_else(invalid)?;
    let requested = ActionSet::try_from_unique(&actions).ok_or_else(invalid)?;
    Ok((input, requested))
}

fn render(scopes: Vec<String>, lease: super::owner_scope::IssuedLease) -> Response {
    let output = serde_json::json!({
        "access_token": lease.token,
        "token_type": "Bearer",
        "expires_in": lease.expires_in,
        "scopes": scopes,
    });
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from(output.to_string()),
    )
        .into_response()
}

// Wire action names are verified against the accepted canonical contract in
// the route parity gate when the provider is enabled.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    scopes: Vec<String>,
}

fn role(name: &str) -> Option<OwnerAction> {
    use OwnerAction::*;
    Some(match name {
        "grants:read" => ReadGrants,
        "grants:revoke" => RevokeGrant,
        "stats:read" => ReadStats,
        "query-log:read" => ReadQueryLog,
        "reviews:read" => ReadReviews,
        "sync-conflicts:read" => ReadSyncConflicts,
        _ => return None,
    })
}

#[cfg(test)]
fn admitted_proof(parts: &Parts, peer: SocketAddr, bound: SocketAddr) -> V1Result<&str> {
    owner_request::admit_selected(parts, peer, bound)?;
    let proof = parts
        .headers
        .get(PROOF)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(unauthorized)?;
    Ok(proof)
}

#[cfg(test)]
async fn collect_guarded(
    parts: &Parts,
    peer: SocketAddr,
    bound: SocketAddr,
    body: Body,
    proof_is_live: impl FnOnce(&str) -> bool,
) -> Result<axum::body::Bytes, Response> {
    let proof = admitted_proof(parts, peer, bound).map_err(IntoResponse::into_response)?;
    if !proof_is_live(proof) {
        return Err(unauthorized().into_response());
    }
    collect_body_before(body, Instant::now() + HANDSHAKE_BODY_TIMEOUT).await
}

async fn collect_body_before(body: Body, deadline: Instant) -> Result<axum::body::Bytes, Response> {
    // Route-local collection is the FIRST consumer of the body. Mount this
    // handler outside any general middleware that buffers a request body.
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        axum::body::to_bytes(body, MAX_HANDSHAKE_BODY),
    )
    .await
    {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) => Err(super::close_connection(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_body_limit",
            "Request body exceeds handshake limit",
        ))),
        Err(_) => Err(super::close_connection(Problem::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
            "Handshake request body incomplete",
        ))),
    }
}

/// Admission and redemption are distinct so that owner/identity state can
/// change while the body is read without retaining locks across an await.
#[cfg(test)]
async fn exchange_after_preflight(
    parts: &Parts,
    peer: SocketAddr,
    bound: SocketAddr,
    body: Body,
    proof_is_live: impl FnOnce(&str) -> bool,
    redeem_with_live_owner: impl FnOnce(&[u8]) -> V1Result<Response>,
) -> Response {
    match collect_guarded(parts, peer, bound, body, proof_is_live).await {
        Ok(bytes) => match redeem_with_live_owner(&bytes) {
            Ok(response) => response,
            Err(problem) => problem.into_response(),
        },
        Err(response) => response,
    }
}

fn fenced_sample(
    deadline: Instant,
    monotonic: impl FnOnce() -> Instant,
    utc: impl FnOnce() -> Option<u64>,
) -> V1Result<(u64, Instant)> {
    let wall = utc().ok_or_else(unauthorized)?;
    let now = monotonic();
    if now >= deadline {
        return Err(Problem::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
            "Handshake request deadline elapsed",
        ));
    }
    Ok((wall, now))
}

fn unauthorized() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Authentication required",
    )
}

fn invalid() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid handshake request",
    )
}

#[cfg(test)]
mod tests;
