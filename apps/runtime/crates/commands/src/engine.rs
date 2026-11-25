//! Command engine assembly: library/store/session construction shared by the
//! engine arms and the runtime worker threads.
//!
//! The command arms themselves (`run_local`) and the per-command functions
//! migrate here from the CLI binary in later steps of the runtime split.

use std::sync::Arc;

use anyhow::{anyhow, Result};

use respire_app::auth;
use respire_app::env;
use respire_app::service;
use respire_app::transport::local::LocalStore;
use respire_app::transport::remote::{RemoteConfig, RemoteTransport};
use respire_app::{MemoryTransport, SessionKeys};


/// Assemble transport: RSRS_ADDR (or session.json addr) -> remote backup store; else local store.
pub fn build_transport() -> Result<Arc<dyn MemoryTransport>> {
    if let Some((addr, token)) = remote_config_from_session_or_env()? {
        return Ok(Arc::new(RemoteTransport::new(RemoteConfig {
            address: addr,
            token,
        })));
    }
    Ok(Arc::new(respire_app::service::open_store()?))
}

pub fn build_local() -> Result<LocalStore> {
    respire_app::service::open_store()
}

/// Remote config: session.json first (addr/token written by client login), then env vars.
pub fn remote_config_from_session_or_env() -> Result<Option<(String, String)>> {
    // 1) session.json
    if let Ok(data) = read_session_json() {
        if let (Some(addr), Some(token)) = (
            data["addr"].as_str().filter(|s| !s.is_empty()),
            data["token"].as_str().filter(|s| !s.is_empty()),
        ) {
            return Ok(Some((addr.to_owned(), token.to_owned())));
        }
    }
    // 2) env vars
    if let Ok(addr) = respire_app::env::var("RSRS_ADDR") {
        if !addr.trim().is_empty() {
            let token = respire_app::env::var("RSRS_TOKEN")
                .map_err(|_| anyhow!("remote mode needs RSRS_TOKEN"))?;
            return Ok(Some((addr, token)));
        }
    }
    Ok(None)
}

pub fn build_remote() -> Result<RemoteTransport> {
    let (addr, token) = remote_config_from_session_or_env()?
        .ok_or_else(|| anyhow!("remote store is not configured - have the client/account tool write session.json (addr/token), or set RSRS_ADDR and RSRS_TOKEN"))?;
    Ok(RemoteTransport::new(RemoteConfig {
        address: addr,
        token,
    }))
}

/// Unlock session keys: local keyfile first (from keygen/register), else env vars.
pub fn build_session() -> Result<SessionKeys> {
    build_session_with_owner().map(|(keys, _)| keys)
}

/// Preserve only the owner alias from the local snapshot that supplied these keys.
/// Environment-key fallback must never inherit identity from an unrelated profile.
pub fn build_session_with_owner() -> Result<(SessionKeys, Option<String>)> {
    let result = build_session_with_owner_inner();
    crate::host_hooks::note_credential_access(result.is_ok());
    result
}

pub fn build_session_with_owner_inner() -> Result<(SessionKeys, Option<String>)> {
    if let Ok(session) = auth::load_local_session_with_owner() {
        return Ok(session);
    }
    if read_session_json().is_err() {
        // no local session -> first-run guide
        anyhow::bail!(
            "no local session: rsrs keygen --pass <password> to start fully local (no cloud); \
             or rsrs register / login for a cloud identity; or set the five-keys env vars \
             (RSRS_PASS/SECRET/KDF_SALT/WRAPPED_URK/URK_NONCE)"
        );
    }
    let password = respire_app::env::var("RSRS_PASS")
        .map_err(|_| anyhow!("need RSRS_PASS (master password)"))?;
    let secret = respire_app::env::var("RSRS_SECRET")
        .map_err(|_| anyhow!("need RSRS_SECRET (recovery key)"))?;
    let kdf_salt = respire_app::env::var("RSRS_KDF_SALT")
        .map_err(|_| anyhow!("need RSRS_KDF_SALT (from keygen/register)"))?;
    let wrapped_urk = respire_app::env::var("RSRS_WRAPPED_URK")
        .map_err(|_| anyhow!("need RSRS_WRAPPED_URK (from keygen/login)"))?;
    let urk_nonce = respire_app::env::var("RSRS_URK_NONCE")
        .map_err(|_| anyhow!("need RSRS_URK_NONCE (from keygen/login)"))?;
    Ok((SessionKeys::unlock(&password, &secret, &kdf_salt, &wrapped_urk, &urk_nonce)?, None))
}

