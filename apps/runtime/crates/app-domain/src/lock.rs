//! lock — process-wide mutex for the local library (multi-session protection)
//!
//! A SQLite lock db (`lock.db`) lives in the data directory. The process holds
//! `BEGIN EXCLUSIVE` until exit. Later arrivals poll on SQLITE_BUSY; SQLite
//! releases the lock when the holder exits or crashes. No deadlock, no stale
//! lock files. Every rsrs subcommand acquires this lock in main.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{Connection, ErrorCode};

pub struct LibraryLock {
    _conn: Connection,
}

impl LibraryLock {
    /// Verify that this live ownership guard belongs to the requested library.
    pub fn protects(&self, dir: &Path) -> Result<bool> {
        let held = self
            ._conn
            .path()
            .ok_or_else(|| anyhow::anyhow!("library lock has no persistent path"))?;
        Ok(std::fs::canonicalize(held)? == std::fs::canonicalize(dir.join("lock.db"))?)
    }

    /// Try the exact lock once, with no SQLite busy wait.
    pub fn try_acquire(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("lock.db"))?;
        conn.busy_timeout(Duration::ZERO)?;
        conn.execute_batch("BEGIN EXCLUSIVE;")?;
        Ok(Self { _conn: conn })
    }

    /// A published local space already has a lock database; never create or
    /// follow a replacement path while resolving explicit device-space work.
    pub(super) fn try_acquire_device_space(dir: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let conn = Connection::open_with_flags(
            dir.join("lock.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::ZERO)?;
        conn.execute_batch("BEGIN EXCLUSIVE;")?;
        Ok(Self { _conn: conn })
    }

    /// Take the library lock and hold it until this object (this process) ends.
    /// While occupied, print wait messages on stderr (do not pollute json stdout)
    /// and retry every 300ms. Give up after max_wait so a stuck holder cannot block forever.
    pub fn acquire(dir: &Path, max_wait: Duration) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("failed to create data dir ({}): {e}", dir.display()))?;
        let path = dir.join("lock.db");
        let conn = Connection::open(&path)
            .map_err(|e| anyhow::anyhow!("failed to open lock db ({}): {e}", path.display()))?;
        let _ = conn.busy_timeout(Duration::from_millis(200));
        let began = Instant::now();
        let mut reported_first = false;
        let mut next_report = Duration::from_secs(10);
        loop {
            match conn.execute_batch("BEGIN EXCLUSIVE;") {
                Ok(_) => return Ok(Self { _conn: conn }),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == ErrorCode::DatabaseBusy || e.code == ErrorCode::DatabaseLocked =>
                {
                    let waited = began.elapsed();
                    if waited >= max_wait {
                        anyhow::bail!(
                            "memory library still locked after {}s \u{2014} check the holding process or retry later",
                            waited.as_secs()
                        );
                    }
                    if waited >= next_report {
                        eprintln!("still waiting for another session to release the library ({}s)", waited.as_secs());
                        next_report += Duration::from_secs(10);
                    } else if !reported_first {
                        eprintln!("memory library is locked by another session; waiting (lock.db exclusive, released when the holder exits)");
                        reported_first = true;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn device_space_lock_requires_an_existing_regular_database() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("lock.db");
        assert!(LibraryLock::try_acquire_device_space(root.path()).is_err());
        assert!(!path.exists());
        std::fs::create_dir(&path)?;
        assert!(LibraryLock::try_acquire_device_space(root.path()).is_err());
        assert!(path.is_dir());

        let other = tempfile::tempdir()?;
        let ordinary = LibraryLock::try_acquire(other.path())?;
        assert!(LibraryLock::try_acquire_device_space(other.path()).is_err());
        drop(ordinary);
        let scoped = LibraryLock::try_acquire_device_space(other.path())?;
        assert!(scoped.protects(other.path())?);
        Ok(())
    }

    #[test]
    fn legacy_exclusive_transaction_and_runtime_ownership_exclude_each_other() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let legacy = Connection::open(dir.path().join("lock.db"))?;
        legacy.busy_timeout(Duration::ZERO)?;
        legacy.execute_batch("BEGIN EXCLUSIVE")?;
        let blocked = LibraryLock::acquire(dir.path(), Duration::ZERO);
        assert!(matches!(
            blocked,
            Err(error) if error.to_string().contains("memory library still locked")
        ));
        legacy.execute_batch("ROLLBACK")?;

        let runtime = LibraryLock::acquire(dir.path(), Duration::ZERO)?;
        assert!(runtime.protects(dir.path())?);
        assert!(matches!(
            legacy.execute_batch("BEGIN EXCLUSIVE"),
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy
        ));
        drop(runtime);
        legacy.execute_batch("BEGIN EXCLUSIVE; ROLLBACK")?;
        Ok(())
    }

    #[test]
    fn acquire_and_timeout() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let held = LibraryLock::acquire(dir.path(), Duration::from_secs(2))?;
        let err = LibraryLock::acquire(dir.path(), Duration::from_millis(400));
        assert!(err.is_err(), "second locker must time out");
        drop(held);
        let _again = LibraryLock::acquire(dir.path(), Duration::from_secs(2))?;
        Ok(())
    }
}
