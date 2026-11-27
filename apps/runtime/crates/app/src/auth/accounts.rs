/// Register: login password is auth only; super password (v4 single factor) wraps URK and uploads the vault.
/// A brand-new profile generates the super password. An existing keygen vault uses `--super`, then the keyring.
pub fn register(addr: &str, user: &str, pass: &str, super_pass_arg: &str) -> Result<Option<String>> {
    let mut data = session_for_other_user(user)?;
    let issued: Option<String>;
    if data["wrapped_urk"].as_str().filter(|s| !s.is_empty()).is_some() {
        // keygen (or this same profile) already wrapped the URK. Bind it to this username.
        data["user"] = serde_json::Value::String(user.to_owned());
        let super_pass = existing_super(&data, super_pass_arg, user)?;
        unlock_session_keys(&data, data["pass"].as_str().unwrap_or(pass), Some(&super_pass), user)?;
        crate::keystore::save_super(user, &super_pass)?;
        issued = Some(super_pass);
    } else {
        let (super_pass, _urk, kdf_salt, wrapped_urk, urk_nonce) = new_vault_v4()?;
        apply_vault_v4(&mut data, user, &super_pass, kdf_salt, wrapped_urk, urk_nonce)?;
        issued = Some(super_pass);
    }
    let authorization = registration_authorization(addr, user, pass)?;
    let token = authorization["token"].as_str().context("registration token is missing")?;
    if let Err(e) = crate::keystore::save_login_pass(user, pass) {
        eprintln!("warning: login password not stored in keyring: {e}");
    }
    for field in ["auth_salt", "user", "addr", "token", "session_id"] {
        data[field] = authorization[field].clone();
    }
    data.as_object_mut().map(|o| o.remove("pass"));
    write_session_json(&data)?;
    put_vault(addr, token, &data)?;
    Ok(issued)
}

/// The caller must durably admit the operation before this non-idempotent POST.
/// This helper changes neither local credentials nor the selected profile.
pub fn registration_authorization(addr: &str, user: &str, pass: &str) -> Result<serde_json::Value> {
    let auth_salt = server_authentication_salt(addr, user)?;
    let pass_hash = crypto::derive_pass_hash(pass, &auth_salt)?;
    let agent = ureq::AgentBuilder::new().redirects(0)
        .timeout(std::time::Duration::from_secs(30)).build();
    let resp = match agent.post(&format!("{}/register", addr.trim().trim_end_matches('/')))
        .send_json(serde_json::json!({
            "user": user,
            "pass_hash": pass_hash,
            "salt": auth_salt,
            "device_name": device_name(),
        })) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _r)) if code == 409 => {
            return Err(anyhow::anyhow!(
                "user \"{user}\" is already registered on this server \u{2014} use rsrs login --interactive --user {user} --pass <password>",
            ));
        }
        Err(e) => return Err(anyhow::anyhow!("register request failed: {e}")),
    };
    let reply: serde_json::Value = resp.into_json().map_err(|e| anyhow::anyhow!("failed to parse response: {e}"))?;
    let token = reply["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("register failed: {}", reply.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")))?
        .to_owned();
    Ok(serde_json::json!({"auth_salt":auth_salt,"user":user,
        "addr":addr.trim().trim_end_matches('/'),"token":token,
        "session_id":reply.get("session_id")}))
}

