//! Owner endpoints are composed only by the capability-gated v1 router.
use axum::{
    body::{self, Body},
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode, Uri},
    response::Response,
    Router,
};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use super::{
    compatibility::full_grant_id,
    owner_control::{admit_action, bounded_blocking, submit},
    owner_operations::OwnerCall,
    owner_scope::OwnerAction,
    problem::{Problem, V1Result},
};

pub(super) fn routes(bound: SocketAddr) -> Router {
    use super::registration::{bind, RouteSpec, Verb};
    let router = bind(Router::new(), RouteSpec::new(Verb::Get, "/v1/grants", "listAccessGrants"), list_grants);
    let router = bind(router, RouteSpec::new(Verb::Delete, "/v1/grants/{grantId}", "revokeAccessGrant"), revoke);
    let router = bind(router, RouteSpec::new(Verb::Get, "/v1/stats", "getLibraryStats"), stats);
    let router = bind(router, RouteSpec::new(Verb::Get, "/v1/query-log", "getQueryLog"), query_log);
    let router = bind(router, RouteSpec::new(Verb::Get, "/v1/memories/{memoryId}/reviews", "listMemoryReviews"), reviews);
    bind(router, RouteSpec::new(Verb::Get, "/v1/sync/conflicts", "listSyncConflicts"), conflicts)
        .method_not_allowed_fallback(super::method_not_allowed)
        .with_state(bound)
}

#[derive(Clone, Copy)]
enum Endpoint {
    Grants,
    Revoke,
    Stats,
    QueryLog,
    Reviews,
    Conflicts,
}

impl Endpoint {
    fn action(self) -> OwnerAction {
        match self {
            Self::Grants => OwnerAction::ReadGrants,
            Self::Revoke => OwnerAction::RevokeGrant,
            Self::Stats => OwnerAction::ReadStats,
            Self::QueryLog => OwnerAction::ReadQueryLog,
            Self::Reviews => OwnerAction::ReadReviews,
            Self::Conflicts => OwnerAction::ReadSyncConflicts,
        }
    }

    fn method(self) -> Method {
        match self {
            Self::Revoke => Method::DELETE,
            _ => Method::GET,
        }
    }

    fn parse(self, uri: &Uri) -> V1Result<OwnerCall> {
        let mut args = Arguments::default();
        if let Some(query) = uri.query().filter(|query| !query.is_empty()) {
            for segment in query.split('&') {
                let (key, value) = segment.split_once('=').ok_or_else(invalid)?;
                let key = decode(key, true)?;
                let value = decode(value, true)?;
                match (self, key.as_str()) {
                    (Self::QueryLog | Self::Reviews, "limit") if args.limit.is_none() => {
                        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                            return Err(invalid());
                        }
                        let limit = value.parse::<usize>().map_err(|_| invalid())?;
                        if !(1..=200).contains(&limit) {
                            return Err(invalid());
                        }
                        args.limit = Some(limit);
                    }
                    (Self::Conflicts, "memory_id") if args.memory_id.is_none() => {
                        if value.is_empty() {
                            return Err(invalid());
                        }
                        args.memory_id = Some(value);
                    }
                    (Self::Conflicts, "include_processed") if args.include_processed.is_none() => {
                        args.include_processed = Some(match value.as_str() {
                            "true" => true,
                            "false" => false,
                            _ => return Err(invalid()),
                        });
                    }
                    _ => return Err(invalid()),
                }
            }
        }
        Ok(match self {
            Self::Grants => OwnerCall::ListGrants,
            Self::Revoke => OwnerCall::RevokeGrant {
                grant_id: full_grant_id(&segment(uri, "/v1/grants/", "")?)
                    .map_err(|_| invalid())?,
            },
            Self::Stats => OwnerCall::Stats,
            Self::QueryLog => OwnerCall::QueryLog { limit: args.limit },
            Self::Reviews => OwnerCall::Reviews {
                memory_id: segment(uri, "/v1/memories/", "/reviews")?,
                limit: args.limit,
            },
            Self::Conflicts => OwnerCall::Conflicts {
                memory_id: args.memory_id,
                include_processed: args.include_processed.unwrap_or(false),
            },
        })
    }
}

#[derive(Default)]
struct Arguments {
    limit: Option<usize>,
    memory_id: Option<String>,
    include_processed: Option<bool>,
}

fn invalid() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid request",
    )
}

fn segment(uri: &Uri, prefix: &str, suffix: &str) -> V1Result<String> {
    let raw = uri
        .path()
        .strip_prefix(prefix)
        .and_then(|remaining| remaining.strip_suffix(suffix))
        .filter(|raw| !raw.is_empty() && !raw.contains('/'))
        .ok_or_else(invalid)?;
    decode(raw, false)
}

