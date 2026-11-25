use super::*;
use crate::{profiles::Selection, Prepared};
use respire_protocol::MemoryEntry;
use std::path::{Path, PathBuf};

static CANDIDATE: Mutex<Option<Arc<Process>>> = Mutex::new(None);

/// A single-use, local CPU-only model lane. It receives unlocked text for the
/// typed prepare operation, never a Vault, database, or transport callback.
pub struct CandidateWorker {
    process: Arc<Process>,
    index_root: PathBuf,
    selection: Selection,
}

impl CandidateWorker {
    /// The caller creates and exclusively owns both job-local directories.
    pub fn start(data_root: &Path, index_root: &Path, selection: Selection) -> Result<Self> {
        Self::start_inner(data_root, index_root, selection, None)
    }

    fn start_inner(
        data_root: &Path,
        index_root: &Path,
        selection: Selection,
        fixture: Option<&Path>,
    ) -> Result<Self> {
        anyhow::ensure!(
            selection.engine == crate::onnx::Engine::Cpu
                && selection.backend_profile.backend == "onnx"
                && selection.backend_profile.native.is_none(),
            "candidate worker supports local CPU model selection only"
        );
        let data_root = data_root.canonicalize()?;
        let index_root = index_root.canonicalize()?;
        anyhow::ensure!(
            index_root != data_root && index_root.starts_with(&data_root),
            "candidate index directory is outside its private job root"
        );
        let mut slot = CANDIDATE
            .lock()
            .map_err(|_| anyhow::anyhow!("candidate worker state poisoned"))?;
        anyhow::ensure!(!STOPPING.load(Ordering::Acquire), "Core is shutting down");
        if let Some(previous) = slot.as_ref() {
            anyhow::ensure!(
                previous.retired.load(Ordering::Acquire),
                "a candidate worker is already active"
            );
            previous.retire("previous candidate reap retry")?;
        }
        let process = start_process(&mut slot, Some(&data_root), fixture)?;
        Ok(Self {
            process,
            index_root,
            selection,
        })
    }

    /// Reap the owned child before disposing of its index tree.
    pub fn close(self) -> Result<()> {
        self.process.retire("candidate lane closed")
    }

    pub fn probe(&mut self, check: impl FnMut() -> Result<()>) -> Result<Value> {
        self.call(
            "model_probe",
            json!({
                "model":self.selection.model, "text":"Respire candidate load verification"
            }),
            check,
        )
    }

    pub fn prepare(
        &mut self,
        entry: &MemoryEntry,
        check: impl FnMut() -> Result<()>,
    ) -> Result<Prepared> {
        let prepared = self.call(
            "candidate_prepare_v1",
            json!({"model":self.selection.model, "entry":entry}),
            check,
        )?;
        let prepared: Prepared = serde_json::from_value(prepared)?;
        anyhow::ensure!(
            !prepared.artifact.is_empty() && !prepared.artifact.starts_with(b"rsi1:"),
            "candidate did not return raw opaque artifact bytes"
        );
        Ok(prepared)
    }

    fn call(
        &mut self,
        operation: &str,
        payload: Value,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Value> {
        check()?;
        let (sender, receiver) = mpsc::channel();
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        self.process
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("candidate IPC state poisoned"))?
            .insert(
                id,
                Pending {
                    sender,
                    started: Instant::now(),
                },
            );
        let result = self.process.send(&RequestFrame {
            id,
            operation,
            payload: &payload,
            index_root: Some(self.index_root.clone()),
            transport: false,
            selection: Some(self.selection.clone()),
        });
        let receive = || -> Result<Value> {
            result?;
            loop {
                check()?;
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(mut frame) => {
                        check()?;
                        anyhow::ensure!(
                            !self.process.retired.load(Ordering::Acquire)
                                && frame.get("callback").is_none(),
                            "candidate worker response is no longer admissible"
                        );
                        if let Some(error) = frame["error"].as_str() {
                            bail!("{error}");
                        }
                        return frame
                            .get_mut("result")
                            .map(Value::take)
                            .context("candidate worker result missing");
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        bail!("candidate worker disconnected")
                    }
                }
            }
        };
        let outcome = receive();
        if outcome.is_err() {
            self.process.retire("candidate call failed or cancelled")?;
        }
        outcome
    }
}

impl Drop for CandidateWorker {
    fn drop(&mut self) {
        if let Err(error) = self.process.retire("candidate lane closed") {
            eprintln!("candidate Core cleanup incomplete: {error}");
        }
    }
}

pub(super) fn shutdown() -> Result<()> {
    let slot = CANDIDATE
        .lock()
        .map_err(|_| anyhow::anyhow!("candidate worker state poisoned"))?;
    if let Some(worker) = slot.as_ref() {
        worker.retire("host shutdown")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
