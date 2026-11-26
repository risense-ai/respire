//! Native claim of the registry's original owner-private upload ticket.
use super::*;
use crate::runtime_http::v1::owner_control;
use crate::runtime_http::v1::{owner_request, owner_scope::OwnerBinding};
use axum::{
    body,
    extract::{ConnectInfo, Request},
    http::header,
    response::Response,
    routing::post,
    Router,
};
use respire_spawn::owner_credential::{
    archive_proof::ArchiveClaim, ArchiveTarget, ArchiveOperation, ArchivePublishError, BackupArchiveHandoff, PendingBackupArchive,
    ReadyBackupArchive,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

#[path = "archive_handoff.rs"]
mod handoff;

const TTL: Duration = Duration::from_secs(120);
const PATH: &str = "/api/runtime/private-backup/claim";

pub(super) struct Pending {
    _slot: tokio::sync::SemaphorePermit<'static>,
    wire: PendingBackupArchive,
    target: ArchiveTarget,
    digest: [u8; 32],
    deadline: Instant,
}

pub(super) struct Claimed {
    pub(super) at: Instant,
    target: ArchiveTarget,
    digest: [u8; 32],
    handoff: BackupArchiveHandoff,
}

/// Dispatcher shutdown retires custody without publishing. The process-local
/// registry is never reconstructed from the filesystem after startup.
pub(in crate::runtime_http::v1) fn retire_pending_archives() {
    if let Ok(entries) = registry().live_entries() {
        for entry in entries.values() {
            if let Ok(mut state) = entry.lock() {
                if state.archive.is_some() {
                    entry.finish(&mut state, Phase::Failed);
                }
            }
        }
    }
}

impl Job {
    /// Queue receiver transfers the same Job and PendingBackupArchive it received.
    pub(in crate::runtime_http::v1::operations) fn stage_archive(
        self,
        target: ArchiveTarget,
        digest: [u8; 32],
        wire: PendingBackupArchive,
    ) -> V1Result<()> {
        static SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        self.stage_in_slot(target, digest, wire, Instant::now() + TTL, &SLOT)
    }
    #[cfg(test)]
    pub(in crate::runtime_http::v1::operations) fn stage_fixture(
        self, target: ArchiveTarget, digest: [u8; 32], wire: PendingBackupArchive, short: bool,
    ) -> V1Result<()> {
        static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);
        let ttl = if short { Duration::from_millis(100) } else { TTL };
        self.stage_in_slot(target, digest, wire, Instant::now() + ttl, &SLOTS)
    }

    fn stage_in_slot(
        mut self,
        target: ArchiveTarget,
        digest: [u8; 32],
        wire: PendingBackupArchive,
        deadline: Instant,
        slots: &'static tokio::sync::Semaphore,
    ) -> V1Result<()> {
        let slot = slots.try_acquire().map_err(|_| transport::unavailable())?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| transport::unavailable())?;
        let id = Uuid::parse_str(&self.id()?).map_err(|_| transport::invalid())?;
        let operation =
            ArchiveOperation::from_v4_bytes(*id.as_bytes()).map_err(|_| transport::invalid())?;
        let root = &self.scope().canonical_profile;
        if target.verify().is_err() || !wire.matches_ticket(root, operation, digest) {
            return Err(transport::invalid());
        }
        transport::with_scope(self.scope(), || {
            self.entry.install_archive(Pending {
                _slot: slot,
                wire,
                target,
                digest,
                deadline,
            })
        })?;
        self.fail_on_drop = false; // Entry owns the pending file and cancellation now.
        let weak = Arc::downgrade(&self.entry);
        let mut updates = self.entry.subscribe();
        handle.spawn(async move {
            loop {
                if updates.borrow_and_update().state != Phase::Pending {
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline.into()) => {
                        if let Some(entry) = weak.upgrade() { entry.expire_archive(); }
                        return;
                    }
                    result = updates.changed() => if result.is_err() { return; },
                }
            }
        });
        Ok(())
    }
}

impl Entry {
    fn install_archive(&self, pending: Pending) -> V1Result<()> {
        let mut state = self.lock()?;
        if !owner_control::dispatch_live()
            || state.snapshot.kind != "backup_import"
            || state.snapshot.state != Phase::Pending
            || state.archive.is_some()
            || Instant::now() >= pending.deadline
        {
            return Err(transport::unavailable());
        }
        state.archive = Some(pending);
        Ok(())
    }

    fn expire_archive(&self) {
        if let Ok(mut state) = self.lock() {
            if state
                .archive
                .as_ref()
                .is_some_and(|a| Instant::now() >= a.deadline)
            {
                self.finish(&mut state, Phase::Failed);
            }
        }
    }

