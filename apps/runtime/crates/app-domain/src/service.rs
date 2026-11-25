//! service — business layer (single source of truth for the CLI and the local client)
//!
//! Assembles session/store/embedder and owns CRUD, the causal tree, candidate-before-store, sync, accounts, and tools.
//! The CLI owns printing and interactive flow; the client (Tauri) calls this layer directly. Embedder is BGE everywhere, no fallback.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::memory::bge::BgeEmbedder;
use crate::memory::engine::MemoryEngine;
use crate::memory::model::{Kind, MemoryEntry, MemoryQuery};
use crate::memory::search::Embedder;
use crate::memory::SessionKeys;
use crate::sync::{remote_configured, sync_all};
use crate::transport::local::LocalStore;
use crate::transport::MemoryTransport;

mod host_config_transaction;
pub use host_config_transaction::{recover_host_config, HostConfigTransaction};
mod deepen;
pub use deepen::{deepen_plan, deepen_suggestions, scoped_deepen_plan, DeepenAuto, DeepenRoot};
mod merge;
pub use merge::{merge_entries, merge_entries_with_metadata, merge_entries_with_metadata_revision};

/// Shared candidate receipt contract for agent and client consumers.
pub fn write_decision_actions() -> &'static [&'static str] {
    &[
        "\u{2460} duplicate (bool): do both texts state the same fact with no new detail? If so, merge with merge-ids; inspect descendants and back up first.",
        "\u{2461} contradiction (bool): are they mutually exclusive at the same point in time? If so, merge, keep the newer text and record why the old one was dropped.",
        "\u{2462} obsolete (bool): does the new text clearly supersede the old one? If so, update the old entry, append a note on the supersession and do not delete it.",
        "\u{2463} linkable (bool): would linking them help later recall? If so, attach the cause with --parent.",
        "\u{2464} representation (keep_separate|merge|promote|uncertain): keep them apart, merge, promote or defer? Keep separate / write one synthesized entry / promote a repeated event into a principle / defer while uncertain.",
        "Choose update > merge > parent > create, or explicitly defer. Candidate/pending is not saved; verify returned IDs/content. For unknown outcomes inspect receipts read-only before any replay.",
    ]
}
mod related;
pub use related::{store_related, remap_relations};

static RUNTIME_PROFILE: OnceLock<PathBuf> = OnceLock::new();

/// A runtime keeps ownership of its boot library until host-managed restart.
pub fn install_runtime_profile(profile: PathBuf) {
    let _ = RUNTIME_PROFILE.set(profile);
}
pub fn require_profile_change_host() -> Result<()> {
    if RUNTIME_PROFILE.get().is_some() {
        anyhow::bail!("runtime owns its boot profile; stop it on the host with `rsrs --runtime-internal --stop`, run the profile command with `rsrs --direct`, then restart `rsrs --runtime-internal`");
    }
    Ok(())
}
pub fn check_runtime_user(user: &str) -> Result<()> {
    if RUNTIME_PROFILE.get().is_some() {
        if let Ok(session) = auth::read_session_json() {
            let current = session["user"].as_str().unwrap_or("");
            if !current.is_empty() && current != user {
                require_profile_change_host()?;
            }
        }
    }
    Ok(())
}

pub fn ensure_runtime_profile() -> Result<()> {
    check_runtime_profile(&data_dir())
}

fn check_runtime_profile(path: &Path) -> Result<()> {
    if let Some(owned) = RUNTIME_PROFILE.get() {
        if path != owned {
            require_profile_change_host()?;
        }
    }
    Ok(())
}

static AUTO_SYNC_NOTIFY: OnceLock<fn()> = OnceLock::new();
static INDEX_NOTIFY: OnceLock<fn()> = OnceLock::new();

pub fn install_index_notifier(notify: fn()) {
    let _ = INDEX_NOTIFY.set(notify);
}

/// Missing artifacts are durable work; the runtime notification only wakes its worker.
pub fn notify_index() {
    if let Some(notify) = INDEX_NOTIFY.get() {
        notify();
    }
}

#[derive(Debug)]
pub struct IndexPending;

impl std::fmt::Display for IndexPending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local retrieval index is being prepared; retry after indexing completes")
    }
}

impl std::error::Error for IndexPending {}

/// Validate the actual snapshot sent to Core, including its opaque index artifacts.
pub fn require_candidates_ready(store: &LocalStore, candidates: &[crate::StoredMemory]) -> Result<()> {
    if !store.candidates_index_ready(candidates)? {
        notify_index();
        return Err(IndexPending.into());
    }
    Ok(())
}

/// Recall stays available while synchronized sources await background indexing.
/// Only current, source-checked artifacts may participate in semantic retrieval.
pub fn recall_candidates(store: &LocalStore) -> Result<(Vec<crate::StoredMemory>, usize)> {
    let candidates = store.all(false)?;
    let (ready, pending): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|memory| !memory.local_artifact.is_empty());
    let pending_count = pending.len();
    if pending_count > 0 {
        notify_index();
        if ready.is_empty() {
            return Err(IndexPending.into());
        }
    }
    require_candidates_ready(store, &ready)?;
    Ok((ready, pending_count))
}

/// The resident runtime installs its single durable-outbox worker notifier.
pub fn install_autosync_notifier(notify: fn()) {
    let _ = AUTO_SYNC_NOTIFY.set(notify);
}
pub fn notify_autosync() {
    if autosync_active() && remote_configured() {
        if let Some(notify) = AUTO_SYNC_NOTIFY.get() {
            notify();
        }
    }
}

/// Unlocked session + local authoritative store + embedder, as one unit.
/// Production `open()` always loads BGE (no hashing fallback). Tests inject `HashingEmbedder`.
pub struct App {
    pub keys: crate::memory::SessionKeys,
    pub store: LocalStore,
    pub embedder: Box<dyn Embedder>,
}

include!("service/memory_operations.rs");
include!("service/memory_types.rs");
include!("service/session_access.rs");
include!("service/profiles.rs");
include!("service/maintenance_preferences.rs");
include!("service/runtime_settings.rs");
include!("service/writing_material.rs");
include!("service/import_export.rs");
include!("service/sharing_and_backup.rs");
include!("service/tree_maintenance.rs");
include!("service/local_store.rs");
include!("service/utility.rs");
#[cfg(test)]
mod tests;
