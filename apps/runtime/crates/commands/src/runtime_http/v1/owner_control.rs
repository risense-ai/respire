//! Live scoped owner authority and bounded work for the existing serving dispatcher.
use axum::{
    http::{header, request::Parts, StatusCode},
    response::Response,
};
use respire_app::{lock::LibraryLock, transport::local::LocalStore};
use std::{
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

use super::{
    owner_native::NativeOwner,
    owner_operations::OwnerCall,
    owner_scope::{LeaseTicket, OwnerAction, OwnerBinding},
    problem::{Problem, V1Result},
};

static ACTIVE: Mutex<Option<NativeOwner>> = Mutex::new(None);
static QUEUE: OnceLock<mpsc::SyncSender<Work>> = OnceLock::new();
static AUTH_BUDGET: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);
const WORK_LIMIT: usize = 16;
const WORK_DEADLINE: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: Duration = Duration::from_millis(250);

struct Work {
    call: OwnerCall,
    ticket: LeaseTicket,
    deadline: Instant,
    reply: oneshot::Sender<V1Result<Response>>,
}

pub(crate) struct OwnerDispatch {
    receiver: mpsc::Receiver<Work>,
    last_refresh: Option<Instant>,
}

pub(crate) fn start_dispatch() -> Option<OwnerDispatch> {
    let (sender, receiver) = mpsc::sync_channel(WORK_LIMIT);
    QUEUE.set(sender).ok()?;
    Some(OwnerDispatch {
        receiver,
        last_refresh: None,
    })
}

impl OwnerDispatch {
    pub(crate) fn tick(&mut self, store: Option<&LocalStore>, ownership: &LibraryLock) {
        let now = Instant::now();
        if self
            .last_refresh
            .is_none_or(|last| now.duration_since(last) >= REFRESH_INTERVAL)
        {
            self.last_refresh = Some(now);
            refresh(store.is_some(), ownership);
        }
        if let Ok(work) = self.receiver.try_recv() {
            service_work(work, store, ownership);
        }
    }
}

impl Drop for OwnerDispatch {
    fn drop(&mut self) {
        revoke();
    }
}

pub(crate) fn revoke() {
    if let Ok(mut slot) = ACTIVE.lock() {
        *slot = None;
    }
}

fn refresh(ready: bool, ownership: &LibraryLock) {
    if !ready
        || !crate::rpc_serve::store_ready()
        || crate::rpc_worker::STOPPING.load(std::sync::atomic::Ordering::Acquire)
    {
        revoke();
        return;
    }
    let observed = crate::rpc_serve::try_verified_owner_scope(
        |account, profile, instance, generation, vault_epoch| {
            if !ownership.protects(profile).unwrap_or(false) {
                revoke();
                return;
            }
            let live = binding(account, profile, instance, generation, vault_epoch);
            let base = crate::rpc_worker::runtime_dir();
            if let Ok(mut slot) = ACTIVE.lock() {
                if let Some(state) = slot.as_mut() {
                    if state.synchronize(&live).is_err() || !state.provider_live(&base) {
                        *slot = None;
                    }
                } else {
                    *slot = NativeOwner::establish(&base, live).ok();
                }
            }
        },
    );
    if matches!(observed, Ok(None)) {
        revoke();
    }
}

fn binding(
    account: &str,
    profile: &std::path::Path,
    instance: uuid::Uuid,
    generation: u64,
    vault_epoch: uuid::Uuid,
) -> OwnerBinding {
    OwnerBinding {
        account: account.to_owned(),
        canonical_profile: profile.to_owned(),
        instance,
        generation,
        vault_epoch,
    }
}

pub(super) fn with_active<T>(
    run: impl FnOnce(&mut NativeOwner, &OwnerBinding) -> V1Result<T>,
) -> V1Result<T> {
    crate::rpc_serve::with_verified_owner_scope(
        |account, profile, instance, generation, vault_epoch| {
            with_bound_native(
                &binding(account, profile, instance, generation, vault_epoch),
                run,
            )
        },
    )
    .unwrap_or_else(|| Err(unavailable()))
}

