use super::{
    super::{
        reads,
        rest_dispatch::{self, Access},
        transport, Problem, V1Result,
    },
    state::{self, Job, Phase},
};
use anyhow::Result;
use axum::{http::StatusCode, response::Response};
use respire_app::{
    sync::{self, SyncAccess, SyncLocal},
    transport::remote::RemoteTransport,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct Trigger {
    space_id: Option<String>,
}

pub(super) async fn enqueue(
    scope: transport::RestScope,
    request: Option<Trigger>,
) -> V1Result<Response> {
    rest_dispatch::submit(scope, Access::Write, move |ctx| {
        if let Some(space) = request.and_then(|request| request.space_id) {
            if space != reads::space_id(ctx)? {
                return Err(Problem::new(
                    StatusCode::NOT_FOUND,
                    "space_not_found",
                    "Space not found",
                ));
            }
        }
        check_mode()?;
        let remote = crate::engine::build_remote().map_err(|_| {
            Problem::new(
                StatusCode::CONFLICT,
                "sync_unconfigured",
                "Remote sync is unavailable",
            )
        })?;
        let boundary = ctx
            .store
            .outgoing_boundary()
            .map_err(|_| transport::unavailable())?;
        let job = state::registry().begin(ctx.scope.clone(), "sync")?;
        let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
        crate::rpc_serve::enqueue_v1_sync(PendingSync {
            job,
            remote,
            boundary,
        })
        .map_err(|_| transport::unavailable())?;
        Ok(receipt)
    })
    .await
}

fn check_mode() -> V1Result<()> {
    if respire_app::service::service_paused()
        || crate::host_hooks::background_paused()
        || crate::rpc_worker::STOPPING.load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(transport::unavailable());
    }
    Ok(())
}

pub(crate) struct PendingSync {
    job: Job,
    remote: RemoteTransport,
    boundary: i64,
}

impl PendingSync {
    pub(crate) fn run(self) -> bool {
        let live = transport::with_scope(self.job.scope(), || {
            check_mode()?;
            self.job.start()
        });
        if !matches!(live, Ok(true)) {
            return false;
        }
        let control = DispatchSync(&self.job);
        let result = sync::sync_with_access(&self.remote, &control, Some(self.boundary));
        let cancelled = result.as_ref().is_err_and(|error| error.is::<Cancelled>());
        crate::sync_flow::record_sync_result((!cancelled).then_some(&result));
        match result {
            Ok(stats) => {
                self.job.finish(Phase::Completed);
                if let Err(error) = crate::rpc_serve::deliver_post_sync(stats.pulled, stats.pushed)
                {
                    eprintln!("command=sync status=warn mode=rest host_plugin_error={error:#}");
                }
                true
            }
            Err(_) if cancelled => {
                self.job.finish(Phase::Cancelled);
                false
            }
            Err(error) => {
                eprintln!("command=sync status=fail mode=rest error={error:#}");
                self.job.finish(Phase::Failed);
                false
            }
        }
    }
}

#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("sync operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

struct DispatchSync<'a>(&'a Job);

impl SyncAccess for DispatchSync<'_> {
    fn progress(&self, phase: &str) {
        let percent = match phase {
            "capabilities" => 5,
            "pull" => 15,
            "push" => 40,
            "materialize" => 65,
            "conflicts" => 80,
            "verify" => 95,
            _ => return,
        };
        let _ = self.0.progress(percent);
    }

    fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(SyncLocal<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let entry = self.0.entry();
        rest_dispatch::submit_sync(self.0.scope().clone(), move |ctx| {
            check_mode()?;
            if entry.cancelled()? {
                return Ok(Err(Cancelled.into()));
            }
            Ok(action(ctx.library.sync_access(ctx.store)))
        })
        .map_err(|_| anyhow::anyhow!("active library scope or write admission unavailable"))?
    }

    fn remote<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let cancelled = transport::with_scope(self.0.scope(), || {
            check_mode()?;
            respire_app::service::ensure_writable().map_err(|_| transport::unavailable())?;
            self.0.entry().cancelled()
        })
        .map_err(|_| anyhow::anyhow!("active library scope unavailable"))?;
        if cancelled {
            return Err(Cancelled.into());
        }
        // An issued network request may finish. The next local phase rechecks authority.
        action()
    }

    fn begin_epoch(&self, epoch: String) -> Result<bool> {
        let generation = crate::rpc_worker::sync_generation();
        self.local(move |local| {
            let requeued = local.begin_epoch(&epoch)?;
            if requeued {
                crate::rpc_worker::invalidate_sync_boundary(generation)?;
            }
            Ok(requeued)
        })
    }
}
