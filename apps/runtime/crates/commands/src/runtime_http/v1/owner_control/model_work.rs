//! Separate, single-slot typed jobs for purpose-bound private model operations.
//! This receives only real NativeOwner grants, never general RPC bearer leases.
use super::{
    binding, bounded_blocking, busy, unauthorized, unavailable, wall_ms, with_active,
    with_bound_native,
};
use crate::model_removal::{self, serving::StagedModelTransfer, Binding, Receipt, Request};
use respire_app::{lock::LibraryLock, transport::local::LocalStore};
use respire_spawn::owner_credential::model_proof::{valid_model_id, ModelAction};
use std::{
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Semaphore, SemaphorePermit};

use crate::runtime_http::v1::owner_scope::OwnerBinding;
use crate::runtime_http::v1::problem::{Problem, V1Result};
use axum::http::StatusCode;

const EXECUTION_LIMIT: Duration = Duration::from_secs(15);
static MODEL_QUEUE: OnceLock<mpsc::SyncSender<Work>> = OnceLock::new();
const VERIFY_LIMIT: Duration = Duration::from_secs(120);
static MODEL_ASSET_IO: Semaphore = Semaphore::const_new(1);

pub(super) struct Work {
    owner: OwnerBinding,
    operation: Operation,
    deadline: Instant,
    reply: oneshot::Sender<V1Result<Reply>>,
}

enum Operation {
    PrepareRemoval(String),
    Remove(model_removal::VerifiedRemoval),
    Prepare(String),
    Publish(model_removal::serving::SealedModelTransfer),
}

enum Reply {
    PendingRemoval(model_removal::PendingRemoval),
    Removed(Receipt),
    Prepared(StagedModelTransfer),
    Published,
}

pub(in crate::runtime_http::v1) struct Upload {
    owner: OwnerBinding,
    _slot: SemaphorePermit<'static>,
    pub(in crate::runtime_http::v1) transfer: StagedModelTransfer,
}

pub(in crate::runtime_http::v1) struct SealedUpload {
    owner: OwnerBinding,
    _slot: SemaphorePermit<'static>,
    transfer: model_removal::serving::SealedModelTransfer,
}

impl Upload {
    pub(in crate::runtime_http::v1) fn seal(self) -> V1Result<SealedUpload> {
        let transfer = self.transfer.seal().map_err(sanitize_error)?;
        Ok(SealedUpload {
            owner: self.owner,
            _slot: self._slot,
            transfer,
        })
    }
}

pub(super) fn start() -> Option<mpsc::Receiver<Work>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    MODEL_QUEUE.set(sender).ok()?;
    Some(receiver)
}

/// Require a NativeOwner grant for a specific model and operation. Callers must
/// have checked loopback/host, Origin and the request envelope first.
async fn authorize(model_id: &str, proof: String, action: ModelAction) -> V1Result<OwnerBinding> {
    if !valid_model_id(model_id) {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_model_id",
            "Invalid model identifier",
        ));
    }
    let model_id = model_id.to_owned();
    bounded_blocking(move || {
        with_active(|owner, live| {
            let now = wall_ms().ok_or_else(unavailable)?;
            owner
                .consume_model(live, action, &model_id, &proof, now)
                .map_err(|_| unauthorized())?;
            Ok(live.clone())
        })
    })
    .await
}

async fn queue(owner: OwnerBinding, operation: Operation) -> V1Result<Reply> {
    let sender = MODEL_QUEUE.get().ok_or_else(unavailable)?;
    let deadline = Instant::now() + EXECUTION_LIMIT;
    let (reply, receiver) = oneshot::channel();
    sender
        .try_send(Work {
            owner,
            operation,
            deadline,
            reply,
        })
        .map_err(|_| busy())?;
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver).await {
        Ok(Ok(result)) => result,
        _ => Err(busy()),
    }
}

pub(in crate::runtime_http::v1) async fn admit_remove(
    model_id: String,
    proof: String,
) -> V1Result<Receipt> {
    let owner = authorize(&model_id, proof, ModelAction::Remove).await?;
    let slot = MODEL_ASSET_IO.try_acquire().map_err(|_| busy())?;
    let pending = match queue(owner.clone(), Operation::PrepareRemoval(model_id)).await? {
        Reply::PendingRemoval(value) => value,
        _ => return Err(unavailable()),
    };
    let deadline = Instant::now() + VERIFY_LIMIT;
    let (reply, receive) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let verified = pending.verify(|| {
            anyhow::ensure!(
                !reply.is_closed()
                    && Instant::now() < deadline
                    && !crate::rpc_worker::STOPPING.load(std::sync::atomic::Ordering::Acquire),
                "model_operation_cancelled"
            );
            Ok(())
        });
        let _ = reply.send((verified.map_err(sanitize_error), slot));
    });
    let (verified, _slot) =
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receive)
            .await
            .map_err(|_| busy())?
            .map_err(|_| unavailable())?;
    match queue(owner, Operation::Remove(verified?)).await? {
        Reply::Removed(receipt) => Ok(receipt),
        _ => Err(unavailable()),
    }
}

