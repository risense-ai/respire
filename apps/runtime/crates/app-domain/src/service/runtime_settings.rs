
// ── subtree members (shared helper: share/grant take all descendants of a root) ──

/// Subtree members: root and all descendants (BFS). Root missing → empty.
pub fn subtree_members(
    all: &[crate::memory::model::StoredMemory],
    root: &str,
) -> std::collections::HashSet<String> {
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in all {
        if !m.deleted && !m.local_parent_id.is_empty() {
            children_of
                .entry(m.local_parent_id.as_str())
                .or_default()
                .push(m.id.as_str());
        }
    }
    let mut out = std::collections::HashSet::new();
    if !all.iter().any(|m| m.id == root && !m.deleted) {
        return out;
    }
    let mut frontier = vec![root];
    while let Some(cur) = frontier.pop() {
        if !out.insert(cur.to_owned()) {
            continue;
        }
        if let Some(kids) = children_of.get(cur) {
            frontier.extend(kids.iter().copied());
        }
    }
    out
}

/// Lightweight status: no session unlock, no embedder load; only store counts and config.
///
/// Why (2026-09-14): the client calls status at startup, and the old path `App::open()` blindly loaded
/// a 389MB ONNX (~2s); on Windows each fork also popped a console, stacking into "startup freeze".
/// status does not need vectors, so this path skips one model load.
pub fn status_light() -> Result<StatusInfo> {
    let (root, store) = open_store_with_profile()?;
    let (local_total, local_alive) = store.catalog_counts()?;
    Ok(StatusInfo {
        local_total,
        local_alive,
        remote_configured: remote_configured(),
        max_updated_at: store.max_updated_at().ok().flatten(),
        data_dir: root.to_string_lossy().into_owned(),
    })
}

/// Local memory snapshot identity for clients. This is not a sync cursor.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct MemoryRevision {
    pub profile: String,
    pub revision: String,
}

/// Capture the CLI-resolved profile once so identity and token name the same DB.
/// This path is strictly read-only and does not unlock, initialize, or scan memory.
pub fn memory_revision() -> Result<MemoryRevision> {
    let root = data_dir();
    check_runtime_profile(&root)?;
    let revision = crate::transport::local::read_memory_revision(&database_path(&root)?)?;
    Ok(MemoryRevision {
        profile: root.to_string_lossy().into_owned(),
        revision,
    })
}

