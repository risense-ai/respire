use respire_app::{
    auth::ModelSwitchVault,
    core_sdk::profiles::{self, Selection},
    memory::bge::BgeEmbedder,
    transport::local::LocalStore,
};

pub(crate) struct ModelSwitchReservation {
    operation: crate::model_progress::Operation,
    _admission: AdmissionGuard,
}

pub(crate) fn model_switch_running() -> bool {
    PROFILE_SWITCH_RUNNING.load(Ordering::Acquire)
}

impl Drop for ModelSwitchReservation {
    fn drop(&mut self) {
        PROFILE_SWITCH_RUNNING.store(false, Ordering::Release);
    }
}

impl ModelSwitchReservation {
    /// The serving dispatcher already holds the foreground store fence.
    pub(crate) fn reserve(task_id: String) -> Result<Option<Self>> {
        let admission = begin_admission(task_id.clone(), "model_switch")
            .context("runtime is draining and rejected new business")?;
        let _gate = PROFILE_ADMISSION
            .lock()
            .map_err(|_| anyhow::anyhow!("model admission lock poisoned"))?;
        if model_switch_running()
            || RUNNING.load(Ordering::Acquire) != 0
            || CLASSIFY_RUNNING.load(Ordering::Acquire)
            || WRITE_RUNNING.load(Ordering::Acquire)
            || sync_running()
        {
            return Ok(None);
        }
        let Some(operation) = crate::model_progress::Operation::try_reserve_task(task_id)? else {
            return Ok(None);
        };
        PROFILE_SWITCH_RUNNING.store(true, Ordering::Release);
        Ok(Some(Self {
            operation,
            _admission: admission,
        }))
    }

    pub(crate) fn enter(&self) {
        self.operation.enter_thread();
    }
}

pub(crate) fn run_model_switch(
    store: &LocalStore,
    vault: &ModelSwitchVault,
    task_id: &str,
    target: &mut Selection,
    check: impl Fn() -> Result<()>,
    progress: impl Fn(&str, u64, Option<u64>) -> Result<()>,
    commit: impl Fn(bool, &mut dyn FnMut() -> Result<()>) -> Result<()>,
) -> Result<Value> {
    use crate::model_progress;

    let checkpoint = || {
        check()?;
        model_progress::check()
    };
    let phase = |name| {
        store.meta_set(
            "embedding_pending_task_v1",
            &serde_json::to_string(&json!({"id": task_id, "phase": name}))?,
        )
    };
    let report = |name, model: &str, done, total| {
        model_progress::update(name, model, done, total)?;
        progress(name, done, total)
    };
    let previous = store.active_selection()?;
    let source_revision = store
        .meta_get("memory_revision_v1")?
        .context("source revision unavailable")?;
    phase("prepare")?;
    checkpoint()?;
    report("switch-verify", &target.model, 0, None)?;
    crate::model_assets::verify_selection_assets(target)?;
    checkpoint()?;
    target.generation = profiles::generation_key_for_profile(target)?;
    store.meta_set(
        "embedding_pending_selection_v1",
        &serde_json::to_string(target)?,
    )?;
    profiles::exclusive(|| {
        checkpoint()?;
        let attempt = profiles::scoped(target.clone(), || {
            phase("load")?;
            report("switch-load", &target.model, 0, None)?;
            profiles::load(target)?;
            checkpoint()?;
            let embedder = BgeEmbedder::load_model(&target.model)?;
            phase("main-index")?;
            vault.build_main(store, &embedder, target, |done, total| {
                check()?;
                report(
                    "switch-index",
                    &target.model,
                    done as u64,
                    Some(total as u64),
                )
            })?;
            phase("diary-index")?;
            vault.build_diary(
                store,
                &embedder,
                checkpoint,
                |action| commit(false, action),
                |done| report("switch-diary", &target.model, done as u64, None),
            )?;
            checkpoint()?;
            anyhow::ensure!(
                !store.index_pending(&target.model)? && !respire_app::diary::needs_rebuild(store)?,
                "candidate index/source validation failed"
            );
            respire_app::resident::clear()?;
            *crate::engine::EMBEDDER_SLOT
                .lock()
                .map_err(|_| anyhow::anyhow!("embedder cache poisoned"))? = None;
            commit(true, &mut || {
                store.activate_selection(target, &source_revision)?;
                profiles::set_active(Some(target.clone()));
                Ok(())
            })?;
            Ok(())
        });
        if let Err(error) = attempt {
            let restored = match previous {
                Some(ref selection) => profiles::load(selection).map(|_| ()),
                None => {
                    profiles::set_active(None);
                    respire_app::memory::onnx::reset_sessions().and_then(|_| {
                        BgeEmbedder::load_model("m3")?
                            .probe("Respire rollback verification")
                            .map(|_| ())
                    })
                }
            };
            return match restored {
                Ok(()) => Err(anyhow::anyhow!("model switch failed: {error:#}; previous selection restored and trial inference passed")),
                Err(recovery) => Err(anyhow::anyhow!("recovery_failed: model switch failed: {error:#}; previous model reload failed: {recovery:#}; semantic availability is unverified")),
            };
        }
        Ok(json!({"selection":target,"active_changed":true,"semantic":"ready"}))
    })
}

pub(crate) fn record_switch_result(task_id: &str, result: &Result<Value>) -> Value {
    let state = match result {
        Ok(result) => {
            json!({"id":task_id,"state":"ready","active_changed":true,"semantic":"ready","result":result})
        }
        Err(error) => {
            let error = format!("{error:#}");
            let semantic = if error.contains("recovery_failed") {
                "unverified"
            } else if error.contains("previous selection restored and trial inference passed") {
                "previous_ready"
            } else if profile_state()["semantic"] == "unverified" {
                "unverified"
            } else {
                "unchanged"
            };
            json!({"id":task_id,"state":"failed","active_changed":false,"error":error,"semantic":semantic})
        }
    };
    set_profile_state(state.clone());
    state
}
