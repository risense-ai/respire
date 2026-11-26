//! Local snapshot inventory. Manifests describe files; they never grant access.
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub created_at: String,
    pub note: String,
    pub database_sha256: String,
    pub bytes: u64,
    pub entries: u64,
    pub source_library: String,
    pub account_fingerprint: String,
    pub sqlite_user_version: i64,
    pub database_schema_sha256: String,
    pub sdk_version: String,
    pub sdk_abi: u32,
    pub model: String,
    #[serde(default)]
    pub witness_head: Option<String>,
    #[serde(default)]
    pub witness_count: Option<u64>,
    #[serde(default)]
    pub witness_intact: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_problem: Option<String>,
}

fn private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        let meta = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "snapshot directory must be a real directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "snapshot directory is not private; host operator must restrict it to mode 0700"
            );
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(path)?;
    }
    Ok(())
}

fn private_new(path: &Path) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn digest_file(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "snapshot must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "snapshot file is not private"
        );
    }
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn inventory(path: &Path) -> Result<(u64, i64)> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let check: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    anyhow::ensure!(check == "ok", "snapshot integrity check failed");
    let grants: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='access_grants')",
        [],
        |row| row.get(0),
    )?;
    if grants {
        let count: u64 =
            db.query_row("SELECT count(*) FROM access_grants", [], |row| row.get(0))?;
        anyhow::ensure!(count == 0, "snapshot must not contain access grants");
    }
    Ok((
        db.query_row("SELECT count(*) FROM memories", [], |row| row.get(0))?,
        db.query_row("PRAGMA user_version", [], |row| row.get(0))?,
    ))
}

fn schema_digest(path: &Path) -> Result<String> {
    schema_digest_without_triggers(path, &[])
}

fn schema_digest_without_triggers(path: &Path, omitted: &[String]) -> Result<String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement=db.prepare("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name<>'snapshot_key_check' ORDER BY type,name")?;
    let mut rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.retain(|(kind, name, _)| kind != "trigger" || !omitted.contains(name));
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(version, rows))?)
    ))
}

fn legacy_revision_trigger_sql(column: &str) -> String {
    format!(
        "CREATE TRIGGER memory_revision_{column}_update
                 AFTER UPDATE OF {column} ON memories
                 WHEN OLD.{column} IS NOT NEW.{column}
                 BEGIN
                     UPDATE meta SET value=lower(hex(randomblob(16))) WHERE key='memory_revision_v1';
                 END;"
    )
}

fn missing_legacy_revision_triggers(live: &Path, source: &Path) -> Result<Vec<&'static str>> {
    let live = Connection::open_with_flags(live, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut missing = Vec::new();
    for column in ["when_to_use", "superseded_by"] {
        let name = format!("memory_revision_{column}_update");
        let has_column: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('memories') WHERE name=?1)",
            [column],
            |row| row.get(0),
        )?;
        let has_trigger = source.prepare(
            "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1 COLLATE NOCASE",
        )?.exists([&name])?;
        let current: Option<String> = live.query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1 AND tbl_name='memories'",
            [&name],
            |row| row.get(0),
        ).optional()?;
        let expected = legacy_revision_trigger_sql(column);
        if has_column && !has_trigger && current.as_deref() == Some(expected.trim_end_matches(';')) {
            missing.push(column);
        }
    }
    Ok(missing)
}