pub fn server_addr() -> String {
    read_client_config()
        .and_then(|d| {
            d["addr"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| DEFAULT_SERVER_ADDR.to_owned())
}

/// Address the web form and resume use. Session addr wins, then client.json, then the public server.
/// Never returns an empty string.
pub fn preferred_addr(session_addr: &str) -> String {
    let session_addr = session_addr.trim().trim_end_matches('/');
    if session_addr.starts_with("http://") || session_addr.starts_with("https://") {
        return session_addr.to_owned();
    }
    let configured = server_addr();
    if !configured.is_empty() {
        return configured;
    }
    DEFAULT_SERVER_ADDR.to_owned()
}

/// Open the local web without asking the user to retype credentials.
///
/// Already-joined profiles (wrapped key on disk) return immediately. The super
/// password stays in the OS keyring and is loaded by the CLI when a command
/// unlocks. If the profile is not joined yet but this machine has stored the
/// login password, log in with that and the keyring super password.
/// The returned JSON never contains either password.
pub fn resume_session() -> Result<serde_json::Value> {
    let si = session_info();
    let addr = preferred_addr(&si.addr);
    let has_super = crate::keystore::load_super(&si.user).is_some();
    if si.has_local_keys {
        return Ok(serde_json::json!({
            "ok": true,
            "resumed": true,
            "need_login": false,
            "user": si.user,
            "addr": addr,
            "has_super": has_super,
        }));
    }
    if !si.user.is_empty() {
        if let Some(pass) = crate::keystore::load_login_pass(&si.user) {
            match login(&addr, &si.user, &pass, None, None, false) {
                Ok(issued) => {
                    return Ok(serde_json::json!({
                        "ok": true,
                        "resumed": true,
                        "need_login": false,
                        "user": si.user,
                        "addr": addr,
                        "has_super": true,
                        "super_issued": issued,
                    }));
                }
                Err(error) => {
                    return Ok(serde_json::json!({
                        "ok": false,
                        "resumed": false,
                        "need_login": true,
                        "user": si.user,
                        "addr": addr,
                        "has_super": has_super,
                        "error": format!("{error:#}"),
                    }));
                }
            }
        }
    }
    Ok(serde_json::json!({
        "ok": false,
        "resumed": false,
        "need_login": true,
        "user": si.user,
        "addr": addr,
        "has_super": has_super,
    }))
}

pub fn set_server_addr(addr: &str) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    validate_config_sync_change(true, false)?;
    let addr = addr.trim();
    if !(addr.starts_with("http://") || addr.starts_with("https://")) {
        anyhow::bail!("server address must start with http:// or https://");
    }
    let mut data = read_client_config_for_write()?;
    data["addr"] = serde_json::json!(addr);
    write_client_config(&data)
}

/// Auto-sync switch: RSRS_NO_AUTOSYNC env wins (set and not 0 → off);
/// then client.json autosync (the client "auto-sync after write" toggle);
/// default = on (if remote is configured, auto-sync; backward compatible).
pub fn autosync_enabled() -> bool {
    if crate::env::var("RSRS_NO_AUTOSYNC").is_ok_and(|v| v != "0") {
        return false;
    }
    read_client_config()
        .and_then(|d| d["autosync"].as_bool())
        .unwrap_or(true)
}

/// Actual write-path sync gate: always off inside tests.
///
/// Why (proven 2026-09-16): tests build fixtures with a fixed URK (`[7u8; 32]`); if the write path triggered
/// auto-sync, and that check read the **real** ~/.rsrs/session.json, a logged-in test machine
/// would push fake rows to the real cloud — ciphertext the local key cannot open (14 rows).
/// split_exec / deepen_apply each missed this and got one-off patches; this is the single gate:
/// every write-path auto-sync goes through this fn; new write paths do not need a private check.
pub fn autosync_active() -> bool {
    !cfg!(test) && autosync_enabled() && crate::snapshot::ensure_sync_allowed(&data_dir()).is_ok()
}

pub fn set_autosync(on: bool) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    validate_config_sync_change(false, on)?;
    let mut data = read_client_config_for_write()?;
    data["autosync"] = serde_json::json!(on);
    write_client_config(&data)
}

/// The original setters and private config validation share exactly this DB gate.
/// Caller holds the client config lock; a helper also holds root/profile ownership.
pub fn validate_config_sync_change(changing_addr: bool, enabling_autosync: bool) -> Result<()> {
    if (changing_addr || enabling_autosync) && database_path(&data_dir())?.exists() {
        crate::metadata_gate::ensure_sync_allowed(&open_store()?)?;
    }
    Ok(())
}

/// Periodic tree-cure switch (client.json cure_auto, default on): client background iterates cure every 10 minutes.
pub fn cure_auto_enabled() -> bool {
    read_client_config()
        .and_then(|d| d["cure_auto"].as_bool())
        .unwrap_or(true)
}

pub fn set_cure_auto(on: bool) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    let mut data = read_client_config_for_write()?;
    data["cure_auto"] = serde_json::json!(on);
    write_client_config(&data)
}

/// Saved RPC compute worker limit (`client.json` `rpc_parallelism`), bounded by available CPU parallelism.
/// Two extra lightweight read slots remain available. `0` is stored as absent.
/// The resident runtime reads this only at start.
pub fn rpc_parallelism_setting() -> Option<usize> {
    read_client_config()
        .and_then(|data| data.get("rpc_parallelism").and_then(|value| value.as_u64()))
        .map(|value| value as usize)
        .filter(|value| *value > 0)
}

pub fn set_rpc_parallelism(workers: u32) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    let mut data = read_client_config_for_write()?;
    if workers == 0 {
        if let Some(obj) = data.as_object_mut() {
            obj.remove("rpc_parallelism");
        }
    } else {
        data["rpc_parallelism"] = serde_json::json!(workers);
    }
    write_client_config(&data)
}

