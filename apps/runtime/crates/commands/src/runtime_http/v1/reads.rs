//! Read-only projections of the verified, already-open serving library.
use super::{
    registration::{bind, RouteSpec, Verb},
    rest_dispatch::{self, Access, LibraryContext},
    transport::{self, invalid, unavailable},
    Problem, V1Result,
};
use axum::{
    body::{self, Body},
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::Response,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use respire_app::service;
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(Clone, Copy)]
enum Read {
    Memory,
    Page,
    Light,
    Sync,
    ModelCatalog,
    ActiveAssets,
    Diary,
    OrganizeSuggestions,
    SpaceMembers,
}

pub(super) fn routes(bound: SocketAddr) -> Router {
    let mut router = Router::new();
    for (path, operation, read) in [
        ("/v1/memories/{memoryId}", "getMemory", Read::Memory),
        ("/v1/memories", "listMemories", Read::Page),
        ("/v1/status/light", "getLightStatus", Read::Light),
        ("/v1/sync/status", "getSyncStatus", Read::Sync),
        ("/v1/models/catalog", "getModelCatalog", Read::ModelCatalog),
        (
            "/v1/models/active-assets",
            "getActiveModelAssets",
            Read::ActiveAssets,
        ),
        ("/v1/diary", "listDiaryEntries", Read::Diary),
        ("/v1/spaces/{spaceId}/members", "listSpaceMembers", Read::SpaceMembers),
        (
            "/v1/organize/suggestions",
            "getOrganizeSuggestions",
            Read::OrganizeSuggestions,
        ),
    ] {
        router = bind(
            router,
            RouteSpec::new(Verb::Get, path, operation),
            move |State(bound): State<SocketAddr>,
                  ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request<Body>| { handle(read, bound, peer, request) },
        );
    }
    bind(
        router,
        RouteSpec::new(Verb::Get, "/v1/host/account", "getHostAccount"),
        host_account,
    )
    .method_not_allowed_fallback(super::method_not_allowed)
    .with_state(bound)
}

async fn host_account(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::GET {
        return Err(super::method_not_allowed().await);
    }
    let bearer = transport::require_local_bearer(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(invalid());
    }
    transport::json_optional::<Value>(body, 0).await?;
    let status = super::owner_control::bounded_blocking(move || {
        let status = crate::rpc_serve::v1_host_account_status().map_err(|_| unavailable())?;
        if !transport::bearer_is_current(&bearer) {
            return Err(transport::unauthorized());
        }
        Ok(status)
    })
    .await?;
    transport::json_response(StatusCode::OK, status)
}

async fn handle(
    read: Read,
    bound: SocketAddr,
    peer: SocketAddr,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::GET {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    tokio::time::timeout(crate::network::IO_WAIT, body::to_bytes(body, 0))
        .await
        .map_err(|_| {
            Problem::new(
                StatusCode::REQUEST_TIMEOUT,
                "request_body_timeout",
                "Request body incomplete",
            )
        })?
        .map_err(|_| invalid())?;
    let mut query = transport::query(parts.uri.query())?;
    let space = matches!(
        read,
        Read::Page | Read::Light | Read::Diary | Read::OrganizeSuggestions
    )
    .then(|| query.remove("space_id"))
    .flatten();
    let call = match read {
        Read::Memory => {
            let raw = parts
                .uri
                .path()
                .strip_prefix("/v1/memories/")
                .ok_or_else(invalid)?;
            Call::Memory(transport::decode(raw, false)?)
        }
        Read::Page => {
            let limit = parse_limit(query.remove("limit"))?;
            let cursor = query
                .remove("cursor")
                .map(|raw| {
                    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?;
                    serde_json::from_slice(&bytes).map_err(|_| invalid())
                })
                .transpose()?;
            Call::Page(limit, cursor)
        }
        Read::SpaceMembers => {
            let raw = parts.uri.path().strip_prefix("/v1/spaces/")
                .and_then(|path| path.strip_suffix("/members")).ok_or_else(invalid)?;
            Call::SpaceMembers(transport::decode(raw, false)?)
        }
        Read::Light => Call::Light,
        Read::Sync => Call::Sync,
        Read::ModelCatalog => Call::ModelCatalog,
        Read::ActiveAssets => Call::ActiveAssets,
        Read::OrganizeSuggestions => Call::OrganizeSuggestions(parse_limit(query.remove("top"))?),
        Read::Diary => Call::Diary(
            query
                .remove("date")
                .map(|date| super::diary::parse_date(&date).map(|_| date))
                .transpose()?,
        ),
    };
    if !query.is_empty() {
        return Err(invalid());
    }
    let value = rest_dispatch::submit(scope, Access::Read, move |ctx| call.run(ctx, space)).await?;
    transport::json_response(StatusCode::OK, value)
}

fn parse_limit(raw: Option<String>) -> V1Result<usize> {
    match raw {
        None => Ok(50),
        Some(raw) if raw.bytes().all(|byte| byte.is_ascii_digit()) => raw
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=200).contains(value))
            .ok_or_else(invalid),
        Some(_) => Err(invalid()),
    }
}

