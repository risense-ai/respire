use super::{
    super::{
        reads,
        rest_dispatch::{self, Access, LibraryContext},
        transport::{self, RestScope},
        writes, Problem, V1Result,
    },
    state::{self, Phase},
};
use axum::{http::StatusCode, response::Response};
use respire_app::{auth::PlannedOutline, service};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct PlanRequest {
    space_id: Option<String>,
    parent_id: Option<String>,
    #[serde(default)]
    auto: bool,
}

#[derive(Deserialize)]
pub(super) struct ApplyRequest {
    plan_id: String,
}

struct Preview {
    scope: RestScope,
    revision: String,
    generation: String,
    groups: Vec<PlannedOutline>,
    created: Instant,
}

static PREVIEWS: LazyLock<Mutex<HashMap<String, Arc<Preview>>>> = LazyLock::new(Mutex::default);
const PREVIEW_LIFETIME: Duration = Duration::from_secs(600);

pub(super) async fn plan(scope: RestScope, input: PlanRequest) -> V1Result<Response> {
    if input.parent_id.as_deref().is_some_and(str::is_empty) {
        return Err(transport::invalid());
    }
    let response = rest_dispatch::submit_recall(scope.clone(), move |ctx| {
        reads::check_space(ctx, input.space_id)?;
        // Legacy auto mode scans every eligible root even when a target is supplied.
        let target = if input.auto { None } else { input.parent_id.as_deref() };
        if let Some(id) = target {
            let owned = ctx
                .store
                .active_memory_exact(id)
                .map_err(|_| transport::unavailable())?
                .is_some_and(|row| {
                    row.user == ctx.scope.account && row.local_importance != "trivial"
                });
            if !owned {
                return Err(writes::missing_memory());
            }
        }
        let (revision, generation) = version(ctx)?;
        let plans = service::scoped_deepen_plan(ctx.store, &ctx.scope.account, target)
            .map_err(plan_failure)?;
        let mut groups = Vec::new();
        let mut moves = Vec::new();
        for (root, plan) in plans {
            for group in plan.sub_roots {
                let id = Uuid::new_v4().to_string();
                for member in &group.member_ids {
                    moves.push(json!({"memory_id": member, "new_parent_id": id}));
                }
                groups.push(PlannedOutline {
                    root: root.clone(),
                    id,
                    title: group.title,
                    members: group.member_ids,
                });
            }
        }
        let preview = Preview {
            scope: ctx.scope.clone(),
            revision,
            generation,
            groups,
            created: Instant::now(),
        };
        check_version(ctx, &preview)?;
        let plan_id = Uuid::new_v4().to_string();
        let body =
            transport::json_response(StatusCode::OK, json!({"plan_id": plan_id, "moves": moves}))?;
        let mut previews = PREVIEWS.lock().map_err(|_| transport::unavailable())?;
        previews.retain(|_, plan| plan.created.elapsed() < PREVIEW_LIFETIME);
        if previews.len() >= 128 {
            return Err(transport::unavailable());
        }
        previews.insert(plan_id, Arc::new(preview));
        Ok(body)
    })
    .await?;
    super::super::owner_control::bounded_blocking(move || {
        transport::with_scope(&scope, || Ok(response))
    })
    .await
}

fn plan_failure(error: anyhow::Error) -> Problem {
    if error.is::<respire_app::input_error::InputError>() {
        return Problem::new(
            StatusCode::CONFLICT,
            "outline_ineligible",
            "Outline is not eligible for deepening",
        );
    }
    transport::unavailable()
}

pub(super) async fn apply(scope: RestScope, input: ApplyRequest) -> V1Result<Response> {
    rest_dispatch::submit(scope, Access::Write, move |ctx| {
        check_write_mode()?;
        let mut previews = PREVIEWS.lock().map_err(|_| transport::unavailable())?;
        let preview = previews
            .get(&input.plan_id)
            .filter(|plan| &plan.scope == ctx.scope && plan.created.elapsed() < PREVIEW_LIFETIME)
            .ok_or_else(missing_plan)?;
        check_version(ctx, preview)?;
        let preview = Arc::clone(preview);
        let job = state::registry().begin(ctx.scope.clone(), "organize_deepen")?;
        let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
        rest_dispatch::enqueue_write(ctx.scope.clone(), move |ctx| {
            if !job.start()? {
                return Ok(());
            }
            check_write_mode()?;
            let result = ctx.library.apply_deepen(
                ctx.store,
                &ctx.scope.account,
                &preview.revision,
                &preview.generation,
                &preview.groups,
                || {
                    anyhow::ensure!(
                        ctx.scope.bearer_is_current(),
                        "local bearer changed before commit"
                    );
                    service::ensure_writable()
                },
            );
            if let Err(error) = result {
                eprintln!("command=organize_deepen status=fail mode=rest error={error:#}");
                return Err(transport::unavailable());
            }
            if !preview.groups.is_empty() {
                writes::notify();
            }
            job.finish(Phase::Completed);
            Ok(())
        })?;
        previews.remove(&input.plan_id);
        Ok(receipt)
    })
    .await
}

fn version(ctx: &LibraryContext<'_>) -> V1Result<(String, String)> {
    let revision = ctx
        .store
        .memory_revision()
        .map_err(|_| transport::unavailable())?;
    let generation = ctx
        .store
        .active_generation_key()
        .map_err(|_| transport::unavailable())?;
    Ok((revision, generation))
}

fn check_version(ctx: &LibraryContext<'_>, preview: &Preview) -> V1Result<()> {
    let (revision, generation) = version(ctx)?;
    if preview.revision != revision || preview.generation != generation {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "outline_changed",
            "Outline changed after planning",
        ));
    }
    Ok(())
}

fn missing_plan() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "plan_not_found", "Plan not found")
}

fn check_write_mode() -> V1Result<()> {
    if service::off_mode() {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "memory_off",
            "Memory service disabled",
        ));
    }
    Ok(())
}
