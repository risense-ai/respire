use crate::model_progress;
#[cfg(not(test))]
use respire_app::core_sdk::worker::CandidateWorker;
use respire_app::{
    core_sdk::onnx::Engine, diary, transport::local::ModelSwitchArtifact, MemoryEntry, StoredMemory,
};
#[cfg(not(test))]
use respire_spawn::owner_credential::ModelJobRoot;
#[cfg(test)]
mod native_fixture {
    use super::*;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/rpc_serve/parts/shadow_switch_fixture.rs"
    ));
}
#[cfg(test)]
pub(crate) use native_fixture::{
    check_shadow_admission_fixture, legacy_model_fixture, pause_after_checkpoint,
};
#[cfg(test)]
use native_fixture::{CandidateWorker, ModelJobRoot};

pub(crate) fn shadow_target(store: &LocalStore, target: &mut Selection) -> Result<bool> {
    if target.engine != Engine::Cpu
        || target.backend_profile.backend != "onnx"
        || target.backend_profile.native.is_some()
    {
        return Ok(false);
    }
    target.generation = profiles::generation_key_for_profile(target)?;
    let active = match store.active_selection()? {
        Some(active) if !active.generation.is_empty() => active.generation,
        _ => store.active_generation_key()?,
    };
    Ok(target.generation != active)
}

