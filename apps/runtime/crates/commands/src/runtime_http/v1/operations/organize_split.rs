use super::{
    super::{
        reads,
        registration::{bind, RouteSpec, Verb},
        rest_dispatch::{self, Access, LibraryContext},
        transport::{self, RestScope},
        writes, Problem, V1Result,
    },
    state::{self, Job, Phase},
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::Response,
    Router,
};
use respire_app::{service, StoredMemory};
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc};

mod plan;
#[cfg(test)]
pub(super) mod tests;

#[derive(Deserialize)]
struct SplitRequest {
    memory_id: String,
    space_id: Option<String>,
}

struct Prepared {
    space: String,
    source: StoredMemory,
    spec: service::SplitSpec,
}

pub(in crate::runtime_http::v1) fn routes(bound: SocketAddr) -> Router {
    const SPLIT: RouteSpec = RouteSpec::new(Verb::Post, "/v1/organize/split", "splitOrganizeNode");
    bind(Router::new(), SPLIT, handle)
        .method_not_allowed_fallback(super::super::method_not_allowed)
        .with_state(bound)
}

async fn handle(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST {
        return Err(super::super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    let input: SplitRequest = transport::json(body, 8192).await?;
    enqueue(scope, input).await
}

async fn enqueue(scope: RestScope, input: SplitRequest) -> V1Result<Response> {
    if input.memory_id.trim().is_empty()
        || input
            .space_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
    {
        return Err(transport::invalid());
    }
    let runtime = tokio::runtime::Handle::current();
    rest_dispatch::submit(scope, Access::Write, move |ctx| {
        let prepared = prepare(ctx, &input)?;
        if !ctx.may_commit() {
            return Err(transport::unavailable());
        }
        let job = Arc::new(state::registry().begin(ctx.scope.clone(), "organize_split")?);
        let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
        let worker = Arc::clone(&job);
        let outcome = rest_dispatch::enqueue_write_result(ctx.scope.clone(), move |ctx| {
            if worker.start()? {
                execute(ctx, &worker, &input, &prepared)?;
            }
            Ok(())
        })
        .inspect_err(|problem| job.fail_problem(problem))?;
        runtime.spawn(supervise(job, outcome));
        Ok(receipt)
    })
    .await
}

async fn supervise(job: Arc<Job>, outcome: tokio::sync::oneshot::Receiver<V1Result<()>>) {
    match outcome.await {
        Ok(Ok(())) => {}
        Ok(Err(problem)) => job.fail_problem(&problem),
        Err(_) => job.fail_problem(&transport::unavailable()),
    }
}

fn prepare(ctx: &LibraryContext<'_>, input: &SplitRequest) -> V1Result<Prepared> {
    let space = selected_space(ctx, input.space_id.clone())?;
    reads::with_memory_store(ctx, Some(space.clone()), |store, _| {
        let source = store
            .active_memory_exact(&input.memory_id)
            .map_err(|_| transport::unavailable())?
            .filter(|row| row.user == ctx.scope.account)
            .ok_or_else(writes::missing_memory)?;
        let entry = ctx
            .library
            .open_memory(&source)
            .map_err(|_| transport::unavailable())?;
        if entry.user != ctx.scope.account {
            return Err(writes::missing_memory());
        }
        if entry.importance == "trivial" || source.local_importance == "trivial" {
            return Err(Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "split_ineligible",
                "Diary entries cannot be split",
            ));
        }
        let spec = plan::build(&entry)?;
        Ok(Prepared {
            space,
            source,
            spec,
        })
    })
}

fn selected_space(ctx: &LibraryContext<'_>, requested: Option<String>) -> V1Result<String> {
    match reads::resolve_memory_space(ctx, requested)? {
        Some(space) => Ok(space),
        None => reads::space_id(ctx),
    }
}

fn execute(
    ctx: &LibraryContext<'_>,
    job: &Job,
    input: &SplitRequest,
    prepared: &Prepared,
) -> V1Result<()> {
    // Check the same selection under the serving library fence before opening its store.
    if selected_space(ctx, input.space_id.clone())? != prepared.space {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "split_space_changed",
            "Selected space changed after planning",
        ));
    }
    job.progress(50)?;
    let terminal =
        reads::with_memory_store(ctx, Some(prepared.space.clone()), |store, device_local| {
            let result = job.commit(true, || {
                ctx.library.apply_split(
                    store,
                    &ctx.scope.account,
                    &prepared.source,
                    &prepared.spec,
                    || {
                        anyhow::ensure!(ctx.may_commit(), "split commit admission expired");
                        anyhow::ensure!(
                            !service::off_mode(),
                            "memory service disabled before split commit"
                        );
                        service::ensure_writable()
                    },
                )
            });
            match result {
                Ok(()) => {
                    if !device_local {
                        writes::notify();
                    }
                    Ok(Phase::Completed)
                }
                Err(error) if error.is::<crate::model_progress::OperationStopped>() => {
                    Ok(Phase::Cancelled)
                }
                Err(error) => Err(writes::write_failure(error)),
            }
        })?;
    job.finish(terminal);
    Ok(())
}
