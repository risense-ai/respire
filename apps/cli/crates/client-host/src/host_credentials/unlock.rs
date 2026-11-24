/// The caller verifies the Runtime process/image and profile before delivering any key.
/// Missing credentials leave the library locked; they never authorize another keygen.
pub fn unlock_runtime(
    base: &str, installation: &std::path::Path, profile: &std::path::Path,
    instance_id: &str, generation: u64,
) -> Result<RuntimeUnlockReceipt> {
    use sha2::{Digest, Sha256};
    let url = url::Url::parse(base)?;
    anyhow::ensure!(url.scheme() == "http" && url.username().is_empty() && url.password().is_none()
        && url.path() == "/" && url.query().is_none() && url.fragment().is_none()
        && url.host_str().is_some_and(|host| host.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())), "unlock requires a literal loopback Runtime URL");
    let path = profile.join("session.json");
    let bytes = std::fs::read(path).context("Runtime session credentials are unavailable; library remains locked")?;
    let session: serde_json::Value = serde_json::from_slice(&bytes)?;
    let (super_password, login_password) = read_session_credentials(&session)?;
    let legacy_secret_material = read_session_legacy_secret(&session)?;
    let version = session["vault_version"].as_u64().unwrap_or(1);
    anyhow::ensure!(!((version < 2 && login_password.is_none())
        || (version >= 2 && super_password.is_none())),
        "Runtime session key is unavailable; library remains locked");
    let token_path = installation.join("runtime/token");
    let token = match crate::env::var("RSRS_RPC_TOKEN").ok().filter(|value| !value.trim().is_empty()) {
        Some(value) => value.trim().to_owned(),
        None => match crate::credential::read_token_file(&token_path)? {
            Some(value) => value,
            None => {
                let value = crate::credential::generate_runtime_token()?;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
                match options.open(&token_path) {
                    Ok(mut file) => { file.write_all(value.as_bytes())?; file.sync_all()?; value }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                        crate::credential::read_token_file(&token_path)?.context("host token disappeared")?,
                    Err(error) => return Err(error.into()),
                }
            }
        },
    };
    let provider_update = match provider_credentials(installation, profile) {
        Ok(credentials) => ProviderCredentialUpdate::Replace { credentials },
        Err(code) => ProviderCredentialUpdate::Unavailable { code },
    };
    let expected_provider_status = match &provider_update {
        ProviderCredentialUpdate::Replace { .. } => ProviderCredentialStatus::Ready,
        ProviderCredentialUpdate::Unavailable { code } => ProviderCredentialStatus::Unavailable { code: *code },
    };
    let input = RuntimeUnlockV2 {
        schema_version: RUNTIME_UNLOCK_SCHEMA_V2, profile: profile.canonicalize()?,
        session_sha256: format!("{:x}", Sha256::digest(&bytes)),
        instance_id: instance_id.into(), generation, super_password, login_password,
        legacy_secret: legacy_secret_material,
        provider_update,
    };
    let bytes = serde_json::to_vec(&input)?;
    anyhow::ensure!(bytes.len() <= 65536, "Runtime unlock input exceeds the private frame limit");
    let reply = ureq::AgentBuilder::new().redirects(0)
        .timeout(std::time::Duration::from_secs(15)).build()
        .post(&format!("{}/api/runtime/unlock", base.trim_end_matches('/')))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json").send_bytes(&bytes)
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => anyhow::anyhow!(
                "Runtime V2 unlock rejected (HTTP {status}); inspect Runtime health before retrying"),
            ureq::Error::Transport(error) => anyhow::anyhow!(
                "Runtime V2 unlock transport failed ({:?}); delivery is unconfirmed; inspect Runtime health before retrying",
                error.kind()),
        })?;
    let receipt: RuntimeUnlockReceipt = reply.into_json()
        .map_err(|_| anyhow::anyhow!("invalid Runtime V2 unlock receipt"))?;
    anyhow::ensure!(receipt.schema_version == RUNTIME_UNLOCK_SCHEMA_V2,
        "Runtime unlock receipt schema mismatch: expected {RUNTIME_UNLOCK_SCHEMA_V2}, received {}",
        receipt.schema_version);
    anyhow::ensure!(receipt.store_ready, "Runtime V2 unlock receipt reports a locked library");
    anyhow::ensure!(receipt.provider_status == expected_provider_status,
        "Runtime provider credential receipt mismatch: expected {expected_provider_status:?}, received {:?}",
        receipt.provider_status);
    if let ProviderCredentialStatus::Unavailable { code } = receipt.provider_status {
        eprintln!("Runtime library unlocked; provider credentials unavailable ({code:?}); provider operations remain disabled");
    }
    Ok(receipt)
}

fn credential_backends() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    { &["secret_service", "kernel"] }
    #[cfg(not(target_os = "linux"))]
    { &["native"] }
}

