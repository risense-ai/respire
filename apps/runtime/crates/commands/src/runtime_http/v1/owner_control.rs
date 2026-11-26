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

pub(super) mod model_work;

static ACTIVE: Mutex<Option<NativeOwner>> = Mutex::new(None);
static QUEUE: OnceLock<mpsc::SyncSender<DispatchWork>> = OnceLock::new();
static DISPATCH_LIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static AUTH_BUDGET: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);
const WORK_LIMIT: usize = 16;
const WORK_DEADLINE: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: Duration = Duration::from_millis(250);

enum DispatchWork {
    SoftLogout(SoftLogout),
    Scoped(Work),
    Local(super::rest_dispatch::Work),
    ModelSwitch(Box<super::operations::model_switch::PendingSwitch>),
}

/// Internal queue entry. Only a purpose-authorized host request may construct one.
pub(crate) struct SoftLogout {
    owner: OwnerBinding,
    id: String,
    key: String,
    params: respire_protocol::identity_write_control::HostIdentityWriteParams,
    reply: oneshot::Sender<anyhow::Result<()>>,
}

impl SoftLogout {
    fn begin_drain(&self, held: &LibraryLock) -> bool {
        !self.reply.is_closed() && with_active(|native, live| {
            use sha2::{Digest, Sha256};
            let account_id = format!("{:x}", Sha256::digest(live.account.as_bytes()));
            let matches = native.matches(live) && *live == self.owner
                && self.params.binding.profile == live.canonical_profile
                && self.params.binding.launch_id == live.instance.to_string()
                && self.params.binding.expected_account_id.as_deref() == Some(account_id.as_str())
                && held.protects(&live.canonical_profile).unwrap_or(false);
            // Fence new unlocks before releasing the verified owner scope.
            Ok(matches && crate::rpc_worker::STOPPING.compare_exchange(false, true,
                std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_ok())
        }).unwrap_or(false)
    }

    pub(crate) fn finish(self, held: &LibraryLock, clean: bool) {
        let outcome = (|| {
            anyhow::ensure!(clean, "logout resource cleanup incomplete");
            let receipt = respire_app::login_transaction::serving_soft_logout(
                held, &self.id, &self.key, &self.params)?;
            anyhow::ensure!(receipt.operation_id == self.id && receipt.outcome ==
                respire_protocol::identity_write_control::HostIdentityWriteOutcome::Committed,
                "logout journal did not confirm the admitted operation");
            Ok(())
        })();
        let _ = self.reply.send(outcome);
    }
}

struct Work {
    call: OwnerCall,
    ticket: LeaseTicket,
    deadline: Instant,
    reply: oneshot::Sender<V1Result<Response>>,
}

pub(crate) struct OwnerDispatch {
    receiver: mpsc::Receiver<DispatchWork>,
    model_receiver: mpsc::Receiver<model_work::Work>,
    last_refresh: Option<Instant>,
    switching: Option<super::operations::model_switch::RunningSwitch>,
}

pub(crate) fn start_dispatch() -> Option<OwnerDispatch> {
    let (sender, receiver) = mpsc::sync_channel(WORK_LIMIT);
    QUEUE.set(sender).ok()?;
    DISPATCH_LIVE.store(true, std::sync::atomic::Ordering::Release);
    Some(OwnerDispatch {
        receiver,
        model_receiver: model_work::start()?,
        last_refresh: None,
        switching: None,
    })
}

impl OwnerDispatch {
    pub(crate) fn tick(&mut self, store: Option<&LocalStore>, ownership: &LibraryLock) -> Option<SoftLogout> {
        let now = Instant::now();
        if self
            .last_refresh
            .is_none_or(|last| now.duration_since(last) >= REFRESH_INTERVAL)
        {
            self.last_refresh = Some(now);
            refresh(store.is_some(), ownership);
        }
        match self.receiver.try_recv() {
            Ok(DispatchWork::SoftLogout(work)) => {
                if work.begin_drain(ownership) { return Some(work); }
            }
            Ok(DispatchWork::Scoped(work)) => service_work(work, store, ownership),
            Ok(DispatchWork::Local(work)) => super::rest_dispatch::service(work, store, ownership),
            Ok(DispatchWork::ModelSwitch(work)) if self.switching.is_none() => {
                self.switching = work.launch(store, ownership);
            }
            Ok(DispatchWork::ModelSwitch(_)) | Err(_) => {}
        }
        if self.switching.as_mut().is_some_and(|switch| switch.tick(store, ownership)) {
            self.switching = None;
        }
        if let Ok(work) = self.model_receiver.try_recv() {
            model_work::service(work, store, ownership);
        }
        None
    }
}

impl Drop for OwnerDispatch {
    fn drop(&mut self) {
        DISPATCH_LIVE.store(false, std::sync::atomic::Ordering::Release);
        revoke();
    }
}

pub(super) fn dispatch_live() -> bool {
    DISPATCH_LIVE.load(std::sync::atomic::Ordering::Acquire)
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

pub(super) fn enqueue_rest(work: super::rest_dispatch::Work) -> V1Result<()> {
    let sender = QUEUE.get().ok_or_else(super::transport::unavailable)?;
    sender.try_send(DispatchWork::Local(work)).map_err(|_| busy())
}

pub(super) fn enqueue_model_switch(work: super::operations::model_switch::PendingSwitch) -> V1Result<()> {
    let sender = QUEUE.get().ok_or_else(super::transport::unavailable)?;
    sender.try_send(DispatchWork::ModelSwitch(Box::new(work))).map_err(|_| busy())
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
        .try_send(DispatchWork::Scoped(Work {
            call,
            ticket,
            deadline,
            reply,
        }))
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
        |account, profile, instance, generation, vault_epoch, library| {
            let result = (|| {
                let store = store
                    .filter(|_| ownership.protects(profile).unwrap_or(false))
                    .ok_or_else(unavailable)?;
                let live = binding(account, profile, instance, generation, vault_epoch);
                with_bound_native(&live, |owner, live| {
                    let action = call.action();
                    owner.recheck(live, &ticket, action).map_err(|_| unauthorized())?;
                    let current = || reply.as_ref().is_some_and(|sender| !sender.is_closed())
                        && Instant::now() < deadline;
                    if !current() {
                        return Err(busy());
                    }
                    let Some(space) = library.active_device_space(ownership)
                        .map_err(|_| super::transport::unavailable())?
                    else {
                        owner.recheck(live, &ticket, action).map_err(|_| unauthorized())?;
                        if !current() {
                            return Err(busy());
                        }
                        return call.run(store, false);
                    };
                    // Same verified Vault, account/profile/instance/generation
                    // and exact scoped lease; no parent fallback on any child error.
                    library.with_device_space_store(ownership, &space, current, |child_store| {
                        owner.recheck(live, &ticket, action).map_err(|_| unauthorized())?;
                        if !current() {
                            return Err(busy());
                        }
                        call.run(child_store, true)
                    }).map_err(|_| super::transport::unavailable())?
                        .ok_or_else(super::transport::unavailable)?
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

/// Handle the bodyless logout request from a verified native owner.
pub(super) async fn host_logout(request: axum::extract::Request, peer: std::net::SocketAddr,
    bound: std::net::SocketAddr) -> V1Result<Response> {
    use respire_protocol::{ControlBinding, HostAuthIdentity, identity_write_control::{
        HostIdentityWriteOperation, HostIdentityWriteParams, HOST_IDENTITY_WRITE_SCHEMA_V}};
    use sha2::{Digest, Sha256};
    let (parts, body) = request.into_parts();
    super::owner_request::admit_selected(&parts, peer, bound)?;
    if parts.method != axum::http::Method::POST || parts.uri.path() != "/v1/host/transactions/logout"
        || parts.uri.query().is_some() || parts.headers.contains_key(header::COOKIE)
        || parts.headers.contains_key(header::ORIGIN) { return Err(unauthorized()); }
    let field = |name| super::owner_request::single(&parts.headers, name)
        .ok().flatten().ok_or_else(unauthorized);
    let id = uuid::Uuid::parse_str(field("x-respire-operation-id")?).map_err(|_| unauthorized())?;
    let proof = field("x-respire-host-proof")?.to_owned();
    let mut revision = [0; 32];
    hex::decode_to_slice(field("x-respire-session-revision")?, &mut revision).map_err(|_| unauthorized())?;
    tokio::time::timeout(Duration::from_secs(1), axum::body::to_bytes(body, 0)).await
        .map_err(|_| busy())?.map_err(|_| Problem::new(StatusCode::BAD_REQUEST, "invalid_body", "Expected empty body"))?;
    let (send, receive) = oneshot::channel();
    bounded_blocking(move || with_active(|native, live| {
        if !native.matches(live) { return Err(unauthorized()); }
        native.authorize_logout(live, id, &revision, &proof, wall_ms().ok_or_else(unavailable)?)
            .map_err(|_| unauthorized())?;
        let account_id = format!("{:x}", Sha256::digest(live.account.as_bytes()));
        let identity = HostAuthIdentity::Bound { account_id: account_id.clone(), session_sha256: hex::encode(revision) };
        let root = respire_app::service::main_data_dir().canonicalize().map_err(|_| unavailable())?;
        respire_app::login_transaction::verify_locked_auth_identity(&root, &live.canonical_profile, &identity)
            .map_err(|_| unauthorized())?;
        let params = HostIdentityWriteParams { schema_version: HOST_IDENTITY_WRITE_SCHEMA_V,
            binding: ControlBinding { installation_root: root, profile: live.canonical_profile.clone(),
                launch_id: live.instance.to_string(), expected_account_id: Some(account_id) },
            identity: identity.clone(), source_identity: identity, operation: HostIdentityWriteOperation::Logout { full: false } };
        QUEUE.get().ok_or_else(unavailable)?.try_send(DispatchWork::SoftLogout(SoftLogout {
            owner: live.clone(), id: id.to_string(), key: id.to_string(), params, reply: send,
        })).map_err(|_| busy())
    })).await?;
    tokio::time::timeout(Duration::from_secs(30), receive).await.map_err(|_| busy())?
        .map_err(|_| busy())?.map_err(|_| busy())?;
    use axum::response::IntoResponse;
    Ok(([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], r#"{"status":"completed"}"#).into_response())
}