/// Login: send the login password to the server → fetch cloud vault and reconcile with local material → unlock using the cloud as source of truth.
/// v3/v2 accounts auto-upgrade to v4 after a successful login (URK unchanged, data stays).
/// Returns a newly issued super password (copy it down now); None if one already exists.
pub fn login(
    addr: &str,
    user: &str,
    pass: &str,
    super_pass: Option<&str>,
    secret_key: Option<&str>,
    reset_vault: bool,
) -> Result<Option<String>> {
    let mut authenticated = serde_json::json!({});
    authenticate_session(&mut authenticated, addr, user, pass)?;
    let mut data = read_session_json().unwrap_or_else(|_| serde_json::json!({}));
    if let Some(s) = secret_key.filter(|s| !s.is_empty()) {
        data["secret_key"] = serde_json::Value::String(s.to_owned());
    }

    // 0. Multi-account on one machine: if the login user differs from the current profile owner
    // (including offline with no owner), auto-create and switch — library/keys/session move with the
    // profile; the old profile stays in place (no need to logout --full to wipe material).
    let cur_user = data["user"].as_str().unwrap_or("").to_owned();
    if !cur_user.is_empty() && cur_user != user {
        data = session_for_other_user(user)?;
        if let Some(s) = secret_key.filter(|s| !s.is_empty()) {
            data["secret_key"] = serde_json::Value::String(s.to_owned());
        }
    } else if cur_user != user {
        crate::service::require_profile_change_host()?;
        let main_user = crate::service::session_user_of_dir(&crate::service::main_data_dir());
        if main_user == user {
            crate::service::set_data_dir("")?;
        } else {
            let dir = crate::service::account_dir(user)?;
            std::fs::create_dir_all(&dir)?;
            crate::service::set_data_dir(&dir.to_string_lossy())?;
        }
        data = read_session_json().unwrap_or_else(|_| serde_json::json!({}));
        if let Some(s) = secret_key.filter(|s| !s.is_empty()) {
            data["secret_key"] = serde_json::Value::String(s.to_owned());
        }
    }
    let has_wrap = data["wrapped_urk"].as_str().filter(|s| !s.is_empty()).is_some();

    // 1. Auth first: send the login password to the server. On failure local material is untouched —
    // the old impl unlocked locally first and died on "local key unlock failed" when material was
    // stale, never reaching the cloud or asking for the super password (reported 2026-09-16 on a new device).
    for field in ["user", "addr", "token", "session_id", "auth_salt", "pass"] {
        data[field] = authenticated[field].clone();
    }
    if let Err(e) = crate::keystore::save_login_pass(user, pass) {
        eprintln!("warning: login password not stored in keyring: {e}");
    }
    let token = data["token"].as_str().ok_or_else(|| anyhow!("login did not return a token"))?.to_owned();

    // 2. Fetch cloud vault and reconcile with local material
    let vault = fetch_vault(addr, &token)?;
    match vault {
        Some(vault) if has_wrap && vault_matches(&vault, &data) => {
            // Ordinary login verifies the existing key without migrating it.
            unlock_session_keys(&data, pass, super_pass, user).context(
                "local key unlock failed \u{2014} the keyring code does not match this account: pass the correct code with --super, or export from the old machine with keys-export",
            )?;
            // On unlock success, write back to the keyring: this branch never did that before —
            // a machine with full local material but an empty keyring had to pass --super/env every time
            // (reported 2026-09-18 on fslong-hasee; doctor said "nothing in the keyring").
            if let Some(s) = super_pass.filter(|s| !s.is_empty()) {
                if let Err(e) = crate::keystore::save_super(user, s) {
                    eprintln!("warning: super password not stored in keyring: {e}; this session can use RSRS_SUPER");
                }
            }
            write_session_json(&data)?;
            return Ok(None);
        }
        Some(vault) => {
            // Cloud vault does not match local material (rotated code / reissued / local keygen) or there is no local material:
            // **cloud wins** — login means attach to that account's library.
            // Overwrite guard: local memories encrypted with the current local key become undecryptable if we switch keys.
            if has_wrap && !reset_vault {
                use crate::transport::MemoryTransport as _;
                let alive = crate::service::open_store().and_then(|s| s.count()).unwrap_or(0);
                if alive > 0 {
                    anyhow::bail!(
                        "local library already has {alive} memories encrypted with the current local key, which does not match cloud account \"{user}\". \
                         switching now would make them undecryptable. export first: rsrs export <file>; \
                         to drop local memories and use the cloud key, add --reset-vault"
                    );
                }
            }
            let super_pass = obtain_super(user, super_pass)?;
            return unlock_cloud_vault(&mut data, addr, user, &token, &vault, &super_pass, secret_key);
        }
        None => {
            // Overwrite guard: server has data but no vault — those rows were encrypted with another key.
            // Minting a new URK and put_vault would lock them forever (hit 2026-09-16).
            let count = fetch_count(addr, &token)?;
            if count > 0 && !reset_vault {
                anyhow::bail!(
                    "server already has {count} encrypted memories but no key wrap \u{2014} they were encrypted on an earlier device with another key. \
                     export the key from the old device then login: old machine `rsrs keys-export`, this machine \
                     login --super <super-password> --secret-key <A3-\u{2026}>. \
                     to drop the old data and start from this machine, add --reset-vault"
                );
            }
            let (super_pass, _urk, kdf_salt, wrapped_urk, urk_nonce) = new_vault_v4()?;
            apply_vault_v4(&mut data, user, &super_pass, kdf_salt, wrapped_urk, urk_nonce)?;
            put_vault(addr, &token, &data)?;
            write_session_json(&data)?;
            return Ok(Some(super_pass));
        }
    }
}

