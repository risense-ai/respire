use super::{
    super::{
        owner_control,
        registration::{bind, RouteSpec, Verb},
        transport, Problem, V1Result,
    },
    state::{self, Entry, Snapshot},
    deepen, model_download, model_switch, sync,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Router,
};
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{watch, Semaphore, SemaphorePermit};
use uuid::Uuid;

static STREAMS: Semaphore = Semaphore::const_new(16);
const CHECK_INTERVAL: Duration = Duration::from_secs(1);
const HEARTBEAT: Duration = Duration::from_secs(15);

pub(in crate::runtime_http::v1) fn routes(bound: SocketAddr) -> Router {
    let mut router = Router::new();
    for (verb, path, operation, route) in [
        (
            Verb::Get,
            "/v1/operations/{operationId}",
            "getOperation",
            Route::Status,
        ),
        (
            Verb::Delete,
            "/v1/operations/{operationId}",
            "cancelOperation",
            Route::Cancel,
        ),
        (
            Verb::Get,
            "/v1/operations/{operationId}/events",
            "streamOperationEvents",
            Route::Events,
        ),
        (
            Verb::Post,
            "/v1/sync/trigger",
            "triggerSync",
            Route::TriggerSync,
        ),
        (Verb::Post, "/v1/organize/deepen/plan", "planOrganizeDeepen", Route::PlanDeepen),
        (Verb::Post, "/v1/organize/deepen/apply", "applyOrganizeDeepen", Route::ApplyDeepen),
        (Verb::Post, "/v1/models/switch", "switchModel", Route::SwitchModel),
        (Verb::Post, "/v1/models/downloads", "startModelDownload", Route::DownloadModel),
    ] {
        router = bind(
            router,
            RouteSpec::new(verb, path, operation),
            move |State(bound): State<SocketAddr>,
                  ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| handle(route, bound, peer, request),
        );
    }
    router
        .method_not_allowed_fallback(super::super::method_not_allowed)
        .with_state(bound)
}

#[derive(Clone, Copy)]
enum Route {
    Status,
    Cancel,
    Events,
    TriggerSync,
    PlanDeepen,
    ApplyDeepen,
    SwitchModel,
    DownloadModel,
}

