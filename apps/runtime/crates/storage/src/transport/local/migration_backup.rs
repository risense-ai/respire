use anyhow::{ensure, Result};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection,
};

/// An online backup made while a different source connection holds
/// BEGIN IMMEDIATE. Pending SQLite WAL data is included; do not file-copy.
pub(super) fn copy_and_check(source: &Connection, destination: &mut Connection) -> Result<()> {
    {
        let backup = Backup::new(source, destination)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            ensure!(
                std::time::Instant::now() < deadline,
                "migration backup exceeded its deadline"
            );
            match backup.step(256)? {
                StepResult::Done => break,
                StepResult::More => continue,
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                _ => anyhow::bail!("unexpected migration backup step"),
            }
        }
    }
    let integrity: String = destination.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    ensure!(
        integrity == "ok",
        "migration backup failed SQLite quick_check"
    );
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(super) fn publish_before_ddl(
    database: &std::path::Path,
    next_version: i64,
    name_suffix: &str,
) -> Result<std::path::PathBuf> {
    use respire_spawn::owner_credential::MigrationCopy;
    use rusqlite::OpenFlags;
    ensure!(next_version > 0, "invalid target schema version");
    let meta = std::fs::symlink_metadata(database)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "unsafe SQLite backup source"
    );
    let parent = database
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing database parent"))?
        .canonicalize()?;
    let leaf = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("expected portable database filename"))?;
    let filename = format!("{leaf}.bak-pre-v{next_version}-{name_suffix}");
    let pending = format!(".{filename}.pending");
    let stage = MigrationCopy::create(&parent, &pending)?;
    let source_identity = stage.source_identity(leaf)?;
    // Online Backup reads committed pages through WAL. Check the SQLite
    // companion names through the same native dirfd/handle, never through links.
    for extension in ["-wal", "-shm", "-journal"] {
        match stage.source_identity(&format!("{leaf}{extension}")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let output = parent.join(&filename);
    let flags = OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let source =
        Connection::open_with_flags(parent.join(leaf), OpenFlags::SQLITE_OPEN_READ_ONLY | flags)?;
    {
        let mut copy =
            Connection::open_with_flags(stage.path(), OpenFlags::SQLITE_OPEN_READ_WRITE | flags)?;
        copy.busy_timeout(std::time::Duration::from_secs(3))?;
        // Do not create a sibling SQLite journal outside the private file's ACL.
        let staging_mode: String =
            copy.query_row("PRAGMA journal_mode=MEMORY", [], |r| r.get(0))?;
        ensure!(
            staging_mode.eq_ignore_ascii_case("memory"),
            "private backup staged with disk journal"
        );
        copy_and_check(&source, &mut copy)?;
        // Online Backup writes a standalone SQLite database. A WAL destination
        // would make this single-file publish incomplete on crash.
        let mode: String = copy.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        ensure!(
            mode.eq_ignore_ascii_case("delete"),
            "backup is not standalone"
        );
        copy.close().map_err(|(_, error)| error)?;
    }
    ensure!(
        stage.source_identity(leaf)? == source_identity,
        "migration source changed during backup"
    );
    drop(source);
    stage.publish(&filename)?;
    Ok(output)
}
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[cfg(test)]
fn guarded_schema_edit(
    live: &mut Connection,
    database: &std::path::Path,
    suffix: &str,
    edit: impl FnOnce(&Connection) -> Result<()>,
) -> Result<std::path::PathBuf> {
    let tx = live.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    super::schema_version::require_supported(&tx)?;
    let backup = publish_before_ddl(database, 1, suffix)?;
    super::witness::register_witness_functions(&tx)?;
    super::schema::apply_schema_steps(&tx)?;
    edit(&tx)?;
    let check: String = tx.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    ensure!(check == "ok", "migrated database failed SQLite quick_check");
    tx.commit()?;
    Ok(backup)
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{OpenFlags, TransactionBehavior};

    #[test]
    fn migration_backup_keeps_writer_fence_against_a_separate_process() -> Result<()> {
        const CHILD: &str = "RESPIRE_GOLDEN_LOCK_CHILD_DB";
        if let Some(path) = std::env::var_os(CHILD) {
            let mut writer = Connection::open(path)?;
            writer.busy_timeout(std::time::Duration::ZERO)?;
            let outcome = writer.transaction_with_behavior(TransactionBehavior::Immediate);
            assert!(matches!(outcome,
                Err(rusqlite::Error::SqliteFailure(ref error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy));
            return Ok(());
        }
        let temporary = tempfile::tempdir()?;
        #[cfg(windows)]
        let root = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(
            temporary.path(),
        )?;
        #[cfg(not(windows))]
        let root = temporary.path().canonicalize()?;
        let path = root.join("source.db");
        let mut source = Connection::open(&path)?;
        source.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE data(value TEXT);
            INSERT INTO data VALUES ('committed synthetic witness');",
        )?;
        let fence = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
        publish_before_ddl(&path, 1, "native-fence")?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact",
                "transport::local::migration_backup::tests::migration_backup_keeps_writer_fence_against_a_separate_process",
                "--nocapture"])
            .env(CHILD, &path)
            .output()?;
        assert!(
            child.status.success(),
            "competing process did not fail closed: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        fence.rollback()?;
        Ok(())
    }
    #[test]
    fn online_backup_keeps_wal_grants_and_pending_outbox_before_any_ddl() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("original.db");
        let mut original = Connection::open(&path)?;
        original.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA wal_autocheckpoint=0;
            CREATE TABLE memories(id TEXT PRIMARY KEY,ciphertext TEXT);
            CREATE TABLE grants(id TEXT PRIMARY KEY,token_hash TEXT);
            CREATE TABLE sync_outbox(id TEXT PRIMARY KEY,payload TEXT,status TEXT);
            INSERT INTO memories VALUES ('fixture','synthetic historical ciphertext');
            INSERT INTO grants VALUES ('fixture','synthetic private token hash');
            INSERT INTO sync_outbox VALUES ('fixture','synthetic durable operation','pending');
        ",
        )?;
        let fence = original.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let other_writer = Connection::open(&path)?;
        other_writer.busy_timeout(std::time::Duration::ZERO)?;
        assert!(other_writer.execute("DELETE FROM grants", []).is_err());
        let source = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let dest = directory.path().join("backup.db");
        let mut destination = Connection::open(&dest)?;
        copy_and_check(&source, &mut destination)?;
        let text = |sql| destination.query_row::<String, _, _>(sql, [], |r| r.get(0));
        assert_eq!(
            text("SELECT ciphertext FROM memories")?,
            "synthetic historical ciphertext"
        );
        assert_eq!(
            text("SELECT token_hash FROM grants")?,
            "synthetic private token hash"
        );
        assert_eq!(text("SELECT status FROM sync_outbox")?, "pending");
        assert_eq!(
            text("SELECT payload FROM sync_outbox")?,
            "synthetic durable operation"
        );
        assert_eq!(
            destination.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            0
        );
        fence.execute("CREATE INDEX example ON memories(ciphertext)", [])?;
        fence.rollback()?;
        assert!(!original
            .prepare("SELECT 1 FROM sqlite_schema WHERE name='example'")?
            .exists([])?);
        assert!(!destination
            .prepare("SELECT 1 FROM sqlite_schema WHERE name='example'")?
            .exists([])?);
        Ok(())
    }
    #[test]
    fn owner_only_publication_precedes_schema_changes_and_collision_aborts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        #[cfg(windows)]
        let profile =
            respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(
                dir.path(),
            )?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let profile = dir.path().canonicalize()?;
        let path = profile.join("original.db");
        let mut original = Connection::open(&path)?;
        original.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE access_grants(id TEXT, token_hash TEXT);
            CREATE TABLE sync_outbox(id TEXT, state TEXT);
            INSERT INTO access_grants VALUES ('g','synthetic private hash');
            INSERT INTO sync_outbox VALUES ('o','pending');",
        )?;
        let fence = original.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let backup = publish_before_ddl(&path, 1, "synthetic-unique")?;
        assert_eq!(
            backup.file_name().unwrap(),
            "original.db.bak-pre-v1-synthetic-unique"
        );
        fence.execute("CREATE INDEX only_new ON access_grants(id)", [])?;
        fence.commit()?;
        let snapshot = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            snapshot.query_row::<String, _, _>(
                "SELECT token_hash FROM access_grants",
                [],
                |row| row.get(0)
            )?,
            "synthetic private hash"
        );
        assert_eq!(
            snapshot
                .query_row::<String, _, _>("SELECT state FROM sync_outbox", [], |row| row.get(0))?,
            "pending"
        );
        assert!(!snapshot
            .prepare("SELECT 1 FROM sqlite_schema WHERE name='only_new'")?
            .exists([])?);
        drop(snapshot);
        let original_backup = std::fs::read(&backup)?;
        let fence = original.transaction_with_behavior(TransactionBehavior::Immediate)?;
        assert!(publish_before_ddl(&path, 1, "synthetic-unique").is_err());
        fence.rollback()?;
        assert_eq!(std::fs::read(&backup)?, original_backup);
        let alias = profile.join("same-inode-alias.db");
        std::fs::hard_link(&path, &alias)?;
        assert!(publish_before_ddl(&path, 1, "blocked-hardlink").is_err());
        assert!(!profile
            .join("original.db.bak-pre-v1-blocked-hardlink")
            .exists());
        std::fs::remove_file(alias)?;
        assert!(!profile
            .join(".original.db.bak-pre-v1-synthetic-unique.pending")
            .exists());
        assert!(original
            .prepare("SELECT 1 FROM sqlite_schema WHERE name='only_new'")?
            .exists([])?);
        Ok(())
    }
    #[test]
    #[ignore = "requires the exact published Golden file passed by the native operator"]
    fn published_golden_repair_is_durable_and_failure_keeps_original() -> Result<()> {
        use sha2::{Digest, Sha256};
        let golden = std::path::PathBuf::from(
            std::env::var_os("RESPIRE_TEST_GOLDEN_DB")
                .ok_or_else(|| anyhow::anyhow!("provide the published Golden DB explicitly"))?,
        );
        let bytes = std::fs::read(golden)?;
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            "a081b149521d3dd516fdda50bc744249293fb30990e3b98d1ee392189fe02c9d"
        );
        let dir = tempfile::tempdir()?;
        #[cfg(windows)]
        let root = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(
            dir.path(),
        )?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let root = dir.path().canonicalize()?;
        let path = root.join("rsrs.db");
        std::fs::write(&path, bytes)?;
        let mut current = Connection::open(&path)?;
        current.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            DROP INDEX idx_memories_updated;
            INSERT INTO access_grants(id,label,root_id,token_hash,created_at)
            VALUES('synthetic','known','root','synthetic-token-hash','2026-01-01');
            INSERT INTO sync_outbox(op_id,id,user,ciphertext,nonce,updated_at,deleted)
            VALUES('synthetic','root','synthetic-user','synthetic-ciphertext','synthetic-nonce','2026-01-01',0);")?;
        let has_index = |db: &Connection| -> Result<bool> {
            Ok(db
                .prepare("SELECT 1 FROM sqlite_schema WHERE name='idx_memories_updated'")?
                .exists([])?)
        };
        assert!(!has_index(&current)?);
        let blocked = root.join("rsrs.db.bak-pre-v1-collision");
        std::fs::write(&blocked, b"unchanged collision sentinel")?;
        assert!(
            guarded_schema_edit(&mut current, &path, "collision", |_| panic!("no admission"))
                .is_err()
        );
        assert!(!has_index(&current)?);
        assert_eq!(std::fs::read(blocked)?, b"unchanged collision sentinel");
        assert!(
            guarded_schema_edit(&mut current, &path, "injected-fail", |_| anyhow::bail!(
                "post-DDL rollback"
            ))
            .is_err()
        );
        assert!(!has_index(&current)?);
        assert!(root.join("rsrs.db.bak-pre-v1-injected-fail").is_file());
        let backup = guarded_schema_edit(&mut current, &path, "verified", |tx| {
            assert!(has_index(tx)?);
            let original = Connection::open_with_flags(
                root.join("rsrs.db.bak-pre-v1-verified"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            assert!(!has_index(&original)?);
            assert_eq!(
                original.query_row::<String, _, _>(
                    "SELECT token_hash FROM access_grants",
                    [],
                    |r| r.get(0)
                )?,
                "synthetic-token-hash"
            );
            assert_eq!(
                original
                    .query_row::<String, _, _>("SELECT state FROM sync_outbox", [], |r| r.get(0))?,
                "pending"
            );
            Ok(())
        })?;
        assert!(backup.is_file());
        drop(current);
        let reopened = super::super::LocalStore::open(&path)?;
        drop(reopened);
        let current = Connection::open(&path)?;
        assert!(has_index(&current)?);
        assert_eq!(
            current.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            current.query_row::<String, _, _>("SELECT token_hash FROM access_grants", [], |r| r
                .get(0))?,
            "synthetic-token-hash"
        );
        assert_eq!(
            current.query_row::<String, _, _>("SELECT state FROM sync_outbox", [], |r| r.get(0))?,
            "pending"
        );
        Ok(())
    }
}
