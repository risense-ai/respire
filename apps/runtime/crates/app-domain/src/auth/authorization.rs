
/// Mutate the in-memory candidate session only; do not overwrite saved material on network or unlock failure.
pub fn password_authorization(addr: &str, user: &str, pass: &str) -> Result<serde_json::Value> {
    password_authorization_with_totp(addr,user,pass,None)
}

pub fn password_authorization_with_totp(addr: &str, user: &str, pass: &str, totp:Option<&str>) -> Result<serde_json::Value> {
    let mut authorization = serde_json::json!({});
    authenticate_session_with_totp(&mut authorization, addr, user, pass,totp)?;
    Ok(authorization)
}

/// Private control cannot prompt on its framed input channel.
pub fn password_authorization_noninteractive(addr: &str, user: &str, pass: &str, totp: Option<&str>) -> Result<serde_json::Value> {
    let mut authorization = serde_json::json!({});
    authenticate_session_inner(&mut authorization, addr, user, pass, totp, false)?;
    Ok(authorization)
}

fn server_authentication_salt(addr: &str, user: &str) -> Result<String> {
    let salt_response = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
        .get(&format!("{}/auth/salt", addr.trim().trim_end_matches('/'))).query("user", user).call();
    let salt = match salt_response {
        Ok(response) => response.into_json::<serde_json::Value>()?["salt"].as_str()
            .filter(|salt| !salt.is_empty()).context("server did not return an authentication salt")?.to_owned(),
        Err(ureq::Error::Status(404, _)) => crypto::derive_legacy_auth_salt(user)?,
        Err(error) => return Err(anyhow!("authentication salt request failed: {error}")),
    };
    Ok(salt)
}

fn authenticate_session(data: &mut serde_json::Value, addr: &str, user: &str, pass: &str) -> Result<()> {
    authenticate_session_with_totp(data,addr,user,pass,None)
}

fn authenticate_session_with_totp(data: &mut serde_json::Value, addr: &str, user: &str, pass: &str, totp:Option<&str>) -> Result<()> {
    authenticate_session_inner(data, addr, user, pass, totp, true)
}

fn authenticate_session_inner(data: &mut serde_json::Value, addr: &str, user: &str, pass: &str, totp: Option<&str>, allow_prompt: bool) -> Result<()> {
    let authorization = match begin_password_authorization(addr, user, pass)? {
        PasswordAuthorizationStep::Authorized(authorization) => authorization,
        PasswordAuthorizationStep::TotpRequired(challenge) => {
            if totp.is_none() && (!allow_prompt || !crate::prompt::interactive()) {
                anyhow::bail!("TOTP verification requires an interactive terminal or browser authorization");
            }
            let code = match totp { Some(code) => code.to_owned(), None => crate::prompt::ask_secret("TOTP code (6 digits): ")? };
            continue_password_authorization(addr, user, &challenge, &code)?
        }
    };
    for (field, value) in authorization.as_object().context("authorization must be an object")? {
        data[field] = value.clone();
    }
    data["pass"] = serde_json::Value::String(pass.to_owned());
    Ok(())
}

/// Sensitive challenge material belongs only in the existing credential checkpoint.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordAuthorizationChallenge {
    ticket: String,
    auth_salt: String,
}

pub enum PasswordAuthorizationStep {
    Authorized(serde_json::Value),
    TotpRequired(PasswordAuthorizationChallenge),
}

/// Perform the password step once; callers must checkpoint before any continuation.
pub fn begin_password_authorization(addr: &str, user: &str, pass: &str) -> Result<PasswordAuthorizationStep> {
    let auth_salt = server_authentication_salt(addr, user)?;
    let pass_hash = crypto::derive_pass_hash(pass, &auth_salt)?;

    let agent = ureq::AgentBuilder::new().redirects(0)
        .timeout(std::time::Duration::from_secs(30)).build();
    let resp = agent.post(&format!("{}/login", addr.trim().trim_end_matches('/')))
        .send_json(serde_json::json!({
            "user": user,
            "pass_hash": pass_hash,
            "device_name": device_name(),
        }))
        .map_err(|e| anyhow::anyhow!("login request failed: {e}"))?;
    let reply: serde_json::Value = resp.into_json().map_err(|e| anyhow::anyhow!("failed to parse response: {e}"))?;
    if reply["totp_required"].as_bool() == Some(true) {
        let ticket = reply["ticket"].as_str().ok_or_else(|| anyhow!("TOTP challenge did not return a ticket"))?;
        anyhow::ensure!(!ticket.is_empty() && ticket.len() <= 16 * 1024, "invalid TOTP challenge ticket");
        return Ok(PasswordAuthorizationStep::TotpRequired(PasswordAuthorizationChallenge {
            ticket: ticket.to_owned(), auth_salt,
        }));
    }
    Ok(PasswordAuthorizationStep::Authorized(authorization_reply(addr, user, &auth_salt, &reply)?))
}

