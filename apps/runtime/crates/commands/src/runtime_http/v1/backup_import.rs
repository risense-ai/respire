//! Test-only transport binding. Never mounts the import operation or issues
//! an acceptance receipt; offline supervisor ownership is a separate step.
use super::{
    backup_upload_stream, owner_request, reads, rest_dispatch::{self, Access},
    transport, Problem, V1Result,
};
use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};
use respire_app::snapshot::backup_archive::{BackupArchiveHeader, MAX_WIRE};
use std::net::SocketAddr;
use tokio::sync::Semaphore;

static ADMISSION: Semaphore = Semaphore::const_new(2);

pub(super) async fn stage(bound: SocketAddr, peer: SocketAddr, request: Request) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    let mut query = transport::query(parts.uri.query())?;
    let selected = query.remove("space_id");
    if !query.is_empty() || selected.as_deref().is_some_and(str::is_empty) {
        return Err(transport::invalid());
    }
    let media = owner_request::single(&parts.headers, header::CONTENT_TYPE.as_str())
        .map_err(|_| transport::invalid())?;
    if !media.is_some_and(|value| value.eq_ignore_ascii_case("application/octet-stream"))
        || parts.headers.contains_key(header::CONTENT_ENCODING)
    {
        return Err(Problem::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "backup_upload_media", "Application/octet-stream required"));
    }
    let declared = owner_request::single(&parts.headers, header::CONTENT_LENGTH.as_str())
        .map_err(|_| transport::invalid())?
        .map(|value| {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(transport::invalid());
            }
            value.parse::<u64>().map_err(|_| transport::invalid())
        })
        .transpose()?;
    let permit = ADMISSION.try_acquire().map_err(|_| transport::unavailable())?;
    let target = rest_dispatch::submit(scope.clone(), Access::Read, move |ctx| {
        reads::with_memory_store(ctx, selected, |store, _| {
            store.prepare_backup_import_target().map_err(|_| transport::unavailable())
        })
    }).await?;
    backup_upload_stream::receive(body, MAX_WIRE, declared, move |reader| {
        let _permit = permit;
        let parsed = BackupArchiveHeader::read(&mut *reader).map_err(|_| transport::invalid())?;
        let proof = crate::rpc_serve::with_verified_owner_library_scope(|account, path, instance, generation, epoch, library| {
            if !scope.matches(account, path, instance, generation, epoch) {
                return Err(transport::unauthorized());
            }
            library.authenticate_backup_import_header(parsed).map_err(|_| transport::invalid())
        }).unwrap_or_else(|| Err(transport::unavailable()))?;
        let selected = target.checked_path().map_err(|_| transport::unavailable())?;
        let staged = proof.copy_into_owner_stage(reader, &scope.canonical_profile, selected, || {
            anyhow::ensure!(transport::scope_is_live(&scope), "backup scope changed");
            target.checked_path()?;
            Ok(())
        }).map_err(|_| {
            if transport::scope_is_live(&scope) { transport::invalid() }
            else { transport::unauthorized() }
        })?;
        // Fixture never hands the stage to maintenance or modifies live data.
        let bytes = staged.manifest().bytes;
        drop(staged);
        Ok((StatusCode::OK, bytes.to_string()).into_response())
    }).await
}
