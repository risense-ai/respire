//! Reject ambiguous security fields before the shared legacy metadata projection.
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use std::net::SocketAddr;

use super::problem::{Problem, V1Result};

const HANDSHAKE: &str = "/v1/auth/handshake";
const PROOF: &str = "x-respire-handshake-proof";

fn owner_path(path: &str) -> bool {
    matches!(
        path,
        HANDSHAKE | "/v1/grants" | "/v1/stats" | "/v1/query-log" | "/v1/sync/conflicts"
    ) || path.strip_prefix("/v1/grants/").is_some_and(one_segment)
        || path
            .strip_prefix("/v1/memories/")
            .and_then(|rest| rest.strip_suffix("/reviews"))
            .is_some_and(one_segment)
}

fn one_segment(value: &str) -> bool {
    !value.is_empty() && !value.contains('/')
}

pub(super) fn validate(parts: &Parts, peer: SocketAddr) -> V1Result<()> {
    let path = parts.uri.path();
    if !owner_path(path) {
        return Ok(());
    }
    validate_fields(parts, peer)
}

/// Bound to each owner action by registration, not by a separately decoded URI.
pub(super) fn admit_selected(parts: &Parts, peer: SocketAddr, bound: SocketAddr) -> V1Result<()> {
    validate_fields(parts, peer)?;
    let request = super::super::request_metadata(parts, peer).ok_or_else(|| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_method",
            "Invalid HTTP method",
        )
    })?;
    super::super::authorize(&request, bound).map_err(|denial| match denial {
        super::super::Denied::Token => Problem::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Authentication required",
        ),
        super::super::Denied::Host => {
            Problem::new(StatusCode::FORBIDDEN, "forbidden_host", "Forbidden host")
        }
        super::super::Denied::Origin => Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Forbidden origin",
        ),
    })
}

fn validate_fields(parts: &Parts, peer: SocketAddr) -> V1Result<()> {
    let path = parts.uri.path();
    let forbidden = |code, title| Problem::new(StatusCode::FORBIDDEN, code, title);
    if !peer.ip().is_loopback() {
        return Err(forbidden(
            "loopback_required",
            "Loopback connection required",
        ));
    }
    if !matches!(single(&parts.headers, header::HOST.as_str()), Ok(Some(_))) {
        return Err(forbidden("forbidden_host", "Forbidden host"));
    }
    if single(&parts.headers, header::ORIGIN.as_str()).is_err() {
        return Err(forbidden("forbidden_origin", "Forbidden origin"));
    }
    let auth = single(&parts.headers, header::AUTHORIZATION.as_str());
    let proof = single(&parts.headers, PROOF);
    if auth.is_err()
        || proof.is_err()
        || (path == HANDSHAKE
            && (parts.uri.query().is_some() || parts.headers.contains_key(header::COOKIE)))
    {
        let problem = Problem::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Authentication required",
        );
        return Err(if path == HANDSHAKE {
            problem
        } else {
            problem.bearer_challenge()
        });
    }
    Ok(())
}

/// Do not lose invalid fields or choose an arbitrary duplicate. Absent
/// Origin remains distinguishable from a provided empty or malformed Origin.
pub(super) fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    if value.is_empty()
        || value.trim() != value
        || value.contains(',')
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return Err(());
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, Request};

    #[test]
    fn owner_gate_rejects_ambiguous_and_nonlocal_fields_before_conversion() {
        let peer = SocketAddr::from(([127, 0, 0, 1], 1));
        for name in ["host", "origin", "authorization", PROOF] {
            let (mut parts, _) = Request::builder()
                .uri(HANDSHAKE)
                .header("host", "127.0.0.1:1234")
                .body(())
                .expect("synthetic request")
                .into_parts();
            parts
                .headers
                .insert(name, HeaderValue::from_static("synthetic"));
            assert!(validate(&parts, peer).is_ok());
            parts
                .headers
                .append(name, HeaderValue::from_static("synthetic"));
            assert!(validate(&parts, peer).is_err());
            parts.headers.insert(
                name,
                HeaderValue::from_bytes(b"\xff").expect("valid header wire bytes"),
            );
            assert!(validate(&parts, peer).is_err());
        }
        let (parts, _) = Request::builder()
            .uri(HANDSHAKE)
            .header("host", "127.0.0.1:1234")
            .body(())
            .expect("synthetic request")
            .into_parts();
        assert!(validate(&parts, peer).is_ok());
        assert!(validate(&parts, SocketAddr::from(([192, 0, 2, 1], 1))).is_err());

        let (parts, _) = Request::builder()
            .uri(format!("{HANDSHAKE}?fixture=1"))
            .header("host", "127.0.0.1:1234")
            .body(())
            .expect("synthetic request")
            .into_parts();
        assert!(validate(&parts, peer).is_err());
    }
}
