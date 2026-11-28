use super::*;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

pub(in crate::runtime_http::v1) struct RunningSwitch {
    worker: Option<JoinHandle<(PendingSwitch, Result<serde_json::Value>)>>,
    stop: Arc<AtomicBool>,
    database: PathBuf,
    task_id: String,
}

impl PendingSwitch {
    pub(in crate::runtime_http::v1) fn launch(
        mut self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
    ) -> Option<RunningSwitch> {
        let database = (|| {
            let profile = &self.job.scope().canonical_profile;
            anyhow::ensure!(
                ownership.protects(profile)?,
                "model switch ownership changed"
            );
            let path = store
                .context("serving store unavailable")?
                .backup_target()?;
            anyhow::ensure!(path.parent() == Some(profile), "serving store changed");
            Ok(path)
        })();
        let database = match database {
            Ok(database) => database,
            Err(error) => {
                self.finish(Err(error));
                return None;
            }
        };
        let task_id = self.task_id.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_path = database.clone();
        let epoch = rpc_worker::sync_generation();
        match std::thread::Builder::new()
            .name("respire-model-candidate".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let result = self.prepare(&worker_path, epoch, &worker_stop);
                (self, result)
            }) {
            Ok(worker) => Some(RunningSwitch {
                worker: Some(worker),
                stop,
                database,
                task_id,
            }),
            Err(error) => {
                rpc_serve::record_switch_result(&task_id, Err(&error.into()));
                None
            }
        }
    }

    fn finish(self, result: Result<serde_json::Value>) {
        if result.is_ok() {
            respire_app::core_sdk::profiles::set_active(Some(self.target.clone()));
        }
        let profile = rpc_serve::record_switch_result(&self.task_id, result.as_ref());
        let phase = match &result {
            Ok(_) => Phase::Completed,
            Err(error)
                if profile["semantic"] != "unverified"
                    && (error.is::<model_progress::OperationStopped>()
                        && self
                            .job
                            .entry()
                            .cancelled()
                            .is_ok_and(|cancelled| cancelled)
                        || model_progress::control(&self.task_id, false)
                            .is_ok_and(|progress| progress["cancelled"] == true)) =>
            {
                Phase::Cancelled
            }
            Err(_) => Phase::Failed,
        };
        if let Err(error) = result {
            eprintln!("command=model_switch status=fail mode=rest error={error:#}");
        }
        drop(self.reservation);
        self.job.finish(phase);
        rpc_worker::kick_index();
    }
}

impl RunningSwitch {
    pub(in crate::runtime_http::v1) fn tick(
        &mut self,
        store: Option<&LocalStore>,
        ownership: &LibraryLock,
    ) -> bool {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok((pending, result)) => {
                    let result = result.or_else(|error| {
                        cleanup_pending(
                            store,
                            ownership,
                            &pending.job.scope().canonical_profile,
                            &self.task_id,
                        )
                        .with_context(|| format!("{error:#}; pending cleanup failed"))?;
                        Err(error)
                    });
                    pending.finish(result);
                }
                Err(_) => {
                    let result = self
                        .cleanup()
                        .and_then(|()| anyhow::bail!("candidate thread panicked"));
                    rpc_serve::record_switch_result(&self.task_id, result.as_ref());
                }
            }
        }
        true
    }

    fn cleanup(&self) -> Result<()> {
        LocalStore::open_existing(&self.database)?.clear_pending_selection(Some(&self.task_id))
    }
}

impl Drop for RunningSwitch {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stop.store(true, Ordering::Release);
            let joined = worker.join();
            if joined.as_ref().is_err() || joined.as_ref().is_ok_and(|(_, result)| result.is_err())
            {
                if let Err(error) = self.cleanup() {
                    eprintln!("model switch shutdown cleanup failed: {error:#}");
                }
            }
            if let Ok((pending, result)) = joined {
                pending.finish(result);
            }
        }
    }
}