/// Whether cloud vault material and local session material are the same key.
fn vault_matches(vault: &serde_json::Value, data: &serde_json::Value) -> bool {
    vault["wrapped_urk"] == data["wrapped_urk"]
        && vault["kdf_salt"] == data["kdf_salt"]
        && vault["urk_nonce"] == data["urk_nonce"]
}

/// Use the explicit super password before the local keyring.
fn obtain_super(user: &str, super_arg: Option<&str>) -> Result<String> {
    if let Some(s) = super_arg.filter(|s| !s.is_empty()) {
        return Ok(s.to_owned());
    }
    crate::keystore::load_super(user).ok_or_else(|| {
        anyhow!(
            "super password required: --super <A3-\u{2026}> (system-generated recovery code; the login password only authenticates to the server and cannot unwrap memories). \
             no super password? export from the old machine with `rsrs keys-export`; a headless server can set RSRS_SUPER"
        )
    })
}

/// Unlock the cloud vault with the super password (v4 single factor / v3 two-factor / v2 old passphrase) and write the local session.
/// Login preserves the cloud format and original user factors. Migration is explicit.
fn unlock_cloud_vault(
    data: &mut serde_json::Value,
    _addr: &str,
    user: &str,
    _token: &str,
    vault: &serde_json::Value,
    super_pass: &str,
    secret_key: Option<&str>,
) -> Result<Option<String>> {
    let salt = vault["kdf_salt"].as_str().ok_or_else(|| anyhow!("vault missing kdf_salt"))?;
    let wrapped = vault["wrapped_urk"].as_str().ok_or_else(|| anyhow!("vault missing wrapped_urk"))?;
    let nonce = vault["urk_nonce"].as_str().ok_or_else(|| anyhow!("vault missing urk_nonce"))?;
    let version = vault["version"].as_i64().unwrap_or(2);
    if version >= 4 {
        match crate::memory::SessionKeys::unlock_v4(super_pass, salt, wrapped, nonce) {
            Ok(_) => {
                apply_vault_v4(data, user, super_pass, salt.to_owned(), wrapped.to_owned(), nonce.to_owned())?;
                write_session_json(data)?;
                Ok(None)
            }
            Err(e) => Err(anyhow!("super password cannot unwrap cloud memories ({e:#}) \u{2014} verify with keys-export on the old machine and retry")),
        }
    } else if version == 3 {
        // Read v3 without replacing its passphrase with its Secret Key.
        let legacy_secret = secret_key
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
            .or_else(|| data["secret_key"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_owned()))
            .ok_or_else(|| {
                anyhow!(
                    "this account is still v3 two-factor: need --super <old passphrase> --secret-key <A3-\u{2026}>. \
                     export the original material with `rsrs keys-export`; migrate explicitly to change the format"
                )
            })?;
        crate::memory::SessionKeys::unlock_vault(super_pass, &legacy_secret, salt, wrapped, nonce)
            .map_err(|e| anyhow!("v3 key unlock failed ({e}) \u{2014} retry with material from keys-export on the old machine"))?;
        data["kdf_salt"] = serde_json::json!(salt);
        data["wrapped_urk"] = serde_json::json!(wrapped);
        data["urk_nonce"] = serde_json::json!(nonce);
        data["vault_version"] = serde_json::json!(3);
        data["secret_key"] = serde_json::json!(legacy_secret);
        crate::keystore::save_super(user, super_pass)?;
        write_session_json(data)?;
        Ok(None)
    } else if version == 2 {
        crate::memory::SessionKeys::unlock_super(super_pass, salt, wrapped, nonce)?;
        data["kdf_salt"] = serde_json::json!(salt);
        data["wrapped_urk"] = serde_json::json!(wrapped);
        data["urk_nonce"] = serde_json::json!(nonce);
        data["vault_version"] = serde_json::json!(2);
        crate::keystore::save_super(user, super_pass)?;
        write_session_json(data)?;
        Ok(None)
    } else {
        anyhow::bail!("unsupported legacy vault version; original keys were preserved")
    }
}

