//! Streaming backup import into bounded leaf-locked pending custody.
//! Database writes wait for separate native owner confirmation via /api/runtime/private-backup/confirm.
use super::{
    state::{self, Entry, Phase},
    super::{owner_scope::OwnerBinding, reads, rest_dispatch, transport, Problem, V1Result},
};
use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    response::Response,
};
use respire_app::snapshot::{backup_archive::BackupArchiveHeader, Manifest};
use sha2::{Digest, Sha256};
#[path = "backup_import_stream.rs"]
mod stream;
use std::{
    collections::BTreeMap,
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

const TOTAL: Duration = Duration::from_secs(30 * 60);
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING: usize = 2;
static UPLOADS: Semaphore = Semaphore::const_new(2);
static PENDING_IMPORTS: OnceLock<Mutex<BTreeMap<Uuid, PendingImport>>> = OnceLock::new();
#[cfg(test)]
static BEFORE_RESTORE_COMMIT: OnceLock<Mutex<Option<Box<dyn FnOnce() + Send>>>> = OnceLock::new();

#[cfg(test)]
pub(in crate::runtime_http::v1) fn set_before_restore_hook(hook: Box<dyn FnOnce() + Send>) {
    let slot = BEFORE_RESTORE_COMMIT.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(hook);
    }
}

struct PendingImport {
    owner: transport::RestScope,
    selected: Option<String>,
    target: PathBuf,
    target_sha256: [u8; 32],
    archive_sha256: [u8; 32],
    staged: tempfile::TempPath,
    manifest: Manifest,
    job: state::Job,
    created_at: Instant,
}

pub(in crate::runtime_http::v1) struct LocalRestoreConfirmation {
    pub(in crate::runtime_http::v1) operation_id: Uuid,
    pub(in crate::runtime_http::v1) archive_sha256: [u8; 32],
    pub(in crate::runtime_http::v1) target_sha256: [u8; 32],
    pub(in crate::runtime_http::v1) confirmed_owner: OwnerBinding,
}

fn pending_map() -> &'static Mutex<BTreeMap<Uuid, PendingImport>> {
    PENDING_IMPORTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn is_entry_stale(
    owner: &transport::RestScope,
    entry: &Entry,
    created_at: Instant,
    now: Instant,
) -> bool {
    now.saturating_duration_since(created_at) >= PENDING_TTL
        || entry.cancelled().unwrap_or(true)
        || !transport::scope_is_live(owner)
}

/// Strictly leaf-locked: inspects candidates under PENDING_IMPORTS, drops lock before checking
/// session/operation state, then removes expired entries before dropping temp files or finishing jobs.
pub(in crate::runtime_http::v1) fn reap_stale() {
    let snapshot: Vec<(Uuid, transport::RestScope, Arc<Entry>, Instant)> = {
        let Ok(guard) = pending_map().lock() else {
            return;
        };
        guard
            .iter()
            .map(|(id, entry)| (*id, entry.owner.clone(), entry.job.entry(), entry.created_at))
            .collect()
    };
    if snapshot.is_empty() {
        return;
    }
    let now = Instant::now();
    let stale_ids: Vec<Uuid> = snapshot
        .into_iter()
        .filter(|(_, owner, entry, created_at)| is_entry_stale(owner, entry, *created_at, now))
        .map(|(id, _, _, _)| id)
        .collect();
    if stale_ids.is_empty() {
        return;
    }
    let removed: Vec<PendingImport> = {
        let Ok(mut guard) = pending_map().lock() else {
            return;
        };
        stale_ids.into_iter().filter_map(|id| guard.remove(&id)).collect()
    };
    for PendingImport { staged, job, .. } in removed {
        drop(staged);
        job.finish(Phase::Cancelled);
    }
}

/// Must only be called with no SessionScope or Entry locks held.
pub(super) fn discard_cancelled(id: Uuid) {
    let removed = pending_map()
        .lock()
        .ok()
        .and_then(|mut guard| guard.remove(&id));
    if let Some(PendingImport { staged, job, .. }) = removed {
        drop(staged);
        job.finish(Phase::Cancelled);
    }
}

