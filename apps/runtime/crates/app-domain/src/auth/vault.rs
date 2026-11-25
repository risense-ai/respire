//! Vault wrapping material and server-side Vault metadata access.

use anyhow::{anyhow, Result};

use crate::memory::crypto;

pub fn wrap_with_v4(super_pass: &str, urk: &[u8; 32]) -> Result<(String, String, String)> {
    let kdf_salt = crypto::random_hex(16);
    let kek = crypto::derive_kek_v4(super_pass, &kdf_salt)?;
    let (urk_nonce, wrapped_urk) = crypto::wrap_key(urk, &kek)?;
    Ok((kdf_salt, wrapped_urk, urk_nonce))
}

pub fn wrap_with_rsrs(super_pass: &str, urk: &[u8; 32]) -> Result<(String, String, String)> {
    let salt = crypto::random_hex(16);
    let kek = crypto::derive_rsrs_kek(super_pass, &salt)?;
    let (nonce, wrapped) = crypto::wrap_key(urk, &kek)?;
    Ok((salt, format!("{}{wrapped}", crypto::RSRS_PREFIX), nonce))
}

/// v4 key material: generate a super password (A3- recovery code) and a fresh URK.
pub fn new_vault_v4() -> Result<(String, [u8; 32], String, String, String)> {
    let super_pass = crypto::generate_secret_key();
    let urk = crypto::generate_key();
    let (kdf_salt, wrapped_urk, urk_nonce) = wrap_with_rsrs(&super_pass, &urk)?;
    Ok((super_pass, urk, kdf_salt, wrapped_urk, urk_nonce))
}

/// Write v4 material into session: super password goes only to the OS keyring (headless warns and uses RSRS_SUPER); never plaintext on disk.
pub(super) fn apply_vault_v4(
    data: &mut serde_json::Value,
    user: &str,
    super_pass: &str,
    kdf_salt: String,
    wrapped_urk: String,
    urk_nonce: String,
) -> Result<()> {
    data["kdf_salt"] = serde_json::Value::String(kdf_salt);
    data["wrapped_urk"] = serde_json::Value::String(wrapped_urk);
    data["urk_nonce"] = serde_json::Value::String(urk_nonce);
    data["vault_version"] = serde_json::json!(4);
    // Super password and login password never go to disk
    data.as_object_mut().map(|o| {
        o.remove("secret_key");
        o.remove("super");
        o.remove("pass");
    });
    match crate::keystore::save_super(user, super_pass) {
        Ok(()) => {}
        Err(e) => eprintln!("warning: {e}; this session can still pass --super <super-password>"),
    }
    Ok(())
}

pub(super) fn put_vault(addr: &str, token: &str, data: &serde_json::Value) -> Result<()> {
    let base = addr.trim().trim_end_matches('/');
    ureq::post(&format!("{base}/api/self/vault"))
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({
            "kdf_salt": data["kdf_salt"],
            "wrapped_urk": data["wrapped_urk"],
            "urk_nonce": data["urk_nonce"],
            "version": data["vault_version"].as_i64().unwrap_or(3),
        }))
        .map_err(|e| anyhow!("failed to upload key wrap: {e}"))?;
    Ok(())
}

pub(super) fn fetch_vault(addr: &str, token: &str) -> Result<Option<serde_json::Value>> {
    let base = addr.trim().trim_end_matches('/');
    match ureq::get(&format!("{base}/api/self/vault"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(resp) => resp
            .into_json()
            .map(Some)
            .map_err(|e| anyhow!("failed to parse vault response: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(anyhow!("failed to fetch key wrap: {e}")),
    }
}

/// Live memory count for this user on the server (overwrite protection / prompts).
pub(super) fn fetch_count(addr: &str, token: &str) -> Result<i64> {
    let base = addr.trim().trim_end_matches('/');
    match ureq::get(&format!("{base}/count"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(resp) => Ok(resp.into_json::<serde_json::Value>()?["count"].as_i64().unwrap_or(0)),
        Err(ureq::Error::Status(404, _)) => Ok(0),
        Err(e) => Err(anyhow!("failed to fetch memory count: {e}")),
    }
}
