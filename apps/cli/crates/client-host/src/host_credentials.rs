//! Native credential custody belongs to the host, including private control helpers.
pub mod space;
pub mod legacy_migration;
pub mod key_control;
pub mod login;

use anyhow::{Context, Result};
use respire_protocol::credentials::{
    CredentialError, CredentialOperation, CredentialReply, CredentialRequest, CREDENTIAL_SCHEMA_V,
    ProviderCredentialStatus, ProviderCredentialUpdate, ProviderUnavailableCode,
    RuntimeUnlockReceipt, RuntimeUnlockV2, RUNTIME_UNLOCK_SCHEMA_V2,
};
use std::io::{Read, Write};

fn entry(service: &str, account: &str, backend: &str) -> Result<keyring::Entry, CredentialError> {
    let result = match backend {
        "native" => keyring::Entry::new(service, account),
        #[cfg(target_os = "linux")]
        "secret_service" => keyring::secret_service::default_credential_builder()
            .build(None, service, account).map(keyring::Entry::new_with_credential),
        #[cfg(target_os = "linux")]
        "kernel" => keyring::keyutils::default_credential_builder()
            .build(None, service, account).map(keyring::Entry::new_with_credential),
        _ => return Err(CredentialError::Unavailable),
    };
    result.map_err(credential_error)
}

fn credential_error(error: keyring::Error) -> CredentialError {
    match error {
        keyring::Error::NoEntry => CredentialError::NoEntry,
        keyring::Error::Ambiguous(_) => CredentialError::Ambiguous,
        keyring::Error::BadEncoding(_) => CredentialError::BadEncoding,
        keyring::Error::Invalid(_, _) => CredentialError::Invalid,
        _ => CredentialError::Unavailable,
    }
}

fn execute(request: &CredentialRequest) -> Result<Option<String>, CredentialError> {
    if request.credential_request != CREDENTIAL_SCHEMA_V
        || request.id.len() != 36 || uuid::Uuid::parse_str(&request.id).is_err()
        || request.service.is_empty() || request.service.len() > 512
        || request.service.contains(['\0', '\r', '\n'])
        || request.account.is_empty() || request.account.len() > 512
        || request.account.contains(['\0', '\r', '\n'])
    {
        return Err(CredentialError::Invalid);
    }
    if matches!(request.operation, CredentialOperation::ReadConfigured) {
            if request.service != "rsrs" || !(request.account.starts_with("super:") || request.account.starts_with("pass:")) {
                return Err(CredentialError::Invalid);
            }
            let name = if request.account.starts_with("super:") { "RSRS_SUPER" } else { "RSRS_PASS" };
            if let Some(value) = crate::env::var(name).ok().filter(|value| !value.is_empty()) {
                return Ok(Some(value));
            }
        }
    let entry = entry(&request.service, &request.account, &request.backend)?;
    match &request.operation {
        CredentialOperation::Read | CredentialOperation::ReadConfigured => entry.get_password().map(Some).map_err(credential_error),
        CredentialOperation::Write(value) => {
            if request.service != "rsrs" || value.len() > 65536 {
                return Err(CredentialError::Invalid);
            }
            entry.set_password(value).map_err(credential_error)?;
            Ok(None)
        }
        CredentialOperation::Delete => {
            if request.service != "rsrs" { return Err(CredentialError::Invalid); }
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(credential_error(error)),
            }
        }
    }
}