fn install_legacy_revision_triggers(path: &Path, columns: &[&str]) -> Result<()> {
    let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON;")?;
    let transaction = db.transaction()?;
    for column in columns {
        transaction.execute_batch(&legacy_revision_trigger_sql(column))?;
    }
    // A trigger on meta must not change snapshot content during this refresh.
    let before = transaction.total_changes();
    anyhow::ensure!(
        transaction.execute(
            "UPDATE meta SET value=lower(hex(randomblob(16))) WHERE key='memory_revision_v1'",
            [],
        )? == 1 && transaction.total_changes() == before + 1,
        "memory revision could not be refreshed without other changes"
    );
    transaction.commit()?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn create(
    root: &Path,
    account: &str,
    note: &str,
    keys: &crate::SessionKeys,
) -> Result<Manifest> {
    anyhow::ensure!(
        note.chars().count() <= 1000,
        "snapshot note exceeds 1000 characters"
    );
    let source = crate::service::database_path(root)?;
    reject_live_symlink(&source)?;
    anyhow::ensure!(source.is_file(), "snapshot requires an existing library");
    let directory = root.join("snapshots");
    private_dir(&directory)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let database = directory.join(format!("{id}.db"));
    let temporary = directory.join(format!(".{id}.json.tmp"));
    let published = directory.join(format!("{id}.json"));
    // backup_database owns cleanup if its exclusive creation fails; never
    // remove a file that was present before our successful backup operation.
    crate::service::backup_database(&source, &database)?;
    let result = (|| -> Result<Manifest> {
        let (entries, sqlite_user_version) = inventory(&database)?;
        let sdk_version = crate::core_sdk::CORE_VERSION.to_owned();
        let source_library = format!(
            "{:x}",
            Sha256::digest(fs::canonicalize(&source)?.to_string_lossy().as_bytes())
        );
        let account_fingerprint = format!("{:x}", Sha256::digest(account.as_bytes()));
        let database_schema_sha256 = schema_digest(&database)?;
        let challenge =
            format!("snapshot-key-check:v1:{id}:{source_library}:{account_fingerprint}");
        let (nonce, ciphertext) = keys.encrypt_content(&challenge)?;
        {
            let copy = Connection::open(&database)?;
            copy.execute_batch("DROP TABLE IF EXISTS snapshot_key_check; CREATE TABLE snapshot_key_check(singleton INTEGER PRIMARY KEY CHECK(singleton=1),nonce TEXT NOT NULL,ciphertext TEXT NOT NULL);")?;
            copy.execute(
                "INSERT INTO snapshot_key_check VALUES(1,?1,?2)",
                rusqlite::params![nonce, ciphertext],
            )?;
        }
        // A diagnostic pre-restore backup must preserve a damaged ledger too.
        // Restore targets remain fail-closed in read_manifest below.
        let (witness, witness_problem) =
            match crate::transport::local::witness_checkpoint_file(&database) {
                Ok(report) => (report, None),
                Err(_) => (
                    None,
                    Some("witness checkpoint is unverifiable; diagnostic copy only".to_owned()),
                ),
            };
        let manifest = Manifest {
            schema_version: 1,
            id,
            created_at: chrono::Utc::now().to_rfc3339(),
            note: note.to_owned(),
            database_sha256: digest_file(&database)?,
            bytes: fs::metadata(&database)?.len(),
            entries,
            source_library,
            account_fingerprint,
            database_schema_sha256,
            sqlite_user_version,
            sdk_version,
            sdk_abi: crate::core_sdk::ABI_VERSION,
            model: {
                let copy = Connection::open_with_flags(&database,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let snapshot: Option<String> = copy.query_row("SELECT value FROM meta WHERE key=?1",[crate::core_sdk::profiles::ACTIVE_SELECTION_KEY],|r|r.get(0)).optional()?;
                snapshot.map(|v| serde_json::from_str::<crate::core_sdk::profiles::Selection>(&v).map(|s|s.model)).transpose()?.unwrap_or_else(||"m3".to_owned())
            },
            witness_head: witness.as_ref().map(|report| report.head.clone()),
            witness_count: witness.as_ref().map(|report| report.count),
            witness_intact: if witness_problem.is_some() {
                Some(false)
            } else {
                witness.as_ref().map(|report| report.intact)
            },
            witness_problem,
        };
        let mut file = private_new(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        file.sync_all()?;
        drop(file);
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&database)?
            .sync_all()?;
        // An atomic, no-overwrite publication: a crash before this leaves no
        // visible manifest, and an existing manifest can never be overwritten.
        respire_spawn::durable::publish_new_file(&temporary, &published)?;
        respire_spawn::durable::sync_directory(root)?;
        Ok(manifest)
    })();
    if result.is_err() {
        // Only this invocation's UUID files are eligible for failure cleanup.
        let _ = fs::remove_file(&temporary);
        if !published.exists() {
            fs::remove_file(&database).context("failed to clean incomplete snapshot")?;
        }
    }
    result
}

#[derive(Debug, Serialize)]
pub struct SnapshotProblem {
    pub id: String,
    pub error: String,
}
#[derive(Debug, Serialize)]
pub struct SnapshotInventory {
    pub snapshots: Vec<Manifest>,
    pub problems: Vec<SnapshotProblem>,
}

fn read_manifest(root: &Path, id: &str) -> Result<Manifest> {
    anyhow::ensure!(valid_id(id), "snapshot requires a full snapshot ID");
    let directory = root.join("snapshots");
    private_dir(&directory)?;
    let path = directory.join(format!("{id}.json"));
    let metadata = fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 1048576,
        "invalid snapshot manifest file"
    );
    let manifest: Manifest = serde_json::from_slice(&fs::read(&path)?)?;
    anyhow::ensure!(
        manifest.schema_version == 1 && manifest.id == id,
        "unsupported snapshot manifest or identity mismatch"
    );
    chrono::DateTime::parse_from_rfc3339(&manifest.created_at)
        .context("invalid snapshot timestamp")?;
    let database = directory.join(format!("{id}.db"));
    validate_manifest_database(&database, &manifest)?;
    Ok(manifest)
}

fn validate_manifest_database(database: &Path, manifest: &Manifest) -> Result<()> {
    anyhow::ensure!(
        manifest.bytes == fs::metadata(&database)?.len()
            && manifest.database_sha256 == digest_file(&database)?,
        "snapshot digest mismatch"
    );
    anyhow::ensure!(
        schema_digest(&database)? == manifest.database_schema_sha256,
        "snapshot schema digest mismatch"
    );
    verify_witness(&database, &manifest)?;
    let (entries, version) = inventory(&database)?;
    anyhow::ensure!(
        entries == manifest.entries && version == manifest.sqlite_user_version,
        "snapshot inventory mismatch"
    );
    Ok(())
}

fn verify_witness(database: &Path, manifest: &Manifest) -> Result<()> {
    anyhow::ensure!(
        manifest.witness_problem.is_none(),
        "snapshot witness checkpoint is unverifiable; diagnostic copy cannot be a restore target"
    );
    match crate::transport::local::witness_checkpoint_file(database)? {
        None => anyhow::ensure!(
            manifest.witness_head.is_none()
                && manifest.witness_count.is_none()
                && manifest.witness_intact.is_none(),
            "snapshot witness checkpoint exists without a witness schema"
        ),
        Some(report) => {
            anyhow::ensure!(
                manifest.witness_head.as_ref() == Some(&report.head)
                    && manifest.witness_count == Some(report.count)
                    && manifest.witness_intact == Some(report.intact),
                "snapshot witness checkpoint mismatch or missing checkpoint"
            );
            anyhow::ensure!(
                report.intact,
                "snapshot witness chain is invalid; preserve the copy for explicit recovery/repair"
            );
        }
    }
    Ok(())
}

pub(crate) fn verify_backup(root: &Path, id: &str, keys: &crate::SessionKeys) -> Result<Manifest> {
    let manifest = read_manifest(root, id)?;
    verify_key(&root.join("snapshots").join(format!("{id}.db")), &manifest, keys)?;
    Ok(manifest)
}

pub fn list_report(root: &Path) -> Result<SnapshotInventory> {
    let directory = root.join("snapshots");
    if !directory.exists() {
        return Ok(SnapshotInventory {
            snapshots: Vec::new(),
            problems: Vec::new(),
        });
    }
    private_dir(&directory)?;
    let mut valid = Vec::new();
    let mut problems = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|v| v != "json") || !valid_id(id) {
            continue;
        }
        match read_manifest(root, id) {
            Ok(manifest) => valid.push((
                chrono::DateTime::parse_from_rfc3339(&manifest.created_at)?,
                manifest,
            )),
            Err(error) => problems.push(SnapshotProblem {
                id: id.to_owned(),
                error: error.to_string(),
            }),
        }
    }
    valid.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.id.cmp(&a.1.id)));
    problems.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(SnapshotInventory {
        snapshots: valid.into_iter().map(|(_, manifest)| manifest).collect(),
        problems,
    })
}

pub fn list(root: &Path) -> Result<Vec<Manifest>> {
    let report = list_report(root)?;
    anyhow::ensure!(
        report.problems.is_empty(),
        "snapshot inventory contains damaged entries; inspect snapshot list diagnostics"
    );
    Ok(report.snapshots)
}

pub mod backup_archive;

include!("snapshot/journal.rs");
include!("snapshot/legacy_recovery.rs");
include!("snapshot/restore.rs");
include!("snapshot/restore_lifecycle.rs");
#[cfg(test)]
mod tests;
