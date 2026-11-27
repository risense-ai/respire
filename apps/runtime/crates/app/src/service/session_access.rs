/// Local session overview (recovery key is masked; full text needs an explicit reveal).
pub fn session_info() -> SessionInfo {
    match auth::read_session_json() {
        Ok(d) => {
            // From v4, session.json no longer stores plaintext keys (super/secret_key/pass all removed),
            // the only test for "local key material exists" is whether wrapped_urk is present —
            // the old impl looked at secret_key/secret, which is always false on v4, so offline local mode was judged "not connected"
            // and kept prompting login (reported 2026-09-16).
            let has_wrap = d["wrapped_urk"].as_str().is_some_and(|s| !s.is_empty());
            let legacy = d["secret_key"].as_str().is_some_and(|s| !s.is_empty())
                || d["secret"].as_str().is_some_and(|s| !s.is_empty());
            let secret = d["secret_key"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| d["secret"].as_str())
                .unwrap_or("");
            SessionInfo {
                has_session: true,
                user: d["user"].as_str().unwrap_or("").to_owned(),
                addr: d["addr"].as_str().unwrap_or("").to_owned(),
                has_token: d["token"].as_str().is_some_and(|t| !t.is_empty()),
                secret_masked: mask_secret(secret),
                has_local_keys: has_wrap || legacy,
            }
        }
        Err(_) => SessionInfo {
            has_session: false,
            user: String::new(),
            addr: String::new(),
            has_token: false,
            secret_masked: String::new(),
            has_local_keys: false,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub has_session: bool,
    pub user: String,
    pub addr: String,
    pub has_token: bool,
    pub secret_masked: String,
    pub has_local_keys: bool,
}

/// Full recovery key (local only; UI must confirm twice).
pub fn session_secret_full() -> Result<String> {
    let d = auth::read_session_json()?;
    // Old v4 sessions kept inline keys; any credential claim must stay on its bound backend.
    let bound = ["keyring_account", "keyring_backend", "auth_operation_id"]
        .iter().any(|field| d.get(*field).is_some());
    if d["vault_version"].as_i64().is_some_and(|version| version >= 4) && bound {
        let alias = d["keyring_account"].as_str().filter(|value| !value.is_empty())
            .context("session recovery credential alias is missing")?;
        let backend = d["keyring_backend"].as_str().filter(|value| !value.is_empty())
            .context("session recovery credential backend is missing")?;
        let secret = crate::keystore::read_imported(alias, "super", backend)?;
        auth::unlock_session_keys(&d, "", Some(&secret), alias)
            .context("recovery credential does not unlock the selected session")?;
        return Ok(secret);
    }
    d["secret_key"].as_str().filter(|value| !value.is_empty())
        .or_else(|| d["secret"].as_str().filter(|value| !value.is_empty()))
        .map(str::to_owned)
        .context("session recovery secret is unavailable")
}

/// New-device five-piece kit (manual handoff across devices; local only).
pub fn session_five_keys() -> Result<serde_json::Value> {
    let d = auth::read_session_json()?;
    Ok(serde_json::json!({
        "pass": d["pass"],
        "super": d["super"],
        "secret": d["secret"],
        "secret_key": d["secret_key"],
        "kdf_salt": d["kdf_salt"],
        "wrapped_urk": d["wrapped_urk"],
        "urk_nonce": d["urk_nonce"],
        "vault_version": d["vault_version"],
    }))
}

pub fn register(addr: &str, user: &str, pass: &str, super_pass: &str) -> Result<Option<String>> {
    validate_client_config()?;
    auth::register(addr, user, pass, super_pass)
}

pub fn login(
    addr: &str,
    user: &str,
    pass: &str,
    super_pass: Option<&str>,
    secret_key: Option<&str>,
    reset_vault: bool,
) -> Result<Option<String>> {
    validate_client_config()?;
    auth::login(addr, user, pass, super_pass, secret_key, reset_vault)
}

pub fn keygen() -> Result<(PathBuf, String)> {
    validate_client_config()?;
    auth::keygen()
}