// Only missing or empty entries permit trying another backend; failures carry public codes only.
fn read_provider_credential(service: &str, account: &str, backends: &[&str])
    -> Result<Option<String>, ProviderUnavailableCode> {
    for backend in backends {
        let credential = match entry(service, account, backend) {
            Ok(credential) => credential,
            Err(CredentialError::NoEntry) => continue,
            Err(error) => return Err(match error {
                CredentialError::Ambiguous => ProviderUnavailableCode::Ambiguous,
                CredentialError::BadEncoding => ProviderUnavailableCode::BadEncoding,
                CredentialError::Invalid => ProviderUnavailableCode::InvalidConfiguration,
                _ => ProviderUnavailableCode::BackendUnavailable,
            }),
        };
        match credential.get_password() {
            Ok(value) if value.is_empty() => {},
            Ok(value) => return Ok(Some(value)),
            Err(keyring::Error::NoEntry) => {},
            Err(error) => return Err(match error {
                keyring::Error::NoStorageAccess(_) => ProviderUnavailableCode::AccessDenied,
                keyring::Error::PlatformFailure(_error) => {
                    #[cfg(target_os = "macos")]
                    let os_status = _error.downcast_ref::<security_framework::base::Error>()
                        .map(|error| error.code());
                    #[cfg(not(target_os = "macos"))]
                    let os_status = None::<i32>;
                    eprintln!("HOST_PROVIDER_CREDENTIAL_FAILURE {}", serde_json::json!({
                        "schema_version": 1, "stage": "keyring_read",
                        "error_type": if os_status.is_some() { "security_framework" } else { "platform_failure" },
                        "os_status": os_status, "io_error_code": null,
                    }));
                    ProviderUnavailableCode::PlatformFailure
                },
                keyring::Error::Ambiguous(_) => ProviderUnavailableCode::Ambiguous,
                keyring::Error::BadEncoding(_) => ProviderUnavailableCode::BadEncoding,
                keyring::Error::Invalid(_, _) => ProviderUnavailableCode::InvalidConfiguration,
                _ => ProviderUnavailableCode::BackendUnavailable,
            }),
        }
    }
    Ok(None)
}

fn provider_io_error(stage: &str, error: std::io::Error) -> ProviderUnavailableCode {
    eprintln!("HOST_PROVIDER_CREDENTIAL_FAILURE {}", serde_json::json!({
        "schema_version": 1, "stage": stage, "error_type": "io",
        "os_status": null, "io_error_code": error.raw_os_error(),
    }));
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        ProviderUnavailableCode::AccessDenied
    } else {
        ProviderUnavailableCode::PlatformFailure
    }
}

fn provider_credentials(root: &std::path::Path, profile: &std::path::Path)
    -> Result<Vec<respire_protocol::credentials::ProvidedCredential>, ProviderUnavailableCode> {
    fn collect(value: &serde_json::Value, result: &mut Vec<respire_protocol::credentials::ProvidedCredential>)
        -> Result<(), ProviderUnavailableCode> {
        match value {
            serde_json::Value::Object(fields) => {
                if fields.contains_key("keyring_service") || fields.contains_key("keyring_account") {
                    let service = fields.get("keyring_service").and_then(|v| v.as_str())
                        .ok_or(ProviderUnavailableCode::InvalidConfiguration)?;
                    let account = fields.get("keyring_account").and_then(|v| v.as_str())
                        .ok_or(ProviderUnavailableCode::InvalidConfiguration)?;
                    if service.is_empty() || account.is_empty() || service.len() > 512 || account.len() > 512
                        || service.contains(['\0', '\r', '\n']) || account.contains(['\0', '\r', '\n']) {
                        return Err(ProviderUnavailableCode::InvalidConfiguration);
                    }
                    if !result.iter().any(|item| item.service == service && item.account == account) {
                        if let Some(value) = read_provider_credential(service, account, credential_backends())? {
                            result.push(respire_protocol::credentials::ProvidedCredential {
                                service: service.into(), account: account.into(), value,
                            });
                        }
                    }
                }
                for value in fields.values() { collect(value, result)?; }
            }
            serde_json::Value::Array(values) => for value in values { collect(value, result)?; },
            _ => {},
        }
        Ok(())
    }
    let mut result = Vec::new();
    // Classification settings live in fixed OS slots, independently of embedding profiles.
    for slot in ["typesafe", "ds", "last-backend", "ds-last-base"] {
        collect(&serde_json::json!({"keyring_service": "rsrs", "keyring_account": format!("classify:{slot}")}), &mut result)?;
    }
    let mut hosts = vec!["api.deepseek.com".to_owned()];
    if let Some(base) = result.iter().find(|item| item.service == "rsrs" && item.account == "classify:ds-last-base") {
        hosts.push(crate::classification::host_of(&base.value));
    }
    let agent = match std::fs::read(profile.join("agent.json")) {
        Ok(bytes) => Some(serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|_| ProviderUnavailableCode::InvalidConfiguration)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(provider_io_error("agent_read", error)),
    };
    if let Some(base) = agent.as_ref().and_then(|agent| agent["recall_api_base"].as_str()) {
        hosts.push(crate::classification::host_of(base));
    }
    hosts.sort();
    hosts.dedup();
    for host in hosts.into_iter().filter(|host| !host.is_empty()) {
        for slot in [format!("ds@{host}"), format!("ds-model@{host}")] {
            collect(&serde_json::json!({"keyring_service": "rsrs", "keyring_account": format!("classify:{slot}")}), &mut result)?;
        }
    }
    let bytes = match std::fs::read(root.join("embedding-profiles-v1.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(provider_io_error("embedding_profiles_read", error)),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(ProviderUnavailableCode::InvalidConfiguration);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| ProviderUnavailableCode::InvalidConfiguration)?;
    collect(&value, &mut result)?;
    Ok(result)
}
