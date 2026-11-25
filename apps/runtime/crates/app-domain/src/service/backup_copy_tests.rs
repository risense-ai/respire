use super::{backup_database_policy, sanitize_detached_backup};
use rusqlite::{params, Connection};
use std::fs;

const GRANT: &[u8] = b"synthetic-no-authority-grant-page-marker";
const HISTORIC: &[u8] = b"synthetic-no-authority-retired-grant-page";

fn contains(bytes: &[u8], expected: &[u8]) -> bool {
    bytes.windows(expected.len()).any(|window| window == expected)
}

#[test]
fn copy_lanes_keep_same_sqlite_state_but_only_rollback_retains_grants() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("fixture.db");
    let portable = root.path().join("portable.db");
    let rollback = root.path().join("rollback.db");
    let connection = Connection::open(&source)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
         PRAGMA secure_delete=OFF; PRAGMA user_version=7;
         CREATE TABLE memories(id INTEGER PRIMARY KEY, payload TEXT);
         CREATE TABLE access_grants(id INTEGER PRIMARY KEY, token BLOB);",
    )?;
    connection.execute("INSERT INTO memories VALUES(1,'committed fixture')", [])?;
    connection.execute("INSERT INTO access_grants VALUES(1,?1)", params![GRANT])?;
    connection.execute("INSERT INTO access_grants VALUES(2,?1)", params![HISTORIC])?;
    connection.execute("DELETE FROM access_grants WHERE id=2", [])?;

    backup_database_policy(&source, &portable, true)?;
    backup_database_policy(&source, &rollback, false)?;
    for image in [&portable, &rollback] {
        let detached = Connection::open(image)?;
        let row: String = detached.query_row("SELECT payload FROM memories", [], |r| r.get(0))?;
        let version: i64 = detached.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        assert_eq!(row, "committed fixture");
        assert_eq!(version, 7);
    }
    let exported = fs::read(&portable)?;
    assert!(!contains(&exported, GRANT));
    assert!(!contains(&exported, HISTORIC));
    assert!(contains(&fs::read(&rollback)?, GRANT));
    let original: Vec<u8> =
        connection.query_row("SELECT token FROM access_grants", [], |row| row.get(0))?;
    assert_eq!(original, GRANT);
    Ok(())
}

#[test]
fn refuses_a_backup_if_grant_removal_alters_other_rows() -> anyhow::Result<()> {
    let mut backup = Connection::open_in_memory()?;
    backup.execute_batch(
        "CREATE TABLE memories(id INTEGER PRIMARY KEY, payload TEXT);
         CREATE TABLE access_grants(id INTEGER PRIMARY KEY, token BLOB);
         INSERT INTO memories VALUES(1,'source');
         INSERT INTO access_grants VALUES(1,x'00');
         CREATE TRIGGER unrelated AFTER DELETE ON access_grants BEGIN
             UPDATE memories SET payload='changed' WHERE id=1;
         END;",
    )?;
    assert!(sanitize_detached_backup(&mut backup).is_err());
    let original: String = backup.query_row("SELECT payload FROM memories", [], |r| r.get(0))?;
    let grants: i64 = backup.query_row("SELECT count(*) FROM access_grants", [], |r| r.get(0))?;
    assert_eq!(original, "source");
    assert_eq!(grants, 1);
    Ok(())
}

#[test]
fn leaves_a_legacy_database_without_a_grant_schema_compatible() -> anyhow::Result<()> {
    let mut backup = Connection::open_in_memory()?;
    backup.execute_batch("PRAGMA user_version=3; CREATE TABLE memories(id INTEGER PRIMARY KEY);")?;
    sanitize_detached_backup(&mut backup)?;
    let version: i64 = backup.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 3);
    assert!(!backup
        .prepare("SELECT 1 FROM sqlite_master WHERE name='access_grants'")?
        .exists([])?);
    Ok(())
}
