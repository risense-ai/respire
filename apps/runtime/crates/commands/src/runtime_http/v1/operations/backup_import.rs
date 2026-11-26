//! Unmounted parent-library staging candidate. Only the fixture supplies the
//! serving target and private receiver. Dropping an unclaimed ticket discards
//! its pending custody and fails its existing operation registry entry.
use super::{state, super::{reads, rest_dispatch, transport, Problem, V1Result}};
use axum::{body, extract::Request, http::{header, Method, StatusCode}, response::Response};
use respire_app::snapshot::backup_archive::BackupArchiveHeader;
use respire_spawn::owner_credential::{ArchiveOperation, PendingBackupArchive};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::{mpsc, Semaphore};

const LIMIT: usize = 2 * 1024 * 1024;
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
    target: PathBuf, worker: mpsc::Sender<Ticket>,
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
    rest_dispatch::submit(scope.clone(), rest_dispatch::Access::Read, move |ctx| {
        reads::with_memory_store(ctx, selected, |_, child| {
            if child { Err(transport::invalid()) } else { Ok(()) }
        })
    }).await?;
    let wire = tokio::time::timeout(Duration::from_secs(10), body::to_bytes(body, LIMIT)).await
        .map_err(|_| transport::invalid())?.map_err(|_| Problem::new(
            StatusCode::BAD_REQUEST, "invalid_upload", "Incomplete or oversized upload",
        ))?;
    let (ticket, accepted) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut database = wire.as_ref();
        let header = BackupArchiveHeader::read(&mut database).map_err(|_| transport::invalid())?;
        let proof = crate::rpc_serve::with_verified_owner_library_scope(|account, profile, instance, generation, epoch, library| {
            if !scope.matches(account, profile, instance, generation, epoch) { return Err(transport::unauthorized()); }
            library.authenticate_backup_import_header(header).map_err(|_| transport::invalid())
        }).ok_or_else(transport::unavailable)??;
        if proof.wire_length() != wire.len() as u64 || target.parent() != Some(scope.canonical_profile.as_path()) {
            return Err(transport::invalid());
        }
        let job = transport::with_scope(&scope, || state::registry().begin(scope.clone(), "backup_import"))?;
        let id = uuid::Uuid::parse_str(&job.id()?).map_err(|_| transport::unavailable())?;
        let operation = ArchiveOperation::from_v4_bytes(*id.as_bytes()).map_err(|_| transport::unavailable())?;
        let pending = proof.stage_for_restore(database, &scope.canonical_profile, &target, operation, || {
            anyhow::ensure!(transport::scope_is_live(&scope), "owner scope expired");
            Ok(())
        }).map_err(|_| transport::invalid())?;
        let accepted = job.accepted()?;
        let ticket = Ticket { pending, target, archive_sha256: Sha256::digest(&wire).into(), job };
        Ok((ticket, accepted))
    }).await.map_err(|_| transport::unavailable())??;
    let receipt = transport::json_response(StatusCode::ACCEPTED, accepted)?;
    let owner = ticket.job.scope().clone();
    transport::with_scope(&owner, || {
        worker.try_send(ticket).map_err(|_| transport::unavailable())?;
        Ok(receipt)
    })
}
