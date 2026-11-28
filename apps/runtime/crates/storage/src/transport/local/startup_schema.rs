use super::{schema, schema_version, witness, Connection, LocalStore, OpenFlags, Result};
use anyhow::{ensure, Context};
use rusqlite::TransactionBehavior;
use std::{collections::BTreeSet, path::Path, sync::OnceLock};

#[cfg(test)]
#[path = "startup_schema_tests.rs"]
mod startup_schema_tests;

const COLUMN_REPAIR_TABLES: &[&str] = &["memories", "query_log", "sync_remote_heads"];

struct Shape {
    objects: BTreeSet<(String, String)>,
    columns: Vec<BTreeSet<String>>,
    meta: BTreeSet<String>,
}

fn catalog(db: &Connection) -> Result<Shape> {
    let mut entries =
        db.prepare("SELECT type,name FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'")?;
    let objects = entries
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut columns = Vec::with_capacity(COLUMN_REPAIR_TABLES.len());
    for table in COLUMN_REPAIR_TABLES {
        columns.push(
            db.prepare("SELECT name FROM pragma_table_info(?1)")?
                .query_map([table], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
        );
    }
    let meta = if db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta'")?
        .exists([])?
    {
        db.prepare("SELECT key FROM meta")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        BTreeSet::new()
    };
    Ok(Shape {
        objects,
        columns,
        meta,
    })
}

fn all_steps(db: &Connection) -> Result<()> {
    schema::apply_schema_steps(db)?;
    db.execute_batch(include_str!("sync_schema.sql"))?;
    db.execute_batch(include_str!("resident_schema.sql"))?;
    LocalStore::initialize_sync_schema(db)?;
    Ok(())
}

/// Derive expected shape from the actual initialization DDL; historical
/// extra columns, tables and indexes are permitted and left untouched.
fn requires_repair(db: &Connection) -> Result<bool> {
    static SHAPE: OnceLock<Shape> = OnceLock::new();
    if SHAPE.get().is_none() {
        let reference = Connection::open_in_memory()?;
        witness::register_witness_functions(&reference)?;
        all_steps(&reference)?;
        let _ = SHAPE.set(catalog(&reference)?);
    }
    let expected = SHAPE.get().context("could not derive migration catalog")?;
    let current = catalog(db)?;
    if !expected.objects.is_subset(&current.objects)
        || expected
            .columns
            .iter()
            .zip(&current.columns)
            .any(|(expected, current)| !expected.is_subset(current))
        || !expected.meta.is_subset(&current.meta)
        || !schema::metadata_insert_trigger_current(db)?
    {
        return Ok(true);
    }
    if !db
        .prepare("SELECT 1 FROM retrieval_revision WHERE singleton=1")?
        .exists([])?
    {
        return Ok(true);
    }
    for column in ["when_to_use", "superseded_by"] {
        let trigger = (
            "trigger".to_owned(),
            format!("memory_revision_{column}_update"),
        );
        if current.columns[0].contains(column) && !current.objects.contains(&trigger) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn open(
    path: &Path,
    initial_model: impl FnOnce() -> Result<Option<respire_core_sdk::profiles::Selection>>,
) -> Result<Connection> {
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("failed to open local memory db: {}", path.display()))?;
    schema_version::require_supported(&db)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=30000;")?;
    witness::register_witness_functions(&db)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old = schema_version::require_supported(&tx)?;
    let existing = tx
        .prepare("SELECT 1 FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'")?
        .exists([])?;
    schema::validate_existing_memory_schema(&tx)?;
    if old < schema_version::CURRENT || !existing || requires_repair(&tx)? {
        if existing {
            // The first guarded repair can migrate v0 only. Do not invent a
            // second transition on a damaged already-versioned database.
            ensure!(
                old == 0,
                "versioned local memory database requires verified restore"
            );
            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            {
                let suffix = format!(
                    "{}-{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S"),
                    uuid::Uuid::new_v4().simple()
                );
                super::migration_backup::publish_before_ddl(
                    path,
                    schema_version::CURRENT,
                    &suffix,
                )?;
            }
            #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
            anyhow::bail!("no private backup support on this platform");
        }
        all_steps(&tx)?;
        let outcome: String = tx.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        ensure!(
            outcome == "ok",
            "migrated local memory database failed SQLite quick_check"
        );
        tx.pragma_update(None, "user_version", schema_version::CURRENT)?;
        if !existing {
            if let Some(selection) = initial_model()? {
                super::retrieval::write_active_selection(&tx, &selection, &serde_json::to_string(&selection)?)?;
            }
        }
    } else {
        // The existing sync maintenance is not a schema transition.
        LocalStore::backfill_sync_heads(&tx)?;
    }
    tx.commit()?;
    db.execute_batch("PRAGMA journal_mode=WAL;")?;
    Ok(db)
}

// The caller has custody of an authenticated detached file. No live-library
// marker transition or pre-upgrade original authority is carried in this copy.
pub(super) fn validate_detached(mut db: Connection) -> Result<()> {
    let approved = schema_version::require_supported(&db)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=30000;")?;
    witness::register_witness_functions(&db)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    schema::validate_existing_memory_schema(&tx)?;
    if requires_repair(&tx)? {
        ensure!(approved == 0, "versioned detached database requires verified restore");
        all_steps(&tx)?;
    }
    let check: String = tx.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    ensure!(
        check == "ok" && schema_version::require_supported(&tx)? == approved,
        "detached migration staging failed integrity or changed version"
    );
    tx.commit()?;
    Ok(())
}
