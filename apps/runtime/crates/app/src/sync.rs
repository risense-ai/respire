//! sync — two-way sync engine (CLI and web console share this)
//!
//! local-first: local store = authoritative working library; cloud = dumb ciphertext backup.
//! Sync = git fetch/push semantics: fetch pulls cloud **increment** (server arrival-order rev>cursor; first pull is full)
//! → local LWW merge; push local dirty objects → cloud LWW. Delete = tombstone propagation.
//!
//! Incremental cursor = **server arrival order** (rev incremented on each push in server.rs), not a client timestamp —
//! a fast local clock cannot skip pulls (the updated_at-cursor bug: any local stamp ahead makes since ahead,
//! and a truly new remote object is skipped forever). Cursor lives in local meta.sync_cursor and advances on success.

use anyhow::{anyhow, Result};

use crate::memory::engine::SessionKeys;
use crate::memory::model::StoredMemory;
use crate::transport::local::LocalStore;
use crate::transport::remote::RemoteTransport;
use crate::transport::MemoryTransport;

/// Sync stats.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub protocol: u32,
    pub pending: i64,
    pub conflicts: i64,
    pub undecodable: i64,
    pub conflict_history: i64,
    pub processed_conflicts: i64,
    pub historical_conflicts: i64,
    pub resolving_conflicts: i64,
    pub resolution_supported: bool,
    /// New/changed rows pulled from cloud into local (increment — not a full count)
    pub pulled: usize,
    /// New/changed rows pushed to the cloud
    pub pushed: usize,
    /// Remote object count (including tombstones)
    pub remote_total: usize,
    /// Remote live object count
    pub remote_alive: usize,
}

/// Two-way sync (git fetch/push semantics):
///
/// 1) **fetch** — incremental pull of cloud new/changed from the local cursor (meta.sync_cursor), including tombstones;
///    no cursor → full. Cursor is server arrival-order rev; LWW merge (not dirty).
/// 2) **push** — push local dirty rows (including tombstones) → cloud LWW → clear_dirty on success.
/// 3) On success, advance local meta.sync_cursor to the server cursor.
#[derive(Debug)]
pub struct SyncBoundaryChanged;
impl std::fmt::Display for SyncBoundaryChanged {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("queue identities migrated; local data preserved; run sync again")
    }
}
impl std::error::Error for SyncBoundaryChanged {}

/// Controls local consistency phases independently from network calls.
pub trait SyncControl {
    fn progress(&self, _phase: &str) {}
    fn boundary_changed(&self) -> Result<()> {
        Ok(())
    }
    fn local<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T>;
    fn remote<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T>;
}
pub struct DirectSync;
impl SyncControl for DirectSync {
    fn local<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        action()
    }
    fn remote<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        action()
    }
}
/// The dispatcher lends the active store and vault for one local sync phase.
pub struct SyncLocal<'a> {
    store: &'a LocalStore,
    keys: &'a SessionKeys,
}

impl<'a> SyncLocal<'a> {
    pub(crate) fn new(store: &'a LocalStore, keys: &'a SessionKeys) -> Self {
        Self { store, keys }
    }

    pub fn begin_epoch(&self, epoch: &str) -> Result<bool> {
        self.store.begin_sync_epoch_requeued(epoch)
    }
}

pub trait SyncAccess {
    fn progress(&self, _phase: &str) {}
    fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(SyncLocal<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T>;
    fn remote<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T>;

    fn begin_epoch(&self, epoch: String) -> Result<bool>;
}

struct DirectAccess<'a, C> {
    store: &'a LocalStore,
    keys: &'a SessionKeys,
    control: &'a C,
}

impl<C: SyncControl> SyncAccess for DirectAccess<'_, C> {
    fn progress(&self, phase: &str) {
        self.control.progress(phase);
    }

    fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(SyncLocal<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.control
            .local(|| action(SyncLocal::new(self.store, self.keys)))
    }

    fn remote<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        self.control.remote(action)
    }

    fn begin_epoch(&self, epoch: String) -> Result<bool> {
        self.control.local(|| {
            let requeued = self.store.begin_sync_epoch_requeued(&epoch)?;
            if requeued {
                self.control.boundary_changed()?;
            }
            Ok(requeued)
        })
    }
}

