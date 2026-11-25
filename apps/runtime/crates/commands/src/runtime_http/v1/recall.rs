use super::{
    reads,
    registration::{bind, RouteSpec, Verb},
    rest_dispatch::{self, LibraryContext},
    transport, V1Result,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::Response,
    Router,
};
use respire_app::{
    core_sdk::{self, profiles},
    memory::bge::BgeEmbedder,
    MemoryEntry, MemoryQuery,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

pub(super) fn routes(bound: SocketAddr) -> Router {
    bind(
        Router::new(),
        RouteSpec::new(Verb::Post, "/v1/memories/recall", "recallMemories"),
        handle,
    )
    .method_not_allowed_fallback(super::method_not_allowed)
    .with_state(bound)
}

#[derive(Deserialize)]
struct Recall {
    query: String,
    space_id: Option<String>,
    top_k: Option<usize>,
}

async fn handle(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let input = transport::json::<Recall>(body, super::super::MAX_BODY as usize).await?;
    let limit = input.top_k.unwrap_or(10);
    if input.query.trim().is_empty() || !(1..=100).contains(&limit) {
        return Err(transport::invalid());
    }
    let query = MemoryQuery::new(&input.query).limit(limit);
    let hits = rest_dispatch::submit_recall(scope.clone(), move |ctx| {
        reads::check_space(ctx, input.space_id)?;
        rank(ctx, &query).map_err(|_| transport::unavailable())
    })
    .await?;
    super::owner_control::bounded_blocking(move || transport::with_scope(&scope, || Ok(())))
        .await?;
    transport::json_response(
        StatusCode::OK,
        json!({"hits": hits.into_iter().map(|(score, entry)| {
        json!({"memory": reads::item(entry), "score": score})
    }).collect::<Vec<_>>()}),
    )
}

fn rank(ctx: &LibraryContext<'_>, query: &MemoryQuery) -> anyhow::Result<Vec<(f32, MemoryEntry)>> {
    let store = ctx.store;
    let selection = store.active_selection()?;
    let run = || {
        anyhow::ensure!(
            profiles::current()?.is_none_or(|selection| {
                matches!(selection.backend_profile.backend.as_str(), "onnx" | "mlx")
            }),
            "recall requires a local embedding backend"
        );
        ctx.library
            .with_recall_review(store, &ctx.scope.account, |review, source| {
                if source.entries().next().is_none() {
                    return Ok(Vec::new());
                }
                if source
                    .memories()
                    .iter()
                    .any(|row| !row.deleted && row.local_artifact.is_empty())
                {
                    crate::rpc_worker::kick_index();
                }
                let embedder = BgeEmbedder::load_model(&store.retrieval_model()?)?;
                if source.has_metadata() {
                    crate::recall_select::require_metadata(false, None)?;
                    let (items, ..) = crate::recall_select::recall_metadata(
                        review,
                        source,
                        &embedder,
                        query,
                        store,
                        None,
                        None,
                        "auto",
                        Some("fast"),
                    )?;
                    Ok(items
                        .into_iter()
                        .map(|item| (item.score, item.entry))
                        .collect())
                } else {
                    core_sdk::query(&embedder, &[], query, "scored")
                }
            })
    };
    match selection {
        Some(selection) => profiles::scoped(selection, run),
        None => run(),
    }
}
