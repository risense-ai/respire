//! Streaming backup import into bounded pending custody. Writes wait for separate local confirmation.
use super::{
    state::{self, Phase},
    super::{reads, rest_dispatch, transport, V1Result},
};
use anyhow::{ensure, Context, Result};
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
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

const TOTAL: Duration = Duration::from_secs(30 * 60);
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING: usize = 2;
static UPLOADS: Semaphore = Semaphore::const_new(2);
static PENDING_IMPORTS: OnceLock<Mutex<BTreeMap<Uuid, PendingImport>>> = OnceLock::new();

struct PendingImport {
    owner: transport::RestScope,
    selected: Option<String>,
    target: PathBuf,
    archive_sha256: [u8; 32],
    staged: tempfile::TempPath,
    manifest: Manifest,
    job: state::Job,
    created_at: Instant,
}

#[allow(dead_code)]
pub struct LocalRestoreConfirmation {
    pub operation_id: Uuid,
    pub archive_sha256: [u8; 32],
    pub target: PathBuf,
}

fn pending_map() -> &'static Mutex<BTreeMap<Uuid, PendingImport>> {
    PENDING_IMPORTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn is_stale(entry: &PendingImport, now: Instant) -> bool {
    now.saturating_duration_since(entry.created_at) >= PENDING_TTL
        || entry.job.entry().cancelled().unwrap_or(true)
        || !transport::scope_is_live(&entry.owner)
}

pub(super) fn reap_stale() {
    let now = Instant::now();
    let expired: Vec<PendingImport> = {
        let Ok(mut guard) = pending_map().lock() else {
            return;
        };
        let ids: Vec<Uuid> = guard
            .iter()
            .filter(|(_, entry)| is_stale(entry, now))
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter().filter_map(|id| guard.remove(&id)).collect()
    };
    for PendingImport { staged, job, .. } in expired {
        drop(staged);
        job.finish(Phase::Cancelled);
    }
}

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
        let expected_source = format!("{:x}", Sha256::digest(target_text.as_bytes()));
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
        Ok::<_, super::super::Problem>((staged, manifest, archive_sha256))
    });
    let (staged, manifest, archive_sha256) = tokio::time::timeout_at(deadline, async {
        let fed = stream::feed(body, sender).await;
        let staged = stage.await.map_err(|_| transport::unavailable())??;
        fed?;
        Ok::<_, super::super::Problem>(staged)
    })
    .await
    .map_err(|_| transport::invalid())??;
    drop(permit);
    if resolve_target(selected.clone(), owner.clone()).await? != target {
        return Err(transport::invalid());
    }
    reap_stale();
    let mut guard = pending_map().lock().map_err(|_| transport::unavailable())?;
    if guard.len() >= MAX_PENDING {
        return Err(transport::unavailable());
    }
    let job = transport::with_scope(&owner, || {
        state::registry().begin(owner.clone(), "backup_import")
    })?;
    let id = Uuid::parse_str(&job.id()?).map_err(|_| transport::unavailable())?;
    let accepted = job.accepted()?;
    let receipt = transport::json_response(StatusCode::ACCEPTED, accepted)?;
    guard.insert(
        id,
        PendingImport {
            owner,
            selected,
            target,
            archive_sha256,
            staged,
            manifest,
            job,
            created_at: Instant::now(),
        },
    );
    Ok(receipt)
}

#[allow(dead_code)]
pub(crate) async fn confirm_local_restore(confirmation: LocalRestoreConfirmation) -> Result<()> {
    reap_stale();
    let requested_target = confirmation.target.canonicalize()?;
    let removed_stale: Option<PendingImport>;
    let admitted = {
        let mut guard = pending_map()
            .lock()
            .map_err(|_| anyhow::anyhow!("pending backup registry unavailable"))?;
        let pending = guard
            .get(&confirmation.operation_id)
            .context("pending backup import not found")?;
        ensure!(
            pending.archive_sha256 == confirmation.archive_sha256,
            "confirmed backup archive digest differs"
        );
        ensure!(
            requested_target == pending.target,
            "confirmed backup restore target differs"
        );
        if is_stale(pending, Instant::now()) {
            removed_stale = guard.remove(&confirmation.operation_id);
            None
        } else {
            removed_stale = None;
            guard.remove(&confirmation.operation_id)
        }
    };
    if let Some(PendingImport { staged, job, .. }) = removed_stale {
        drop(staged);
        job.finish(Phase::Cancelled);
        anyhow::bail!("pending backup import is no longer active");
    }
    let PendingImport {
        owner,
        selected,
        target,
        staged,
        manifest,
        job,
        ..
    } = admitted.context("pending backup import not found")?;
    let cancelled = job.entry().cancelled().unwrap_or(true);
    if cancelled || !job.start().unwrap_or(false) {
        drop(staged);
        job.finish(Phase::Cancelled);
        anyhow::bail!("pending backup import was cancelled before execution");
    }
    let _ = job.progress(50);
    let staged_path = staged.to_path_buf();
    let result = rest_dispatch::submit_backup_restore(owner, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            ctx.library
                .restore_staged_backup(&target, &staged_path, &manifest, store)
                .map_err(|_| transport::unavailable())
        })
    })
    .await;
    drop(staged);
    match result {
        Ok(()) => {
            let _ = job.progress(100);
            job.finish(Phase::Completed);
            Ok(())
        }
        Err(problem) => {
            job.fail_problem(&problem);
            anyhow::bail!("backup restore failed after local confirmation");
        }
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
