
/// Default server address (client can change it; stored in ~/.rsrs/client.json, separate from session.json auth).
pub const DEFAULT_SERVER_ADDR: &str = "https://api.rsrs.rs";

/// Expand `~` (`~/` and Windows `~\` both count) — **the only impl**; do not rewrite elsewhere.
///
/// Why (2026-09-20 audit): this fn used to have three independent impls (two here + web.rs),
/// and they disagreed — inject.rs accepted `~\`, service.rs three sites only `~/`, so on Windows
/// `~\...` paths did not expand. Now collected here; every caller uses this.
pub fn expand_tilde(v: &str) -> PathBuf {
    if let Some(rest) = v.strip_prefix("~/").or_else(|| v.strip_prefix("~\\")) {
        if let Ok(h) = home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(v)
}

/// Resolve `RSRS_DATA_DIR` to an absolute path (None if unset or empty).
/// Semantics: the env var names the profile root — the `main` profile is that root, space profiles live under its `accounts/`,
/// and client.json lives there too (an isolated instance must not write config into the real user dir).
pub fn env_root_dir() -> Option<PathBuf> {
    let v = crate::env::var("RSRS_DATA_DIR").ok()?;
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if internally_configured_root(v) {
        return None;
    }
    Some(expand_tilde(v))
}

// Sunk from the client-side migration module (WP-2c): engine flows must classify the
// internally configured default home, and the domain crate cannot reach back into the
// client crate. The client migration module re-imports this function.
const DEFAULT_ENV: &str = "RSRS_DEFAULT_DATA_DIR";

/// The SDK accepts the original data-root environment variable. This marker lets
/// app configuration retain its default-home and active-profile semantics.
pub fn internally_configured_root(value: &str) -> bool {
    crate::env::var(DEFAULT_ENV).ok().is_some_and(|marker| {
        equivalent_path(
            &crate::service::expand_tilde(&marker),
            &crate::service::expand_tilde(value),
        ) && equivalent_path(
            &crate::service::expand_tilde(value),
            &crate::service::default_data_dir(),
        )
    })
}

fn equivalent_path(left: &Path, right: &Path) -> bool {
    let absolute = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|root| root.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    };
    let normalize = |path: PathBuf| {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    };
    let left = normalize(absolute(left));
    let right = normalize(absolute(right));
    if cfg!(windows) {
        left.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
    } else {
        left == right
    }
}

/// client.json path is special: it itself decides data_dir, so **usually** we always read the copy in the default dir
/// (otherwise after changing data_dir the new dir has no client.json and config locks itself).
///
/// **Exception (2026-09-20)**: when `RSRS_DATA_DIR` is set, client.json is read from that root
/// (`<root>/client.json`). Why: that env var is the only isolation entry for tests/multi-lib; if config still landed in the real
/// user dir, an isolated instance switching profiles would write the real client.json and pollute the user env (hit twice:
/// after teardown the real library status became "user local, 0 entries" until data_dir was cleared by hand).
/// An isolated root carries its own client.json — config follows the library — and cannot self-lock, because
/// data_dir is decided by the env var (in-tree first), not this file.
/// Host profile transactions preserve the complete configuration on failure.
pub fn client_config_path() -> PathBuf {
    if let Some(root) = env_root_dir() {
        return root.join("client.json");
    }
    home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rsrs")
        .join("client.json")
}

