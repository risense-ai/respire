use super::*;
use std::path::PathBuf;

pub fn candidate_cache_path(generation: &str) -> Result<PathBuf> {
    Ok(cache_path().with_file_name(candidate_filename(generation)))
}

fn candidate_filename(generation: &str) -> String {
    format!("diary-fragments-{}.db", digest(generation))
}

pub fn candidate_needs_rebuild(store: &LocalStore, generation: &str) -> Result<bool> {
    let path = store
        .backup_target()?
        .with_file_name(candidate_filename(generation));
    let sources = store.active_diary_entries(None, |_| String::new())?;
    let Some(db) = cache_at(&path, true)? else {
        return Ok(!sources.is_empty());
    };
    cache_needs_rebuild(&db, &sources, || Ok(generation.to_owned()))
}

/// Progress counts checked fragments, including ready checkpoints reused on resume.
pub fn rebuild_candidate_with_progress(
    store: &LocalStore,
    generation: &str,
    task_id: &str,
    mut open_entry: impl FnMut(&StoredMemory) -> Result<MemoryEntry>,
    mut prepare_entry: impl FnMut(&MemoryEntry) -> Result<core_sdk::Prepared>,
    mut commit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
    mut progress: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    anyhow::ensure!(
        !generation.is_empty(),
        "candidate diary generation is missing"
    );
    progress(0)?;
    let path = store
        .backup_target()?
        .with_file_name(candidate_filename(generation));
    let sources = store.active_diary_entries(None, |_| String::new())?;
    let mut commit_checkpoint = |action: &mut dyn FnMut() -> Result<()>| {
        commit(&mut || checkpoint(store, task_id, generation, &mut *action))
    };
    let mut db = None;
    commit_checkpoint(&mut || {
        db = Some(writable_cache(&path)?);
        Ok(())
    })?;
    let mut db = db.context("candidate diary cache was not opened")?;
    let mut completed = 0;
    let mut live_sources = HashSet::new();
    for source in sources {
        let current_version = version(&source);
        let entry =
            open_entry(&source).with_context(|| format!("cannot decode diary {}", source.id))?;
        let mut live = HashSet::new();
        for fragment in split(&entry, &current_version) {
            let cached: Option<Vec<u8>> = db.query_row(
                "SELECT artifact FROM diary_artifacts WHERE id=?1 AND source_id=?2 AND generation=?3",
                params![fragment.index_id, source.id, generation], |row| row.get(0),
            ).optional()?;
            let ready = match cached {
                Some(artifact) => core_sdk::index_ready(&[artifact])?,
                None => false,
            };
            if !ready {
                let prepared = prepare_entry(&indexed_entry(&entry, &fragment))?;
                anyhow::ensure!(
                    !prepared.artifact.is_empty(),
                    "candidate diary artifact is empty"
                );
                commit_checkpoint(&mut || {
                    db.execute(
                        "INSERT OR REPLACE INTO diary_artifacts(id,source_id,generation,artifact) VALUES(?1,?2,?3,?4)",
                        params![fragment.index_id, source.id, generation, prepared.artifact],
                    )?;
                    Ok(())
                })?;
            }
            live.insert(fragment.index_id);
            completed += 1;
            progress(completed)?;
        }
        commit_checkpoint(&mut || {
            let tx = db.transaction()?;
            let ids = {
                let mut statement =
                    tx.prepare("SELECT id FROM diary_artifacts WHERE source_id=?1")?;
                let rows = statement.query_map([&source.id], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in ids {
                if !live.contains(&id) {
                    tx.execute("DELETE FROM diary_artifacts WHERE id=?1", [id])?;
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO diary_sources(id,source) VALUES(?1,?2)",
                params![source.id, current_version],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        live_sources.insert(source.id);
    }
    commit_checkpoint(&mut || {
        let tx = db.transaction()?;
        let ids = {
            let mut statement = tx.prepare(
                "SELECT id FROM diary_sources UNION SELECT source_id FROM diary_artifacts",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in ids {
            if !live_sources.contains(&id) {
                tx.execute("DELETE FROM diary_artifacts WHERE source_id=?1", [&id])?;
                tx.execute("DELETE FROM diary_sources WHERE id=?1", [&id])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}

fn checkpoint(
    store: &LocalStore,
    task_id: &str,
    generation: &str,
    action: impl FnOnce() -> Result<()>,
) -> Result<()> {
    store.write_transaction(|| {
        let task: serde_json::Value = serde_json::from_str(
            &store
                .meta_get("embedding_pending_task_v1")?
                .context("model switch task missing")?,
        )?;
        anyhow::ensure!(task["id"] == task_id, "model switch task changed");
        let pending = store
            .pending_selection()?
            .context("model switch target missing")?;
        anyhow::ensure!(
            pending.generation == generation,
            "model switch target changed"
        );
        let active_generation = match store.active_selection()? {
            Some(active) if !active.generation.is_empty() => active.generation,
            _ => store.active_generation_key()?,
        };
        anyhow::ensure!(
            active_generation != generation,
            "candidate diary cache is active"
        );
        action()
    })
}

#[cfg(test)]
#[path = "diary_candidate_tests.rs"]
mod tests;
