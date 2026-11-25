//! Wire-only extraction. Domain code sees typed request values, never argv.
use axum::{
    body::{self, Body},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use super::{Problem, V1Result};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct RestScope {
    pub(super) account: String,
    pub(super) canonical_profile: PathBuf,
    pub(super) instance: uuid::Uuid,
    pub(super) generation: u64,
    pub(super) vault_epoch: uuid::Uuid,
    bearer: [u8; 32],
}

/// Existing installation-local credential only, not one of the six scoped
/// leases. Check real peer, raw fields, Host, and Origin before file access.
pub(super) async fn require_local_bearer(
    parts: &axum::http::request::Parts,
    peer: SocketAddr,
    bound: SocketAddr,
) -> V1Result<[u8; 32]> {
    super::owner_request::admit_selected(parts, peer, bound)?;
    let (scheme, presented) = parts.headers.get(header::AUTHORIZATION)
        .and_then(|field| field.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || presented.is_empty()
        || presented.bytes().any(|value| value.is_ascii_whitespace())
    {
        return Err(unauthorized());
    }
    let presented = fingerprint(presented);
    super::owner_control::bounded_blocking(move || {
        let current = current_bearer().ok_or_else(unauthorized)?;
        if equal_fingerprint(&current, &presented) {
            Ok(current)
        } else {
            Err(unauthorized())
        }
    }).await
}

/// Bind the checked local credential to the currently verified unlocked
/// profile and generation; identity is never selected by the HTTP caller.
pub(super) async fn authorize(
    parts: &axum::http::request::Parts,
    peer: SocketAddr,
    bound: SocketAddr,
) -> V1Result<RestScope> {
    let bearer = require_local_bearer(parts, peer, bound).await?;
    super::owner_control::bounded_blocking(move || capture_scope(bearer)).await
}

fn capture_scope(bearer: [u8; 32]) -> V1Result<RestScope> {
    crate::rpc_serve::with_verified_owner_scope(|account, profile, instance, generation, vault_epoch| {
        if !bearer_is_current(&bearer) {
            return Err(unauthorized());
        }
        Ok(RestScope {
            account: account.to_owned(),
            canonical_profile: profile.to_owned(),
            instance,
            generation,
            vault_epoch,
            bearer,
        })
    }).unwrap_or_else(|| Err(unavailable()))
}

pub(super) fn has_live_scope() -> bool {
    current_bearer().is_some_and(|bearer| capture_scope(bearer).is_ok())
}

/// Blocking only, for an async caller to run through the shared bounded
/// worker. Never hold another domain lock while resolving live authority.
pub(super) fn scope_is_live(scope: &RestScope) -> bool {
    with_scope(scope, || Ok(())).is_ok()
}

pub(super) fn with_scope<T>(scope: &RestScope, run: impl FnOnce() -> V1Result<T>) -> V1Result<T> {
    crate::rpc_serve::with_verified_owner_scope(|account, profile, instance, generation, vault_epoch| {
        if !scope.matches(account, profile, instance, generation, vault_epoch) {
            return Err(unauthorized());
        }
        run()
    }).unwrap_or_else(|| Err(unavailable()))
}

impl RestScope {
    pub(super) fn bearer_is_current(&self) -> bool {
        bearer_is_current(&self.bearer)
    }

    #[cfg(test)]
    pub(super) fn receipt_fixture() -> Self {
        Self {
            account: "fixture-owner".to_owned(),
            canonical_profile: PathBuf::from("fixture-profile"),
            instance: uuid::Uuid::new_v4(),
            generation: 1,
            vault_epoch: uuid::Uuid::new_v4(),
            bearer: [1; 32],
        }
    }

    pub(super) fn matches(
        &self,
        account: &str,
        profile: &std::path::Path,
        instance: uuid::Uuid,
        generation: u64,
        vault_epoch: uuid::Uuid,
    ) -> bool {
        self.account == account && self.canonical_profile == profile
            && self.instance == instance && self.generation == generation
            && self.vault_epoch == vault_epoch && self.bearer_is_current()
    }
}

fn fingerprint(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn current_bearer() -> Option<[u8; 32]> {
    crate::net_rpc::read_token().ok().flatten().map(|token| fingerprint(&token))
}

fn equal_fingerprint(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter().zip(right).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

pub(super) fn bearer_is_current(expected: &[u8; 32]) -> bool {
    current_bearer().is_some_and(|actual| equal_fingerprint(&actual, expected))
}

pub(super) fn unauthorized() -> Problem {
    Problem::new(StatusCode::UNAUTHORIZED, "unauthorized", "Authentication required")
        .bearer_challenge()
}

pub(super) fn unavailable() -> Problem {
    Problem::new(StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable", "Runtime unavailable")
}

pub(super) fn invalid() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid request",
    )
}

pub(super) async fn json<T: DeserializeOwned>(body: Body, limit: usize) -> V1Result<T> {
    json_optional(body, limit).await?.ok_or_else(invalid)
}

pub(super) async fn json_optional<T: DeserializeOwned>(
    incoming: Body,
    limit: usize,
) -> V1Result<Option<T>> {
    let bytes = match tokio::time::timeout(crate::network::IO_WAIT, body::to_bytes(incoming, limit.min(super::super::MAX_BODY as usize)))
        .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return Err(Problem::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_body_limit",
                "Request body exceeds route limit",
            ))
        }
        Err(_) => {
            return Err(Problem::new(
                StatusCode::REQUEST_TIMEOUT,
                "request_body_timeout",
                "Request body incomplete",
            ))
        }
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| invalid())
}

pub(super) fn query(raw: Option<&str>) -> V1Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    if let Some(raw) = raw.filter(|value| !value.is_empty()) {
        if raw.len() > 8192 {
            return Err(invalid());
        }
        for pair in raw.split('&') {
            let (name, value) = pair.split_once('=').ok_or_else(invalid)?;
            let name = decode(name, true)?;
            let value = decode(value, true)?;
            if name.is_empty() || fields.insert(name, value).is_some() {
                return Err(invalid());
            }
        }
    }
    Ok(fields)
}

pub(super) fn json_response(status: StatusCode, value: impl Serialize) -> V1Result<Response> {
    let data = serde_json::to_vec(&value).map_err(|_| {
        Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_unavailable",
            "Response unavailable",
        )
    })?;
    Ok((
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(data),
    )
        .into_response())
}

/// Decode exactly once and reject malformed percent escapes and UTF-8.
/// '+' means space in a form query only, not in a path identifier.
pub(super) fn decode(raw: &str, form: bool) -> V1Result<String> {
    let mut output = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    let hex = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    while let Some(byte) = input.next() {
        output.push(match byte {
            b'%' => {
                let high = input.next().and_then(hex).ok_or_else(invalid)?;
                let low = input.next().and_then(hex).ok_or_else(invalid)?;
                (high << 4) | low
            }
            b'+' if form => b' ',
            _ => byte,
        });
    }
    let value = String::from_utf8(output).map_err(|_| invalid())?;
    if value.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(value)
}
