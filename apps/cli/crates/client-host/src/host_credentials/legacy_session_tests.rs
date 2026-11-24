use super::*;
use std::collections::BTreeMap;

#[test]
fn legacy_slots_preserve_empty_v1_and_v3_material_without_changing_the_source() -> Result<()> {
    for (version, has_alias) in [(1, false), (1, true), (3, false), (3, true)] {
        let mut session = if version == 1 {
            serde_json::json!({"vault_version":1, "pass":"", "secret":"synthetic-factor", "wrapped_urk":"unchanged-wrap"})
        } else {
            serde_json::json!({"vault_version":3, "super":"synthetic-primary", "secret_key":"synthetic-factor",
                "pass":"synthetic-login", "wrapped_urk":"unchanged-wrap"})
        };
        if has_alias { session["keyring_account"] = serde_json::json!("login-existing"); }
        let original = session.clone();
        let mut slots = BTreeMap::<(String, String), String>::new();
        let migrated = prepare_legacy_session_with(&session, "legacy-proposed", |request| {
            let key = (request.backend.clone(), request.account.clone());
            match &request.operation {
                CredentialOperation::Read => slots.get(&key).cloned().map(Some).ok_or(CredentialError::NoEntry),
                CredentialOperation::Write(value) => { slots.insert(key, value.clone()); Ok(None) }
                _ => Err(CredentialError::Invalid),
            }
        })?;
        assert_eq!(session, original);
        assert_eq!(migrated["wrapped_urk"], original["wrapped_urk"]);
        for name in ["secret", "secret_key", "super", "pass"] { assert!(migrated.get(name).is_none()); }
        let alias = if has_alias { "login-existing" } else { "legacy-proposed" };
        assert_eq!(migrated["keyring_account"], alias);
        for (slot, field) in [(if version == 1 { "pass" } else { "super" }, "keyring_backend"),
            ("legacy-secret", "legacy_secret_backend")] {
            let backend = migrated[field].as_str().context("fixture backend missing")?;
            let value = slots.get(&(backend.to_owned(), format!("{slot}:{alias}"))).context("fixture slot missing")?;
            assert_eq!(value, if slot == "legacy-secret" { "synthetic-factor" }
                else if version == 1 { "" } else { "synthetic-primary" });
            assert_ne!(backend, "kernel");
        }
        assert!(!legacy_session_needs_migration(&migrated));
    }
    Ok(())
}

#[test]
fn failed_or_conflicting_custody_never_returns_a_sanitized_session() {
    let session = serde_json::json!({"vault_version":1, "pass":"", "secret":"synthetic-sensitive-factor"});
    let original = session.clone();
    for failure in ["read", "write", "readback", "conflict"] {
        let mut written = false;
        let result = prepare_legacy_session_with(&session, "legacy-fixture", |request| {
            if request.account.starts_with("legacy-secret:") {
                return match &request.operation {
                    CredentialOperation::Read if failure == "read" => Err(CredentialError::Unavailable),
                    CredentialOperation::Read if failure == "conflict" || written => Ok(Some("different-factor".into())),
                    CredentialOperation::Read => Err(CredentialError::NoEntry),
                    CredentialOperation::Write(_) if failure == "write" => Err(CredentialError::Unavailable),
                    CredentialOperation::Write(_) => { written = true; Ok(None) }
                    _ => Err(CredentialError::Invalid),
                };
            }
            Ok(Some(String::new()))
        });
        assert!(result.is_err());
        if let Err(error) = result { assert!(!error.to_string().contains("synthetic-sensitive-factor")); }
        assert_eq!(session, original);
    }
}

#[test]
fn recorded_legacy_backend_is_never_repaired_from_inline_factors() {
    for version in [1, 2, 3, 4] {
        let session = serde_json::json!({"vault_version":version, "super":"original-super",
            "secret":"original-account", "secret_key":"original-code"});
        let (primary, legacy) = read_export_factors(&session).unwrap_or_else(|error| panic!("inline export failed: {error}"));
        assert_eq!(primary.as_deref(), match version { 1 => None, 4 => Some("original-code"), _ => Some("original-super") });
        assert_eq!(legacy.as_deref(), match version { 1 => Some("original-account"), 3 => Some("original-code"), _ => None });
    }
    for version in [2, 3, 4] {
        for marker in [serde_json::Value::Null, serde_json::json!("invalid-backend")] {
            let session = serde_json::json!({"vault_version":version, "super":"original-super",
                "secret_key":"original-code", "keyring_backend":marker, "keyring_account":"login-fixture"});
            assert!(read_export_factors(&session).is_err());
        }
    }
    for marker in [serde_json::Value::Null, serde_json::json!("invalid-backend")] {
        let session = serde_json::json!({"vault_version":3, "secret_key":"synthetic-factor",
            "legacy_secret_backend":marker, "keyring_account":"login-fixture"});
        assert!(!legacy_session_needs_migration(&session));
        assert!(read_session_legacy_secret(&session).is_err());
    }
}
