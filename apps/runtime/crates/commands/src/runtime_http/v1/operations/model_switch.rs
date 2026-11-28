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
        .filter(|s| s.backend_profile.backend == "onnx"
            && (s.engine == Engine::Cpu || !embedding::variants(&s.model).is_empty()))
        .ok_or_else(|| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                "invalid_model",
                "Invalid model or unavailable engine",
            )
        })?;
        let job = state::registry().begin(ctx.scope.clone(), "model_switch")?;
        let task_id = job.id()?;
        let reservation = ModelSwitchReservation::reserve_switch(task_id.clone(), &target.model)
            .map_err(|_| transport::unavailable())?
            .ok_or_else(busy)?;
        let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
        let queued = owner_control::enqueue_model_switch(PendingSwitch {
            job,
            task_id: task_id.clone(),
            target,
            reservation,
        });
        if queued.is_err() {
            rpc_serve::record_switch_result(&task_id, Err(&anyhow::anyhow!("model switch dispatcher unavailable")));
        }
        queued?;
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

// Housekeeping uses the exact library lock and task identity, even after business
// admission closes. It never needs a Vault unlock or a foreground store fence.
#[cfg(test)]
#[path = "model_switch/cleanup_tests.rs"]
mod cleanup_tests;

fn cleanup_pending(
    store: Option<&LocalStore>, ownership: &LibraryLock,
    profile: &std::path::Path, task_id: &str,
) -> Result<()> {
    let store = store.context("model switch cleanup store unavailable")?;
    anyhow::ensure!(ownership.protects(profile)?, "model switch cleanup ownership changed");
    anyhow::ensure!(store.backup_target()?.parent() == Some(profile), "model switch cleanup library changed");
    store.clear_pending_selection(Some(task_id))
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
