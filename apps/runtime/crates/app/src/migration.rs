//! Copy legacy default homes without modifying the original libraries or credentials.

#[cfg(test)]
#[path = "migration_readiness_tests.rs"]
mod readiness_tests;

#[cfg(test)]
pub(crate) fn memory_keyring_for_test() -> impl Drop {
    readiness_tests::memory_keyring()
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection, OpenFlags,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// WP-2c: internally_configured_root sank into the domain service module; the
// engine side must classify the internally configured default home.
use crate::service::internally_configured_root;

const RECEIPT: &str = ".rsrs-migration.json";
const SOURCE_RECEIPT: &str = ".rsrs-migrated.json";
const DEFAULT_ENV: &str = "RSRS_DEFAULT_DATA_DIR";

#[path = "migration_intent.rs"]
mod intent;

#[path = "migration_material.rs"]
mod host_material;
use respire_protocol::migration_control::HostMigrationMaterial;


/// Receipt fields bind the migration context; the persistent credential and
/// authenticated unwrap prove that the selected vault is still usable.
pub fn verify_local_session(session: &Value) -> Result<()> {
    let alias = session["keyring_account"].as_str().context("migration credential alias is missing")?;
    let backend = session["keyring_backend"].as_str().context("migration credential backend is missing")?;
    let version = match session.get("vault_version") {
        None => 1,
        Some(value) => value.as_i64().context("invalid migration vault version")?,
    };
    anyhow::ensure!(backend != "kernel", "migrated local session requires a persistent credential backend");
    let secret = if version == 1 {
        crate::keystore::read_imported_v1_password(alias, backend)?
    } else {
        crate::keystore::read_imported(alias, "super", backend)?
    };
    crate::auth::unlock_session_keys(session, if version == 1 { &secret } else { "" },
        if version == 1 { None } else { Some(secret.as_str()) }, "")?;
    Ok(())
}

/// Durable, pre-effect rejection of this exact migration operation.
#[derive(Debug)]
pub struct SchemaRejected;
impl std::fmt::Display for SchemaRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("legacy database schema is incompatible; original rows were preserved")
    }
}
impl std::error::Error for SchemaRejected {}

#[derive(Debug)]
struct InvalidSchema;
impl std::fmt::Display for InvalidSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("legacy database uses an incompatible pre-encrypted demo format; export with its original version and import into rsrs; original rows were preserved")
    }
}
impl std::error::Error for InvalidSchema {}

