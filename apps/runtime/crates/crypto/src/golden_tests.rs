//! Fixed synthetic vectors from Node.js crypto, never from the Rust writer.
use crate::{crypto::*, SessionKeys};
use anyhow::{Context, Result};
use serde_json::Value;

#[test]
fn golden_vaults_domains_auth_and_ciphertexts() -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!("../fixtures/vault-v1-v4.json"))?;
    let text = |name: &str| fixture[name].as_str().context("missing fixture field");
    let salt = text("salt")?;
    let login = text("login")?;
    let super_pass = text("super_password")?;
    let secret = text("secret_key")?;
    let expected: [u8; 32] = hex::decode(text("urk")?)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid fixture URK"))?;
    for vector in fixture["vectors"].as_array().context("missing vectors")? {
        let field = |name: &str| vector[name].as_str().context("missing vector field");
        let current = field("namespace")? == "rsrs";
        let wrapped = field("wrapped_urk")?;
        let nonce = field("nonce")?;
        assert_eq!(hex::decode(nonce)?.len(), NONCE_BYTES);
        let (kek, keys) = match vector["version"].as_u64() {
            Some(1) => (
                if current {
                    derive_rsrs_password_kek(login, text("account_secret")?, salt)?
                } else {
                    derive_kek(login, text("account_secret")?, salt)?
                },
                SessionKeys::unlock(login, text("account_secret")?, salt, wrapped, nonce)?,
            ),
            Some(2) => (
                derive_super_kek(super_pass, salt)?,
                SessionKeys::unlock_super(super_pass, salt, wrapped, nonce)?,
            ),
            Some(3) => (
                if current {
                    derive_rsrs_vault_kek(super_pass, secret, salt)?
                } else {
                    derive_vault_kek(super_pass, secret, salt)?
                },
                SessionKeys::unlock_vault(super_pass, secret, salt, wrapped, nonce)?,
            ),
            Some(4) => (
                if current {
                    derive_rsrs_kek(secret, salt)?
                } else {
                    derive_kek_v4(secret, salt)?
                },
                SessionKeys::unlock_v4(secret, salt, wrapped, nonce)?,
            ),
            _ => anyhow::bail!("unsupported fixture version"),
        };
        assert_eq!(hex::encode(kek), field("kek")?);
        assert_eq!(keys.urk, expected);
        assert_eq!(
            keys.decrypt_content(field("ciphertext")?, nonce)?,
            "Golden legacy memory."
        );
        assert!(keys.decrypt_content(field("ciphertext")?, "00").is_err());
        let ciphertext = field("ciphertext")?;
        let mut tampered = ciphertext.to_owned();
        let tail = tampered.pop().context("empty golden ciphertext")?;
        tampered.push(if tail == '0' { '1' } else { '0' });
        assert!(keys.decrypt_content(&tampered, nonce).is_err());
        assert!(keys.decrypt_content("rsrs:v99:00", nonce).is_err());
        assert!(unwrap_key(
            wrapped.strip_prefix(RSRS_PREFIX).unwrap_or(wrapped),
            nonce,
            &[0; 32]
        )
        .is_err());
        let namespace = field("namespace")?;
        for (purpose, pin) in [("data", "data_key"), ("embedding", "embedding_key")] {
            let key = derive_subkey(&expected, format!("{namespace}:{purpose}:v1").as_bytes())?;
            assert_eq!(hex::encode(key), field(pin)?);
            if purpose == "embedding" {
                assert_eq!(
                    decrypt_bytes(&key, field("embedding")?, nonce)?,
                    [0, 0, 128, 63]
                );
            }
        }
        let auth_salt = if current {
            derive_auth_salt(" Golden-User ")?
        } else {
            derive_legacy_auth_salt(" Golden-User ")?
        };
        assert_eq!(auth_salt, field("auth_salt")?);
        assert_eq!(derive_pass_hash(login, &auth_salt)?, field("auth_hash")?);
    }
    Ok(())
}