async fn handle(
    route: Route,
    bound: SocketAddr,
    peer: SocketAddr,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    let method = match route {
        Route::Status | Route::Events => Method::GET,
        Route::Cancel => Method::DELETE,
        Route::TriggerSync | Route::PlanDeepen | Route::ApplyDeepen | Route::SwitchModel | Route::DownloadModel => Method::POST,
    };
    if parts.method != method {
        return Err(super::super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    if matches!(route, Route::TriggerSync) {
        let request = transport::json_optional::<sync::Trigger>(body, 8192).await?;
        return sync::enqueue(scope, request).await;
    }
    if matches!(route, Route::PlanDeepen) {
        return deepen::plan(scope, transport::json(body, 8192).await?).await;
    }
    if matches!(route, Route::ApplyDeepen) {
        return deepen::apply(scope, transport::json(body, 8192).await?).await;
    }
    if matches!(route, Route::SwitchModel) {
        return model_switch::enqueue(scope, transport::json(body, 8192).await?).await;
    }

    if matches!(route, Route::DownloadModel) {
        return model_download::enqueue(scope, transport::json(body, 8192).await?).await;
    }

    transport::json_optional::<serde_json::Value>(body, 0).await?;
    let id = parts
        .uri
        .path()
        .strip_prefix("/v1/operations/")
        .and_then(|path| {
            if matches!(route, Route::Events) {
                path.strip_suffix("/events")
            } else {
                Some(path)
            }
        })
        .and_then(|raw| transport::decode(raw, false).ok())
        .and_then(|raw| {
            Uuid::parse_str(&raw)
                .ok()
                .filter(|id| id.to_string() == raw)
        })
        .ok_or_else(state::not_found)?;
    if matches!(route, Route::Events) {
        let after = last_event_id(&parts.headers)?;
        let live = scope.clone();
        let entry = owner_control::bounded_blocking(move || {
            transport::with_scope(&live, || state::registry().lookup(&live, id))
        })
        .await?;
        return events(scope, entry, after);
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    #[cfg(target_os = "linux")]
    let receipt_scope = scope.clone();
    let snapshot = owner_control::bounded_blocking(move || {
        transport::with_scope(&scope, || {
            let Some(entry) = state::registry().lookup_optional(&scope, id)? else { return Ok(None); };
            if matches!(route, Route::Cancel) {
                if Instant::now() >= deadline {
                    return Err(transport::unavailable());
                }
                entry.cancel().map(Some)
            } else {
                entry.snapshot().map(Some)
            }
        })
    })
    .await?;
    let code = if matches!(route, Route::Cancel) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let mut response = match snapshot {
        Some(snapshot) => transport::json_response(code, snapshot)?,
        #[cfg(target_os = "linux")]
        None if matches!(route, Route::Status) => {
            let terminal = super::super::backup_export::lookup_completed(receipt_scope, id).await?
                .ok_or_else(state::not_found)?;
            transport::json_response(code, terminal)?
        }
        None => return Err(state::not_found()),
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

fn last_event_id(headers: &HeaderMap) -> V1Result<u64> {
    let mut ids = headers.get_all("last-event-id").iter();
    let Some(id) = ids.next() else {
        return Ok(0);
    };
    if ids.next().is_some() {
        return Err(invalid_cursor());
    }
    id.to_str()
        .ok()
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|id| id.parse().ok())
        .ok_or_else(invalid_cursor)
}

fn invalid_cursor() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_last_event_id",
        "Invalid Last-Event-ID",
    )
}

fn events(scope: transport::RestScope, entry: Arc<Entry>, after: u64) -> V1Result<Response> {
    let permit = STREAMS
        .try_acquire()
        .map_err(|_| transport::unavailable())?;
    let latest = entry.subscribe();
    if after > latest.borrow().revision {
        return Err(invalid_cursor());
    }
    let stream = futures_util::stream::unfold(
        Cursor {
            scope,
            latest,
            after,
            finished: false,
            heartbeat: Instant::now(),
            _permit: permit,
        },
        next,
    );
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

struct Cursor {
    scope: transport::RestScope,
    latest: watch::Receiver<Snapshot>,
    after: u64,
    finished: bool,
    heartbeat: Instant,
    _permit: SemaphorePermit<'static>,
}

async fn next(mut cursor: Cursor) -> Option<(Result<Event, Infallible>, Cursor)> {
    if cursor.finished {
        return None;
    }
    loop {
        let snapshot = cursor.latest.borrow_and_update().clone();
        let revision = snapshot.revision;
        let terminal = snapshot.terminal();
        let heartbeat = cursor.heartbeat.elapsed() >= HEARTBEAT;
        let scope = cursor.scope.clone();
        let after = cursor.after;
        let frame = owner_control::bounded_blocking(move || {
            transport::with_scope(&scope, || {
                if revision > after {
                    Ok(Some(snapshot_frame(&snapshot)))
                } else if heartbeat && !terminal {
                    Ok(Some(Event::default().comment("keepalive")))
                } else {
                    Ok(None)
                }
            })
        })
        .await
        .ok()?;
        if let Some(frame) = frame {
            cursor.after = revision;
            cursor.finished = terminal;
            cursor.heartbeat = Instant::now();
            return Some((Ok(frame), cursor));
        }
        if terminal
            || matches!(
                tokio::time::timeout(CHECK_INTERVAL, cursor.latest.changed()).await,
                Ok(Err(_))
            )
        {
            return None;
        }
    }
}

pub(super) fn snapshot_frame(snapshot: &Snapshot) -> Event {
    let event = snapshot.event();
    let data = serde_json::json!({"event": event, "data": snapshot});
    Event::default()
        .id(snapshot.revision.to_string())
        .event(event)
        .data(data.to_string())
}
