use super::{
    prepare::{self, Artifact, Input, Prepared, Stage},
    *,
};
use respire_app::{
    core_sdk::profiles,
    transport::local::{
        ModelSwitchArtifact, MODEL_SWITCH_MAX_PROMOTION_BYTES, MODEL_SWITCH_MAX_PROMOTION_ROWS,
        MODEL_SWITCH_MAX_PROMOTION_ROW_BYTES,
    },
};
use respire_spawn::owner_credential::ModelJobRoot;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

pub(in crate::runtime_http::v1) struct RunningSwitch {
    pending: Option<PendingSwitch>,
    worker: Option<JoinHandle<Result<Prepared>>>,
    prepared: Option<Prepared>,
    promotion: Vec<Artifact>,
    remaining: usize,
    stop: Arc<AtomicBool>,
    epoch: usize,
}

impl PendingSwitch {
    pub(in crate::runtime_http::v1) fn launch(
        mut self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
    ) -> Option<RunningSwitch> {
        let Some(store) = store else {
            self.run(store, ownership);
            return None;
        };
        let scope = self.job.scope().clone();
        let outcome = rpc_serve::with_owner_store_fence(|| -> Result<Start> {
            with_authority(&scope, |_| {
                anyhow::ensure!(
                    scope.bearer_is_current() && ownership.protects(&scope.canonical_profile)?,
                    "model switch owner changed"
                );
                // Detect the established diary path before calling any SDK
                // generation calculation: it must keep its original prepare
                // and cancellation ordering even under an exclusive Core gate.
                if store.has_live_switch_diary()? {
                    return Ok(Start::Established);
                }
                let generation = profiles::generation_key_for_profile(&self.target)?;
                if store.active_generation_key()? == generation
                    || respire_app::diary::has_candidate_diary_cache(&generation)?
                {
                    return Ok(Start::Established);
                }
                let stage = match ModelJobRoot::create(&scope.canonical_profile) {
                    Ok(stage) => Stage::new(stage),
                    // Older valid owner layouts may not support private
                    // staging; the established safe path never uses it.
                    Err(_) => return Ok(Start::Established),
                };
                if !self
                    .job
                    .start()
                    .map_err(|_| anyhow::anyhow!("model operation unavailable"))?
                {
                    return Ok(Start::Cancelled);
                }

                self.target.generation = generation;
                self.job.model_commit(false, || {
                    model_progress::in_model_operation(&self.task_id, || {
                        model_progress::commit(|| {
                            store.queue_selection(&self.target, &self.task_id)
                        })
                    })
                })?;
                let source = store.begin_model_switch_read()?;
                let epoch = rpc_worker::sync_generation();
                let stop = Arc::new(AtomicBool::new(false));
                let input = Input {
                    scope: scope.clone(),
                    source,
                    stage,
                    epoch,
                    stop: Arc::clone(&stop),
                    entry: self.job.entry(),
                    task_id: self.task_id.clone(),
                    target: self.target.clone(),
                };
                let worker = std::thread::Builder::new()
                    .name("respire-model-candidate".into())
                    .stack_size(16 * 1024 * 1024)
                    .spawn(move || prepare::work(input))?;
                self.reservation.keep_incumbent_recall_live(true);
                Ok(Start::Detached(worker, stop, epoch))
            })
        });
        match outcome {
            Some(Ok(Start::Established)) => {
                self.run(Some(store), ownership);
                None
            }
            Some(Ok(Start::Detached(worker, stop, epoch))) => Some(RunningSwitch {
                pending: Some(self),
                worker: Some(worker),
                prepared: None,
                stop,
                epoch,
                promotion: Vec::new(),
                remaining: MODEL_SWITCH_MAX_PROMOTION_BYTES,
            }),
            Some(Ok(Start::Cancelled)) => {
                self.job.finish(Phase::Cancelled);
                None
            }
            Some(Err(error)) => {
                report_failure(&self, Some(store), ownership, error);
                None
            }
            None => {
                self.job.finish(Phase::Failed);
                None
            }
        }
    }
}

enum Start {
    Established,
    Cancelled,
    Detached(JoinHandle<Result<Prepared>>, Arc<AtomicBool>, usize),
}

impl RunningSwitch {
    /// One bounded publication batch per owner dispatcher turn; poll native
    /// preparation without retaining either the store fence or Vault lock.
    pub(in crate::runtime_http::v1) fn tick(
        &mut self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
    ) -> bool {
        if self.worker.as_ref().is_some_and(|job| !job.is_finished()) {
            return false;
        }
        let result = self.advance(store, ownership);
        match result {
            Ok(false) => false,
            Ok(true) => {
                let Some(pending) = self.pending.as_ref() else {
                    return true;
                };
                let result = Ok(serde_json::json!({
                    "selection":pending.target,"active_changed":true,"semantic":"ready"
                }));
                rpc_serve::record_switch_result(&pending.task_id, &result);
                pending.job.finish(Phase::Completed);
                true
            }
            Err(error) if error.is::<prepare::SpoolLimit>() => {
                self.prepared = None;
                self.promotion.clear();
                if let Some(pending) = self.pending.take() {
                    pending.reservation.keep_incumbent_recall_live(false);
                    pending.run_established(store, ownership, true);
                }
                true
            }
            Err(error) => {
                if let Some(pending) = self.pending.as_ref() {
                    report_failure(pending, store, ownership, error);
                }
                true
            }
        }
    }