pub(in crate::runtime_http::v1) async fn prepare_upload(
    model_id: String,
    proof: String,
) -> V1Result<Upload> {
    let owner = authorize(&model_id, proof, ModelAction::Publish).await?;
    let slot = MODEL_ASSET_IO.try_acquire().map_err(|_| busy())?;
    match queue(owner.clone(), Operation::Prepare(model_id)).await? {
        Reply::Prepared(transfer) => Ok(Upload {
            owner,
            transfer,
            _slot: slot,
        }),
        _ => Err(unavailable()),
    }
}

pub(in crate::runtime_http::v1) async fn publish_upload(upload: SealedUpload) -> V1Result<()> {
    match queue(upload.owner, Operation::Publish(upload.transfer)).await? {
        Reply::Published => Ok(()),
        _ => Err(unavailable()),
    }
}

pub(super) fn service(work: Work, store: Option<&LocalStore>, ownership: &LibraryLock) {
    let Work {
        owner,
        operation,
        deadline,
        reply,
    } = work;
    if reply.is_closed() || Instant::now() >= deadline {
        let _ = reply.send(Err(busy()));
        return;
    }
    let result = crate::rpc_serve::with_verified_owner_store_scope(
        |account, profile, instance, generation, vault_epoch| {
            let live = binding(account, profile, instance, generation, vault_epoch);
            if live != owner {
                return Err(unauthorized());
            }
            with_bound_native(&live, |native, live| {
                native.matches(live).then_some(()).ok_or_else(unauthorized)
            })?;
            let store = store
                .filter(|_| ownership.protects(profile).unwrap_or(false))
                .ok_or_else(unavailable)?;
            let bound = Binding {
                owner: account.to_owned(),
                canonical_profile: profile.to_owned(),
                instance,
                generation,
                vault_epoch,
            };
            let checkpoint = || {
                anyhow::ensure!(
                    !reply.is_closed()
                        && Instant::now() < deadline
                        && !crate::rpc_worker::STOPPING.load(std::sync::atomic::Ordering::Acquire),
                    "model_operation_cancelled"
                );
                Ok(())
            };
            checkpoint().map_err(|_| busy())?;
            match operation {
                Operation::PrepareRemoval(model_id) => {
                    model_removal::serving::prepare_removal_in_fenced_scope(
                        &bound,
                        store,
                        ownership,
                        Request { model_id },
                        checkpoint,
                    )
                    .map(Reply::PendingRemoval)
                    .map_err(sanitize_error)
                }
                Operation::Remove(removal) => {
                    let mut receipt = model_removal::serving::commit_removal_in_fenced_scope(
                        &bound, store, ownership, removal, checkpoint,
                    )
                    .map_err(sanitize_error)?;
                    if receipt.cleanup_error.is_some() {
                        receipt.cleanup_error = Some("model_cleanup_pending".into());
                    }
                    Ok(Reply::Removed(receipt))
                }
                Operation::Prepare(model_id) => {
                    model_removal::serving::prepare_pair_in_fenced_scope(
                        &bound, ownership, &model_id,
                    )
                    .map(Reply::Prepared)
                    .map_err(sanitize_error)
                }
                Operation::Publish(transfer) => {
                    model_removal::serving::publish_pair_in_fenced_scope(
                        &bound, ownership, transfer, checkpoint,
                    )
                    .map(|()| Reply::Published)
                    .map_err(sanitize_error)
                }
            }
        },
    )
    .unwrap_or_else(|| Err(busy()));
    let _ = reply.send(result);
}

fn sanitize_error(error: anyhow::Error) -> Problem {
    if error
        .chain()
        .any(|cause| cause.to_string() == "model_operation_cancelled")
    {
        return busy();
    }
    let label = error.to_string();
    let (status, code, title) = match label.as_str() {
        "model_in_use" | "shared_asset" => {
            (StatusCode::CONFLICT, "model_in_use", "Model still in use")
        }
        "unknown_managed_model" | "unknown_model" => (
            StatusCode::NOT_FOUND,
            "model_not_found",
            "Managed model not found",
        ),
        "model_operation_cancelled" => return busy(),
        _ => (
            StatusCode::CONFLICT,
            "managed_model_unavailable",
            "Managed model unavailable",
        ),
    };
    Problem::new(status, code, title)
}