pub fn validate_totp_code(code: &str) -> Result<&str> {
    let code = code.trim();
    anyhow::ensure!(code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()),
        "TOTP code must contain 6 digits");
    Ok(code)
}

/// Submit only the admitted ticket: never fetch salt or POST /login here.
pub fn continue_password_authorization(addr: &str, user: &str,
    challenge: &PasswordAuthorizationChallenge, code: &str) -> Result<serde_json::Value> {
    let code = validate_totp_code(code)?;
    anyhow::ensure!(!challenge.ticket.is_empty() && challenge.ticket.len() <= 16 * 1024
        && !challenge.auth_salt.is_empty(), "invalid TOTP challenge checkpoint");
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build();
    let response = agent.post(&format!("{}/login/totp", addr.trim().trim_end_matches('/')))
        .send_json(serde_json::json!({"ticket":challenge.ticket,"code":code,"device_name":device_name()}))
        .map_err(|error| anyhow!("TOTP verification failed: {error}"))?;
    let reply = response.into_json().map_err(|error| anyhow!("failed to parse TOTP response: {error}"))?;
    authorization_reply(addr, user, &challenge.auth_salt, &reply)
}

fn authorization_reply(addr: &str, user: &str, auth_salt: &str, reply: &serde_json::Value) -> Result<serde_json::Value> {
    let token = reply["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("login failed: {}", reply.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")))?
        .to_owned();

    Ok(serde_json::json!({"user":user,"addr":addr.trim().trim_end_matches('/'),"token":token,
        "session_id":reply.get("session_id").cloned().unwrap_or(serde_json::Value::Null),"auth_salt":auth_salt}))
}

/// Five-piece kit attach (manual decrypt path on another machine): unlock verifies → write the full local session (then login fills the token).
/// Any wrong item (password/secret/salt/wrap) is refused — a write is always a valid session.
#[allow(clippy::too_many_arguments)]
pub fn fivekeys_login(
    addr: &str,
    user: &str,
    pass: &str,
    secret: &str,
    kdf_salt: &str,
    wrapped_urk: &str,
    urk_nonce: &str,
    super_pass: &str,
) -> Result<()> {
    crate::service::check_runtime_user(user)?;
    // A different account must not overwrite the profile that is already open.
    let previous = session_for_other_user(user)?;
    let mut data = if !super_pass.is_empty() {
        let trimmed_addr = addr.trim().trim_end_matches('/').to_owned();
        // Current accounts wrap with v4 (derive_kek_v4). unlock_super is the older
        // passphrase KDF and cannot open a v4 wrap even when the super password is right.
        match crate::memory::SessionKeys::unlock_v4(super_pass, kdf_salt, wrapped_urk, urk_nonce) {
            Ok(_) => {
                let mut data = serde_json::json!({
                    "user": user,
                    "addr": trimmed_addr,
                });
                if let Some(alias) = previous.get("keyring_account") {
                    data["keyring_account"] = alias.clone();
                }
                apply_vault_v4(
                    &mut data,
                    user,
                    super_pass,
                    kdf_salt.to_owned(),
                    wrapped_urk.to_owned(),
                    urk_nonce.to_owned(),
                )?;
                data
            }
            Err(v4_err) => {
                crate::memory::SessionKeys::unlock_super(super_pass, kdf_salt, wrapped_urk, urk_nonce)
                    .map_err(|_| v4_err)
                    .context("super password cannot unwrap the wrap")?;
                serde_json::json!({
                    "pass": pass,
                    "super": super_pass,
                    "kdf_salt": kdf_salt,
                    "wrapped_urk": wrapped_urk,
                    "urk_nonce": urk_nonce,
                    "vault_version": 2,
                    "user": user,
                    "addr": trimmed_addr,
                })
            }
        }
    } else {
        crate::memory::SessionKeys::unlock(pass, secret, kdf_salt, wrapped_urk, urk_nonce)
            .context("five-piece kit verification failed \u{2014} each item must match keygen/register output from the first device")?;
        serde_json::json!({
            "pass": pass,
            "secret": secret,
            "kdf_salt": kdf_salt,
            "wrapped_urk": wrapped_urk,
            "urk_nonce": urk_nonce,
            "user": user,
            "addr": addr.trim().trim_end_matches('/'),
        })
    };
    if let Some(alias) = previous.get("keyring_account") {
        data["keyring_account"] = alias.clone();
    }
    if !addr.trim().is_empty() {
        authenticate_session(&mut data, addr, user, pass)?;
    }
    write_session_json(&data)?;
    Ok(())
}

