use super::*;

#[test]
fn offline_endpoint_evidence_is_exact_not_health_or_drain() -> Result<()> {
    let absent = OfflineProof {
        endpoint_sha256: None,
        endpoint_pid: None,
        prior_runtime: None,
    };
    let bytes = br#"{"pid":123,"v":1}"#;
    assert!(offline_endpoint_matches(&absent, None)?);
    assert!(!offline_endpoint_matches(&absent, Some(bytes))?);
    let proof = OfflineProof {
        endpoint_sha256: Some(hash(bytes)),
        endpoint_pid: Some(123),
        prior_runtime: None,
    };
    assert!(offline_endpoint_matches(&proof, Some(bytes))?);
    assert!(!offline_endpoint_matches(&proof, None)?);
    assert!(!offline_endpoint_matches(
        &proof,
        Some(br#"{"pid":124,"v":1}"#)
    )?);
    let wrong_pid = OfflineProof {
        endpoint_sha256: Some(hash(bytes)),
        endpoint_pid: Some(124),
        prior_runtime: None,
    };
    assert!(!offline_endpoint_matches(&wrong_pid, Some(bytes))?);
    let partial = OfflineProof {
        endpoint_sha256: Some(hash(bytes)),
        endpoint_pid: None,
        prior_runtime: None,
    };
    assert!(!offline_endpoint_matches(&partial, Some(bytes))?);
    let malformed = OfflineProof {
        endpoint_sha256: Some(hash(b"invalid")),
        endpoint_pid: Some(123),
        prior_runtime: None,
    };
    assert!(offline_endpoint_matches(&malformed, Some(b"invalid")).is_err());
    let oversized = vec![b' '; 16 * 1024 + 1];
    assert!(!offline_endpoint_matches(&proof, Some(&oversized))?);
    use respire_app::memory::crypto;
    use respire_protocol::host_control::{HostAuthSessionMaterial, HostAuthVaultFactors as Factors};
    for current in [false, true] {
        for version in 1..=4 {
            let salt = crypto::random_hex(16);
            let super_password = crypto::generate_secret_key();
            let secret_key = crypto::generate_secret_key();
            let kek = match (version, current) {
                (1, false) => crypto::derive_kek("password", "account-secret", &salt)?,
                (1, true) => crypto::derive_rsrs_password_kek("password", "account-secret", &salt)?,
                (2, _) => crypto::derive_super_kek(&super_password, &salt)?,
                (3, false) => crypto::derive_vault_kek(&super_password, &secret_key, &salt)?,
                (3, true) => crypto::derive_rsrs_vault_kek(&super_password, &secret_key, &salt)?,
                (4, false) => crypto::derive_kek_v4(&super_password, &salt)?,
                (4, true) => crypto::derive_rsrs_kek(&super_password, &salt)?,
                _ => unreachable!("four fixture vault versions"),
            };
            let (nonce, wrapped) = crypto::wrap_key(&[7; 32], &kek)?;
            let prefix = if current { crypto::RSRS_PREFIX } else { "" };
            let mut session = serde_json::json!({"user":"account", "vault_version":version,
                "kdf_salt":salt, "wrapped_urk":format!("{prefix}{wrapped}"), "urk_nonce":nonce});
            let factors = match version {
                1 => Factors::V1 { password: "password".into(), account_secret: "account-secret".into() },
                2 => Factors::V2 { super_password: super_password.clone() },
                3 => Factors::V3 { super_password: super_password.clone(), secret_key: secret_key.clone() },
                4 => Factors::V4 { super_password: super_password.clone() },
                _ => unreachable!("four fixture vault versions"),
            };
            let bytes = serde_json::to_vec(&session)?;
            let mut material = HostAuthSessionMaterial { session_sha256: hash(&bytes), factors };
            let keys = crate::maintenance_control::restore_session_keys(&bytes, &material, "account")?;
            assert_eq!(keys.urk, [7; 32]);
            assert_eq!(keys.encrypt_content("fixture")?.1.starts_with(crypto::RSRS_PREFIX), current);
            assert!(crate::maintenance_control::restore_session_keys(&bytes, &material, "other-account").is_err());
            session["crypto_namespace"] = serde_json::json!(crypto::RSRS_PREFIX);
            let changed = serde_json::to_vec(&session)?;
            assert!(crate::maintenance_control::restore_session_keys(&changed, &material, "account").is_err());
            material.session_sha256 = hash(&changed);
            assert!(crate::maintenance_control::restore_session_keys(&changed, &material, "account")?
                .encrypt_content("fixture")?.1.starts_with(crypto::RSRS_PREFIX));
            material.factors = Factors::V4 { super_password: crypto::generate_secret_key() };
            assert!(crate::maintenance_control::restore_session_keys(&changed, &material, "account").is_err());
        }
    }
    Ok(())
}