    pub(in crate::runtime_http::v1::operations) fn claim_archive(&self, claim: ArchiveClaim, mut authorize: impl FnMut() -> bool) -> V1Result<Value> {
        let mut state = self.lock()?;
        if !authorize() {
            return Err(transport::unauthorized());
        }
        if claim.inspect_only {
            if state.committed { return Err(archive_committed()); }
            let pending = state.archive.as_ref().ok_or_else(not_found)?;
            if claim.query_only || state.cancel_requested || state.snapshot.state != Phase::Pending
                || Instant::now() >= pending.deadline || state.snapshot.operation_id != Uuid::from_bytes(claim.operation).to_string()
                || claim.target_identity != [0;32] {
                return Err(not_found());
            }
            let path = pending.target.path().to_str().ok_or_else(transport::unavailable)?;
            if pending.digest != claim.archive || <[u8;32]>::from(Sha256::digest(path.as_bytes())) != claim.target { return Err(transport::unauthorized()); }
            let identity = pending.target.identity_digest().map_err(|_| transport::unavailable())?;
            if !authorize() || Instant::now() >= pending.deadline { return Err(transport::unauthorized()); }
            return Ok(json!({"schema_version":2,"state":"pending","operation_id":state.snapshot.operation_id,
                "archive_sha256":hex::encode(pending.digest),"target_sha256":hex::encode(claim.target),
                "target_identity_sha256":hex::encode(identity),"vault_epoch":self.scope.vault_epoch.to_string(),
                "target":path}));
        }
        if claim.query_only {
            let held = state.claimed.as_ref().ok_or_else(not_found)?;
            let path = held.target.path().to_str().ok_or_else(transport::invalid)?;
            if held.at.elapsed() >= TERMINAL_RETENTION || held.digest != claim.archive
                || <[u8; 32]>::from(Sha256::digest(path.as_bytes())) != claim.target
                || held.target.identity_digest().ok() != Some(claim.target_identity)
            {
                return Err(transport::unauthorized());
            }
            // Reconcile uncertain final publication against the originally held inode and intent.
            ReadyBackupArchive::open(&self.scope.canonical_profile, held.handoff)
                .map_err(|_| transport::unavailable())?;
            respire_spawn::durable::sync_directory(&self.scope.canonical_profile)
                .map_err(|_| transport::unavailable())?;
            if !authorize() || held.target.verify().is_err() || held.at.elapsed() >= TERMINAL_RETENTION {
                return Err(transport::unauthorized());
            }
            return Ok(receipt(claim, held.handoff, held.target.identity_digest().map_err(|_| transport::unavailable())?, self.scope.vault_epoch));
        }
        if state.committed {
            return Err(archive_committed());
        }
        if state.snapshot.state != Phase::Pending || state.cancel_requested {
            return Err(not_found());
        }
        let pending = state.archive.as_ref().ok_or_else(not_found)?;
        if Instant::now() >= pending.deadline {
            self.finish(&mut state, Phase::Failed);
            return Err(not_found());
        }
        let target = pending.target.path().to_str().ok_or_else(transport::invalid)?;
        let target_digest: [u8; 32] = Sha256::digest(target.as_bytes()).into();
        if state.snapshot.operation_id != Uuid::from_bytes(claim.operation).to_string()
            || claim.target != target_digest
            || claim.archive != pending.digest
            || !authorize()
        {
            return Err(transport::unauthorized());
        }
        if Instant::now() >= pending.deadline {
            self.finish(&mut state, Phase::Failed);
            return Err(not_found());
        }
        let identity = match pending.target.identity_digest() { Ok(identity) => identity, Err(_) => {
            self.finish(&mut state, Phase::Failed);
            return Err(transport::unavailable());
        }};
        if identity != claim.target_identity { return Err(transport::unauthorized()); }
        let pending = state.archive.take().ok_or_else(not_found)?;
        let handoff = pending.wire.handoff();
        // Linearization under the same lock as cancel. Even a late fsync error
        // cannot reopen cancellation or permit a second publication.
        state.committed = true;
        let target = pending.target;
        let result = pending.wire.publish_claim(|| {
            if !authorize() { return Err(std::io::ErrorKind::PermissionDenied.into()); }
            target.verify()
        });
        if matches!(result, Err(ArchivePublishError::BeforeRename(_))) {
            self.finish(&mut state, Phase::Failed);
            return Err(transport::unavailable());
        }
        state.snapshot.state = Phase::Running;
        state.claimed = Some(Claimed { at: Instant::now(), target, digest: pending.digest, handoff });
        self.publish(&mut state);
        result.map_err(|_| transport::unavailable())?;
        Ok(receipt(claim, handoff, identity, self.scope.vault_epoch))
    }
}

