use super::*;
use std::{fs, sync::mpsc::sync_channel, thread};

fn stage_names(profile: &Path) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(profile)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".backup-export-") {
            matches.push(entry.path());
        }
    }
    Ok(matches)
}

#[cfg(unix)]
fn fixture() -> Result<(tempfile::TempDir, LocalStore)> {
    let dir = tempfile::tempdir()?;
    let profile = dir.path().canonicalize()?;
    let store = LocalStore::open(&profile.join("test.db"))?;
    store.connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         INSERT INTO meta(key,value) VALUES ('fixture-key','admitted-value');",
    )?;
    Ok((dir, store))
}

#[cfg(unix)]
#[test]
fn real_concurrent_wal_write_does_not_change_exported_snapshot() -> Result<()> {
    let (dir, store) = fixture()?;
    let source = store.prepare_backup_export()?;
    let (ready, snapshot_admitted) = sync_channel(0);
    let (resume, copied) = sync_channel(0);
    let worker = thread::spawn(move || {
        let mut checks = 0;
        source.copy(|| {
            checks += 1;
            if checks == 2 {
                ready.send(())?;
                copied.recv_timeout(Duration::from_secs(5))?;
            }
            Ok(())
        })
    });
    snapshot_admitted.recv_timeout(Duration::from_secs(5))?;
    let writer = Connection::open(dir.path().join("test.db"))?;
    writer.execute("UPDATE meta SET value='later-value' WHERE key='fixture-key'", [])?;
    resume.send(())?;
    let copy = worker.join().map_err(|_| anyhow::anyhow!("backup worker failed"))??;
    let path = copy.checked_path()?;
    assert_eq!(stage_names(dir.path())?, [path.clone()]);
    let exported = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let value: String = exported.query_row("SELECT value FROM meta WHERE key='fixture-key'", [], |r| r.get(0))?;
    assert_eq!(value, "admitted-value");
    drop(exported);
    drop(copy);
    assert!(stage_names(dir.path())?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn revocation_after_capture_and_small_budget_remove_only_owned_stage() -> Result<()> {
    let (dir, store) = fixture()?;
    let unrelated = dir.path().join("unrelated.db");
    fs::write(&unrelated, b"fixture contents")?;
    let source = store.prepare_backup_export()?;
    assert!(source.copy_with_limit(1, || Ok(())).is_err());
    assert!(stage_names(dir.path())?.is_empty());
    let source = store.prepare_backup_export()?;
    let mut checks = 0;
    let error = source.copy(|| {
        checks += 1;
        ensure!(checks < 2, "fixture bearer revoked");
        Ok(())
    }).err().context("revoked copy unexpectedly completed")?;
    assert_eq!(error.to_string(), "fixture bearer revoked");
    assert!(stage_names(dir.path())?.is_empty());
    assert_eq!(fs::read(unrelated)?, b"fixture contents");
    Ok(())
}

#[cfg(unix)]
#[test]
fn moved_space_no_longer_releases_its_retained_stage_path() -> Result<()> {
    let parent = tempfile::tempdir()?;
    let profile = parent.path().canonicalize()?.join("child");
    fs::create_dir(&profile)?;
    let store = LocalStore::open(&profile.join("test.db"))?;
    let copy = store.prepare_backup_export()?.copy(|| Ok(()))?;
    assert_eq!(stage_names(&profile)?.len(), 1);
    let retired = parent.path().join("retired");
    fs::rename(&profile, &retired)?;
    assert!(copy.checked_path().is_err());
    drop(copy);
    assert!(stage_names(&retired)?.is_empty());
    assert!(retired.join("test.db").is_file());
    Ok(())
}
