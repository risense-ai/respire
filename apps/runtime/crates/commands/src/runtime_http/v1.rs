use axum::{
    body::{self, Body},
    extract::{ConnectInfo, Request, State},
    http::{self, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use std::net::SocketAddr;

mod compatibility;
mod diary;
mod host_config;
mod host_config_patch;
mod models;
pub(crate) mod operations;
pub(super) mod owner_control;
mod owner_handshake;
mod owner_native;
mod owner_operations;
mod owner_request;
mod owner_routes;
mod owner_scope;
mod private_model_install;
mod private_model_route;
mod problem;
mod reads;
mod recall;
mod registration;
mod rest_dispatch;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod device_spaces;
mod transport;
mod writes;
#[cfg(test)]
mod route_parity;
use registration::{bind, RouteSpec, Verb};
use problem::{Problem, V1Result};

#[derive(Clone)]
pub(super) struct V1State {
    bound: SocketAddr,
}

pub(super) fn router(bound: SocketAddr) -> Router {
    let state = V1State { bound };
    const HEALTH: RouteSpec = RouteSpec::new(Verb::Get, "/v1/health", "getHealth");
    let router = bind(Router::new(), HEALTH, health)
        // Explicit routes are merged here by operation-owning modules.
        .route("/v1", any(not_found))
        .route("/v1/", any(not_found))
        .route("/v1/{*unmatched}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state.clone(), admit))
        .with_state(state)
        .merge(owner_handshake::routes(bound))
        .merge(owner_routes::routes(bound))
        .merge(private_model_route::routes(bound))
        .merge(private_model_install::routes(bound))
        .merge(reads::routes(bound))
        .merge(host_config::routes(bound))
        .merge(recall::routes(bound))
        .merge(diary::routes(bound))
        .merge(writes::routes(bound))
        .merge(operations::routes(bound));
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    let router = router.merge(device_spaces::routes(bound));
    router
}

async fn health() -> V1Result<Response> {
    let (identity, scoped_auth, operations) = owner_control::bounded_blocking(|| {
        let identity = crate::rpc_serve::v1_health_identity();
        let ready = matches!(identity, Some(("ready", _)));
        Ok((
            identity,
            ready && owner_control::capable(),
            ready && owner_control::dispatch_live() && transport::has_live_scope(),
        ))
    })
    .await?;
    let Some((status, instance_id)) = identity else {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "Runtime unavailable",
        ));
    };
    let payload = serde_json::json!({
        "status": status,
        "instance_id": instance_id,
        "capabilities": health_capabilities(status, scoped_auth, operations)
    });
    Ok((
        [(http::header::CONTENT_TYPE, "application/json")],
        payload.to_string(),
    )
        .into_response())
}

fn health_capabilities(
    status: &str,
    scoped_auth: bool,
    dispatch_and_scope_live: bool,
) -> Vec<&'static str> {
    if status != "ready" {
        return Vec::new();
    }
    let mut flags = Vec::new();
    if dispatch_and_scope_live {
        flags.extend(["sse.operations.v1", "model.catalog.v2"]);
    }
    if scoped_auth {
        flags.push("auth.handshake.v1");
    }
    // Active-assets has a separate authenticated client migration; managed
    // download must not promise eligibility beyond the managed CPU lane.
    flags
}

async fn not_found() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "route_not_found", "Route not found")
}

async fn method_not_allowed() -> Problem {
    Problem::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method not allowed",
    )
}

async fn admit(
    State(state): State<V1State>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
    next: Next,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    owner_request::validate(&parts, peer)?;
    let request = super::request_metadata(&parts, peer).ok_or_else(|| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_method",
            "Invalid HTTP method",
        )
    })?;
    if let Err(denial) = super::authorize(&request, state.bound) {
        return Err(match denial {
            super::Denied::Token => Problem::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication required",
            ),
            super::Denied::Host => {
                Problem::new(StatusCode::FORBIDDEN, "forbidden_host", "Forbidden host")
            }
            super::Denied::Origin => Problem::new(
                StatusCode::FORBIDDEN,
                "forbidden_origin",
                "Forbidden origin",
            ),
        });
    }
    let result = tokio::time::timeout(
        crate::network::IO_WAIT,
        body::to_bytes(body, super::MAX_BODY as usize),
    )
    .await;
    let body = match result {
        Ok(Ok(bytes)) => Body::from(bytes),
        Ok(Err(_)) => {
            return Ok(close_connection(Problem::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_body_limit",
                "Request body exceeds runtime frame limit",
            )))
        }
        Err(_) => {
            return Ok(close_connection(Problem::new(
                StatusCode::REQUEST_TIMEOUT,
                "request_body_timeout",
                "Request body incomplete",
            )))
        }
    };
    Ok(next.run(Request::from_parts(parts, body)).await)
}

fn close_connection(problem: Problem) -> Response {
    let mut response = problem.into_response();
    response.headers_mut().insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("close"),
    );
    response
}