enum Call {
    Memory(String),
    Page(usize, Option<(String, i64)>),
    Light,
    Sync,
    ModelCatalog,
    ActiveAssets,
    Diary(Option<String>),
    OrganizeSuggestions(usize),
    SpaceMembers(String),
}

pub(super) fn space_id(ctx: &LibraryContext<'_>) -> V1Result<String> {
    let path = &ctx.scope.canonical_profile;
    if service::main_data_dir()
        .canonicalize()
        .map_err(|_| unavailable())?
        == *path
    {
        return Ok("main".to_owned());
    }
    let root = service::accounts_root()
        .canonicalize()
        .map_err(|_| unavailable())?;
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|_| path.parent() == Some(root.as_path()))
        .map(str::to_owned)
        .ok_or_else(unavailable)
}

pub(super) fn check_space(ctx: &LibraryContext<'_>, requested: Option<String>) -> V1Result<()> {
    check_requested_space(ctx, requested.as_deref())
}

fn check_requested_space(ctx: &LibraryContext<'_>, requested: Option<&str>) -> V1Result<()> {
    if let Some(requested) = requested {
        if requested != space_id(ctx)? {
            return Err(Problem::new(
                StatusCode::NOT_FOUND,
                "space_not_found",
                "Space not found",
            ));
        }
    }
    Ok(())
}

pub(super) fn is_device_local_space(space: Option<&str>) -> bool {
    space.and_then(|space| space.strip_prefix("local-"))
        .and_then(|id| uuid::Uuid::parse_str(id).ok().map(|uuid| (id, uuid)))
        .is_some_and(|(id, uuid)| uuid.get_version_num() == 4 && uuid.simple().to_string() == id)
}

pub(super) fn with_memory_store<T>(
    ctx: &LibraryContext<'_>,
    requested: Option<String>,
    run: impl FnOnce(&respire_app::transport::local::LocalStore, bool) -> V1Result<T>,
) -> V1Result<T> {
    if let Some(id) = requested.as_deref().filter(|id| is_device_local_space(Some(id))) {
        return ctx.library.with_device_space_store(
            ctx.ownership, id, || ctx.may_commit(), |store| run(store, true),
        ).map_err(|_| {
            if ctx.scope.bearer_is_current() { unavailable() } else { transport::unauthorized() }
        })?.ok_or_else(|| Problem::new(StatusCode::NOT_FOUND, "space_not_found", "Space not found"))?;
    }
    check_space(ctx, requested)?;
    run(ctx.store, false)
}

