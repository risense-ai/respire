use super::{
    super::{owner_control, rest_dispatch, transport, transport::RestScope, Problem, V1Result},
    state::{self, Job, Phase},
};
use crate::{
    model_progress,
    rpc_serve::{self, ModelSwitchReservation},
    rpc_worker,
};
use anyhow::{Context, Result};
use axum::{http::StatusCode, response::Response};
use respire_app::{
    auth::RuntimeLibrary,
    core_sdk::{onnx::Engine, profiles::Selection},
    embedding::{self, Choice},
    lock::LibraryLock,
    service,
    transport::local::LocalStore,
};
use serde::Deserialize;

mod dispatch;
mod prepare;
pub(in crate::runtime_http::v1) use dispatch::RunningSwitch;

#[derive(Deserialize)]
pub(super) struct Request {
    model_id: String,
    #[serde(default)]
    engine: Engine,
    variant: Option<String>,
}

pub(super) async fn enqueue(scope: RestScope, input: Request) -> V1Result<Response> {
    if rpc_serve::model_switch_running() {
        return Err(busy());
    }
    rest_dispatch::submit(scope, rest_dispatch::Access::Write, move |ctx| {
        check_mode().map_err(|_| transport::unavailable())?;
        let registry = embedding::registry().map_err(|_| transport::unavailable())?;
        let target = embedding::select_from_registry(
            Choice {
                model: input.model_id,
                backend: "onnx".to_owned(),
                engine: input.engine,
                variant: input.variant,
            },
            &registry,
        )
        .ok()
        .filter(|s| {
            s.engine == Engine::Cpu
                && s.backend_profile.backend == "onnx"
                && s.backend_profile.native.is_none()
        })
        .ok_or_else(|| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                "invalid_model",
                "Invalid model or unavailable engine",
            )
        })?;
        let job = state::registry().begin(ctx.scope.clone(), "model_switch")?;
        let task_id = job.id()?;
        let reservation = ModelSwitchReservation::reserve(task_id.clone())
            .map_err(|_| transport::unavailable())?
            .ok_or_else(busy)?;
        let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
        owner_control::enqueue_model_switch(PendingSwitch {
            job,
            task_id,
            target,
            reservation,
        })?;
        Ok(receipt)
    })
    .await
}

fn busy() -> Problem {
    Problem::new(
        StatusCode::CONFLICT,
        "model_switch_busy",
        "Model switch cannot start while another operation is running",
    )
}

pub(in crate::runtime_http::v1) struct PendingSwitch {
    job: Job,
    task_id: String,
    target: Selection,
    reservation: ModelSwitchReservation,
}

impl PendingSwitch {
    pub(in crate::runtime_http::v1) fn run(
        self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
    ) {
        self.run_established(store, ownership, false)
    }

    fn run_established(
        self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
        already_started: bool,
    ) {
        let Self {
            job,
            task_id,
            mut target,
            reservation,
        } = self;
        let result = rpc_serve::with_owner_store_fence(|| -> Result<Phase> {
            let scope = job.scope();
            let Some(vault) = with_authority(scope, |library| {
                if already_started {
                    if job
                        .entry()
                        .cancelled()
                        .map_err(|_| anyhow::anyhow!("operation state unavailable"))?
                    {
                        return Ok(None);
                    }
                } else if !job
                    .start()
                    .map_err(|_| anyhow::anyhow!("operation state unavailable"))?
                {
                    return Ok(None);
                }
                Ok(Some(library.model_switch_vault()))
            })?
            else {
                return Ok(Phase::Cancelled);
            };
            anyhow::ensure!(
                ownership.protects(&scope.canonical_profile)?,
                "profile lock unavailable"
            );
            let store = store.context("serving store unavailable")?;
            let epoch = rpc_worker::sync_generation();
            let entry = job.entry();
            let check = || {
                rpc_worker::check_sync_context(epoch)?;
                check_mode()?;
                anyhow::ensure!(
                    scope.bearer_is_current(),
                    "local bearer changed during model switch"
                );
                anyhow::ensure!(
                    !entry
                        .cancelled()
                        .map_err(|_| anyhow::anyhow!("operation state unavailable"))?,
                    model_progress::OperationStopped::Cancelled
                );
                Ok(())
            };
            let commit = |final_commit, action: &mut dyn FnMut() -> Result<()>| {
                check()?;
                with_authority(scope, |_| {
                    job.model_commit(final_commit, || {
                        if final_commit {
                            model_progress::commit_final(action)
                        } else {
                            model_progress::commit(action)
                        }
                    })
                })
            };
            reservation.enter();
            let result = (|| {
                commit(false, &mut || store.queue_selection(&target, &task_id))?;
                rpc_serve::run_model_switch(
                    store,
                    &vault,
                    &task_id,
                    &mut target,
                    check,
                    |phase, done, total| {
                        let percent = match phase {
                            "switch-verify" => 5,
                            "switch-load" => 15,
                            "switch-index" => {
                                20 + (done.saturating_mul(59) / total.unwrap_or(1).max(1)).min(59)
                                    as u8
                            }
                            "switch-diary" => 80,
                            _ => return Ok(()),
                        };
                        job.progress(percent)
                            .map_err(|_| anyhow::anyhow!("operation state unavailable"))
                    },
                    commit,
                )
            })();
            let profile = rpc_serve::record_switch_result(&task_id, &result);
            let phase = match &result {
                Ok(_) => Phase::Completed,
                Err(_)
                    if profile["semantic"] != "unverified"
                        && (entry.cancelled().unwrap_or(false)
                            || model_progress::control(&task_id, false)
                                .is_ok_and(|p| p["cancelled"] == true)) =>
                {
                    Phase::Cancelled
                }
                Err(_) => Phase::Failed,
            };
            if let Err(error) = result {
                eprintln!("command=model_switch status=fail mode=rest error={error:#}");
            }
            Ok(phase)
        });
        drop(reservation);
        match result {
            Some(Ok(phase)) => job.finish(phase),
            Some(Err(error)) => {
                eprintln!("command=model_switch status=fail mode=rest error={error:#}");
                job.finish(Phase::Failed);
            }
            None => job.finish(Phase::Failed),
        }
        rpc_worker::kick_index();
    }
}

pub(super) fn with_authority<T>(
    scope: &RestScope,
    action: impl FnOnce(RuntimeLibrary<'_>) -> Result<T>,
) -> Result<T> {
    rpc_serve::with_locked_library(|account, profile, instance, generation, epoch, library| {
        anyhow::ensure!(
            scope.matches(account, profile, instance, generation, epoch),
            "model switch owner changed"
        );
        check_mode()?;
        service::ensure_writable()?;
        action(library)
    })
    .context("verified model switch authority unavailable")?
}

pub(super) fn check_mode() -> Result<()> {
    anyhow::ensure!(
        !service::service_paused() && !crate::host_hooks::background_paused(),
        "model switching is paused by service mode"
    );
    Ok(())
}
