//! Install only after the isolated synthetic dispatcher and its SQLite handles have closed.
use super::*;
use respire_app::snapshot::{
    backup_archive::{restore_approved_archive_locked, BackupVaultIdentity},
    SnapshotOperation,
};
use respire_spawn::owner_credential::{backup_grant::VerifiedGrant, ArchiveTarget};
use std::sync::Mutex;

const MODE: &str = "RSRS_BACKUP_TEST_OFFLINE_SELECTION";
static PIN: Mutex<Option<ArchiveTarget>> = Mutex::new(None);
static JOB: Mutex<Option<Job>> = Mutex::new(None);
struct Job {
    pin: ArchiveTarget,
    vault: BackupVaultIdentity,
    grant: VerifiedGrant,
    custody: std::path::PathBuf,
    expected: String,
    snapshot: String,
    keys: respire_app::SessionKeys,
}

pub(super) fn enabled() -> bool {
    std::env::var_os(MODE).is_some()
}
pub(super) fn skip_space(space: &str) -> bool {
    enabled() && (space == "main") != (std::env::var(MODE).as_deref() == Ok("parent"))
}
pub(super) fn remember_pin(store: &respire_app::transport::local::LocalStore) -> Result<()> {
    if enabled() {
        *PIN.lock().map_err(|_| anyhow::anyhow!("target lock"))? = Some(store.pin_backup_target()?);
    }
    Ok(())
}
pub(super) fn claim(
    agent: &ureq::Agent,
    base: &str,
    space: &str,
    wire: &[u8],
    root: &Path,
    keys: &respire_app::SessionKeys,
    expected: String,
) -> Result<()> {
    let vault = BackupVaultIdentity::capture(root)?;
    let archive = open_to_stage(keys, "backup-fixture", wire, std::io::sink(), || Ok(()))?;
    let accepted: Value = agent
        .post(&format!("{base}/v1/backups/import?space_id={space}"))
        .set("Authorization", &format!("Bearer {TOKEN}"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(wire)?
        .into_json()?;
    let id = uuid::Uuid::parse_str(accepted["operation_id"].as_str().context("operation")?)?;
    let pin = PIN
        .lock()
        .map_err(|_| anyhow::anyhow!("target lock"))?
        .take()
        .context("missing selected SQLite handle")?;
    let grant =
        process_fixture::claim_for_offline(agent, base, process_fixture::target()?, id, wire)?;
    vault.check(&grant)?;
    *JOB.lock()
        .map_err(|_| anyhow::anyhow!("offline fixture lock"))? = Some(Job {
        pin,
        vault,
        grant,
        custody: root.to_owned(),
        expected,
        snapshot: archive.id,
        keys: keys.clone(),
    });
    Ok(())
}
pub(super) fn install_after_store_close() -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let job = JOB
        .lock()
        .map_err(|_| anyhow::anyhow!("offline fixture lock"))?
        .take()
        .context("missing native grant")?;
    let root = job.pin.path().parent().context("selected library parent")?;
    let lock = respire_app::lock::LibraryLock::acquire(root, Duration::ZERO)?;
    let op = SnapshotOperation {
        operation_id: uuid::Uuid::from_bytes(job.grant.scope().operation).to_string(),
        request_fingerprint: job.grant.fingerprint(),
    };
    if let Ok(replacement_owner) = std::env::var("RSRS_BACKUP_TEST_REPLACEMENT_OWNER") {
        let before = Sha256::digest(fs::read(job.pin.path())?);
        let (password, _, salt, wrapped, nonce) = auth::new_vault_v4()?;
        let session = auth::write_session_json(&json!({
            "user":replacement_owner, "vault_version":4, "kdf_salt":salt,
            "urk_nonce":nonce,"wrapped_urk":wrapped
        }))?;
        auth::supply_runtime_session(respire_protocol::credentials::RuntimeUnlock {
            schema_version: respire_protocol::credentials::CREDENTIAL_SCHEMA_V,
            profile: job.custody.clone(),
            session_sha256: format!("{:x}", Sha256::digest(fs::read(session)?)),
            instance_id: "fixture".into(),
            generation: 1,
            super_password: Some(password),
            login_password: None,
            legacy_secret: None,
            provider_credentials: Vec::new(),
        })?;
        let replacement = BackupVaultIdentity::capture(&job.custody)?;
        let error = restore_approved_archive_locked(
            root,
            &job.custody,
            &job.pin,
            &replacement,
            &job.keys,
            &job.snapshot,
            &op,
            &lock,
            job.grant,
            || Ok(()),
        )
        .unwrap_err();
        ensure!(
            error.to_string()
                == "backup grant differs from authenticated Vault owner, profile or epoch",
            "unexpected refusal: {error}"
        );
        ensure!(
            before == Sha256::digest(fs::read(job.pin.path())?),
            "refusal changed SQLite"
        );
        return Ok(());
    }
    let result = restore_approved_archive_locked(
        root,
        &job.custody,
        &job.pin,
        &job.vault,
        &job.keys,
        &job.snapshot,
        &op,
        &lock,
        job.grant,
        || Ok(()),
    )?
    .context("missing install receipt")?;
    ensure!(
        result.phase == "installed" && result.receipt.operation == Some(op),
        "unverified install receipt"
    );
    let db = rusqlite::Connection::open_with_flags(
        job.pin.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let ids = db
        .prepare("SELECT id FROM memories WHERE deleted=0")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<BTreeSet<String>>>()?;
    ensure!(
        ids == BTreeSet::from([job.expected]),
        "wrong selected database contents"
    );
    Ok(())
}

#[test]
fn native_grant_installs_selected_parent_and_child_after_fixture_closes() -> Result<()> {
    run_cases(&[("parent", None), ("child", None)])
}
#[test]
fn native_grant_refuses_new_vault_epoch_and_other_owner() -> Result<()> {
    run_cases(&[
        ("parent", Some("backup-fixture")),
        ("child", Some("other-fixture")),
    ])
}

fn run_cases(cases: &[(&str, Option<&str>)]) -> Result<()> {
    for &(mode, replacement) in cases {
        let dir = tempfile::tempdir()?;
        let witness = dir.path().join("verified");
        let mut command = Command::new(std::env::current_exe()?);
        command.env_remove("RSRS_BACKUP_TEST_REPLACEMENT_OWNER");
        if let Some(owner) = replacement {
            command.env("RSRS_BACKUP_TEST_REPLACEMENT_OWNER", owner);
        }
        let output = command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env(MODE, mode)
            .env(FINISHED, &witness)
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_DATA_DIR", dir.path().canonicalize()?)
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_ADDR")
            .env_remove("RSRS_TOKEN")
            .output()?;
        ensure!(
            output.status.success(),
            "{mode} fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(fs::read(witness)? == b"verified", "missing install witness");
    }
    Ok(())
}