pub fn sync_all(
    keys: &SessionKeys,
    local: &LocalStore,
    remote: &impl MemoryTransport,
) -> Result<SyncStats> {
    sync_controlled(keys, local, remote, &DirectSync, None)
}
pub fn sync_controlled(
    keys: &SessionKeys,
    local: &LocalStore,
    remote: &impl MemoryTransport,
    control: &impl SyncControl,
    boundary: Option<i64>,
) -> Result<SyncStats> {
    sync_with_access(
        remote,
        &DirectAccess {
            store: local,
            keys,
            control,
        },
        boundary,
    )
}

pub fn sync_with_access(
    remote: &impl MemoryTransport,
    control: &impl SyncAccess,
    boundary: Option<i64>,
) -> Result<SyncStats> {
    control.local(check_local)?;
    let captured_boundary = boundary.is_some();
    let mut boundary = match boundary {
        Some(value) => value,
        None => control.local(move |local| local.store.outgoing_boundary())?,
    };
    let started = std::time::Instant::now();
    let mut stats = SyncStats::default();
    let cached_epoch = control.local(move |local| local.store.meta_get("sync_v2_epoch"))?;
    control.progress("capabilities");
    let cached_support =
        control.local(move |local| local.store.meta_get("sync_v2_resolution_support"))?;
    let capability =
        if let Some(epoch) = cached_epoch.filter(|_| cached_support.as_deref() == Some("1")) {
            // Once negotiated, use v2 directly. A 404/epoch error must not downgrade.
            Some(crate::transport::protocol::Capabilities {
                protocols: vec![2],
                epoch,
                push_items: crate::transport::protocol::PUSH_ITEMS,
                push_bytes: crate::transport::protocol::BATCH_BYTES,
                conflict_resolution: true,
            })
        } else {
            control.remote(|| remote.capabilities())?
        };
    if let Some(cap) = capability {
        if !cap.protocols.contains(&2) {
            anyhow::bail!("server does not offer a compatible sync protocol");
        }
        let requeued = control.begin_epoch(cap.epoch.clone())?;
        if requeued && captured_boundary {
            anyhow::bail!(SyncBoundaryChanged);
        }
        if !captured_boundary {
            boundary = control.local(move |local| local.store.outgoing_boundary())?;
        }
        control.local(move |local| {
            local.store.meta_set(
                "sync_v2_resolution_support",
                if cap.conflict_resolution { "1" } else { "0" },
            )
        })?;
        stats.resolution_supported = cap.conflict_resolution;
        stats.protocol = 2;
        let snapshot = control
            .local(move |local| local.store.meta_get("sync_v2_snapshot_done"))?
            .as_deref()
            != Some("1");
        pull_pages(remote, control, &cap.epoch, snapshot, &mut stats)?;
        let mut had_outgoing = false;
        loop {
            let items = control.local(move |local| local.store.outgoing_through(boundary))?;
            if items.is_empty() {
                break;
            }
            had_outgoing = true;
            let request = crate::transport::protocol::PushRequest {
                epoch: cap.epoch.clone(),
                items,
            };
            control.progress("push");
            let reply = control.remote(|| remote.push_v2(&request))?;
            control.progress("apply");
            stats.pushed += control.local(move |local| {
                local
                    .store
                    .acknowledge(&request.items, &reply.results, false)
            })?;
        }
        if had_outgoing {
            pull_pages(remote, control, &cap.epoch, false, &mut stats)?;
        }
        let mut materialized_after = String::new();
        control.progress("materialize");
        while let Some(next) = control.local({
            let after = materialized_after.clone();
            move |local| local.store.materialize_received_batch(local.keys, &after)
        })? {
            materialized_after = next;
        }
        if cap.conflict_resolution {
            pull_resolution_pages(remote, control, &cap.epoch)?;
        }
        let mut classified_after = 0;
        control.progress("conflicts");
        while let Some(next) = control.local(move |local| {
            local.store.classify_conflicts_batch(
                local.keys,
                cap.conflict_resolution,
                classified_after,
            )
        })? {
            classified_after = next;
        }
        if cap.conflict_resolution {
            let mut sent = false;
            let mut resolution_after = 0;
            let resolution_until = control.local(move |local| local.store.resolution_boundary())?;
            loop {
                let items = control.local(move |local| {
                    local
                        .store
                        .outgoing_resolutions_through(resolution_after, resolution_until)
                })?;
                if items.is_empty() {
                    break;
                }
                let request = crate::transport::protocol::ResolveRequest {
                    epoch: cap.epoch.clone(),
                    items,
                };
                if let Some(last) = request.items.last() {
                    resolution_after = last.conflict_rev;
                }
                let reply = control.remote(|| remote.resolve_conflicts(&request))?;
                control.local(move |local| {
                    local
                        .store
                        .acknowledge_resolutions(&request.epoch, &request.items, &reply)
                })?;
                sent = true;
            }
            if sent {
                pull_resolution_pages(remote, control, &cap.epoch)?;
            }
            // A processed explicit decision can release a protected diary
            // working branch. Apply that decision in this sync, not a later one.
            let mut after = String::new();
            control.progress("materialize");
            while let Some(next) = control.local({
                let cursor = after.clone();
                move |local| local.store.materialize_received_batch(local.keys, &cursor)
            })? {
                after = next;
            }
        }
    } else {
        if control
            .local(move |local| local.store.meta_get("sync_v2_epoch"))?
            .is_some()
        {
            anyhow::bail!("server sync capability degraded; refusing silent fallback; pending local changes kept");
        }
        stats.protocol = 1;
        let cursor = control
            .local(move |local| local.store.meta_get("sync_cursor"))?
            .and_then(|s| s.parse().ok());
        control.progress("pull");
        let reply = control.remote(|| remote.fetch_rev(cursor))?;
        stats.remote_total = reply.total as usize;
        stats.remote_alive = reply.alive as usize;
        control.progress("apply");
        stats.pulled += control.local(move |local| {
            local
                .store
                .apply_legacy(local.keys, reply.blobs, reply.cursor)
        })?;
        let mut sent = false;
        loop {
            let items = control.local(move |local| local.store.outgoing_through(boundary))?;
            if items.is_empty() {
                break;
            }
            sent = true;
            let blobs: Vec<_> = items.iter().map(|o| o.blob.clone()).collect();
            control.progress("push");
            let replaced = control.remote(|| remote.put_batch(&blobs))?;
            if replaced.len() != items.len() {
                anyhow::bail!("batch response length mismatch");
            }
            let receipts: Vec<_> = items
                .iter()
                .zip(replaced)
                .map(|(o, ok)| crate::transport::protocol::Receipt {
                    op_id: o.op_id.clone(),
                    status: if ok { "applied" } else { "legacy_rejected" }.to_owned(),
                    stored_rev: 0,
                    head_rev: 0,
                })
                .collect();
            stats.pushed +=
                control.local(move |local| local.store.acknowledge(&items, &receipts, true))?;
        }
        if sent {
            control.progress("pull");
            let again = control.remote(|| remote.fetch_rev(Some(reply.cursor)))?;
            stats.remote_total = again.total as usize;
            stats.remote_alive = again.alive as usize;
            stats.pulled += control.local(move |local| {
                local
                    .store
                    .apply_legacy(local.keys, again.blobs, again.cursor)
            })?;
        }
    }
    control.progress("verify");
    control.local(check_local)?;
    (stats.pending, stats.conflicts, stats.undecodable) =
        control.local(move |local| local.store.sync_counts())?;
    (
        stats.conflict_history,
        stats.processed_conflicts,
        stats.historical_conflicts,
        stats.resolving_conflicts,
    ) = control.local(move |local| local.store.conflict_metrics())?;
    if crate::env::var_os("RSRS_SYNC_TIMING").is_some() {
        eprintln!(
            "sync protocol={} elapsed_ms={} pulled={} pushed={} pending={} conflicts={}",
            stats.protocol,
            started.elapsed().as_millis(),
            stats.pulled,
            stats.pushed,
            stats.pending,
            stats.conflicts
        );
    }
    Ok(stats)
}