pub fn read_session_json() -> Result<serde_json::Value> {
    auth::read_session_json()
}

// Runtime command engine.
// Helpers, per-command functions, run_local/run_local_inner and the embedder
// cache, verbatim from their extraction source apart from rebinding `respire::`
// app-core paths, CLI-host services through `host_hooks`, and the host-command
// router that stays in the CLI binary.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Context;
use uuid::Uuid;

use respire_app::memory::bge::BgeEmbedder;
use respire_app::memory::model::Kind;
use respire_app::memory::search::Embedder;
use respire_app::memory::{MemoryEngine, MemoryQuery, StoredMemory};
use respire_app::sync::remote_configured;

use crate::bench;
use crate::classify;
use crate::cli_def::{
    BenchCmd, Cli, Command, GrantCommand, ModelAction, QueryLogCmd,
    SessionCommand, SnapshotCmd,
};
use crate::gates::{is_off_allowed, is_readonly_switch_off, is_write_command, is_write_command_fine};
use crate::metadata;
use crate::output::{self, Item as OutputItem, ResultEnvelope, Status as OutputStatus};
use crate::output_emit::{command_failure, emit_details, emit_result};
use crate::output_state::{
    json_mode, progress_enabled, set_exit_code, set_json_mode, status_exit_code, CAPTURED,
    DEFER_PROFILE_OUTPUT, HOST_RPC_OUTPUT,
};
use crate::progress;
use crate::recall_select;
use crate::review;
use crate::rpc_worker as rpc;
use crate::runtime_policy;
use crate::skill;
use crate::sync_flow::{
    read_remote_sync_history, run_sync_attempts, sync_live, sync_phase, sync_tracked,
    sync_tracked_boundary, sync_with_retry,
};
use crate::util::{current_user, dev_label, list_since_bound, local_day_of, now_stamp, short_id};

pub static DIRECT_MODE: AtomicBool = AtomicBool::new(false);

pub static EMBEDDER_SLOT: std::sync::Mutex<Option<Box<BgeEmbedder>>> = std::sync::Mutex::new(None);

pub struct EmbedderHold(Option<Box<BgeEmbedder>>);

impl Drop for EmbedderHold {
    fn drop(&mut self) {
        if let Ok(mut slot) = EMBEDDER_SLOT.lock() {
            *slot = self.0.take();
        }
    }
}

pub fn scoped_candidates(store: &LocalStore) -> Result<Vec<respire_app::StoredMemory>> {
    let candidates = store.all(false)?;
    respire_app::service::require_candidates_ready(store, &candidates)?;
    Ok(candidates)
}

pub fn candidates_preview(store: &LocalStore) -> Result<Vec<respire_app::StoredMemory>> {
    scoped_candidates(store)
}

pub fn candidates_count_primary(all: &[respire_app::StoredMemory]) -> usize {
    all.iter()
        .filter(|m| m.local_importance == "important")
        .count()
}

pub fn candidates_count_normal(all: &[respire_app::StoredMemory]) -> usize {
    all.iter()
        .filter(|m| m.local_importance == "normal")
        .count()
}

/// Memory-tree outline (--outline): full depth, id+title indent tree, no bodies - the AI tidy's input surface.
pub fn collect_outline(nodes: &[respire_app::service::TreeNode], indent: usize, out: &mut Vec<String>) {
    for n in nodes {
        out.push(format!(
            "{}[{}] {}",
            "  ".repeat(indent),
            respire_app::service::short_id(&n.id),
            n.title
        ));
        collect_outline(&n.children, indent + 1, out);
    }
}

/// Same, but structured rows (for `--json`) - keep depth, id, title for programs.
pub fn collect_outline_json(
    nodes: &[respire_app::service::TreeNode],
    depth: usize,
    out: &mut Vec<serde_json::Value>,
) {
    for n in nodes {
        out.push(serde_json::json!({
            "depth": depth,
            "id": n.id,
            "short_id": respire_app::service::short_id(&n.id),
            "title": n.title,
        }));
        collect_outline_json(&n.children, depth + 1, out);
    }
}

/// Memory tidy (resort): one process batch-reparents - dry-run validates every op (parse/cycle/self-parent); --go writes and syncs once.
/// A successful write bumps the counter +1; at the threshold an alert is returned (§3.8 auto-trigger - the AI sees TIDY and runs three tidy rounds).
/// AI behavior config (agent.json): --set key=value writes a key; no args prints the whole file for the AI to read.

