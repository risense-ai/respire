
// ── maintenance counter (auto-triggers memory resort) ──
// data_dir/maintenance.json: writes_since_resort write count (remember/merge success +1),
// at threshold the store output prints a broom alert — the AI then runs §3.8 resort; resort --go success zeros it.
// Default threshold 30; env RSRS_RESORT_HINT can override (tests).

pub fn maintenance_path() -> PathBuf {
    data_dir().join("maintenance.json")
}

/// Pure logic: count +1, return (new value, threshold). At threshold, alert (every write until reset, so resort happens now).
pub fn counter_bump(path: &Path) -> Result<(u64, u64)> {
    let mut v = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let n = v["writes_since_resort"].as_u64().unwrap_or(0) + 1;
    let threshold = crate::env::var("RSRS_RESORT_HINT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|t| *t > 0)
        .or_else(|| v["resort_threshold"].as_u64().filter(|t| *t > 0))
        .unwrap_or(30);
    v["writes_since_resort"] = serde_json::json!(n);
    if let Some(o) = v.as_object_mut() {
        o.entry("resort_threshold".to_owned())
            .or_insert_with(|| serde_json::json!(threshold));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_vec(&v)?).map_err(|e| {
        anyhow::anyhow!(
            "failed to write maintenance counter ({}): {e}",
            path.display()
        )
    })?;
    Ok((n, threshold))
}

/// Reset (called after resort --go succeeds); records resort_at.
pub fn counter_reset(path: &Path, stamp: &str) -> Result<()> {
    let mut v = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v["writes_since_resort"] = serde_json::json!(0);
    v["resort_at"] = serde_json::json!(stamp);
    std::fs::write(path, serde_json::to_vec(&v)?)
        .map_err(|e| anyhow::anyhow!("failed to write maintenance counter: {e}"))
}

/// Last resort time (maintenance.json resort_at, written by counter_reset).
/// §3.8 take-set: list --since-resort uses this to bound entries added since last resort.
/// Never resorted (no key) → None.
pub fn resort_at() -> Option<String> {
    std::fs::read_to_string(maintenance_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["resort_at"].as_str().map(ToOwned::to_owned))
        .filter(|s| !s.trim().is_empty())
}

/// Normalize --since: a bare date (YYYY-MM-DD) becomes that day's 00:00:00Z; otherwise keep as-is.
/// RFC3339 fixed-length prefixes mean lexicographic compare is time order; list filters by comparing strings.
pub fn normalize_since(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("--since must not be empty");
    }
    Ok(if s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') {
        format!("{s}T00:00:00Z")
    } else {
        s.to_owned()
    })
}

