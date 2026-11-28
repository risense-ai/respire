//! Exact live serving identity evidence for private account completion.
use anyhow::{ensure, Result};
use respire_commands::net_rpc::Health;
use sha2::{Digest, Sha256};

pub(super) fn verified_target_health(target: &std::path::Path, account: &str) -> Result<respire_commands::net_rpc::Health> {
    crate::startup_identity::verify_account_completion_authority()?;
    let health = respire_commands::net_rpc::health()?;
    respire_commands::net_rpc::request_scope(&health)?;
    ensure!(health.store_ready && health.account_id == account
        && std::fs::canonicalize(&health.data_dir)? == target.canonicalize()?
        && health.runtime_version == env!("CARGO_PKG_VERSION"), "target Runtime is not ready for this account receipt");
    let process = respire_commands::net_rpc::runtime_process_identity(health.pid)?;
    ensure!(respire_commands::net_rpc::instance_is_running(&health)?,
        "target Runtime birth identity mismatch");
    let image = std::env::current_exe()?.canonicalize()?;
    ensure!(std::fs::canonicalize(&process.executable)? == image
        && std::fs::canonicalize(&health.image_path)? == image,
        "target Runtime process identity mismatch");
    let mut file = std::fs::File::open(&image)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    ensure!(format!("{:x}", hasher.finalize()) == health.image_sha256, "target Runtime image digest mismatch");
    ensure!(respire_commands::net_rpc::runtime_process_identity(health.pid)? == process,
        "target Runtime changed during verification");
    ensure!(respire_commands::net_rpc::instance_is_running(&health)?,
        "target Runtime birth identity changed during verification");
    let current = respire_commands::net_rpc::health()?;
    respire_commands::net_rpc::request_scope(&current)?;
    ensure!(same_serving_target(&health, &current), "target Runtime scope changed during verification");
    ensure!(respire_commands::net_rpc::instance_is_running(&current)?,
        "target Runtime birth identity changed during verification");
    crate::startup_identity::verify_account_completion_authority()?;
    Ok(current)
}

/// Both journal callbacks in one completion must observe the same serving Runtime.
pub(super) fn check_completion_observation(first: &mut Option<Health>, current: Health) -> Result<()> {
    if let Some(previous) = first {
        ensure!(same_serving_target(previous, &current), "verified Runtime changed during account completion");
    } else {
        *first = Some(current);
    }
    Ok(())
}

fn same_serving_target(before: &Health, after: &Health) -> bool {
    after.store_ready
        && before.pid == after.pid
        && before.process_start_id == after.process_start_id
        && before.installation_id == after.installation_id
        && before.installation_root == after.installation_root
        && before.instance_id == after.instance_id
        && before.generation == after.generation
        && before.account_id == after.account_id
        && before.data_dir == after.data_dir
        && before.image_path == after.image_path
        && before.image_sha256 == after.image_sha256
        && before.runtime_version == after.runtime_version
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn serving() -> serde_json::Value {
        json!({
            "server": "runtime", "bin": "rsrs-runtime", "pid": 27, "v": 1,
            "store_ready": true, "process_start_id": "birth-27",
            "installation_id": "installation", "installation_root": "/fixture/root",
            "instance_id": "instance", "generation": 1, "account_id": "account",
            "data_dir": "/fixture/profile", "image_path": "/fixture/runtime",
            "image_sha256": "fixture-digest", "runtime_version": "fixture-version"
        })
    }

    #[test]
    fn completion_accepts_two_matching_observations() -> Result<()> {
        let mut first = None;
        check_completion_observation(&mut first, serde_json::from_value(serving())?)?;
        check_completion_observation(&mut first, serde_json::from_value(serving())?)?;
        Ok(())
    }

    #[test]
    fn second_observation_must_match_verified_serving_identity() -> Result<()> {
        for (key, new) in [
            ("store_ready", json!(false)),
            ("pid", json!(28)),
            ("process_start_id", json!("replacement-birth")),
            ("installation_id", json!("different-installation")),
            ("installation_root", json!("/fixture/alternate")),
            ("instance_id", json!("replacement-instance")),
            ("generation", json!(2)),
            ("account_id", json!("different-account")),
            ("data_dir", json!("/fixture/alternate")),
            ("image_path", json!("/fixture/replaced")),
            ("image_sha256", json!("changed-digest")),
            ("runtime_version", json!("changed-version")),
        ] {
            let mut first = None;
            check_completion_observation(&mut first, serde_json::from_value(serving())?)?;
            let mut replacement = serving();
            replacement[key] = new;
            assert!(
                check_completion_observation(&mut first, serde_json::from_value(replacement)?).is_err(),
                "accepted a change to {key} on the second observation"
            );
        }
        Ok(())
    }
}
