//! Native-only model ingress. Bytes stream to Runtime-owned handles off the
//! serving worker; the owner and library locks are held only around transitions.
use super::{
    owner_control::model_work,
    owner_request,
    private_model_route::{bad_request, verify_headers},
    problem::{Problem, V1Result},
};
use axum::{
    body::{Body, Bytes, HttpBody},
    extract::{ConnectInfo, Request},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use std::{future::poll_fn, net::SocketAddr, pin::Pin, time::Duration};

const PATH: &str = "/api/runtime/private-model/install";
const TRANSFER_LIMIT: Duration = Duration::from_secs(180);
const MAX_TRANSFER_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(super) fn routes(bound: SocketAddr) -> Router {
    Router::new().route(
        PATH,
        post(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| { install(request, peer, bound) },
        )
        .head(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| async move {
                let (parts, _) = request.into_parts();
                if parts.uri.query().is_some()
                    || parts.headers.contains_key(header::COOKIE)
                    || parts.headers.contains_key(header::ORIGIN)
                {
                    return Err(Problem::new(
                        StatusCode::FORBIDDEN,
                        "native_model_control_required",
                        "Native model control required",
                    ));
                }
                owner_request::admit_selected(&parts, peer, bound)?;
                Ok((
                    StatusCode::NO_CONTENT,
                    [("x-respire-native-model-install", "pinned-pair-v1")],
                ))
            },
        ),
    )
}

async fn install(request: Request, peer: SocketAddr, bound: SocketAddr) -> Response {
    let result = async {
        let (parts, body) = request.into_parts();
        let proof = verify_headers(&parts, peer, bound)?;
        let model = owner_request::single(&parts.headers, "x-respire-model-id")
            .ok()
            .flatten()
            .ok_or_else(bad_request)?
            .to_owned();
        let content_type = owner_request::single(&parts.headers, header::CONTENT_TYPE.as_str())
            .ok()
            .flatten();
        if content_type != Some("application/octet-stream")
            || parts.headers.contains_key(header::CONTENT_ENCODING)
            || parts.headers.contains_key(header::TRANSFER_ENCODING)
        {
            return Err(bad_request());
        }
        let declared = owner_request::single(&parts.headers, header::CONTENT_LENGTH.as_str())
            .ok()
            .flatten()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|length| (1..=MAX_TRANSFER_BYTES).contains(length))
            .ok_or_else(bad_request)?;
        let upload = model_work::prepare_upload(model.clone(), proof).await?;
        let (weights, tokenizer) = upload.transfer.pinned_lengths();
        if weights.checked_add(tokenizer) != Some(declared) {
            return Err(bad_request());
        }
        let sealed =
            tokio::time::timeout(TRANSFER_LIMIT, receive(body, upload, weights, tokenizer))
                .await
                .map_err(|_| {
                    Problem::new(
                        StatusCode::REQUEST_TIMEOUT,
                        "model_transfer_timeout",
                        "Model transfer timed out",
                    )
                })??;
        model_work::publish_upload(sealed).await?;
        let output = serde_json::json!({ "model_id": model, "outcome": "installed" });
        Ok::<_, Problem>(
            (
                StatusCode::CREATED,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                output.to_string(),
            )
                .into_response(),
        )
    }
    .await;
    let mut response = result.into_response();
    // Both fixed Problem Details and our success receipt have an exact length.
    // Keep this one native response explicitly bounded and unchunked even
    // when Hyper returns an admission error before it has polled the upload.
    if let Some(length) = response.body().size_hint().exact() {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    response
}

async fn receive(
    mut body: Body,
    mut upload: model_work::Upload,
    mut weights: u64,
    mut tokenizer: u64,
) -> V1Result<model_work::SealedUpload> {
    let (send, mut pending) = tokio::sync::mpsc::channel::<Bytes>(2);
    let sink = tokio::task::spawn_blocking(move || {
        while let Some(frame) = pending.blocking_recv() {
            let weight_len =
                usize::try_from(weights.min(frame.len() as u64)).map_err(|_| bad_request())?;
            let (weight_bytes, tokenizer_bytes) = frame.split_at(weight_len);
            let tokenizer_len = tokenizer_bytes.len() as u64;
            if tokenizer_len > tokenizer {
                return Err(bad_request());
            }
            upload
                .transfer
                .write_weights(weight_bytes)
                .map_err(|_| bad_request())?;
            upload
                .transfer
                .write_tokenizer(tokenizer_bytes)
                .map_err(|_| bad_request())?;
            weights -= weight_len as u64;
            tokenizer -= tokenizer_len;
        }
        if weights != 0 || tokenizer != 0 {
            return Err(bad_request());
        }
        upload.seal()
    });
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let bytes = frame
            .map_err(|_| bad_request())?
            .into_data()
            .map_err(|_| bad_request())?;
        send.send(bytes).await.map_err(|_| bad_request())?;
    }
    drop(send);
    sink.await.map_err(|_| bad_request())?
}
