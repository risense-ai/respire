use super::*;
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

impl PendingSwitch {
    pub(super) fn prepare(
        &mut self,
        database: &Path,
        epoch: usize,
        stop: &AtomicBool,
    ) -> Result<serde_json::Value> {
        let unavailable = |_| anyhow::anyhow!("operation state unavailable");
        anyhow::ensure!(
            self.job.start().map_err(unavailable)?,
            model_progress::OperationStopped::Cancelled
        );
        self.reservation.enter();
        let scope = self.job.scope();
        let entry = self.job.entry();
        let check = || {
            anyhow::ensure!(
                !stop.load(Ordering::Acquire) && !entry.cancelled().map_err(unavailable)?,
                model_progress::OperationStopped::Cancelled
            );
            rpc_worker::check_sync_context(epoch)?;
            check_mode()?;
            anyhow::ensure!(
                scope.bearer_is_current(),
                "local bearer changed during model switch"
            );
            model_progress::check()
        };
        let progress = |phase: &str, done: u64, total: Option<u64>| {
            let percent = match phase {
                "switch-verify" => 5,
                "switch-load" => 15,
                "switch-index" => {
                    20 + (done.saturating_mul(59) / total.unwrap_or(1).max(1)).min(59) as u8
                }
                "switch-diary" => 80,
                _ => return Ok(()),
            };
            self.job.progress(percent).map_err(unavailable)
        };
        let commit = |final_commit, action: &mut dyn FnMut() -> Result<()>| {
            check()?;
            with_authority(scope, |_| {
                self.job.model_commit(final_commit, || {
                    if final_commit {
                        model_progress::commit_final(action)
                    } else {
                        model_progress::commit(action)
                    }
                })
            })
        };
        let store = rpc_serve::switch_store_fence(&check, || {
            with_authority(scope, |_| LocalStore::open_existing(database))
        })?;
        let target = &mut self.target;
        model_progress::switch_checkpoint(
            &self.task_id,
            Some(("switch-verify", &target.model, 0, None)),
        )?;
        progress("switch-verify", 0, None)?;
        target.validate()?;
        if rpc_serve::shadow_target(&store, target)? {
            rpc_serve::run_shadow_switch(
                &store,
                &self.task_id,
                target,
                check,
                |row| with_authority(scope, |library| library.open_model_switch_row(row)),
                progress,
                commit,
            )
        } else {
            rpc_serve::drain_model_switch_requests(&check)?;
            rpc_serve::switch_store_fence(&check, || {
                let vault = with_authority(scope, |library| Ok(library.model_switch_vault()))?;
                commit(false, &mut || store.queue_selection(target, &self.task_id))?;
                rpc_serve::run_model_switch(
                    &store,
                    &vault,
                    &self.task_id,
                    target,
                    check,
                    progress,
                    commit,
                )
            })
        }
    }
}