pub fn run_agent_config(set: Option<&str>) -> Result<()> {
    if let Some(kv) = set {
        return emit_result(agent_config_set_result(kv)?);
    }
    emit_result(agent_config_read_result()?)
}

/// The caller still owns write gates and maintenance; this does not authorize a write.
pub fn agent_config_set_result(kv: &str) -> Result<ResultEnvelope> {
    let (k, v) = kv
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("format must be key=value (e.g. diary_mode=verbose)"))?;
    let val =
        serde_json::from_str::<serde_json::Value>(v).unwrap_or_else(|_| serde_json::json!(v));
    respire_app::service::write_agent_config_key(k, &val)?;
    agent_config_updated_result(k, v, &val)
}

/// Format the original setter result after Runtime has verified a CAS candidate.
pub fn agent_config_updated_result(k: &str, v: &str, val: &serde_json::Value) -> Result<ResultEnvelope> {
    let mut info = serde_json::json!({"action":"updated", "key":k, "value":val});
    if k.trim() == "read_heat" {
        info["read_heat_runtime"] = respire_app::read_heat::receipt()?;
    }
    Ok(ResultEnvelope::new(
        "agent-config",
        OutputStatus::Ok,
        info,
        vec![OutputItem::new(k, OutputStatus::Ok, v)],
    ))
}

/// Keep Core-owned read-heat and recall-fusion receipts on the Runtime side.
pub fn agent_config_read_result() -> Result<ResultEnvelope> {
    let mut config = respire_app::service::read_agent_config();
    let object = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("agent config must be an object"))?;
    object.insert("readonly".into(), serde_json::json!(respire_app::service::readonly_mode()));
    object.insert("memory_off".into(), serde_json::json!(respire_app::service::off_mode()));
    object.insert("workspace_mode".into(), serde_json::json!(respire_app::service::workspace_mode()));
    object.insert("read_heat_runtime".into(), respire_app::read_heat::receipt()?);
    object.insert("recall_fusion_runtime".into(), respire_app::recall_fusion::receipt()?);
    let rows = object
        .iter()
        .map(|(key, value)| OutputItem::new(key, OutputStatus::Ok, value.to_string()))
        .collect();
    Ok(ResultEnvelope::new(
        "agent-config",
        OutputStatus::Ok,
        config,
        rows,
    ))
}

pub fn note_write_maintenance() -> Option<String> {
    match respire_app::service::counter_bump(&respire_app::service::maintenance_path()) {
        Ok((n, t)) if n >= t => {
            Some(format!(
                "TIDY {n} new entries since last tidy (>= threshold {t}) - AI should judge whether to run the \u{a7}3.8 three-round tidy now (a successful resort --go resets the counter)"
            ))
        }
        Ok(_) => None,
        Err(e) => {
            eprintln!("WARN maintenance counter failed: {e}");
            None
        }
    }
}

