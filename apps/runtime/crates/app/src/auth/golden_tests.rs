//! Exercise frozen external wraps through the actual session unlock boundary.
use super::*;
use anyhow::Context;
use serde_json::{json, Value};

#[test]
fn golden_sessions_unlock_all_vault_versions_and_namespaces() -> Result<()> {
    let _isolate = crate::test_lock::Isolate::new()?;
    let fixture: Value =
        serde_json::from_str(include_str!("../../../crypto/fixtures/vault-v1-v4.json"))?;
    let text = |name: &str| fixture[name].as_str().context("missing golden field");
    for vector in fixture["vectors"]
        .as_array()
        .context("missing golden vectors")?
    {
        let version = vector["version"]
            .as_u64()
            .context("missing vault version")?;
        let session = json!({"user":"golden-user", "vault_version":version,
            "wrapped_urk":vector["wrapped_urk"], "urk_nonce":vector["nonce"],
            "kdf_salt":fixture["salt"], "secret":fixture["account_secret"],
            "secret_key":fixture["secret_key"]});
        let factor = if version == 4 {
            text("secret_key")?
        } else {
            text("super_password")?
        };
        let keys = unlock_session_keys(&session, text("login")?, Some(factor), "golden-user")?;
        assert_eq!(hex::encode(keys.urk), text("urk")?);
        assert_eq!(
            keys.decrypt_content(
                vector["ciphertext"]
                    .as_str()
                    .context("missing ciphertext")?,
                vector["nonce"].as_str().context("missing nonce")?
            )?,
            "Golden legacy memory."
        );
        if version == 1 {
            assert!(unlock_session_keys(&session, "wrong-login", None, "golden-user").is_err());
        } else {
            assert!(unlock_session_keys(
                &session,
                text("login")?,
                Some("A3-000000-000000-000000-000000-000000-000000"),
                "golden-user"
            )
            .is_err());
        }
        let mut corrupt = session.clone();
        corrupt["urk_nonce"] = json!("00");
        assert!(
            unlock_session_keys(&corrupt, text("login")?, Some(factor), "golden-user").is_err()
        );
    }
    Ok(())
}
