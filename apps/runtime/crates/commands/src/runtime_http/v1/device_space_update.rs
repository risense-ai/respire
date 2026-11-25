//! Device-local metadata changes and reversible typed default selection.
use super::{not_found, authority_changed};
use super::super::{reads, rest_dispatch, transport, Problem, V1Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::Response,
};
use respire_app::auth::{valid_device_space_name, DeviceSpacePatch};
use serde_json::json;
use std::{io::Read, net::SocketAddr};

pub(super) async fn handle(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::PATCH {
        return Err(super::super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let patch: DeviceSpacePatch = transport::json(body, 4096).await?;
    if !patch.name.as_deref().is_none_or(valid_device_space_name) {
        return Err(transport::invalid());
    }
    let raw = parts.uri.path().strip_prefix("/v1/spaces/").ok_or_else(not_found)?;
    let id = transport::decode(raw, false).map_err(|_| not_found())?;
    let result = rest_dispatch::submit_device_space_update(scope, move |ctx, admission| {
        let commit = || ctx.may_commit() && admission.admit();
        let convert = |error: anyhow::Error| {
            if authority_changed(&error) && !ctx.scope.bearer_is_current() {
                transport::unauthorized()
            } else {
                transport::unavailable()
            }
        };
        if id == "main" {
            if patch.name.is_some() || patch.active != Some(true) {
                return Err(original_unavailable());
            }
            verify_original(ctx)?;
            ctx.library.restore_original_space(ctx.ownership, commit).map_err(convert)?;
            return Ok(json!({"id":"main", "name":"main", "role":"owner", "active":true}));
        }
        if patch.active == Some(true) {
            // Do not select a child unless this serving profile can be
            // restored without inventing an unproved legacy member role.
            verify_original(ctx)?;
        }
        let space = ctx.library.patch_device_space(ctx.ownership, &id, patch, commit)
            .map_err(convert)?.ok_or_else(not_found)?;
        serde_json::to_value(space).map_err(|_| transport::unavailable())
    }).await?;
    transport::json_response(StatusCode::OK, result)
}

fn original_unavailable() -> Problem {
    Problem::new(
        StatusCode::CONFLICT, "original_space_not_verified",
        "Original space cannot be verified for reversible device selection",
    )
}

fn verify_original(ctx: &rest_dispatch::LibraryContext<'_>) -> V1Result<()> {
    if reads::space_id(ctx)? != "main" || respire_app::sync::remote_configured() {
        return Err(original_unavailable());
    }
    let path = ctx.scope.canonical_profile.join("session.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(original_unavailable()),
    };
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(original_unavailable());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(original_unavailable());
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|_| original_unavailable())?
        .take(64 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| original_unavailable())?;
    if bytes.len() > 64 * 1024 {
        return Err(original_unavailable());
    }
    let session: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| original_unavailable())?;
    let fields = session.as_object().ok_or_else(original_unavailable)?;
    for name in ["addr", "token", "space", "space_session_id", "readonly"] {
        if fields.get(name).is_some_and(|value| {
            !value.is_null() && value.as_str() != Some("") && value.as_bool() != Some(false)
        }) {
            return Err(original_unavailable());
        }
    }
    Ok(())
}
