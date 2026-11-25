//! Protected writes on the serving dispatcher's active library.
use super::{
    registration::{bind, RouteSpec, Verb},
    rest_dispatch::{self, Access},
    transport, Problem, V1Result,
};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use respire_app::{memory::Kind, service, MemoryEntry};
use serde::Deserialize;
use std::net::SocketAddr;

pub(super) fn routes(bound: SocketAddr) -> Router {
    let router = bind(
        Router::new(),
        RouteSpec::new(Verb::Post, "/v1/memories", "createMemory"),
        create,
    );
    let router = bind(
        router,
        RouteSpec::new(Verb::Patch, "/v1/memories/{memoryId}", "updateMemory"),
        update,
    );
    bind(
        router,
        RouteSpec::new(Verb::Delete, "/v1/memories/{memoryId}", "deleteMemory"),
        delete,
    )
    // The GET router supplies the fallback for this shared path.
    .with_state(bound)
}

async fn delete(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::DELETE {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    transport::json_optional::<serde_json::Value>(body, 0).await?;
    let id = memory_id(parts.uri.path())?;
    rest_dispatch::submit(scope, Access::Write, move |ctx| {
        if respire_app::service::off_mode() {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "memory_off",
                "Memory service disabled",
            ));
        }
        if !ctx
            .store
            .forget_memory_exact(&id)
            .map_err(|_| transport::unavailable())?
        {
            return Err(missing_memory());
        }
        notify();
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct Create {
    space_id: Option<String>,
    title: Option<String>,
    content: String,
    parent_id: Option<String>,
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Update {
    title: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
}

async fn create(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let input = transport::json::<Create>(body, super::super::MAX_BODY as usize).await?;
    validate_fields(Some(&input.content), &input.tags)?;
    let kind = match input.category.as_deref() {
        Some(category) => Kind::try_from_str(category).ok_or_else(transport::invalid)?,
        None => Kind::Context,
    };
    if matches!(kind, Kind::Hypothesis | Kind::Reflection | Kind::SelfNote) {
        return Err(transport::invalid());
    }
    let entry = rest_dispatch::submit(scope, Access::Write, move |ctx| {
        super::reads::check_space(ctx, input.space_id)?;
        if let Some(parent) = input.parent_id.as_deref() {
            if ctx
                .store
                .active_memory_exact(parent)
                .map_err(|_| transport::unavailable())?
                .is_none()
            {
                return Err(missing_memory());
            }
        }
        let title = input
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| service::derive_title(&input.content));
        let mut entry = new_entry(ctx.scope.account.clone(), title, input.content);
        entry.parent_id = input.parent_id.unwrap_or_default();
        entry.kind = kind;
        entry.tags = input.tags;
        ctx.library
            .create_memory(ctx.store, &entry)
            .map_err(write_failure)?;
        notify();
        Ok(entry)
    })
    .await?;
    transport::json_response(StatusCode::CREATED, super::reads::item(entry))
}

async fn update(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let id = memory_id(parts.uri.path())?;
    let input = transport::json::<Update>(body, super::super::MAX_BODY as usize).await?;
    validate_fields(
        input.content.as_deref(),
        input.tags.as_deref().unwrap_or_default(),
    )?;
    let entry = rest_dispatch::submit(scope, Access::Write, move |ctx| {
        if input.title.is_none() && input.content.is_none() && input.tags.is_none() {
            return ctx
                .library
                .memory_exact(ctx.store, &id)
                .map_err(|_| transport::unavailable())?
                .ok_or_else(missing_memory);
        }
        let entry = ctx
            .library
            .update_memory_exact(ctx.store, &id, |entry| {
                if let Some(title) = input.title {
                    entry.title = title;
                }
                if let Some(content) = input.content {
                    entry.content = content;
                }
                if let Some(tags) = input.tags {
                    entry.tags = tags;
                }
                entry.modified_by = service::device_tag();
            })
            .map_err(write_failure)?
            .ok_or_else(missing_memory)?;
        notify();
        Ok(entry)
    })
    .await?;
    transport::json_response(StatusCode::OK, super::reads::item(entry))
}

fn validate_fields(content: Option<&str>, tags: &[String]) -> V1Result<()> {
    if content.is_some_and(|text| text.trim().is_empty())
        || tags
            .iter()
            .any(|tag| tag.trim() != tag || tag.is_empty() || tag.contains(','))
    {
        return Err(transport::invalid());
    }
    Ok(())
}

fn memory_id(path: &str) -> V1Result<String> {
    let id = transport::decode(
        path.strip_prefix("/v1/memories/")
            .ok_or_else(transport::invalid)?,
        false,
    )?;
    if id.is_empty() {
        return Err(transport::invalid());
    }
    Ok(id)
}

pub(super) fn missing_memory() -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        "memory_not_found",
        "Memory not found",
    )
}

pub(super) fn write_failure(error: anyhow::Error) -> Problem {
    if error.is::<respire_app::transport::local::MemorySourceChanged>() {
        return Problem::new(
            StatusCode::CONFLICT,
            "memory_changed",
            "Memory changed during update",
        );
    }
    transport::unavailable()
}

pub(super) fn notify() {
    service::notify_index();
    service::notify_autosync();
}

pub(super) fn new_entry(user: String, title: String, content: String) -> MemoryEntry {
    let stamp = crate::util::now_stamp();
    let device = service::device_tag();
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        content,
        user,
        parent_id: String::new(),
        kind: Kind::Context,
        tags: Vec::new(),
        computer: device.clone(),
        device: device.clone(),
        modified_by: device,
        created_at: stamp.clone(),
        updated_at: stamp,
        emotion: -1.0,
        importance: "important".to_owned(),
        project: String::new(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
        metadata: Default::default(),
        unknown_fields: Default::default(),
    }
}
