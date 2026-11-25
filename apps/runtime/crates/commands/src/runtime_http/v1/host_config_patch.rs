use super::host_config::{view, ModelNetwork};
use super::{rest_dispatch, transport, V1Result};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::Response,
};
use respire_app::service;
use serde_json::Value;
use std::net::SocketAddr;

pub(super) async fn patch(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let input: Value = transport::json(body, 8192).await?;
    let fields = input.as_object().ok_or_else(transport::invalid)?;
    for (key, value) in fields {
        let valid = match key.as_str() {
            "language" => matches!(value.as_str(), Some("en" | "zh")),
            "diary_mode" => matches!(value.as_str(), Some("concise" | "verbose")),
            "model_mirror" => value.is_string(),
            "proxy_url" => value.is_null() || value.is_string(),
            _ => false,
        };
        if !valid {
            return Err(transport::invalid());
        }
    }
    ModelNetwork::parse(&input).map_err(|_| transport::invalid())?;
    let config = rest_dispatch::submit(scope, rest_dispatch::Access::Write, move |ctx| {
        let mut transaction = service::HostConfigTransaction::begin(ctx.ownership)
            .map_err(|_| transport::unavailable())?;
        view(&transaction.client, &transaction.agent).map_err(|_| transport::unavailable())?;
        for key in ["language", "model_mirror", "proxy_url"] {
            if let Some(value) = input.get(key) {
                transaction.client[if key == "language" { "lang" } else { key }] = value.clone();
            }
        }
        if let Some(value) = input.get("diary_mode") {
            transaction.agent["diary_mode"] = value.clone();
        }
        let config =
            view(&transaction.client, &transaction.agent).map_err(|_| transport::invalid())?;
        transaction.commit().map_err(|_| transport::unavailable())?;
        Ok(config)
    })
    .await?;
    transport::json_response(StatusCode::OK, config)
}
