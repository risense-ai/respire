use super::{
    reads::check_space,
    registration::{bind, RouteSpec, Verb},
    rest_dispatch::{self, Access, LibraryContext},
    transport::{self, invalid},
    writes::{new_entry, notify, write_failure},
    V1Result,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    response::Response,
    Router,
};
use chrono::{Local, NaiveDate, SecondsFormat, TimeZone};
use respire_app::MemoryEntry;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

pub(super) fn routes(bound: SocketAddr) -> Router {
    bind(
        Router::new(),
        RouteSpec::new(Verb::Post, "/v1/diary", "upsertDiaryEntry"),
        upsert,
    )
    .with_state(bound)
}

pub(super) fn parse_date(raw: &str) -> V1Result<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .filter(|day| day.format("%Y-%m-%d").to_string() == raw)
        .ok_or_else(invalid)
}

pub(super) fn item(entry: MemoryEntry) -> Value {
    json!({
        "id": entry.id, "date": crate::util::local_day_of(&entry.created_at),
        "content": entry.content, "updated_at": entry.updated_at,
    })
}

#[derive(Deserialize)]
struct Upsert {
    space_id: Option<String>,
    date: String,
    content: String,
}

async fn upsert(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(invalid());
    }
    let input = transport::json::<Upsert>(body, super::super::MAX_BODY as usize).await?;
    if input.content.trim().is_empty() {
        return Err(invalid());
    }
    let day = parse_date(&input.date)?;
    let created = Local
        .from_local_datetime(&day.and_hms_opt(12, 0, 0).ok_or_else(invalid)?)
        .single()
        .ok_or_else(invalid)?
        .to_utc()
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let entry = rest_dispatch::submit(scope, Access::Write, move |ctx| {
        check_space(ctx, input.space_id)?;
        let entry = append(ctx, &input.date, input.content, created).map_err(write_failure)?;
        notify();
        Ok(entry)
    })
    .await?;
    transport::json_response(StatusCode::OK, item(entry))
}

fn append(
    ctx: &LibraryContext<'_>,
    date: &str,
    content: String,
    created: String,
) -> anyhow::Result<MemoryEntry> {
    let user = crate::util::current_user();
    let (id, title) = crate::engine::diary_track_identity(&user, date);
    let rows = ctx.store.active_diary_tracks(&id, &title)?;
    let tracks = crate::engine::select_diary_tracks(&rows, &id, &title, |row| {
        let author = ctx.library.open_memory(row)?.user;
        Ok(author == user || author == ctx.scope.account)
    })?;
    let Some((primary, duplicates)) = tracks.split_first() else {
        let mut entry = new_entry(ctx.scope.account.clone(), title, content);
        entry.id = id;
        entry.created_at = created;
        entry.importance = "trivial".to_owned();
        ctx.library.create_memory(ctx.store, &entry)?;
        return Ok(entry);
    };
    let mut entry = ctx.library.open_memory(primary)?;
    let marker = format!("{}\n", crate::engine::diary_track_marker(date));
    let mut merged = String::new();
    for row in &tracks {
        let previous = ctx.library.open_memory(row)?;
        let body = previous
            .content
            .strip_prefix(&marker)
            .unwrap_or(&previous.content);
        merged.push_str(body);
        if !body.is_empty() && !body.ends_with('\n') {
            merged.push('\n');
        }
    }
    merged.push_str(&content);
    entry.content = merged;
    entry.updated_at = ctx.store.edit_stamp(&entry.id)?;
    entry.modified_by = respire_app::service::device_tag();
    ctx.library
        .append_diary(ctx.store, &entry, primary, duplicates)?;
    Ok(entry)
}