    fn advance(&mut self, store: Option<&LocalStore>, ownership: &LibraryLock) -> Result<bool> {
        if let Some(worker) = self.worker.take() {
            let staged = worker
                .join()
                .map_err(|_| anyhow::anyhow!("isolated model job failed"))??;
            self.prepared = Some(staged);
        }
        let store = store.context("serving model store unavailable")?;
        let pending = self
            .pending
            .as_ref()
            .context("model switch reservation unavailable")?;
        let scope = pending.job.scope();
        let prepared = self
            .prepared
            .as_mut()
            .context("model preparation result unavailable")?;
        let entry = pending.job.entry();
        prepare::authorize(scope, self.epoch, &entry, &pending.task_id)?;
        // Move at most two privately prepared records per dispatch turn.
        // No durable candidate artifact or database write exists at this point.
        for _ in 0..2 {
            let Some(artifact) = prepare::read_frame::<Artifact>(
                &mut prepared.artifacts,
                4 * MODEL_SWITCH_MAX_PROMOTION_ROW_BYTES,
            )?
            else {
                break;
            };
            if self.promotion.len() >= MODEL_SWITCH_MAX_PROMOTION_ROWS {
                return Err(prepare::SpoolLimit.into());
            }
            let bytes = artifact
                .id
                .len()
                .saturating_add(artifact.ciphertext.len())
                .saturating_add(artifact.opaque.len());
            if bytes > MODEL_SWITCH_MAX_PROMOTION_ROW_BYTES {
                return Err(prepare::SpoolLimit.into());
            }
            self.remaining = self
                .remaining
                .checked_sub(bytes)
                .ok_or(prepare::SpoolLimit)?;
            self.promotion.push(artifact);
        }
        use std::io::BufRead;
        if !prepared.artifacts.fill_buf()?.is_empty() {
            return Ok(false);
        }
        let rows = self
            .promotion
            .iter()
            .map(|item| ModelSwitchArtifact {
                id: &item.id,
                ciphertext: &item.ciphertext,
                opaque: &item.opaque,
            })
            .collect::<Vec<_>>();
        let target = &pending.target;
        let result = rpc_serve::with_owner_store_fence(|| {
            with_authority(scope, |_| {
                anyhow::ensure!(
                    ownership.protects(&scope.canonical_profile)?,
                    "serving model store changed"
                );
                prepare::authorize(scope, self.epoch, &entry, &pending.task_id)?;
                pending.job.model_commit(true, || {
                    model_progress::in_model_operation(&pending.task_id, || {
                        model_progress::commit_final(|| {
                            store.validate_model_switch_stage(target, &prepared.source)?;
                            anyhow::ensure!(
                                !store.has_live_switch_diary()?
                                    && !respire_app::diary::has_candidate_diary_cache(
                                        &target.generation
                                    )?,
                                "candidate diary state changed during detached preparation"
                            );
                            let mut cached = crate::engine::EMBEDDER_SLOT
                                .lock()
                                .map_err(|_| anyhow::anyhow!("embedder cache unavailable"))?;
                            respire_app::resident::clear()?;
                            store.publish_model_switch_snapshot(target, &prepared.source, &rows)?;
                            *cached = None;
                            profiles::set_active(Some(target.clone()));
                            Ok(())
                        })
                    })
                })
            })
        });
        if let Some(result) = result {
            result?;
            return Ok(true);
        }
        Ok(false)
    }
}

impl Drop for RunningSwitch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Bound native calls cooperatively; the worker reaps its actual
            // child before its trusted empty-stage cleanup. The resident
            // owner store and original LibraryLock still outlive this drop.
            let _ = worker.join();
        }
        rpc_worker::kick_index();
    }
}

fn report_failure(pending: &PendingSwitch, store: Option<&LocalStore>, ownership: &LibraryLock, error: anyhow::Error) {
    let cleanup = cleanup_pending(store, ownership, &pending.job.scope().canonical_profile, &pending.task_id);
    let error = match cleanup {
        Ok(()) => error,
        Err(cleanup) => anyhow::anyhow!("{error:#}; pending cleanup failed: {cleanup:#}"),
    };
    let cancelled = pending.job.entry().cancelled().unwrap_or(false)
        || model_progress::control(&pending.task_id, false).is_ok_and(|p| p["cancelled"] == true);
    let result = Err(error);
    rpc_serve::record_switch_result(&pending.task_id, &result);
    pending.job.finish(if cancelled {
        Phase::Cancelled
    } else {
        Phase::Failed
    });
}