fn memory_page(
    ctx: &LibraryContext<'_>, store: &respire_app::transport::local::LocalStore,
    limit: usize, after: Option<(String, i64)>,
) -> V1Result<Value> {
    let after = after.as_ref().map(|(stamp, id)| (stamp.as_str(), *id));
    let page = ctx.library.memory_page(store, limit, after).map_err(|_| unavailable())?;
    let next = page.next.map(|key| serde_json::to_vec(&key).map(|bytes| URL_SAFE_NO_PAD.encode(bytes)))
        .transpose().map_err(|_| unavailable())?;
    Ok(json!({"items": page.entries.into_iter().map(item).collect::<Vec<_>>(), "next_cursor": next}))
}

pub(super) fn item(entry: respire_app::MemoryEntry) -> Value {
    json!({"id":entry.id, "content":entry.content, "title":entry.title, "tags":entry.tags,
        "category":entry.kind.as_str(), "parent_id":(!entry.parent_id.is_empty()).then_some(entry.parent_id),
        "created_at":entry.created_at, "updated_at":entry.updated_at})
}

impl Call {
    fn run(self, ctx: &LibraryContext<'_>, requested: Option<String>) -> V1Result<Value> {
        if !matches!(self, Self::Page(..)) {
            check_requested_space(ctx, requested.as_deref())?;
        }
        match self {
            Self::Page(limit, after) => with_memory_store(ctx, requested, |store, _| {
                memory_page(ctx, store, limit, after)
            }),
            Self::Memory(id) => ctx
                .library
                .memory_exact(ctx.store, &id)
                .map_err(|_| unavailable())?
                .map(item)
                .ok_or_else(|| {
                    Problem::new(
                        StatusCode::NOT_FOUND,
                        "memory_not_found",
                        "Memory not found",
                    )
                }),
            Self::Light => {
                let generation = ctx
                    .store
                    .active_generation_key()
                    .map_err(|_| unavailable())?;
                let (total, diaries, indexed, updated) = ctx
                    .store
                    .light_status_counts(&generation)
                    .map_err(|_| unavailable())?;
                Ok(
                    json!({"space_id":space_id(ctx)?, "memory_count":total, "diary_count":diaries,
                    "indexed_count":indexed, "last_updated_at":updated}),
                )
            }
            Self::Sync => {
                let (pending, _, _) = ctx.store.sync_counts().map_err(|_| unavailable())?;
                let state = if !respire_app::sync::remote_configured() {
                    "offline"
                } else if crate::rpc_serve::SYNC_RUNNING.load(std::sync::atomic::Ordering::Acquire)
                {
                    "syncing"
                } else if crate::sync_flow::sync_live().phase == "err" {
                    "error"
                } else {
                    "idle"
                };
                Ok(json!({"state":state, "pending_outbox":pending}))
            }
            Self::SpaceMembers(id) => {
                check_space(ctx, Some(id))?;
                respire_app::login_transaction::list_space_members(&ctx.scope.canonical_profile)
                    .map_err(|_| unavailable())
            }
            Self::ModelCatalog => super::models::catalog(ctx.store)
                .and_then(|catalog| Ok(serde_json::to_value(catalog)?))
                .map_err(|_| unavailable()),
            Self::ActiveAssets => super::models::active_assets(ctx.store),
            Self::OrganizeSuggestions(top) => {
                let suggestions = service::deepen_suggestions(ctx.store, &ctx.scope.account, top)
                    .map_err(|_| unavailable())?
                    .into_iter()
                    .map(|(id, children)| json!({
                        "kind": "deepen",
                        "target_id": id,
                        "summary": format!("Consider grouping the {children} direct children of this root."),
                    }))
                    .collect::<Vec<_>>();
                Ok(json!({"suggestions": suggestions}))
            }
            Self::Diary(date) => {
                let entries = ctx
                    .library
                    .diary_entries(ctx.store, date.as_deref(), crate::util::local_day_of)
                    .map_err(|_| unavailable())?
                    .into_iter()
                    .map(super::diary::item)
                    .collect::<Vec<_>>();
                Ok(json!({"entries": entries}))
            }
        }
    }
}