struct Profile {
    source: PathBuf,
    destination: PathBuf,
    identity: String,
    services: [&'static str; 3],
}

/// Configure the current home before parsing. Legacy import is always explicit.
pub fn ensure_default_home() -> Result<()> {
    if ["RSRS_CLIENT_ONLY", "RSRS_NO_AUTOSTART"]
        .iter()
        .any(|name| {
            crate::env::var(name).ok().is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
        })
    {
        return Ok(());
    }
    if let Ok(value) = crate::env::var("RSRS_DATA_DIR") {
        if !value.trim().is_empty() && !internally_configured_root(value.trim()) {
            return Ok(());
        }
    }
    let home = crate::service::home_dir().context("cannot locate the user home")?;
    let root = home.join(".rsrs");
    // This occurs at single-threaded startup, before Core or runtime workers exist.
    std::env::set_var("RSRS_DATA_DIR", &root);
    std::env::set_var(DEFAULT_ENV, &root);
    Ok(())
}

fn legacy_profiles(home: &Path) -> Result<Vec<Profile>> {
    let mut result = Vec::new();
    for (label, services) in [
        ("respire", ["respire", "memocap", "1memory"]),
        ("onememory", ["1memory", "memocap", "respire"]),
        ("rsrs", ["rsrs", "respire", "1memory"]),
    ] {
        let root = home.join(format!(".{label}"));
        if root.exists() { meaningful(&root)?; }
        let mut candidates = vec![("main".to_owned(), root.clone())];
        if root.join("accounts").is_dir() {
            meaningful(&root.join("accounts"))?;
            for entry in std::fs::read_dir(root.join("accounts"))? {
                let entry = entry?;
                candidates.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
            }
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, source) in candidates {
            if !source.join("onememory.db").is_file() && !source.join("session.json").is_file() {
                continue;
            }
            // Reject symlinks before reading identity or any credential material.
            meaningful(&source)?;
            // Current vaults remain current even when an older filename or home is retained.
            let session_path = source.join("session.json");
            if session_path.is_file() {
                let value: Value = serde_json::from_slice(&std::fs::read(&session_path)?)?;
                if value["crypto_namespace"].as_str() == Some(crate::memory::crypto::RSRS_PREFIX)
                    || value["wrapped_urk"].as_str().is_some_and(|wrapped| wrapped.starts_with(crate::memory::crypto::RSRS_PREFIX)) {
                    continue;
                }
            }
            if label == "rsrs" {
                let session = source.join("session.json");
                let legacy_vault = if session.is_file() {
                    let value: Value = serde_json::from_slice(&std::fs::read(&session)?)?;
                    value["wrapped_urk"].as_str().is_some_and(|wrapped|
                        !wrapped.is_empty() && !wrapped.starts_with(crate::memory::crypto::RSRS_PREFIX))
                } else { false };
                if !source.join("onememory.db").is_file() && !legacy_vault { continue; }
            }
            let source_id = identity(&source)?;
            result.push(Profile {
                identity: source_id.clone(), source,
                destination: crate::service::accounts_root().join(format!("imported-{}-{}", &source_id[..8], safe_name(&name))),
                services,
            });
        }
    }
    Ok(result)
}

fn migrated_destination(source_id: &str) -> Result<Option<PathBuf>> {
    let mut destinations = vec![crate::service::main_data_dir()];
    let accounts = crate::service::accounts_root();
    if accounts.is_dir() {
        for entry in std::fs::read_dir(accounts)? {
            destinations.push(entry?.path());
        }
    }
    for destination in destinations {
        if destination.is_dir() {
            meaningful(&destination)?;
            if receipt_matches(&destination, source_id)? { return Ok(Some(destination)); }
        }
    }
    Ok(None)
}

/// Enumerate actual legacy profiles; backup-only roots do not constitute a library.
pub fn list_legacy_profiles() -> Result<Value> {
    let home = crate::service::home_dir()?;
    let mut rows = Vec::new();
    for profile in legacy_profiles(&home)? {
        let source_receipt = profile.source.join(SOURCE_RECEIPT);
        let marked = if source_receipt.is_file() {
            meaningful(&profile.source)?;
            let receipt: Value = serde_json::from_slice(&std::fs::read(source_receipt)?)?;
            if receipt["complete"] == true && receipt["source_identity"] == profile.identity {
                receipt["destination"].as_str().map(PathBuf::from)
            } else { None }
        } else { None };
        rows.push(json!({"source_id":profile.identity,"source":profile.source,
            "user":crate::service::session_user_of_dir(&profile.source),
            "account":profile.destination.file_name().map(|name| name.to_string_lossy().into_owned()),
            "migrated_to":marked.or(migrated_destination(&profile.identity)?)}));
    }
    Ok(json!({"profiles":rows}))
}

/// Discovery is read-only. A completed receipt suppresses repeated suggestions.
pub fn pending_legacy_profiles() -> Result<Vec<Value>> {
    let report = list_legacy_profiles()?;
    let profiles = report["profiles"].as_array()
        .ok_or_else(|| anyhow!("migration discovery did not return profiles"))?;
    Ok(profiles.iter().filter(|profile| profile["migrated_to"].is_null()).cloned().collect())
}

/// Copy only the selected profile into a new account, keeping current selection intact.
include!("migration/profile_control.rs");
include!("migration/legacy_inventory.rs");
include!("migration/snapshot.rs");
include!("migration/credentials.rs");
#[cfg(test)]
mod tests;