fn receipt(claim: ArchiveClaim, handoff: BackupArchiveHandoff, identity: [u8;32], epoch: Uuid) -> Value {
    json!({"schema_version":1,"state":"claimed","operation_id":Uuid::from_bytes(claim.operation).to_string(),
        "archive_sha256":hex::encode(claim.archive),"target_sha256":hex::encode(claim.target),
        "target_identity_sha256":hex::encode(identity),"vault_epoch":epoch.to_string(),"intent_sha256":hex::encode(handoff.intent_sha256())})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    handoff: Option<handoff::Request>,
    #[serde(default)]
    query_only: bool,
    #[serde(default)]
    inspect_only: bool,
    schema_version: u8,
    operation_id: String,
    archive_sha256: String,
    target_sha256: String,
    target_identity_sha256: String,
}

fn digest(value: &str) -> V1Result<[u8; 32]> {
    let mut result = [0; 32];
    if value.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(transport::invalid());
    }
    hex::decode_to_slice(value, &mut result).map_err(|_| transport::invalid())?;
    Ok(result)
}

pub(in crate::runtime_http::v1) fn routes(bound: SocketAddr) -> Router {
    Router::new().route(
        PATH,
        post(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                  request: Request| serve(bound, peer, request, true),
        ),
    )
}

#[cfg(test)]
pub(in crate::runtime_http::v1) fn fixture_routes(bound: SocketAddr) -> Router {
    Router::new().route("/fixture/archive-claim", post(
        move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>, request: Request|
            serve(bound, peer, request, false)))
}

async fn serve(bound: SocketAddr, peer: SocketAddr, request: Request, require_handoff: bool) -> V1Result<Response> {
    let (parts, stream) = request.into_parts();
    owner_request::admit_selected(&parts, peer, bound)?;
    if parts.uri.query().is_some()
        || [header::COOKIE, header::ORIGIN, header::AUTHORIZATION]
            .iter()
            .any(|name| parts.headers.contains_key(name))
    {
        return Err(transport::unauthorized());
    }
    let proof = owner_request::single(&parts.headers, "x-respire-archive-proof")
        .ok()
        .flatten()
        .filter(|p| p.len() == 85 && p.starts_with("rha2_"))
        .ok_or_else(transport::unauthorized)?
        .to_owned();
    let bytes = tokio::time::timeout(Duration::from_secs(1), body::to_bytes(stream, 1024))
        .await
        .map_err(|_| transport::invalid())?
        .map_err(|_| transport::invalid())?;
    let frame: Frame = serde_json::from_slice(&bytes).map_err(|_| transport::invalid())?;
    if require_handoff && !frame.inspect_only && !frame.query_only && frame.handoff.is_none() {
        return Err(transport::unauthorized());
    }
    let id = Uuid::parse_str(&frame.operation_id).map_err(|_| transport::invalid())?;
    if frame.schema_version != if frame.handoff.is_some() { 3 } else { 2 }
        || id.to_string() != frame.operation_id
        || id.get_version_num() != 4
    {
        return Err(transport::invalid());
    }
    let archive = digest(&frame.archive_sha256)?;
    let target = digest(&frame.target_sha256)?;
    let target_identity = digest(&frame.target_identity_sha256)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let receipt = owner_control::bounded_blocking(move || {
        owner_control::with_active(|owner, live: &OwnerBinding| {
            let claim = ArchiveClaim {
                query_only: frame.query_only,
                inspect_only: frame.inspect_only,
                operation: *id.as_bytes(),
                archive,
                target,
                target_identity,
                instance: *live.instance.as_bytes(),
                generation: live.generation,
            };
            let mut fresh = || {
                Instant::now() < deadline
                    && owner_control::wall_ms()
                        .is_some_and(|now| owner.verify_archive(live, claim, &proof, now))
            };
            if !fresh() {
                return Err(transport::unauthorized());
            }
            let entry = registry()
                .live_entries()?
                .get(&id)
                .cloned()
                .ok_or_else(not_found)?;
            let scope_matches = || {
                entry.scope.matches(
                    &live.account,
                    &live.canonical_profile,
                    live.instance,
                    live.generation,
                    live.vault_epoch,
                )
            };
            if !scope_matches() {
                let mut state = entry.lock()?;
                if state.archive.is_some() {
                    entry.finish(&mut state, Phase::Failed);
                }
                return Err(transport::unauthorized());
            }
            let armed = frame.handoff.map(|request| request.arm(claim, live)).transpose()?;
            let mut receipt = entry.claim_archive(claim, || scope_matches() && fresh()
                && armed.as_ref().is_none_or(handoff::Armed::current))?;
            if let Some(armed) = armed { armed.deliver(&mut receipt, live)?; }
            Ok(receipt)
        })
    })
    .await?;
    transport::json_response(StatusCode::OK, receipt)
}