fn read_client_config() -> Option<serde_json::Value> {
    std::fs::read_to_string(client_config_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
}

pub fn validate_client_config() -> Result<()> {
    read_client_config_for_write().map(|_| ())
}

pub fn client_config_lock() -> Result<std::fs::File> {
    let file = client_config_lock_unchecked(true)?;
    host_config_transaction::ensure_no_host_config_transaction()?;
    Ok(file)
}

fn client_config_lock_unchecked(wait: bool) -> Result<std::fs::File> {
    let path = client_config_path().with_extension("json.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
        .with_context(|| format!("cannot open client config lock {}", path.display()))?;
    if wait {
        file.lock().with_context(|| format!("cannot lock client config {}", path.display()))?;
    } else {
        file.try_lock().with_context(|| format!("client config is busy: {}", path.display()))?;
    }
    Ok(file)
}

fn write_client_config(data: &serde_json::Value) -> Result<()> {
    anyhow::ensure!(
        data.is_object(),
        "client config write rejected: value must be a JSON object, found {}",
        json_type_name(data)
    );
    let path = client_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    replace_client_config(&path, json.as_bytes())?;
    Ok(())
}

fn replace_client_config(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("client.json");
    let temp = path.with_file_name(format!(".{file_name}.tmp.{}.{}", std::process::id(), sequence));
    let write = || -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("failed to create temporary client config {}", temp.display()))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .with_context(|| format!("failed to write temporary client config {}", temp.display()))?;
        drop(file);
        std::fs::rename(&temp, path)
            .with_context(|| format!("failed to replace client config {}", path.display()))?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub fn read_client_config_for_write() -> Result<serde_json::Value> {
    host_config_transaction::ensure_no_host_config_transaction()?;
    crate::memory::bge::m3_model_dir()?;
    let path = client_config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}));
        }
        Err(error) => return Err(error)
            .with_context(|| format!("cannot read client config {}", path.display())),
    };
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .with_context(|| format!("client config {} contains invalid JSON", path.display()))?;
    anyhow::ensure!(
        value.is_object(),
        "client config {} must contain a JSON object, found {}",
        path.display(),
        json_type_name(&value)
    );
    for key in ["lang", "data_dir", "addr", "api_base", "service_mode", "engine", "model"] {
        if let Some(field) = value.get(key) {
            anyhow::ensure!(field.is_string(), "client config {} has a {key} of type {}, expected string", path.display(), json_type_name(field));
        }
    }
    for key in ["autosync", "cure_auto"] {
        if let Some(field) = value.get(key) {
            anyhow::ensure!(field.is_boolean(), "client config {} has a {key} of type {}, expected boolean", path.display(), json_type_name(field));
        }
    }
    if let Some(field) = value.get("rpc_parallelism") {
        anyhow::ensure!(field.as_u64().is_some_and(|value| value <= u32::MAX as u64), "client config {} has invalid rpc_parallelism", path.display());
    }
    Ok(value)
}

// ── data dir (client.json data_dir → RSRS_DATA_DIR → ~/.rsrs) ──
// For tests and multi-lib maintenance: changing data_dir switches the whole library (db/session/lock follow).
// Order: env var wins (one-shot override for CI/tests) → client.json data_dir → default ~/.rsrs.

pub fn default_data_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rsrs")
}

/// Main-profile dir: `RSRS_DATA_DIR` when isolated, else default `~/.rsrs`.
/// **Every "back to main" path must use this**, not a hard `default_data_dir()` — otherwise an isolated instance
/// switching back to main would land in the real user dir (hit 2026-09-20).
pub fn main_data_dir() -> PathBuf {
    env_root_dir().unwrap_or_else(default_data_dir)
}

/// Effective data dir. Empty/missing fall back to default — never panics.
///
/// Order: `RSRS_DATA_DIR` (test/CI one-shot) > client.json data_dir > default.
///
/// **Exception (2026-09-20)**: if client.json data_dir sits inside the `RSRS_DATA_DIR` tree,
/// prefer it. Why: `space use` writes client.json data_dir, which always lives under
/// that root (`accounts_root()` also follows RSRS_DATA_DIR); if the env var still always won,
/// switching spaces in isolation would **silently fail** — session falls back to the root, the space profile is empty (hit in tests).
/// Semantically the env var names the root; switching moves inside the root; they do not conflict.
pub fn data_dir() -> PathBuf {
    data_dir_from_config(read_client_config().as_ref())
}

/// Resolve existing parents before appending absent names; never guess filesystem identity.
pub fn canonical_profile_path(path: &Path) -> Option<PathBuf> {
    if path.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
        return None;
    }
    let mut current = path;
    let mut missing = Vec::new();
    loop {
        match current.canonicalize() {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(current) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return None,
                }
                let name = current.file_name()?;
                #[cfg(windows)]
                {
                    let spelling = name.to_str()?;
                    if spelling.ends_with('.') || spelling.ends_with(' ') {
                        return None;
                    }
                }
                missing.push(name);
                current = current.parent()?;
            }
            Err(_) => return None,
        }
    }
}

