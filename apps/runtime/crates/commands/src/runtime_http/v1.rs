use axum::{
    body::{self, Body},
    extract::{ConnectInfo, Request, State},
    http::{self, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use std::net::SocketAddr;

mod compatibility;
pub(super) mod owner_control;
mod owner_handshake;
mod owner_native;
mod owner_operations;
mod owner_request;
mod owner_routes;
mod owner_scope;
mod problem;
use problem::{Problem, V1Result};

#[derive(Clone)]
pub(super) struct V1State {
    bound: SocketAddr,
}

pub(super) fn router(bound: SocketAddr) -> Router {
    let state = V1State { bound };
    Router::new()
        // Explicit routes are merged here by operation-owning modules.
        .route("/v1/health", get(health))
        .route("/v1", any(not_found))
        .route("/v1/", any(not_found))
        .route("/v1/{*unmatched}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state.clone(), admit))
        .with_state(state)
        .merge(owner_handshake::routes(bound))
        .merge(owner_routes::routes(bound))
}

async fn health() -> V1Result<Response> {
    let (identity, scoped_auth) = owner_control::bounded_blocking(|| {
        Ok((
            crate::rpc_serve::v1_health_identity(),
            owner_control::capable(),
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
        "capabilities": if scoped_auth { &["auth.handshake.v1"][..] } else { &[] }
    });
    Ok((
        [(http::header::CONTENT_TYPE, "application/json")],
        payload.to_string(),
    )
        .into_response())
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