fn auth_entry(account: &str, backend: &str, slot: &str) -> Result<keyring::Entry> {
    anyhow::ensure!(matches!(slot, "super" | "pass"), "invalid host auth credential slot");
    anyhow::ensure!(account.starts_with("login-") && account.len() <= 128
        && account.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid host auth credential account");
    anyhow::ensure!(credential_backends().contains(&backend) && backend != "kernel",
        "host auth requires a persistent credential backend");
    entry("rsrs", &format!("{slot}:{account}"), backend)
        .map_err(|error| anyhow::anyhow!("host auth credential store unavailable ({error:?})"))
}

/// Exact host transaction lookup, without environment overrides or backend discovery.
pub fn read_auth_super(account: &str, backend: &str) -> Result<Option<String>> {
    read_auth_slot(account, backend, "super")
}

/// Exact registration recovery lookup; the receipt owner verifies its password commitment.
pub fn read_auth_password(account: &str, backend: &str) -> Result<Option<String>> {
    read_auth_slot(account, backend, "pass")
}

fn read_auth_slot(account: &str, backend: &str, slot: &str) -> Result<Option<String>> {
    match auth_entry(account, backend, slot)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => anyhow::bail!("host auth credential read failed ({:?})", credential_error(error)),
    }
}

/// The owning host transaction has already checkpointed this slot and its public candidate.
pub fn write_auth_super(account: &str, backend: &str, value: &str) -> Result<()> {
    write_auth_slot(account, backend, "super", value)
}

/// The registration host has checkpointed this exact password slot before calling.
pub fn write_auth_password(account: &str, backend: &str, value: &str) -> Result<()> {
    write_auth_slot(account, backend, "pass", value)
}

fn write_auth_slot(account: &str, backend: &str, name: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "host auth credential is empty");
    let slot = auth_entry(account, backend, name)?;
    match read_auth_slot(account, backend, name)? {
        Some(existing) => anyhow::ensure!(existing == value, "host auth credential slot belongs to different material"),
        None => slot.set_password(value)
            .map_err(|error| anyhow::anyhow!("host auth credential write failed ({:?})", credential_error(error)))?,
    }
    anyhow::ensure!(read_auth_slot(account, backend, name)?.as_deref() == Some(value),
        "host auth credential readback mismatch; retain the original operation");
    Ok(())
}

/// Only the host receipt owner may retire newly created slots; conflicts remain untouched.
pub fn remove_auth_material(account: &str, backend: &str, super_password: &str, password: Option<&str>) -> Result<()> {
    let original_super = read_auth_slot(account, backend, "super")?;
    let original_pass = read_auth_slot(account, backend, "pass")?;
    anyhow::ensure!(original_super.as_deref().is_none_or(|found| found == super_password)
        && original_pass.as_deref().is_none_or(|found| Some(found) == password),
        "host auth cleanup credential conflict; original slots retained");
    for (name, original) in [("pass", original_pass), ("super", original_super)] {
        if original.is_some() {
            match auth_entry(account, backend, name)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {},
                Err(error) => anyhow::bail!("host auth cleanup failed ({:?})", credential_error(error)),
            }
            anyhow::ensure!(read_auth_slot(account, backend, name)?.is_none(),
                "host auth cleanup readback failed; retain the operation receipt");
        }
    }
    Ok(())
}

/// Consume private credential frames until the original Runtime response arrives.
/// Credentials are never attached to that response, written to files, or logged.
pub fn read_control_response<T: serde::de::DeserializeOwned>(
    input: &mut impl Write, output: &mut impl Read,
) -> Result<T> {
    loop {
        let mut length = [0u8; 4];
        output.read_exact(&mut length).context("missing host control response frame")?;
        let length = u32::from_be_bytes(length) as usize;
        anyhow::ensure!(length > 0 && length <= respire_protocol::HOST_CONTROL_MAX_FRAME_BYTES,
            "invalid host control response frame size");
        let mut bytes = vec![0; length];
        output.read_exact(&mut bytes).context("incomplete host control response frame")?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid host control response JSON"))?;
        if value.get("credential_request").is_none() {
            return serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("invalid host control response JSON"));
        }
        let request: CredentialRequest = serde_json::from_value(value)
            .map_err(|_| anyhow::anyhow!("invalid private credential request"))?;
        let reply = CredentialReply {
            credential_reply: CREDENTIAL_SCHEMA_V, id: request.id.clone(), result: execute(&request),
        };
        let bytes = serde_json::to_vec(&reply)?;
        input.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
        input.write_all(&bytes)?;
        input.flush()?;
    }
}