fn check_local(local: SyncLocal<'_>) -> Result<()> {
    local.store.ensure_snapshot_sync_allowed()?;
    crate::metadata_gate::ensure_sync_allowed(local.store)?;
    local.store.maintain_sync_storage()
}

fn pull_resolution_pages(
    remote: &impl MemoryTransport,
    control: &impl SyncAccess,
    epoch: &str,
) -> Result<()> {
    let mut after = control
        .local(move |local| local.store.meta_get("sync_v2_resolution_cursor"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut until = None;
    loop {
        let page = control.remote(|| remote.fetch_resolutions(epoch, after, until))?;
        if page.epoch != epoch || until.is_some_and(|h| h != page.until) {
            anyhow::bail!("resolution snapshot changed");
        }
        after = page.cursor;
        until = Some(page.until);
        let has_more = page.has_more;
        control.local(move |local| local.store.apply_resolution_page(&page))?;
        if !has_more {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
fn merge_blobs(keys: &SessionKeys, local: &LocalStore, blobs: Vec<StoredMemory>) -> Result<usize> {
    local.apply_legacy(keys, blobs, 0)
}

fn pull_pages(
    remote: &impl MemoryTransport,
    control: &impl SyncAccess,
    epoch: &str,
    snapshot: bool,
    stats: &mut SyncStats,
) -> Result<()> {
    let key = if snapshot {
        "sync_v2_snapshot_cursor"
    } else {
        "sync_v2_cursor"
    };
    let mut after = control
        .local(move |local| local.store.meta_get(key))?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut until = if snapshot {
        control
            .local(move |local| local.store.meta_get("sync_v2_snapshot_until"))?
            .and_then(|s| s.parse().ok())
    } else {
        None
    };
    let mut restarted = false;
    loop {
        control.progress("pull");
        let page = match control.remote(|| remote.pull_v2(epoch, after, until, snapshot)) {
            Ok(page) => page,
            Err(error)
                if !restarted
                    && error
                        .to_string()
                        .contains("sync history compacted; restart snapshot") =>
            {
                restarted = true;
                if snapshot {
                    control.local(move |local| {
                        local.store.meta_set("sync_v2_snapshot_cursor", "0")?;
                        local.store.meta_set("sync_v2_snapshot_until", "")
                    })?;
                    after = 0;
                }
                until = None;
                continue;
            }
            Err(error) => return Err(error),
        };
        if page.epoch != epoch || until.is_some_and(|h| h != page.until) {
            anyhow::bail!("server page snapshot changed");
        }
        control.progress("apply");
        stats.remote_total = page.total as usize;
        stats.remote_alive = page.alive as usize;
        after = page.cursor;
        until = Some(page.until);
        let has_more = page.has_more;
        stats.pulled +=
            control.local(move |local| local.store.apply_page(local.keys, &page, snapshot))?;
        if !has_more {
            break;
        }
    }
    Ok(())
}

/// Build the remote store: prefer session.json (addr/token from register/login), then env vars.
pub fn build_remote_from_env() -> Result<RemoteTransport> {
    // 1) session.json (written by register/login)
    let path = crate::service::data_dir().join("session.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
            if let (Some(addr), Some(token)) = (
                data["addr"].as_str().filter(|s| !s.is_empty()),
                data["token"].as_str().filter(|s| !s.is_empty()),
            ) {
                return Ok(RemoteTransport::new(
                    crate::transport::remote::RemoteConfig {
                        address: addr.to_owned(),
                        token: token.to_owned(),
                    },
                ));
            }
        }
    }
    // 2) env vars
    let addr = crate::env::var("RSRS_ADDR").map_err(|_| {
        anyhow::anyhow!("sync needs RSRS_ADDR (server address) or a prior register/login")
    })?;
    let token =
        crate::env::var("RSRS_TOKEN").map_err(|_| anyhow::anyhow!("sync needs RSRS_TOKEN"))?;
    Ok(RemoteTransport::new(
        crate::transport::remote::RemoteConfig {
            address: addr,
            token,
        },
    ))
}

/// Whether a remote store is configured (session.json or env).
pub fn remote_configured() -> bool {
    let path = crate::service::data_dir().join("session.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
            if data["addr"].as_str().is_some_and(|s| !s.is_empty())
                && data["token"].as_str().is_some_and(|s| !s.is_empty())
            {
                return true;
            }
        }
    }
    crate::env::var("RSRS_ADDR")
        .map(|a| !a.trim().is_empty())
        .unwrap_or(false)
}

/// Build a blob for tests (not a production path).
#[allow(dead_code)]
fn test_blob(id: &str) -> StoredMemory {
    StoredMemory {
        id: id.to_owned(),
        user: "u".to_owned(),
        ciphertext: "aa".to_owned(),
        nonce: "11".to_owned(),
        embedding_enc: String::new(),
        updated_at: "2026-09-02T00:00:00.000Z".to_owned(),
        deleted: false,
        local_kind: String::new(),
        local_tags: String::new(),
        local_title: String::new(),
        local_project: String::new(),
        local_computer: String::new(),
        local_embedding: None,
        local_parent_id: String::new(),
        local_created_at: String::new(),
        local_content_head: String::new(),
        local_recall_count: 0,
        local_importance: "normal".to_owned(),
        local_device: String::new(),
        local_modified_by: String::new(),
        local_chunks: Vec::new(),
        local_artifact: Vec::new(),
        local_meta: Default::default(),
    }
}

#[cfg(test)]
mod tests;