/// Later of two lower bounds: explicit --since vs --since-resort's resort_at.
/// Neither → 1970 epoch (no filter); missing resort_at (never resorted) is the same.
pub fn later_bound(explicit: Option<String>, from_resort: Option<String>) -> String {
    match (explicit, from_resort) {
        (Some(a), Some(b)) => {
            if a > b {
                a
            } else {
                b
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => "1970-01-01T00:00:00Z".to_owned(),
    }
}

/// Read the current count only (for doctor).
pub fn counter_peek(path: &Path) -> (u64, u64) {
    let v = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    match v {
        Some(v) => (
            v["writes_since_resort"].as_u64().unwrap_or(0),
            v["resort_threshold"].as_u64().unwrap_or(30),
        ),
        None => (0, 30),
    }
}

/// Set the alert threshold.
pub fn counter_set_threshold(path: &Path, t: u64) -> Result<()> {
    if t == 0 {
        anyhow::bail!("threshold must be >0");
    }
    let mut v = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v["resort_threshold"] = serde_json::json!(t);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_vec(&v)?)
        .map_err(|e| anyhow::anyhow!("failed to write maintenance counter: {e}"))
}

// ── agent config (agent.json — AI controls, separate from the app's client.json) ──
// Written by the client settings page; the AI (via the inject source) reads it and behaves. agent.json in the data dir, JSON;
// missing keys have sane defaults; a missing file is not an error. First key diary_mode; later AI control keys go here too.

pub fn agent_config_path() -> PathBuf {
    data_dir().join("agent.json")
}

pub fn read_agent_config() -> serde_json::Value {
    std::fs::read_to_string(agent_config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Validate Core-owned settings without publishing configuration or reloading it.
pub fn validate_agent_runtime_setting(key: &str, value: &serde_json::Value) -> Result<()> {
    let key = key.trim();
    if key == "recall_fusion" {
        crate::recall_fusion::effective(value)?;
    }
    if key == "read_heat" {
        anyhow::ensure!(value.is_boolean(), "read_heat must be true or false");
        crate::read_heat::configured()?;
        crate::read_heat::require_support()?;
    }
    Ok(())
}

pub fn write_agent_config_key(key: &str, value: &serde_json::Value) -> Result<()> {
    host_config_transaction::ensure_no_host_config_transaction()?;
    let key = key.trim();
    validate_agent_runtime_setting(key, value)?;
    if matches!(key, "memory_off" | "readonly") {
        let enabled = value.as_bool().ok_or_else(|| anyhow!("{key} must be true or false"))?;
        return set_workspace_mode(if !enabled { "normal" } else if key == "memory_off" { "off" } else { "readonly" });
    }
    if key == "workspace_mode" {
        return set_workspace_mode(value.as_str().ok_or_else(|| anyhow!("workspace_mode must be normal, readonly or off"))?);
    }
    if key == "recall_mode" && !matches!(value.as_str(), Some("fast" | "quality")) {
        anyhow::bail!("recall_mode must be fast or quality");
    }
    if key.trim().is_empty() {
        anyhow::bail!("key must not be empty");
    }
    let mut v = read_agent_config();
    if let Some(o) = v.as_object_mut() {
        o.insert(key.trim().to_owned(), value.clone());
    }
    if let Some(parent) = agent_config_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(agent_config_path(), serde_json::to_vec_pretty(&v)?)
        .map_err(|e| anyhow::anyhow!("failed to write agent.json: {e}"))?;
    if key == "read_heat" {
        crate::read_heat::reload()?;
    }
    Ok(())
}

/// Read-only mode (team read-only members): local flag; when true every write command is refused.
///
/// Relation to `space`: owner issues a read-only session with `space invite --readonly`,
/// member `space join` sets local agent.json `readonly=true`;
/// the server also refuses writes by session token (belt and braces — the client can be bypassed).
pub fn readonly_mode() -> bool {
    readonly_team() || match device_service_mode().as_deref() {
        Some(mode) => mode == "readonly",
        None => read_agent_config()["readonly"].as_bool().unwrap_or(false),
    }
}

/// Read-only **source=team**: written on space join --readonly (personal self-set read-only has no this key).
/// H2 anti-self-unlock applies only to team read-only — personal read-only is the owner's flag and must be clearable;
/// even if a team member unlocks locally, the server still refuses writes by token (the real fence is the server).
pub fn readonly_team() -> bool {
    read_agent_config()["readonly_team"]
        .as_bool()
        .unwrap_or(false)
}

/// Device disable mode suppresses memory reads and writes until explicitly changed.
/// Existing account-local flags remain compatible when no device mode has been saved.
fn device_service_mode() -> Option<String> {
    read_client_config().and_then(|config| config["service_mode"].as_str()
        .filter(|mode| matches!(*mode, "normal" | "readonly" | "off")).map(str::to_owned))
}

pub fn off_mode() -> bool {
    match device_service_mode().as_deref() {
        Some(mode) => mode == "off",
        None => read_agent_config()["memory_off"].as_bool().unwrap_or(false),
    }
}

/// Pause device memory work without confusing a team's read-only access with a device setting.
pub fn service_paused() -> bool {
    off_mode() || match device_service_mode().as_deref() {
        Some(mode) => mode == "readonly",
        None => !readonly_team() && read_agent_config()["readonly"].as_bool().unwrap_or(false),
    }
}

/// Workspace three-state (GUI/status): off > readonly > normal.
pub fn workspace_mode() -> &'static str {
    if off_mode() {
        "off"
    } else if readonly_mode() {
        "readonly"
    } else {
        "normal"
    }
}

/// Device service mode lives beside the active-profile pointer, never in an account.
/// Existing team permissions remain effective even when the device returns to normal.
pub fn set_workspace_mode(mode: &str) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    if !matches!(mode, "normal" | "readonly" | "off") {
        return Err(crate::input_error::InputError(format!("unknown mode `{mode}` \u{2014} choose: normal | readonly | off")).into());
    }
    let mut config = read_client_config_for_write()?;
    anyhow::ensure!(config.is_object(), "client config must be an object");
    config["service_mode"] = serde_json::json!(mode);
    write_client_config(&config)
}

/// Read-only gate: write commands must pass this. Err aborts (message is for the user and the AI).
pub fn ensure_writable() -> Result<()> {
    if readonly_mode() {
        let hint = if readonly_team() {
            "read-only is enforced by the server via the session token; editing local agent.json has no effect. To write, ask the space owner for a read-write session and run rsrs space join."
        } else {
            "restore normal service on this device with `agent-config --set workspace_mode=normal`."
        };
        return Err(anyhow!(
            "this space is read-only \u{2014} recall/list/show/diary/tree are allowed; write/edit/delete are not.\n  {hint}"
        ));
    }
    Ok(())
}

/// Trivia recording mode: concise = one daily activity-trail entry (default; append one line per event);
/// verbose = one diary entry per event. The AI reads this via agent-config.
pub fn diary_mode() -> String {
    read_agent_config()["diary_mode"]
        .as_str()
        .filter(|s| *s == "verbose" || *s == "concise")
        .unwrap_or("concise")
        .to_owned()
}

/// Append a timestamped line to the activity trail for today.
pub fn activity_append(existing: &str, line: &str) -> String {
    let mut s = existing.to_owned();
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(line);
    s
}