pub fn data_dir_from_config(config: Option<&serde_json::Value>) -> PathBuf {
    let env_root = env_root_dir();
    let from_config = config
        .and_then(|d| {
            d["data_dir"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
        .map(|v| expand_tilde(&v));
    match (env_root, from_config) {
        (Some(root), Some(cfg))
            if canonical_profile_path(&cfg).is_some_and(|profile| {
                canonical_profile_path(&root).is_some_and(|root| profile.starts_with(root))
            }) =>
        {
            cfg
        }
        (Some(root), _) => root,
        (None, Some(cfg)) => cfg,
        (None, None) => default_data_dir(),
    }
}

#[derive(serde::Serialize)]
pub struct ResolvedRoots {
    pub schema_version: u32,
    pub layout: &'static str,
    pub installation_root: PathBuf,
    pub device_config_root: PathBuf,
    pub client_config_path: PathBuf,
    pub account_data_root: PathBuf,
    pub accounts_root: PathBuf,
    pub asset_root: PathBuf,
    pub runtime_state_root: PathBuf,
    pub legacy_whole_isolation: bool,
    pub sources: std::collections::BTreeMap<&'static str, &'static str>,
}

pub fn resolved_roots() -> Result<ResolvedRoots> {
    // Do not clean parent traversals: symlinks can change their actual target.
    let absolute = |path: PathBuf| -> Result<PathBuf> {
        anyhow::ensure!(
            !path
                .components()
                .any(|part| part == std::path::Component::ParentDir),
            "root summary does not support parent traversal; use an explicit root without '..'"
        );
        Ok(std::path::absolute(path)?)
    };
    let asset_root = absolute(crate::memory::bge::m3_model_dir()?)?;
    let installation_root = absolute(main_data_dir())?;
    let client_config_path = absolute(client_config_path())?;
    let config = read_client_config_for_write()?;
    if let Some(value) = config["data_dir"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        absolute(expand_tilde(value))?;
    }
    let device_config_root = client_config_path
        .parent()
        .context("client config has no parent")?
        .to_path_buf();
    let account = data_dir_from_config(Some(&config));
    let legacy_whole_isolation = env_root_dir().is_some();
    let root_source = if legacy_whole_isolation {
        "RSRS_DATA_DIR"
    } else {
        "default_data_dir"
    };
    let account_source = if config["data_dir"]
        .as_str()
        .is_some_and(|value| !value.is_empty() && expand_tilde(value) == account)
    {
        "client.json:data_dir"
    } else {
        root_source
    };
    let asset_source = if crate::env::var("RSRS_M3_DIR").is_ok_and(|value| !value.trim().is_empty())
    {
        "RSRS_M3_DIR"
    } else {
        root_source
    };
    anyhow::ensure!(
        !legacy_whole_isolation || asset_source == "RSRS_M3_DIR" || asset_root.starts_with(&installation_root),
        "model root expansion conflicts with legacy isolation; use an explicit absolute data root or model root"
    );
    Ok(ResolvedRoots {
        schema_version: 1,
        layout: "phase1",
        runtime_state_root: installation_root.join("runtime"),
        installation_root,
        device_config_root,
        client_config_path,
        account_data_root: absolute(account)?,
        accounts_root: absolute(accounts_root())?,
        asset_root,
        legacy_whole_isolation,
        sources: std::collections::BTreeMap::from([
            ("installation_root", root_source),
            ("device_config_root", root_source),
            ("client_config_path", root_source),
            ("account_data_root", account_source),
            ("accounts_root", root_source),
            ("asset_root", asset_source),
            ("runtime_state_root", root_source),
        ]),
    })
}

/// Set the data dir (writes client.json). Empty string = clear back to default.
pub fn set_data_dir(dir: &str) -> Result<()> {
    let _config_lock = client_config_lock()?;
    let _ = read_client_config_for_write()?;
    let dir = dir.trim();
    let requested = if dir.is_empty() {
        main_data_dir()
    } else {
        expand_tilde(dir)
    };
    check_runtime_profile(&requested)?;
    let mut data = read_client_config_for_write()?;
    if dir.is_empty() {
        if let Some(o) = data.as_object_mut() {
            o.remove("data_dir");
        }
    } else {
        if !requested.is_absolute() {
            anyhow::bail!("data dir must be an absolute path or a home-relative path");
        }
        data["data_dir"] = serde_json::json!(dir);
    }
    write_client_config(&data)
}

// ── multi-account profiles on one machine: each account has its own data dir (library/keys/session switch with data_dir) ──
// Profile root is accounts/ under the default dir (**does not follow client.json data_dir** — otherwise after a switch
// accounts_root would become <profile>/accounts and other profiles would vanish).
// But **when RSRS_DATA_DIR is set it must follow** (fixed 2026-09-20): that var is the only isolation entry
// for tests/multi-lib; if the profile root still landed in real HOME, isolated space profiles polluted the user dir (hit:
// a web isolation instance created work/sales under ~/.rsrs/accounts).
// Scene: several accounts on one machine (work/personal/test); login no longer needs logout --full to wipe key material.

pub fn accounts_root() -> PathBuf {
    env_root_dir().unwrap_or_else(default_data_dir).join("accounts")
}

/// Account profile dir (name check: letters/digits/-/_; no path traversal).
pub fn account_dir(name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("account profile name allows only letters/digits/-/_; got `{name}`");
    }
    Ok(accounts_root().join(name))
}

pub fn session_user_of_dir(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["user"].as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

/// List every local account profile: main (main-profile dir) + each dir under accounts/.
pub fn account_list() -> Result<serde_json::Value> {
    let current = data_dir();
    let main_dir = main_data_dir();
    let mut rows = vec![serde_json::json!({
        "name": "main",
        "dir": main_dir.to_string_lossy(),
        "user": session_user_of_dir(&main_dir),
        "current": current == main_dir,
    })];
    if let Ok(rd) = std::fs::read_dir(accounts_root()) {
        let mut names: Vec<std::ffi::OsString> =
            rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        names.sort();
        for n in names {
            let dir = accounts_root().join(&n);
            if !dir.join("session.json").exists() && !database_path(&dir)?.exists() {
                continue;
            }
            rows.push(serde_json::json!({
                "name": n.to_string_lossy(),
                "dir": dir.to_string_lossy(),
                "user": session_user_of_dir(&dir),
                "current": current == dir,
            }));
        }
    }
    Ok(serde_json::json!({ "accounts": rows, "current_dir": current.to_string_lossy() }))
}

/// Switch account profile: main = back to default dir; others = accounts/<name> (create empty if missing; hint to login/register).
pub fn account_use(name: &str) -> Result<serde_json::Value> {
    validate_client_config()?;
    require_profile_change_host()?;
    let is_main = name == "main";
    let dir = if is_main {
        main_data_dir()
    } else {
        let d = account_dir(name)?;
        std::fs::create_dir_all(&d)?;
        d
    };
    set_data_dir(&dir.to_string_lossy())?;
    let user = session_user_of_dir(&dir);
    let hint = if user.is_empty() {
        "this profile has no account yet \u{2014} run rsrs login (existing account) or rsrs register (new account)"
    } else {
        "switched to this profile \u{2014} local library/keys/session follow the profile and do not mix"
    };
    Ok(serde_json::json!({
        "name": if is_main { "main" } else { name },
        "dir": dir.to_string_lossy(),
        "user": user,
        "hint": hint,
    }))
}

/// Delete an account profile (library and keys are gone; needs --yes). Cannot delete the active profile.
pub fn account_remove(name: &str) -> Result<()> {
    validate_client_config()?;
    require_profile_change_host()?;
    let dir = account_dir(name)?;
    if !dir.exists() {
        return Err(crate::input_error::InputError(format!("no such account profile: {}", dir.display())).into());
    }
    if data_dir() == dir {
        return Err(crate::input_error::InputError("this profile is in use \u{2014} run rsrs account use <other> before deleting".into()).into());
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