pub fn run_resort(
    go: bool,
    spec: Option<&str>,
    status: bool,
    reset: bool,
    threshold: Option<u64>,
) -> Result<()> {
    let mpath = respire_app::service::maintenance_path();
    if status {
        let (n, t) = respire_app::service::counter_peek(&mpath);
        let state = if n >= t {
            OutputStatus::Warn
        } else {
            OutputStatus::Ok
        };
        let mut result = ResultEnvelope::new(
            "resort",
            state,
            serde_json::json!({"count":n,"threshold":t}),
            vec![OutputItem::new("tidy counter", state, format!("{n}/{t}"))],
        );
        if n >= t {
            result.actions.push("resort --spec <json> --go".into());
        }
        return emit_result(result);
    }
    if reset {
        respire_app::service::counter_reset(&mpath, &now_stamp())?;
        return emit_result(ResultEnvelope::new(
            "resort",
            OutputStatus::Ok,
            serde_json::json!({"counter":"reset"}),
            vec![OutputItem::new("tidy counter", OutputStatus::Ok, "reset")],
        ));
    }
    if let Some(t) = threshold {
        respire_app::service::counter_set_threshold(&mpath, t)?;
        if spec.is_none() {
            return emit_result(ResultEnvelope::new(
                "resort",
                OutputStatus::Ok,
                serde_json::json!({"threshold":t}),
                vec![OutputItem::new(
                    "threshold",
                    OutputStatus::Ok,
                    t.to_string(),
                )],
            ));
        }
    }
    let spec = spec.ok_or_else(|| respire_app::input_error::InputError("need --spec (or --status to inspect the counter / --reset to zero it / --threshold to set it)".into()))?;
    let session = build_session()?;
    let store = build_local()?;
    let v: serde_json::Value = serde_json::from_str(spec).map_err(|e| {
        respire_app::input_error::InputError(format!("spec JSON failed to parse: {e} (shape {{\"ops\":[{{\"id\":\"child\",\"parent\":\"parent\"}}]}})"))
    })?;
    let ops = v["ops"].as_array().cloned().unwrap_or_default();
    if ops.is_empty() {
        return Err(respire_app::input_error::InputError("spec has no ops (shape {\"ops\":[{\"id\":...,\"parent\":...}]})".into()).into());
    }
    let all = store.all(true)?;
    let mut moved = 0usize;
    let mut rows = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let (Some(cid), Some(pid)) = (op["id"].as_str(), op["parent"].as_str()) else {
            return Err(respire_app::input_error::InputError(format!("op#{i} missing id/parent string")).into());
        };
        let child = respire_app::service::resolve_prefix(&all, cid)?;
        let parent = respire_app::service::resolve_prefix(&all, pid)?;
        if child == parent {
            return Err(respire_app::input_error::InputError(format!("op#{i} cannot parent itself: {child}")).into());
        }
        for anc in store.ancestor_chain(&parent)? {
            if anc == child {
                return Err(respire_app::input_error::InputError(format!(
                    "op#{i} cycle: new parent {} is a descendant of {}",
                    respire_app::service::short_id(&parent),
                    respire_app::service::short_id(&child)
                )).into());
            }
        }
        if go {
            respire_app::service::reparent(&session, &store, &child, &parent)?;
        }
        moved += 1;
        rows.push(OutputItem::new(
            respire_app::service::short_id(&child),
            if go {
                OutputStatus::Ok
            } else {
                OutputStatus::Pending
            },
            format!(
                "-> {} {}",
                respire_app::service::short_id(&parent),
                all.iter()
                    .find(|m| m.id == child)
                    .map(|m| m.local_title.as_str())
                    .unwrap_or("")
            ),
        ));
    }
    if go && moved > 0 {
        respire_app::service::counter_reset(&respire_app::service::maintenance_path(), &now_stamp())?;
        auto_sync(&session, &store);
        emit_result(ResultEnvelope::new(
            "resort",
            OutputStatus::Ok,
            serde_json::json!({"moved":moved,"applied":true,"counter":"reset"}),
            rows,
        ))?;
    } else {
        let mut result = ResultEnvelope::new(
            "resort",
            OutputStatus::Pending,
            serde_json::json!({"moved":moved,"applied":false}),
            rows,
        );
        result.actions.push("resort --spec <json> --go".into());
        emit_result(result)?;
    }
    Ok(())
}

/// Daily identity is stable even when its display title or importance changes.
/// Legacy same-user/title tracks still participate in the existing atomic merge.
pub fn diary_track_identity(user: &str, day: &str) -> (String, String) {
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, format!("activity-track/{user}/{day}").as_bytes());
    (id.to_string(), format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}"))
}

pub fn diary_track_marker(day: &str) -> String {
    format!("\u{3010}\u{6d3b}\u{52a8}\u{8f68}\u{8ff9}\u{3011}{day}")
}

pub fn select_diary_tracks<'a>(
    memories: &'a [respire_app::StoredMemory],
    track_id: &str,
    track_title: &str,
    mut accepted_author: impl FnMut(&respire_app::StoredMemory) -> Result<bool>,
) -> Result<Vec<&'a respire_app::StoredMemory>> {
    let mut tracks = Vec::new();
    for memory in memories.iter().filter(|memory| !memory.deleted) {
        // Envelope IDs and wire owners are not authenticated payload identity.
        // Accept authors only from the logical identity or unlocked local profile.
        if memory.id == track_id || memory.local_title == track_title {
            let accepted = accepted_author(memory)?;
            anyhow::ensure!(memory.id != track_id || accepted,
                "daily track author does not match this profile; append was not applied");
            if accepted { tracks.push(memory); }
        }
    }
    tracks.sort_by(|left, right| {
        (left.id != track_id).cmp(&(right.id != track_id))
            .then_with(|| left.local_created_at.cmp(&right.local_created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(tracks)
}
/// Deepen the tree: --auto one-shots sub-roots under every flat large root; --root inspects/applies one root.


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_tree_deepen.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/report_orphans.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_bench_run.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/build_classify_backend.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_diary.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_tree_float.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_share_import.rs"));

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/parts/run_local_inner.rs"));
