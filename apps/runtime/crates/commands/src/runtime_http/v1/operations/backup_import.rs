//! Unmounted streaming import. Only the private receiver owns the pending ticket.
use super::{state, super::{reads, rest_dispatch, transport, V1Result}};
use axum::{extract::Request, http::{header, Method, StatusCode}, response::Response};
use respire_app::snapshot::backup_archive::BackupArchiveHeader;
use respire_spawn::owner_credential::{ArchiveOperation, PendingBackupArchive};
#[path = "backup_import_stream.rs"]
mod stream;
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::{mpsc, Semaphore};

const TOTAL: Duration = Duration::from_secs(30 * 60);
static UPLOADS: Semaphore = Semaphore::const_new(2);

pub(in crate::runtime_http::v1) struct Ticket {
    pub(in crate::runtime_http::v1) pending: PendingBackupArchive,
    pub(in crate::runtime_http::v1) target: PathBuf,
    pub(in crate::runtime_http::v1) archive_sha256: [u8; 32],
    job: state::Job,
}

impl Ticket {
    pub(in crate::runtime_http::v1) fn operation_id(&self) -> V1Result<String> { self.job.id() }
}

pub(in crate::runtime_http::v1) async fn post(
    bound: SocketAddr, peer: SocketAddr, request: Request,
    worker: mpsc::Sender<Ticket>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST { return Err(super::super::method_not_allowed().await); }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some("application/octet-stream") {
        return Err(transport::invalid());
    }
    let mut query = transport::query(parts.uri.query())?;
    let selected = query.remove("space_id");
    if !query.is_empty() { return Err(transport::invalid()); }
    let permit = UPLOADS.try_acquire().map_err(|_| transport::unavailable())?;
    let resolve = |selected: Option<String>, scope: transport::RestScope| async move {
        rest_dispatch::submit(scope, rest_dispatch::Access::Read, move |ctx| {
            reads::with_memory_store(ctx, selected, |store, _| {
                store.backup_target().map_err(|_| transport::unavailable())
            })
        }).await
    };
    let target = resolve(selected.clone(), scope.clone()).await?;
    let owner = scope.clone();
    let deadline = tokio::time::Instant::now() + TOTAL;
    let (sender, mut database) = stream::Reader::bridge();
    let stage = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let header = BackupArchiveHeader::read(&mut database).map_err(|_| transport::invalid())?;
        let proof = crate::rpc_serve::with_verified_owner_library_scope(|account, profile, instance, generation, epoch, library| {
            if !scope.matches(account, profile, instance, generation, epoch) { return Err(transport::unauthorized()); }
            library.authenticate_backup_import_header(header).map_err(|_| transport::invalid())
        }).ok_or_else(transport::unavailable)??;
        let job = transport::with_scope(&scope, || state::registry().begin(scope.clone(), "backup_import"))?;
        let id = uuid::Uuid::parse_str(&job.id()?).map_err(|_| transport::unavailable())?;
        let operation = ArchiveOperation::from_v4_bytes(*id.as_bytes()).map_err(|_| transport::unavailable())?;
        let pending = proof.stage_for_restore(&mut database, &scope.canonical_profile, &target, operation, || {
            anyhow::ensure!(tokio::time::Instant::now() < deadline, "upload deadline expired");
            anyhow::ensure!(transport::scope_is_live(&scope), "owner scope expired");
            Ok(())
        }).map_err(|_| transport::invalid())?;
        let accepted = job.accepted()?;
        let ticket = Ticket { pending, target, archive_sha256: database.verified_digest()?, job };
        Ok::<_, super::super::Problem>((ticket, accepted))
    });
    // The request task alone offers tickets. On disconnect or timeout dropping
    // feed closes the bridge; a detached blocking task can only drop custody.
    let (ticket, accepted) = tokio::time::timeout_at(deadline, async {
        let fed = stream::feed(body, sender).await;
        let staged = stage.await.map_err(|_| transport::unavailable())??;
        fed?;
        Ok::<_, super::super::Problem>(staged)
    }).await.map_err(|_| transport::invalid())??;
    if resolve(selected, owner.clone()).await? != ticket.target {
        return Err(transport::invalid());
    }
    let receipt = transport::json_response(StatusCode::ACCEPTED, accepted)?;
    transport::with_scope(&owner, || {
        worker.try_send(ticket).map_err(|_| transport::unavailable())?;
        Ok(receipt)
    })
}
