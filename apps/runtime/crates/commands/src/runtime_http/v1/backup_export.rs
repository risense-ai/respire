//! Stream a selected native SQLite backup without retaining the owner
//! dispatcher, write fence, or Vault lock during disk I/O.
use super::{
    reads, rest_dispatch::{self, Access}, transport::{self, RestScope}, V1Result,
};
use anyhow::{ensure, Result};
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};
use respire_app::snapshot::backup_archive::{BackupExportDraft, BackupExportReader};
use std::{
    io,
    net::SocketAddr,
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, SemaphorePermit};

static ADMISSION: Semaphore = Semaphore::const_new(2);
const PREPARATION_LIMIT: Duration = Duration::from_secs(120);
const STREAM_LIMIT: Duration = Duration::from_secs(180);

struct CancelPreparation(Arc<AtomicBool>);

impl Drop for CancelPreparation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) async fn get(bound: SocketAddr, peer: SocketAddr, request: Request) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::GET {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    let mut query = transport::query(parts.uri.query())?;
    let selected = query.remove("space_id");
    if !query.is_empty() || selected.as_deref().is_some_and(str::is_empty) {
        return Err(transport::invalid());
    }
    transport::json_optional::<serde_json::Value>(body, 0).await?;
    let permit = ADMISSION.try_acquire().map_err(|_| transport::unavailable())?;
    let source = rest_dispatch::submit(scope.clone(), Access::Read, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            store.prepare_backup_export(&ctx.scope.canonical_profile).map_err(|_| transport::unavailable())
        })
    }).await?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelPreparation(Arc::clone(&cancelled));
    let worker_scope = scope.clone();
    let setup = tokio::task::spawn_blocking(move || -> Result<_> {
        let deadline = Instant::now() + PREPARATION_LIMIT;
        let check = || -> Result<()> {
            ensure!(!cancelled.load(Ordering::Acquire) && Instant::now() < deadline, "backup preparation ended");
            verify(&worker_scope, || Ok(()))
        };
        let draft = BackupExportDraft::new(source.copy(check)?)?;
        check()?;
        let proof = with_library(&worker_scope, |library| library.prove_backup_snapshot(&draft))?;
        let ready = draft.prepare(proof, check)?;
        check()?;
        let header = with_library(&worker_scope, |library| library.seal_verified_backup_export(&ready))?;
        let reader = ready.into_reader(header)?;
        check()?;
        Ok((reader, permit))
    });
    let result = tokio::time::timeout(PREPARATION_LIMIT, setup).await
        .ok().and_then(|join| join.ok()).and_then(Result::ok);
    let Some((reader, permit)) = result else {
        return if !scope.bearer_is_current() { Err(transport::unauthorized()) }
            else { Err(transport::unavailable()) };
    };
    let context = FrameContext { reader, scope, deadline: Instant::now() + STREAM_LIMIT, _permit: permit };
    let body = Body::from_stream(futures_util::stream::unfold(Some(context), next));
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream"), (header::CACHE_CONTROL, "no-store")],
        body,
    ).into_response())
}

fn verify<T>(scope: &RestScope, action: impl FnOnce() -> Result<T>) -> Result<T> {
    with_library(scope, |_| action())
}

fn with_library<T>(
    scope: &RestScope,
    action: impl FnOnce(respire_app::auth::RuntimeLibrary<'_>) -> Result<T>,
) -> Result<T> {
    crate::rpc_serve::with_verified_owner_library_scope(|account, path, instance, generation, epoch, library| {
        ensure!(scope.matches(account, path, instance, generation, epoch), "backup scope changed");
        action(library)
    }).ok_or_else(|| anyhow::anyhow!("backup scope unavailable"))?
}

struct FrameContext {
    reader: BackupExportReader,
    scope: RestScope,
    deadline: Instant,
    _permit: SemaphorePermit<'static>,
}

async fn next(state: Option<FrameContext>) -> Option<(io::Result<Bytes>, Option<FrameContext>)> {
    let mut ctx = state?;
    let result = tokio::task::spawn_blocking(move || {
        let next = ctx.reader.next_verified(|| {
            ensure!(Instant::now() < ctx.deadline, "backup stream expired");
            verify(&ctx.scope, || Ok(()))
        });
        (next, ctx)
    }).await;
    match result {
        Ok((Ok(None), _)) => None,
        Ok((Ok(Some(bytes)), ctx)) => Some((Ok(Bytes::from(bytes)), Some(ctx))),
        _ => Some((Err(io::Error::other("backup stream interrupted")), None)),
    }
}
