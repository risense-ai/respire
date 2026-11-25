//! Retirement of a verified inactive device-owned space.
use super::{authority_changed, not_found};
use super::super::{rest_dispatch, transport, Problem, V1Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use respire_app::auth::DeviceSpaceRetirement;
use std::net::SocketAddr;

pub(super) async fn handle(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::DELETE {
        return Err(super::super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    transport::json_optional::<serde_json::Value>(body, 0).await?;
    let raw = parts.uri.path().strip_prefix("/v1/spaces/").ok_or_else(not_found)?;
    let id = transport::decode(raw, false).map_err(|_| not_found())?;
    rest_dispatch::submit_device_space_delete(scope, move |ctx, admission| {
        let result = ctx.library.delete_device_space(
            ctx.ownership, &id, || ctx.may_commit() && admission.admit(),
        ).map_err(|error| {
            if error.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
            {
                Problem::new(StatusCode::CONFLICT, "space_retirement_quarantine_full",
                    "Local space retirement quarantine is full; no data was removed")
            } else if authority_changed(&error) && !ctx.scope.bearer_is_current() {
                transport::unauthorized()
            } else {
                transport::unavailable()
            }
        })?;
        match result {
            DeviceSpaceRetirement::Missing => Err(not_found()),
            DeviceSpaceRetirement::Active => Err(Problem::new(
                StatusCode::CONFLICT, "space_is_active", "Restore the original space before deleting",
            )),
            DeviceSpaceRetirement::Retired => Ok(()),
        }
    }).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