pub(in crate::runtime_http::v1) async fn enqueue(
    bound: SocketAddr,
    peer: SocketAddr,
    request: Request,
) -> V1Result<Response> {
    reap_stale();
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST {
        return Err(super::super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(transport::invalid());
    }
    let mut query = transport::query(parts.uri.query())?;
    let selected = query.remove("space_id");
    if !query.is_empty() || selected.as_deref().is_some_and(str::is_empty) {
        return Err(transport::invalid());
    }
    let permit = UPLOADS.try_acquire().map_err(|_| transport::unavailable())?;
    if pending_map()
        .lock()
        .map_err(|_| transport::unavailable())?
        .len()
        >= MAX_PENDING
    {
        return Err(transport::unavailable());
    }
    let target = resolve_target(selected.clone(), scope.clone()).await?;
    let owner = scope.clone();
    let deadline = tokio::time::Instant::now() + TOTAL;
    let (sender, mut database) = stream::Reader::bridge();
    let target_for_stage = target.clone();
    let stage = tokio::task::spawn_blocking(move || {
        let target_text = target_for_stage.to_str().ok_or_else(transport::invalid)?;
        let target_sha256: [u8; 32] = Sha256::digest(target_text.as_bytes()).into();
        let expected_source = hex::encode(target_sha256);
        let header = BackupArchiveHeader::read(&mut database).map_err(|_| transport::invalid())?;
        let authenticated = crate::rpc_serve::with_verified_owner_library_scope(
            |account, profile, instance, generation, epoch, library| {
                if !scope.matches(account, profile, instance, generation, epoch) {
                    return Err(transport::unauthorized());
                }
                let proof = library
                    .authenticate_backup_import_header(header)
                    .map_err(|_| transport::invalid())?;
                if proof.manifest().source_library != expected_source {
                    return Err(transport::invalid());
                }
                Ok(proof)
            },
        )
        .ok_or_else(transport::unavailable)??;
        let mut temp = tempfile::Builder::new()
            .prefix(".backup-archive-")
            .suffix(".pending")
            .tempfile_in(&scope.canonical_profile)
            .map_err(|_| transport::unavailable())?;
        let manifest = authenticated
            .copy_to_stage(&mut database, temp.as_file_mut(), || {
                anyhow::ensure!(tokio::time::Instant::now() < deadline, "upload deadline expired");
                anyhow::ensure!(transport::scope_is_live(&scope), "owner scope expired");
                Ok(())
            })
            .map_err(|_| transport::invalid())?;
        temp.as_file_mut().flush().map_err(|_| transport::unavailable())?;
        temp.as_file().sync_all().map_err(|_| transport::unavailable())?;
        let archive_sha256 = database.verified_digest()?;
        let staged = temp.into_temp_path();
        Ok::<_, Problem>((staged, manifest, archive_sha256, target_sha256))
    });
    let (staged, manifest, archive_sha256, target_sha256) = tokio::time::timeout_at(deadline, async {
        let fed = stream::feed(body, sender).await;
        let staged = stage.await.map_err(|_| transport::unavailable())??;
        fed?;
        Ok::<_, Problem>(staged)
    })
    .await
    .map_err(|_| transport::invalid())??;
    drop(permit);
    if resolve_target(selected.clone(), owner.clone()).await? != target {
        return Err(transport::invalid());
    }
    reap_stale();
    if pending_map()
        .lock()
        .map_err(|_| transport::unavailable())?
        .len()
        >= MAX_PENDING
    {
        return Err(transport::unavailable());
    }
    let job = transport::with_scope(&owner, || {
        state::registry().begin(owner.clone(), "backup_import")
    })?;
    let id = Uuid::parse_str(&job.id()?).map_err(|_| transport::unavailable())?;
    let accepted = job.accepted()?;
    let receipt = transport::json_response(StatusCode::ACCEPTED, accepted)?;
    let overflow = {
        let mut guard = pending_map().lock().map_err(|_| transport::unavailable())?;
        if guard.len() >= MAX_PENDING {
            Some((staged, job))
        } else {
            guard.insert(
                id,
                PendingImport {
                    owner,
                    selected,
                    target,
                    target_sha256,
                    archive_sha256,
                    staged,
                    manifest,
                    job,
                    created_at: Instant::now(),
                },
            );
            None
        }
    };
    if let Some((staged, job)) = overflow {
        drop(staged);
        job.finish(Phase::Cancelled);
        return Err(transport::unavailable());
    }
    Ok(receipt)
}

fn mismatch() -> Problem {
    Problem::new(
        StatusCode::CONFLICT,
        "backup_confirmation_mismatch",
        "Backup restore confirmation does not match staged archive",
    )
}

fn matches_confirmation(pending: &PendingImport, confirmation: &LocalRestoreConfirmation) -> bool {
    let owner = &confirmation.confirmed_owner;
    pending.archive_sha256 == confirmation.archive_sha256
        && pending.target_sha256 == confirmation.target_sha256
        && pending.owner.matches(
            &owner.account,
            &owner.canonical_profile,
            owner.instance,
            owner.generation,
            owner.vault_epoch,
        )
}

pub(in crate::runtime_http::v1) async fn confirm_local_restore(
    confirmation: LocalRestoreConfirmation,
) -> V1Result<()> {
    reap_stale();
    let (owner_scope, entry, created_at) = {
        let guard = pending_map().lock().map_err(|_| transport::unavailable())?;
        let pending = guard
            .get(&confirmation.operation_id)
            .ok_or_else(state::not_found)?;
        if !matches_confirmation(pending, &confirmation) {
            return Err(mismatch());
        }
        (pending.owner.clone(), pending.job.entry(), pending.created_at)
    };
    if is_entry_stale(&owner_scope, &entry, created_at, Instant::now()) {
        discard_cancelled(confirmation.operation_id);
        return Err(mismatch());
    }
    let removed = {
        let mut guard = pending_map().lock().map_err(|_| transport::unavailable())?;
        match guard.get(&confirmation.operation_id) {
            None => return Err(state::not_found()),
            Some(pending) if !matches_confirmation(pending, &confirmation) => return Err(mismatch()),
            Some(_) => guard.remove(&confirmation.operation_id),
        }
    };
    let PendingImport {
        owner,
        selected,
        target,
        staged,
        manifest,
        job,
        ..
    } = removed.ok_or_else(state::not_found)?;
    let cancelled = job.entry().cancelled().unwrap_or(true);
    if cancelled || !job.start().unwrap_or(false) {
        drop(staged);
        job.finish(Phase::Cancelled);
        return Err(mismatch());
    }
    let _ = job.progress(50);
    #[cfg(test)]
    if let Some(hook) = BEFORE_RESTORE_COMMIT
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|mut guard| guard.take()))
    {
        let _ = tokio::task::spawn_blocking(hook).await;
    }
    let staged_path = staged.to_path_buf();
    let result = rest_dispatch::submit_backup_restore(owner, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            match job.commit(true, || {
                ctx.library
                    .restore_staged_backup(&target, &staged_path, &manifest, store)
            }) {
                Ok(()) => Ok(job),
                Err(error) => {
                    if matches!(
                        error.downcast_ref::<crate::model_progress::OperationStopped>(),
                        Some(crate::model_progress::OperationStopped::Cancelled)
                    ) {
                        job.finish(Phase::Cancelled);
                        Err(Problem::new(
                            StatusCode::CONFLICT,
                            "backup_import_cancelled",
                            "Backup restore was cancelled before commit",
                        ))
                    } else {
                        let problem = transport::unavailable();
                        job.fail_problem(&problem);
                        Err(problem)
                    }
                }
            }
        })
    })
    .await;
    drop(staged);
    match result {
        Ok(job) => {
            let _ = job.progress(100);
            job.finish(Phase::Completed);
            Ok(())
        }
        Err(problem) => Err(problem),
    }
}

async fn resolve_target(selected: Option<String>, scope: transport::RestScope) -> V1Result<PathBuf> {
    rest_dispatch::submit(scope, rest_dispatch::Access::Read, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            store.backup_target().map_err(|_| transport::unavailable())
        })
    })
    .await
}
