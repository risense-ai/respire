//! Typed, bounded local-Bearer work on the already-open serving library.
use axum::http::StatusCode;
use respire_app::{lock::LibraryLock, transport::local::LocalStore};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

use super::{owner_control, transport, transport::RestScope, Problem, V1Result};


#[path = "device_space_publication.rs"]
mod device_space_publication;
pub(super) use device_space_publication::Admission;
use device_space_publication::{await_publication, COMMIT_WAIT};

struct AbandonPreparation {
    admission: Arc<Admission>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for AbandonPreparation {
    fn drop(&mut self) {
        if self.admission.abandon() {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

/// Prepare normally. Only the final native publication competes with timeout
/// or disconnect; an admitted caller receives a receipt or an unknown outcome.
pub(super) async fn submit_device_space_create<T, F>(scope: RestScope, run: F) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>, &Admission) -> V1Result<T> + Send + 'static,
{
    submit_publication(scope, run, device_space_publication::outcome_unknown).await
}

pub(super) async fn submit_device_space_memory<T, F>(scope: RestScope, run: F) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>, &Admission) -> V1Result<T> + Send + 'static,
{
    submit_publication(scope, run, || Problem::new(
        StatusCode::CONFLICT, "memory_create_outcome_unknown",
        "Memory creation outcome is unknown; do not retry automatically",
    )).await
}

async fn submit_publication<T, F>(scope: RestScope, run: F, unknown: fn() -> Problem) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>, &Admission) -> V1Result<T> + Send + 'static,
{
    let deadline = Instant::now() + REQUEST_WAIT;
    let cancelled = Arc::new(AtomicBool::new(false));
    let admission = Arc::new(Admission::default());
    let _abandon = AbandonPreparation {
        admission: Arc::clone(&admission),
        cancelled: Arc::clone(&cancelled),
    };
    let worker = Arc::clone(&admission);
    let receiver = enqueue(scope, Access::Write, deadline, cancelled, move |ctx| {
        run(ctx, &worker)
    })?;
    await_publication(&admission, receiver, deadline, COMMIT_WAIT, unknown).await
}

const REQUEST_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
pub(super) enum Access {
    Read,
    Write,
}

/// No key material or caller-selected profile. The store is already open
/// and cannot be retained outside the authorized serving callback.
pub(super) struct LibraryContext<'a> {
    pub(super) store: &'a LocalStore,
    pub(super) ownership: &'a LibraryLock,
    pub(super) library: respire_app::auth::RuntimeLibrary<'a>,
    pub(super) scope: &'a RestScope,
    deadline: Instant,
    cancelled: &'a AtomicBool,
}

impl LibraryContext<'_> {
    pub(super) fn may_commit(&self) -> bool {
        Instant::now() < self.deadline
            && !self.cancelled.load(Ordering::Acquire)
            && self.scope.bearer_is_current()
    }
}

type Completion = Box<dyn for<'a> FnOnce(V1Result<LibraryContext<'a>>) + Send>;

pub(super) struct Work {
    scope: RestScope,
    access: Access,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    complete: Completion,
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) async fn submit<T, F>(scope: RestScope, access: Access, run: F) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>) -> V1Result<T> + Send + 'static,
{
    let deadline = Instant::now() + REQUEST_WAIT;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(Arc::clone(&cancelled));
    let receiver = enqueue(scope, access, deadline, cancelled, run)?;
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver).await {
        Ok(Ok(value)) => value,
        _ => Err(transport::unavailable()),
    }
}

/// Bound queue admission without timing out a started local inference.
pub(super) async fn submit_recall<T, F>(scope: RestScope, run: F) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>) -> V1Result<T> + Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(Arc::clone(&cancelled));
    enqueue(scope, Access::Read, Instant::now() + REQUEST_WAIT, cancelled, run)?
        .await
        .map_err(|_| transport::unavailable())?
}

/// A background phase waits for the active dispatcher to finish or reject
/// its work. The deadline bounds queueing, including while draining.
pub(super) fn submit_sync<T, F>(scope: RestScope, run: F) -> V1Result<T>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>) -> V1Result<T> + Send + 'static,
{
    enqueue(
        scope,
        Access::Write,
        Instant::now() + REQUEST_WAIT,
        Arc::new(AtomicBool::new(false)),
        run,
    )?.blocking_recv().map_err(|_| transport::unavailable())?
}

/// Queue a local operation whose ticket reports the result after HTTP acceptance.
pub(super) fn enqueue_write<F>(scope: RestScope, run: F) -> V1Result<()>
where
    F: for<'a> FnOnce(&LibraryContext<'a>) -> V1Result<()> + Send + 'static,
{
    enqueue(scope, Access::Write, Instant::now() + REQUEST_WAIT,
        Arc::new(AtomicBool::new(false)), run)?;
    Ok(())
}

fn enqueue<T, F>(
    scope: RestScope,
    access: Access,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    run: F,
) -> V1Result<oneshot::Receiver<V1Result<T>>>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&LibraryContext<'a>) -> V1Result<T> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    owner_control::enqueue_rest(Work {
        scope,
        access,
        deadline,
        cancelled,
        complete: Box::new(move |context| {
            let _ = sender.send(context.and_then(|context| run(&context)));
        }),
    })?;
    Ok(receiver)
}

pub(super) fn service(work: Work, store: Option<&LocalStore>, ownership: &LibraryLock) {
    let Work {
        scope,
        access,
        deadline,
        cancelled,
        complete,
    } = work;
    let mut complete = Some(complete);
    let expired = || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline;
    if !expired() {
        let _ = crate::rpc_serve::with_verified_owner_store_scope(
            |account, profile, instance, generation, vault_epoch, library| {
                let result = (|| {
                    if expired() {
                        return Err(transport::unavailable());
                    }
                    if !scope.matches(account, profile, instance, generation, vault_epoch) {
                        return Err(transport::unauthorized());
                    }
                    let store = store
                        .filter(|_| ownership.protects(profile).unwrap_or(false))
                        .ok_or_else(transport::unavailable)?;
                    if matches!(access, Access::Write) {
                        respire_app::service::ensure_writable().map_err(|_| {
                            Problem::new(
                                StatusCode::FORBIDDEN,
                                "write_disabled",
                                "Workspace is read-only",
                            )
                        })?;
                    }
                    if expired() {
                        return Err(transport::unavailable());
                    }
                    Ok(LibraryContext {
                        store,
                        ownership,
                        scope: &scope,
                        deadline,
                        cancelled: &cancelled,
                        library,
                    })
                })();
                if let Some(done) = complete.take() {
                    done(result);
                }
            },
        );
    }
    if let Some(done) = complete {
        done(Err(transport::unavailable()));
    }
}
