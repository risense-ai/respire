use super::*;
use crate::transport::MemoryTransport;
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

fn golden_db() -> Result<Vec<u8>> {
    let encoded = include_str!("../../../fixtures/legacy-1x.db.b64");
    Ok(STANDARD.decode(encoded.lines().collect::<String>())?)
}

fn root(dir: &tempfile::TempDir) -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    return Ok(
        respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(
            &dir.path().canonicalize()?,
        )?,
    );
    #[cfg(not(windows))]
    Ok(dir.path().canonicalize()?)
}

fn backups(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    Ok(std::fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("golden.db.bak-pre-v1-"))
        })
        .collect())
}

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let records = statement
        .query_map([], |row| (0..width).map(|i| row.get(i)).collect())?
        .collect::<rusqlite::Result<_>>()?;
    Ok(records)
}

#[test]
fn golden_1x_upgrade_preserves_columns_rows_trigger_backup_and_restart() -> Result<()> {
    let golden = golden_db()?;
    assert_eq!(
        format!("{:x}", Sha256::digest(&golden)),
        "1eb18c5943d16b54fb245a6372e0bc1d40d811e84d0de617afb75b8168e81716"
    );
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let path = root.join("golden.db");
    std::fs::write(&path, golden)?;
    let legacy = Connection::open(&path)?;
    let mut snapshots = Vec::new();
    for table in [
        "memories",
        "memory_chunks",
        "meta",
        "sync_outbox",
        "sync_remote_heads",
        "query_log",
    ] {
        let columns = rows(&legacy, &format!("PRAGMA table_info({table})"))?;
        let names = columns
            .iter()
            .map(|column| match &column[1] {
                Value::Text(name) => Ok(format!("\"{name}\"")),
                _ => anyhow::bail!("invalid golden column"),
            })
            .collect::<Result<Vec<_>>>()?
            .join(",");
        let filter = if table == "meta" {
            " WHERE key LIKE 'golden_%'"
        } else {
            ""
        };
        let query = format!("SELECT {names} FROM {table}{filter} ORDER BY rowid");
        snapshots.push((table, columns, rows(&legacy, &query)?, query));
    }
    let trigger = rows(
        &legacy,
        "SELECT sql FROM sqlite_schema WHERE name='golden_legacy_sync'",
    )?;
    drop(legacy);
    for _ in 0..2 {
        let store = LocalStore::open(&path)?;
        assert_eq!(
            schema_version::require_supported(&store.connection)?,
            schema_version::CURRENT
        );
        for (table, columns, data, query) in &snapshots {
            let upgraded = rows(&store.connection, &format!("PRAGMA table_info({table})"))?;
            assert_eq!(&upgraded[..columns.len()], columns);
            assert_eq!(&rows(&store.connection, query)?, data);
        }
        assert_eq!(
            rows(
                &store.connection,
                "SELECT sql FROM sqlite_schema WHERE name='golden_legacy_sync'"
            )?,
            trigger
        );
        assert_eq!(
            rows(
                &store.connection,
                "SELECT candidates,candidate_scores FROM query_log"
            )?,
            vec![vec![
                Value::Text("[\"legacy-memory-0001\"]".into()),
                Value::Text("[0.75]".into())
            ]]
        );
        let records = store.all(true)?;
        assert_eq!(records.len(), 1);
        let keys = respire_crypto::SessionKeys::from_legacy_urk([42; 32])?;
        let entry = respire_crypto::MemoryEngine::open(&keys, &records[0])?;
        assert_eq!(entry.title, "Legacy fixture");
        assert_eq!(entry.content, "Frozen synthetic historical format sample.");
        assert_eq!(
            store
                .connection
                .query_row::<String, _, _>("PRAGMA quick_check", [], |r| r.get(0))?,
            "ok"
        );
    }
    let copies = backups(&root)?;
    assert_eq!(
        copies.len(),
        1,
        "restart must not create another migration backup"
    );
    let backup = Connection::open_with_flags(&copies[0], OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    assert_eq!(schema_version::require_supported(&backup)?, 0);
    for (_, _, data, query) in snapshots {
        assert_eq!(rows(&backup, &query)?, data);
    }
    Ok(())
}

#[test]
fn fresh_and_complete_unversioned_databases_are_stamped_once() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let path = root.join("golden.db");
    let store = LocalStore::open(&path)?;
    assert_eq!(
        schema_version::require_supported(&store.connection)?,
        schema_version::CURRENT
    );
    assert!(backups(&root)?.is_empty());
    store.connection.pragma_update(None, "user_version", 0)?;
    drop(store);
    for _ in 0..2 {
        let store = LocalStore::open(&path)?;
        assert_eq!(
            schema_version::require_supported(&store.connection)?,
            schema_version::CURRENT
        );
    }
    assert_eq!(backups(&root)?.len(), 1);
    Ok(())
}

#[test]
fn pending_cleanup_is_task_scoped_and_status_uses_sql_aggregates() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = LocalStore::open(&directory.path().join("fresh.db"))?;
    store.meta_set("embedding_pending_task_v1", r#"{"id":"current"}"#)?;
    store.meta_set("embedding_pending_selection_v1", "candidate")?;
    store.meta_set("incumbent-witness", "unchanged")?;
    store.clear_pending_selection(Some("stale"))?;
    assert_eq!(
        store.meta_get("embedding_pending_selection_v1")?.as_deref(),
        Some("candidate")
    );
    store.clear_pending_selection(Some("current"))?;
    assert_eq!(store.meta_get("embedding_pending_selection_v1")?, None);
    assert_eq!(store.meta_get("embedding_pending_task_v1")?, None);
    store.meta_set("embedding_pending_selection_v1", "orphan")?;
    store.clear_pending_selection(None)?;
    assert_eq!(store.meta_get("embedding_pending_selection_v1")?, None);
    assert_eq!(
        store.meta_get("incumbent-witness")?.as_deref(),
        Some("unchanged")
    );
    assert_eq!(store.status_summary()?, (0, 0, None));
    store.connection.execute_batch(
        "INSERT INTO memories(id,updated_at,deleted) VALUES('live','2026-11-26',0),('deleted','2026-11-27',1);"
    )?;
    assert_eq!(store.status_summary()?, (2, 1, Some("2026-11-27".into())));
    Ok(())
}

#[test]
fn failed_legacy_ddl_keeps_original_schema_and_version() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let path = root.join("golden.db");
    std::fs::write(&path, golden_db()?)?;
    let db = Connection::open(&path)?;
    db.execute_batch("CREATE TABLE idx_memories_updated(wrong TEXT);")?;
    let before = rows(&db, "SELECT type,name,sql FROM sqlite_schema ORDER BY name")?;
    drop(db);
    assert!(LocalStore::open(&path).is_err());
    let db = Connection::open(&path)?;
    assert_eq!(
        rows(&db, "SELECT type,name,sql FROM sqlite_schema ORDER BY name")?,
        before
    );
    assert_eq!(schema_version::require_supported(&db)?, 0);
    assert_eq!(backups(&root)?.len(), 1);
    Ok(())
}