fn with_bound_native<T>(
    live: &OwnerBinding,
    run: impl FnOnce(&mut NativeOwner, &OwnerBinding) -> V1Result<T>,
) -> V1Result<T> {
    let mut owned = ACTIVE.lock().map_err(|_| unavailable())?;
    let owner = owned.as_mut().ok_or_else(unavailable)?;
    if !owner.provider_live(&crate::rpc_worker::runtime_dir()) {
        *owned = None;
        return Err(unavailable());
    }
    run(owner, live)
}

pub(super) fn capable() -> bool {
    QUEUE.get().is_some() && with_active(|owner, live| Ok(owner.matches(live))).unwrap_or(false)
}

pub(super) fn admit_action(parts: &Parts, action: OwnerAction) -> V1Result<LeaseTicket> {
    with_active(|owner, live| {
        if !owner.matches(live) {
            return Err(unavailable());
        }
        let (scheme, token) = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .ok_or_else(unauthorized)?;
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return Err(unauthorized());
        }
        owner
            .ticket(live, token, action)
            .map_err(|_| unauthorized())
    })
}

/// No unbounded background queue of requests that hold owner/session I/O.
pub(super) async fn bounded_blocking<T: Send + 'static>(
    run: impl FnOnce() -> V1Result<T> + Send + 'static,
) -> V1Result<T> {
    let permit = AUTH_BUDGET.try_acquire().map_err(|_| busy())?;
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        run()
    });
    match tokio::time::timeout(WORK_DEADLINE, worker).await {
        Ok(Ok(result)) => result,
        _ => Err(busy()),
    }
}

pub(super) async fn submit(
    call: OwnerCall,
    ticket: LeaseTicket,
    deadline: Instant,
) -> V1Result<Response> {
    if Instant::now() >= deadline {
        return Err(busy());
    }
    let sender = QUEUE.get().ok_or_else(unavailable)?;
    let (reply, receiver) = oneshot::channel();
    sender
        .try_send(Work {
            call,
            ticket,
            deadline,
            reply,
        })
        .map_err(|_| busy())?;
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver).await {
        Ok(Ok(result)) => result,
        _ => Err(busy()),
    }
}

fn service_work(work: Work, store: Option<&LocalStore>, ownership: &LibraryLock) {
    let Work {
        call,
        ticket,
        deadline,
        reply,
    } = work;
    if reply.is_closed() || Instant::now() >= deadline {
        let _ = reply.send(Err(busy()));
        return;
    }
    let mut reply = Some(reply);
    let _ = crate::rpc_serve::with_verified_owner_store_scope(
        |account, profile, instance, generation, vault_epoch| {
            let result = (|| {
                let store = store
                    .filter(|_| ownership.protects(profile).unwrap_or(false))
                    .ok_or_else(unavailable)?;
                let live = binding(account, profile, instance, generation, vault_epoch);
                with_bound_native(&live, |owner, live| {
                    owner
                        .recheck(live, &ticket, call.action())
                        .map_err(|_| unauthorized())?;
                    if reply.as_ref().is_none_or(oneshot::Sender::is_closed)
                        || Instant::now() >= deadline
                    {
                        return Err(busy());
                    }
                    call.run(store)
                })
            })();
            if let Some(sender) = reply.take() {
                let _ = sender.send(result);
            }
        },
    );
    if let Some(sender) = reply {
        let _ = sender.send(Err(busy()));
    }
}

pub(super) fn wall_ms() -> Option<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis(),
    )
    .ok()
}

pub(super) fn unavailable() -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "route_not_found", "Route not found")
}

fn unauthorized() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Authentication required",
    )
    .bearer_challenge()
}

fn busy() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime_unavailable",
        "Runtime unavailable",
    )
}
