use anyhow::{ensure, Result};
use rusqlite::Connection;

// Published 1.x libraries and current Runtime use SQLite's original zero
// application version. A nonzero marker is not a supported migration entry.
const SUPPORTED_SQLITE_USER_VERSION: i64 = 0;

pub(super) fn require_supported(connection: &Connection) -> Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version == SUPPORTED_SQLITE_USER_VERSION,
        "Unsupported local memory database version {version}; this Runtime supports version {SUPPORTED_SQLITE_USER_VERSION}. Use a compatible Runtime or restore a verified backup."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::LocalStore;
    use anyhow::Result;
    use rusqlite::Connection;

    #[test]
    fn refuses_an_unknown_version_before_any_local_database_mutation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("rsrs.db");
        let connection = Connection::open(&database)?;
        connection.execute_batch(
            "CREATE TABLE witness(value TEXT NOT NULL);
             INSERT INTO witness VALUES ('synthetic future database');
             PRAGMA user_version = 999001;",
        )?;
        drop(connection);
        let before = std::fs::read(&database)?;
        for open in [
            LocalStore::open,
            LocalStore::open_existing,
            LocalStore::open_read_only,
        ] {
            let error = match open(&database) {
                Ok(_) => anyhow::bail!("unsupported database version accepted"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("database version 999001"));
            assert_eq!(std::fs::read(&database)?, before);
            assert!(!directory.path().join("rsrs.db-wal").exists());
        }
        Ok(())
    }
}