/// Export uses stored original factors, never process overrides or another recorded backend.
pub fn read_export_factors(session: &serde_json::Value) -> Result<(Option<String>, Option<String>)> {
    let version = match session.get("vault_version") {
        None => 1,
        Some(value) => value.as_u64().context("invalid export vault version")?,
    };
    anyhow::ensure!(version >= 1, "unsupported export vault version");
    let super_password = if version >= 2 {
        let bound = ["keyring_account", "keyring_backend", "auth_operation_id"]
            .iter().any(|field| session.get(*field).is_some());
        let value = if bound {
            let backend = session["keyring_backend"].as_str().context("invalid export credential backend")?;
            anyhow::ensure!(credential_backends().contains(&backend), "unsupported export credential backend");
            let alias = session["keyring_account"].as_str().filter(|alias|
                (alias.starts_with("legacy-") || alias.starts_with("login-"))
                    && alias.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'))
                .context("export credential account is missing or invalid")?;
            read_session_slot(&format!("super:{alias}"), backend)?
        } else {
            // Historical sessions can retain their original factor inline.
            let field = if version >= 4 { "secret_key" } else { "super" };
            match session.get(field) {
                Some(value) => Some(value.as_str().context("invalid original super password")?.to_owned()),
                None => {
                    let user = session["user"].as_str().filter(|user| !user.trim().is_empty())
                        .context("original super password account is unavailable")?;
                    anyhow::ensure!(user.len() <= 512 && !user.contains(['\0', '\r', '\n']),
                        "invalid export credential account");
                    read_provider_credential("rsrs", &format!("super:{}", user.trim()), credential_backends())
                        .map_err(|code| anyhow::anyhow!("host export credential read failed ({code:?})"))?
                }
            }
        };
        Some(value.filter(|value| !value.is_empty()).context("original super password is unavailable")?)
    } else { None };
    let legacy_secret = if matches!(version, 1 | 3) {
        if session.get("legacy_secret_backend").is_some() {
            read_session_legacy_secret(session)?
        } else {
            let field = if version == 1 { "secret" } else { "secret_key" };
            Some(session[field].as_str().filter(|value| !value.is_empty())
                .context("original legacy factor is unavailable")?.to_owned())
        }
    } else { None };
    Ok((super_password, legacy_secret))
}

pub fn read_session_credentials(session: &serde_json::Value) -> Result<(Option<String>, Option<String>)> {
    read_session_credentials_with(session, true)
}

/// Snapshot factors must belong to the original session, never process overrides.
pub fn read_stored_session_credentials(session: &serde_json::Value) -> Result<(Option<String>, Option<String>)> {
    read_session_credentials_with(session, false)
}

fn read_session_credentials_with(session: &serde_json::Value, use_environment: bool) -> Result<(Option<String>, Option<String>)> {
    let account = session["keyring_account"].as_str().filter(|alias|
        (alias.starts_with("legacy-") || alias.starts_with("login-"))
            && alias.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'))
        .or_else(|| session["user"].as_str().map(str::trim).filter(|user| !user.is_empty()))
        .unwrap_or("local");
    anyhow::ensure!(account.len() <= 512 && !account.contains(['\0', '\r', '\n']), "invalid credential account");
    let backend = match session.get("keyring_backend") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(backend)) => {
            anyhow::ensure!(credential_backends().contains(&backend.as_str()),
                "unsupported session credential backend");
            Some(backend.as_str())
        }
        Some(_) => anyhow::bail!("invalid session credential backend metadata"),
    };
    let version = session["vault_version"].as_u64().unwrap_or(1);
    let read = |slot: &str| {
        // A recorded v1 backend owns the password; later login passwords may differ from super.
        let backends: Vec<_> = match backend {
            Some(selected) if slot == "super" || version < 2 => vec![selected],
            Some(selected) => std::iter::once(selected).chain(credential_backends().iter().copied()
                .filter(|candidate| *candidate != selected)).collect(),
            None => credential_backends().to_vec(),
        };
        if version < 2 && slot == "pass" {
            for selected in backends {
                if let Some(value) = read_session_slot(&format!("{slot}:{account}"), selected)? {
                    return Ok(Some(value));
                }
            }
            return Ok(None);
        }
        read_provider_credential("rsrs", &format!("{slot}:{account}"), &backends)
            .map_err(|code| anyhow::anyhow!("host session credential read failed ({code:?})"))
    };
    let session_super = session["super"].as_str().filter(|value| !value.is_empty()).map(str::to_owned);
    let super_password = match use_environment.then(|| crate::env::var("RSRS_SUPER").ok()).flatten().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None if version < 2 => None,
        None if version < 4 && session_super.is_some() => session_super,
        None => read("super")?
            .or_else(|| (version >= 4).then(|| session["secret_key"].as_str().map(str::to_owned)).flatten()),
    };
    let login_password = if version < 2 {
        match use_environment.then(|| crate::env::var("RSRS_PASS").ok()).flatten().filter(|value| !value.is_empty())
            .or_else(|| session["pass"].as_str().map(str::to_owned)) {
            Some(value) => Some(value),
            None => read("pass")?,
        }
    } else {
        None
    };
    Ok((super_password, login_password))
}

