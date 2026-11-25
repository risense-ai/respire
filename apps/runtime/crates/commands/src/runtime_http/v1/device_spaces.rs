use super::{registration::{bind, RouteSpec, Verb}, rest_dispatch::{self, Access}, transport, Problem, V1Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::Response,
    Router,
};
use respire_app::auth::{valid_device_space_name, DeviceSpaceAuthorityChanged};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

const PATH: &str = "/v1/spaces";

pub(super) fn routes(bound: SocketAddr) -> Router {
    let router = bind(Router::new(), RouteSpec::new(Verb::Get, PATH, "listSpaces"), list);
    let router = bind(router, RouteSpec::new(Verb::Post, PATH, "createSpace"), create);
    bind(router, RouteSpec::new(Verb::Get, "/v1/spaces/{spaceId}", "getSpace"), get)
        .method_not_allowed_fallback(super::method_not_allowed)
        .with_state(bound)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSpace {
    name: String,
}

async fn create(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let input: CreateSpace = transport::json(body, 4096).await?;
    if !valid_device_space_name(&input.name) {
        return Err(transport::invalid());
    }
    let created = rest_dispatch::submit_device_space_create(scope, move |ctx, admission| {
        ctx.library.create_device_space(
            ctx.ownership, input.name, || ctx.may_commit(), || admission.admit(),
        )
            .map_err(|error| {
                if authority_changed(&error) && !ctx.scope.bearer_is_current() {
                    transport::unauthorized()
                } else {
                    transport::unavailable()
                }
            })
    }).await?;
    transport::json_response(StatusCode::CREATED, created)
}

async fn list(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    read_request(&parts, body).await?;
    let spaces = rest_dispatch::submit(scope, Access::Read, |ctx| {
        ctx.library.list_device_spaces(ctx.ownership).map_err(|_| transport::unavailable())
    }).await?;
    transport::json_response(StatusCode::OK, json!({"spaces": spaces}))
}

async fn get(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    read_request(&parts, body).await?;
    let id = parts.uri.path().strip_prefix("/v1/spaces/")
        .ok_or_else(not_found)?;
    let id = transport::decode(id, false).map_err(|_| not_found())?;
    let space = rest_dispatch::submit(scope, Access::Read, move |ctx| {
        ctx.library.get_device_space(ctx.ownership, &id).map_err(|_| transport::unavailable())?
            .ok_or_else(not_found)
    }).await?;
    transport::json_response(StatusCode::OK, space)
}

async fn read_request(
    parts: &axum::http::request::Parts,
    body: axum::body::Body,
) -> V1Result<()> {
    if parts.method != Method::GET {
        return Err(super::method_not_allowed().await);
    }
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    transport::json_optional::<serde_json::Value>(body, 0).await?;
    Ok(())
}

fn not_found() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "space_not_found", "Space not found")
}

fn authority_changed(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<DeviceSpaceAuthorityChanged>()
            || cause.downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .is_some_and(|inner| inner.is::<DeviceSpaceAuthorityChanged>())
    })
}

#[cfg(test)]
#[test]
fn preserves_late_revocation_across_io_boundary() {
    let marker = || std::io::Error::new(
        std::io::ErrorKind::PermissionDenied, DeviceSpaceAuthorityChanged,
    );
    assert!(authority_changed(&anyhow::Error::from(marker()).context("native publication")));
    assert!(authority_changed(&DeviceSpaceAuthorityChanged.into()));
    let unrelated = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    assert!(!authority_changed(&unrelated.into()));
}