pub(crate) fn switch_store_fence<T>(
    check: &impl Fn() -> Result<()>,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let gate = shared_exclusive();
    loop {
        check()?;
        let identity = SCOPE_EXECUTION
            .read()
            .map_err(|_| anyhow::anyhow!("model switch identity fence poisoned"))?;
        match gate.try_owner_foreground() {
            Ok(Some(_held)) => {
                check()?;
                return action();
            }
            Ok(None) => anyhow::bail!("model switch store fence poisoned"),
            Err(()) => {
                drop(identity);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

struct ShadowServing;

impl ShadowServing {
    fn set(active: bool) -> Result<()> {
        let _admission = PROFILE_ADMISSION
            .lock()
            .map_err(|_| anyhow::anyhow!("model admission lock poisoned"))?;
        SHADOW_SERVING.store(active, Ordering::Release);
        Ok(())
    }

    fn pause_when_background_idle(check: &impl Fn() -> Result<()>) -> Result<()> {
        loop {
            check()?;
            {
                let _admission = PROFILE_ADMISSION
                    .lock()
                    .map_err(|_| anyhow::anyhow!("model admission lock poisoned"))?;
                if !CLASSIFY_RUNNING.load(Ordering::Acquire) && !sync_running() {
                    SHADOW_SERVING.store(false, Ordering::Release);
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ShadowServing {
    fn drop(&mut self) {
        SHADOW_SERVING.store(false, Ordering::Release);
    }
}

pub(crate) fn run_shadow_switch(
    store: &LocalStore,
    task_id: &str,
    target: &Selection,
    check: impl Fn() -> Result<()>,
    decode: impl Fn(&StoredMemory) -> Result<MemoryEntry>,
    progress: impl Fn(&str, u64, Option<u64>) -> Result<()>,
    commit: impl Fn(bool, &mut dyn FnMut() -> Result<()>) -> Result<()>,
) -> Result<Value> {
    let _serving = ShadowServing;
    ShadowServing::set(true)?;
    let check = || {
        check()?;
        model_progress::switch_checkpoint(task_id, None)
    };
    let report = |phase, done, total| {
        model_progress::switch_checkpoint(task_id, Some((phase, &target.model, done, total)))?;
        progress(phase, done, total)
    };
    switch_store_fence(&check, || {
        commit(false, &mut || store.queue_selection(target, task_id))
    })?;
    let database = store.backup_target()?;
    report("switch-load", 0, None)?;
    let root = ModelJobRoot::create(database.parent().context("model switch library missing")?)
        .context("cannot create private candidate directory")?;
    let paths = root
        .root()
        .and_then(|path| root.index_root().map(|index| (path.to_owned(), index)));
    let (data_root, index_root) = match paths {
        Ok(paths) => paths,
        Err(error) => {
            root.remove_empty()?;
            return Err(error.into());
        }
    };
    let mut root = Some(root);
    let mut worker = Some(
        CandidateWorker::start(&data_root, &index_root, target.clone())
            .context("cannot start candidate Core worker")?,
    );
    let result = (|| {
        worker
            .as_mut()
            .context("candidate worker unavailable")?
            .probe(check)?;
        const BATCH: usize = 32;
        const FINAL_BATCH: usize = 16;
        let counts = || store.pending_candidate_counts(&target.generation);
        let fetch = |limit| store.fetch_pending_candidate_batch(&target.generation, limit);
        let diary_pending = || diary::candidate_needs_rebuild(store, &target.generation);
        let decode_scoped = |row: &StoredMemory| {
            let _identity = SCOPE_EXECUTION
                .read()
                .map_err(|_| anyhow::anyhow!("model switch identity fence poisoned"))?;
            check()?;
            decode(row)
        };
        for _ in 0..16 {
            let (indexed, total) = counts()?;
            report("switch-index", indexed as u64, Some(total as u64))?;
            for _ in 0..total.saturating_sub(indexed).div_ceil(BATCH) {
                check()?;
                let rows = fetch(BATCH)?;
                if rows.is_empty() {
                    break;
                }
                let artifacts = prepare_batch(
                    worker.as_mut().context("candidate worker unavailable")?,
                    &rows,
                    &decode_scoped,
                    &check,
                )?;
                switch_store_fence(&check, || {
                    commit(false, &mut || {
                        checkpoint_batch(store, target, task_id, &rows, &artifacts)
                    })
                })?;
                let (done, total) = counts()?;
                report("switch-index", done as u64, Some(total as u64))?;
                #[cfg(test)]
                native_fixture::after_checkpoint(&target.model, &check)?;
            }
            let (indexed, total) = counts()?;
            if total.saturating_sub(indexed) > FINAL_BATCH {
                continue;
            }
            if diary_pending()? {
                let worker = worker.as_mut().context("candidate worker unavailable")?;
                diary::rebuild_candidate_with_progress(
                    store,
                    &target.generation,
                    task_id,
                    decode_scoped,
                    |entry| worker.prepare(entry, check),
                    |action| switch_store_fence(&check, || commit(false, action)),
                    |done| report("switch-diary", done as u64, None),
                )?;
            }
            ShadowServing::pause_when_background_idle(&check)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while incumbent_reads_running() {
                check()?;
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if !incumbent_reads_running() {
                let mut activate = || {
                    profiles::exclusive(|| {
                        check()?;
                        let (indexed, total) = counts()?;
                        if total.saturating_sub(indexed) > FINAL_BATCH || diary_pending()? {
                            return Ok(false);
                        }
                        if indexed != total {
                            let rows = fetch(FINAL_BATCH)?;
                            let artifacts = prepare_batch(
                                worker.as_mut().context("candidate worker unavailable")?,
                                &rows,
                                &decode,
                                &check,
                            )?;
                            commit(false, &mut || {
                                checkpoint_batch(store, target, task_id, &rows, &artifacts)
                            })?;
                        }
                        let (indexed, total) = counts()?;
                        anyhow::ensure!(
                            indexed == total && !diary_pending()?,
                            "candidate index is incomplete"
                        );
                        report("switch-index", indexed as u64, Some(total as u64))?;
                        worker
                            .take()
                            .context("candidate worker unavailable")?
                            .close()?;
                        root.take()
                            .context("candidate directory unavailable")?
                            .remove_empty()?;
                        check()?;
                        commit(true, &mut || {
                            let mut embedder = crate::engine::EMBEDDER_SLOT
                                .lock()
                                .map_err(|_| anyhow::anyhow!("embedder cache poisoned"))?;
                            respire_app::resident::clear()?;
                            *embedder = None;
                            store.activate_shadow_selection(target, task_id)?;
                            profiles::set_active(Some(target.clone()));
                            Ok(())
                        })?;
                        Ok(true)
                    })
                };
                let deadline = Instant::now() + Duration::from_millis(500);
                let activated = loop {
                    check()?;
                    match try_owner_store_fence(&mut activate) {
                        Err(()) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        result => break result,
                    }
                };
                match activated {
                    Ok(Some(Ok(true))) => {
                        return Ok(
                            json!({"selection":target,"active_changed":true,"semantic":"ready"}),
                        )
                    }
                    Ok(Some(Err(error))) => return Err(error),
                    Ok(None) => anyhow::bail!("model switch cutover fence unavailable"),
                    Ok(Some(Ok(false))) | Err(()) => {}
                }
            }
            ShadowServing::set(true)?;
        }
        anyhow::bail!("candidate did not catch up with the serving library; resume the switch")
    })();
    if let Some(worker) = worker {
        worker
            .close()
            .with_context(|| format!("candidate worker cleanup after {result:?}"))?;
        if let Some(root) = root {
            root.remove_empty()
                .with_context(|| format!("candidate directory cleanup after {result:?}"))?;
        }
    }
    result
}

fn prepare_batch(
    worker: &mut CandidateWorker,
    rows: &[StoredMemory],
    decode: &impl Fn(&StoredMemory) -> Result<MemoryEntry>,
    check: &impl Fn() -> Result<()>,
) -> Result<Vec<Vec<u8>>> {
    rows.iter()
        .map(|row| {
            check()?;
            let entry = decode(row)?;
            Ok(worker.prepare(&entry, check)?.artifact)
        })
        .collect()
}

fn checkpoint_batch(
    store: &LocalStore,
    target: &Selection,
    task_id: &str,
    rows: &[StoredMemory],
    artifacts: &[Vec<u8>],
) -> Result<()> {
    let batch = rows
        .iter()
        .zip(artifacts)
        .map(|(row, opaque)| ModelSwitchArtifact {
            id: &row.id,
            ciphertext: &row.ciphertext,
            opaque,
        })
        .collect::<Vec<_>>();
    store.checkpoint_candidate_artifacts(target, task_id, &batch)?;
    Ok(())
}