// Ordinary reads preserve a present empty v1 password; provider reads intentionally do not.
fn read_session_slot(account: &str, backend: &str) -> Result<Option<String>> {
    let request = CredentialRequest {
        credential_request: CREDENTIAL_SCHEMA_V, id: uuid::Uuid::new_v4().to_string(),
        service: "rsrs".into(), account: account.into(), backend: backend.into(),
        operation: CredentialOperation::Read,
    };
    match execute(&request) {
        Ok(value) => Ok(value),
        Err(CredentialError::NoEntry) => Ok(None),
        Err(error) => anyhow::bail!("host session credential read failed ({error:?})"),
    }
}

fn read_session_legacy_secret(session: &serde_json::Value) -> Result<Option<String>> {
    let version = session["vault_version"].as_u64().unwrap_or(1);
    if version != 1 && version != 3 {
        return Ok(None);
    }
    // Raw historical factors belong to explicit migration, never automatic unlock.
    let backend = session["legacy_secret_backend"].as_str()
        .context("legacy secret backend is missing; explicit historical migration is required")?;
    anyhow::ensure!(credential_backends().contains(&backend), "unsupported legacy secret backend");
    let account = session["keyring_account"].as_str().filter(|alias|
        (alias.starts_with("legacy-") || alias.starts_with("login-"))
            && alias.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'))
        .context("legacy secret credential account is missing or invalid")?;
    let value = read_session_slot(&format!("legacy-secret:{account}"), backend)?
        .filter(|value| !value.is_empty())
        .context("legacy secret credential is missing or empty; library remains locked")?;
    Ok(Some(value))
}

/// A legacy marker is never repaired by falling back to an inline factor.
pub fn legacy_session_needs_migration(session: &serde_json::Value) -> bool {
    matches!(session["vault_version"].as_u64().unwrap_or(1), 1 | 3)
        && session.get("legacy_secret_backend").is_none()
}

/// Stage credential custody only. The installation coordinator owns session publication.
/// Slots survive an interrupted publication for an exact, conflict-checked retry.
pub fn prepare_legacy_session(session: &serde_json::Value, proposed_alias: &str) -> Result<serde_json::Value> {
    prepare_legacy_session_with(session, proposed_alias, execute)
}

fn prepare_legacy_session_with(
    session: &serde_json::Value, proposed_alias: &str,
    mut exchange: impl FnMut(&CredentialRequest) -> Result<Option<String>, CredentialError>,
) -> Result<serde_json::Value> {
    anyhow::ensure!(legacy_session_needs_migration(session), "session does not require legacy credential migration");
    let version = session["vault_version"].as_u64().unwrap_or(1);
    let factor = session[if version == 3 { "secret_key" } else { "secret" }].as_str()
        .filter(|value| !value.is_empty()).context("historical session factor is unavailable; no session was changed")?;
    let alias = match session.get("keyring_account") {
        None => proposed_alias,
        Some(value) => value.as_str().context("invalid legacy credential alias")?,
    };
    anyhow::ensure!((alias.starts_with("legacy-") || alias.starts_with("login-"))
        && alias.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        && alias.len() <= 498, "invalid legacy credential alias");
    let (super_password, login_password) = read_session_credentials(session)?;
    // Preserve an original inline primary exactly, including an empty v1 password.
    let primary = session[if version == 1 { "pass" } else { "super" }].as_str().map(str::to_owned)
        .or(if version == 1 { login_password } else { super_password })
        .context("historical session primary credential is unavailable; no session was changed")?;
    anyhow::ensure!(version == 1 || !primary.is_empty(),
        "historical session primary credential is empty; no session was changed");
    let backend = credential_backends().iter().copied().find(|backend| *backend != "kernel")
        .context("legacy session migration requires persistent host credentials")?;
    // Never overwrite a conflicting slot or erase an original factor before verified custody.
    let primary_backend = session["keyring_backend"].as_str().filter(|value| *value != "kernel")
        .unwrap_or(backend);
    let mut slots = vec![(if version == 1 { "pass" } else { "super" }, primary.as_str(), primary_backend),
        ("legacy-secret", factor, backend)];
    if version == 3 {
        if let Some(value) = session["pass"].as_str() { slots.push(("pass", value, primary_backend)); }
    }
    for (slot, value, backend) in slots {
        ensure_legacy_slot(alias, slot, value, backend, &mut exchange)?;
    }
    let mut migrated = session.clone();
    let fields = migrated.as_object_mut().context("session must contain an object")?;
    for name in ["secret", "secret_key", "super", "pass"] { fields.remove(name); }
    fields.insert("keyring_account".into(), serde_json::json!(alias));
    fields.insert("keyring_backend".into(), serde_json::json!(primary_backend));
    fields.insert("legacy_secret_backend".into(), serde_json::json!(backend));
    Ok(migrated)
}

// Shared custody primitive: never overwrite a conflict or delete a slot after an uncertain write.
fn ensure_legacy_slot(alias: &str, slot: &str, value: &str, backend: &str,
    exchange: &mut impl FnMut(&CredentialRequest) -> Result<Option<String>, CredentialError>) -> Result<()> {
    anyhow::ensure!(backend != "kernel", "legacy migration requires persistent host credentials");
    let mut request = CredentialRequest {
        credential_request: CREDENTIAL_SCHEMA_V, id: uuid::Uuid::new_v4().to_string(),
        service: "rsrs".into(), account: format!("{slot}:{alias}"), backend: backend.into(),
        operation: CredentialOperation::Read,
    };
    match exchange(&request) {
        Ok(Some(found)) => anyhow::ensure!(found == value, "legacy credential destination conflicts; original session retained"),
        Err(CredentialError::NoEntry) => {
            request.operation = CredentialOperation::Write(value.to_owned());
            exchange(&request).map_err(|error| anyhow::anyhow!("legacy credential write failed ({error:?}); original session retained"))?;
        }
        Ok(None) => anyhow::bail!("legacy credential read returned no result; original session retained"),
        Err(error) => anyhow::bail!("legacy credential read failed ({error:?}); original session retained"),
    }
    request.operation = CredentialOperation::Read;
    let found = exchange(&request).map_err(|error| anyhow::anyhow!("legacy credential readback failed ({error:?}); original session retained"))?;
    anyhow::ensure!(found.as_deref() == Some(value), "legacy credential readback mismatch; original session retained");
    Ok(())
}

include!("host_credentials/unlock.rs");

#[cfg(test)]
mod legacy_session_tests;
