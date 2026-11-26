//! Cross-platform streaming backup import over the owner dispatcher.
use super::{
    state::{self, Phase},
    super::{reads, rest_dispatch, transport, V1Result},
};
use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    response::Response,
};
use respire_app::snapshot::backup_archive::BackupArchiveHeader;
use sha2::{Digest, Sha256};
#[path = "backup_import_stream.rs"]
mod stream;
use std::{io::Write, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::Semaphore;

const TOTAL: Duration = Duration::from_secs(30 * 60);
static UPLOADS: Semaphore = Semaphore::const_new(2);

pub(in crate::runtime_http::v1) async fn enqueue(
    bound: SocketAddr,
    peer: SocketAddr,
    request: Request,
) -> V1Result<Response> {
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
        let _digest = database.verified_digest()?;
        let staged = temp.into_temp_path();
        Ok::<_, super::super::Problem>((staged, manifest))
    });
    let (staged, manifest) = tokio::time::timeout_at(deadline, async {
        let fed = stream::feed(body, sender).await;
        let staged = stage.await.map_err(|_| transport::unavailable())??;
        fed?;
        Ok::<_, super::super::Problem>(staged)
    })
    .await
    .map_err(|_| transport::invalid())??;
    if resolve_target(selected.clone(), owner.clone()).await? != target {
        return Err(transport::invalid());
    }
    let job = transport::with_scope(&owner, || {
        state::registry().begin(owner.clone(), "backup_import")
    })?;
    let accepted = job.accepted()?;
    let receipt = transport::json_response(StatusCode::ACCEPTED, accepted)?;
    tokio::spawn(async move {
        let _permit = permit;
        let cancelled = job.entry().cancelled().unwrap_or(true);
        if cancelled || !job.start().unwrap_or(false) {
            drop(staged);
            job.finish(Phase::Cancelled);
            return;
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
            }
            Err(problem) => job.fail_problem(&problem),
        }
    });
    Ok(receipt)
}

async fn resolve_target(selected: Option<String>, scope: transport::RestScope) -> V1Result<PathBuf> {
    rest_dispatch::submit(scope, rest_dispatch::Access::Read, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            store.backup_target().map_err(|_| transport::unavailable())
        })
    })
    .await
}