use super::transport::decode;

async fn handle(
    endpoint: Endpoint,
    bound: SocketAddr,
    peer: SocketAddr,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    // Axum can dispatch HEAD through a GET method router. It is not part of
    // these contracts and must not be admitted to the dispatcher.
    if parts.method != endpoint.method() {
        return Err(super::method_not_allowed().await);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    super::owner_request::admit_selected(&parts, peer, bound)?;
    let expires = tokio::time::Instant::from_std(deadline);

    // Hold a shared permit while checking native ownership and library state.
    // Nothing from the body is read until the action-specific ticket is issued.
    let (parts, ticket) = tokio::time::timeout_at(
        expires,
        bounded_blocking(move || {
            let ticket = admit_action(&parts, endpoint.action())?;
            Ok((parts, ticket))
        }),
    )
    .await
    .map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "Runtime unavailable",
        )
    })??;

    let call = endpoint.parse(&parts.uri)?;
    let body_expires = expires.min(tokio::time::Instant::now() + crate::network::IO_WAIT);
    match tokio::time::timeout_at(body_expires, body::to_bytes(body, 0)).await {
        Ok(Ok(_)) => submit(call, ticket, deadline).await,
        Ok(Err(_)) => Err(invalid()),
        Err(_) => Err(Problem::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
            "Request body incomplete",
        )),
    }
}

macro_rules! handler {
    ($name:ident, $endpoint:ident) => {
        async fn $name(
            State(bound): State<SocketAddr>,
            ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
            request: Request<Body>,
        ) -> V1Result<Response> {
            handle(Endpoint::$endpoint, bound, peer, request).await
        }
    };
}

handler!(list_grants, Grants);
handler!(revoke, Revoke);
handler!(stats, Stats);
handler!(query_log, QueryLog);
handler!(reviews, Reviews);
handler!(conflicts, Conflicts);

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(endpoint: Endpoint, raw: &str) -> V1Result<OwnerCall> {
        endpoint.parse(&raw.parse::<Uri>().expect("synthetic HTTP URI"))
    }

    #[test]
    fn canonical_action_and_exact_revoke_target() {
        for (endpoint, uri) in [
            (Endpoint::Grants, "/v1/grants"),
            (
                Endpoint::Revoke,
                "/v1/grants/00000000-0000-4000-8000-000000000001",
            ),
            (Endpoint::Stats, "/v1/stats"),
            (Endpoint::QueryLog, "/v1/query-log"),
            (Endpoint::Reviews, "/v1/memories/example/reviews"),
            (Endpoint::Conflicts, "/v1/sync/conflicts"),
        ] {
            let call = parse(endpoint, uri).expect("canonical owner request");
            assert_eq!(endpoint.action(), call.action());
        }
        assert!(matches!(
            parse(Endpoint::Revoke, "/v1/grants/00000000-0000-4000-8000-00000000000A"),
            Ok(OwnerCall::RevokeGrant { grant_id }) if grant_id.ends_with('a')
        ));
        for path in [
            "/v1/grants/00000000",
            "/v1/grants/00000000%25",
            "/v1/grants/00000000-0000-4000-8000-000000000001?other=1",
        ] {
            assert!(parse(Endpoint::Revoke, path).is_err());
        }
    }

    #[test]
    fn bounds_duplicates_and_strict_decoding() {
        assert!(matches!(
            parse(Endpoint::Reviews, "/v1/memories/id+%252f/reviews?limit=200"),
            Ok(OwnerCall::Reviews { memory_id, limit: Some(200) }) if memory_id == "id+%2f"
        ));
        assert!(matches!(
            parse(Endpoint::Conflicts, "/v1/sync/conflicts?memory_id=a+b&include_processed=true"),
            Ok(OwnerCall::Conflicts { memory_id: Some(memory), include_processed: true }) if memory == "a b"
        ));
        for query in [
            "limit=0",
            "limit=201",
            "limit=",
            "limit=%ff",
            "limit=1&%6cimit=2",
            "limit=1&",
            "wrong=1",
            "limit=%3",
            "limit=1%00",
            "limit=%2B1",
        ] {
            assert!(parse(Endpoint::QueryLog, &format!("/v1/query-log?{query}")).is_err());
        }
        assert!(parse(
            Endpoint::Conflicts,
            "/v1/sync/conflicts?include_processed=true&include_processed=false"
        )
        .is_err());
        assert!(parse(Endpoint::Reviews, "/v1/memories/%ff/reviews").is_err());
    }
}
