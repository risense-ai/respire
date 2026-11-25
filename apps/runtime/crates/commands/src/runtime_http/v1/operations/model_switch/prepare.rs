use super::*;
use respire_app::{
    core_sdk::worker::CandidateWorker,
    transport::local::{ModelSwitchRead, ModelSwitchSourceStamp, ModelSwitchSourceTooLarge},
};
use respire_spawn::owner_credential::ModelJobRoot;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

const MAX_FRAME: usize = 64 * 1024 * 1024;
const MAX_JOB_SPOOL: usize = 512 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct SpoolLimit;
impl std::fmt::Display for SpoolLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private model switch staging exceeds bounded spool budget")
    }
}
impl std::error::Error for SpoolLimit {}

#[derive(Serialize, Deserialize)]
struct Source {
    stored: respire_app::StoredMemory,
    parent: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Artifact {
    pub(super) id: String,
    pub(super) ciphertext: String,
    pub(super) opaque: Vec<u8>,
}

pub(super) struct Stage {
    root: Option<ModelJobRoot>,
    safe_to_retire: bool,
}

impl Stage {
    pub(super) fn new(root: ModelJobRoot) -> Self {
        Self {
            root: Some(root),
            safe_to_retire: true,
        }
    }

    fn root(&mut self) -> Result<&mut ModelJobRoot> {
        self.root
            .as_mut()
            .context("private model job root unavailable")
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.safe_to_retire {
            if let Some(root) = self.root.take() {
                if root.remove_empty().is_err() {
                    eprintln!("private model job retained for safe cleanup");
                }
            }
        }
    }
}

pub(super) struct Prepared {
    pub(super) artifacts: BufReader<File>,
    pub(super) source: ModelSwitchSourceStamp,
    _stage: Stage,
}

pub(super) struct Input {
    pub(super) scope: RestScope,
    pub(super) entry: Arc<state::Entry>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) epoch: usize,
    pub(super) task_id: String,
    pub(super) target: Selection,
    pub(super) source: ModelSwitchRead,
    pub(super) stage: Stage,
}

pub(super) fn authorize(
    scope: &RestScope,
    epoch: usize,
    entry: &state::Entry,
    task_id: &str,
) -> Result<()> {
    rpc_worker::check_sync_context(epoch)?;
    check_mode()?;
    anyhow::ensure!(
        scope.bearer_is_current(),
        "local bearer changed during model switch"
    );
    anyhow::ensure!(
        !entry
            .cancelled()
            .map_err(|_| anyhow::anyhow!("operation state unavailable"))?,
        model_progress::OperationStopped::Cancelled
    );
    model_progress::switch_checkpoint(task_id, None)
}

pub(super) fn work(input: Input) -> Result<Prepared> {
    let Input {
        scope,
        entry,
        stop,
        epoch,
        task_id,
        target,
        source,
        mut stage,
    } = input;
    let check = || {
        anyhow::ensure!(
            !stop.load(Ordering::Acquire),
            model_progress::OperationStopped::Cancelled
        );
        authorize(&scope, epoch, &entry, &task_id)
    };
    check()?;
    let mut sealed = BufWriter::new(stage.root()?.create_source_spool()?);
    let mut total = 0u64;
    let mut remaining = MAX_JOB_SPOOL;
    let source = source
        .stage(check, |mut stored, _| {
            total += 1;
            let parent = std::mem::take(&mut stored.local_parent_id);
            write_frame(&mut sealed, &Source { stored, parent }, &mut remaining)
        })
        .map_err(|error| {
            if error.is::<ModelSwitchSourceTooLarge>() {
                SpoolLimit.into()
            } else {
                error
            }
        })?;
    let mut sealed = sealed.into_inner()?;
    sealed.rewind()?;
    let mut sealed = BufReader::new(sealed);
    let mut prepared = BufWriter::new(stage.root()?.create_prepared_spool()?);
    model_progress::switch_checkpoint(&task_id, Some(("switch-verify", &target.model, 0)))?;
    let progress = |percent| {
        entry
            .progress(percent)
            .map_err(|_| anyhow::anyhow!("operation state unavailable"))
    };
    progress(5)?;
    crate::model_assets::verify_selection_assets(&target)?;
    check()?;
    let root = stage.root()?.root()?.to_owned();
    let index = stage.root()?.index_root()?;
    // The owned process must be reaped before disposing its staging names.
    // An unconfirmed spawn/cleanup leaves the fixed private slot quarantined.
    stage.safe_to_retire = false;
    let mut worker = CandidateWorker::start(&root, &index, target.clone())?;
    let attempt = (|| {
        model_progress::switch_checkpoint(&task_id, Some(("switch-load", &target.model, 0)))?;
        progress(15)?;
        worker.probe(check)?;
        let mut indexed = 0u64;
        while let Some(mut source) = read_frame::<Source>(&mut sealed, MAX_FRAME)? {
            check()?;
            source.stored.local_parent_id = source.parent;
            let decoded = rpc_serve::with_verified_owner_library_scope(
                |owner, profile, instance, generation, vault, library| -> Result<_> {
                    anyhow::ensure!(
                        scope.matches(owner, profile, instance, generation, vault)
                            && scope.bearer_is_current(),
                        "model switch owner changed"
                    );
                    library.open_model_switch_row(&source.stored)
                },
            )
            .context("verified model switch owner unavailable")??;
            let opaque = worker.prepare(&decoded, check)?.artifact;
            write_frame(
                &mut prepared,
                &Artifact {
                    id: source.stored.id,
                    ciphertext: source.stored.ciphertext,
                    opaque,
                },
                &mut remaining,
            )?;
            indexed += 1;
            progress(20 + (indexed.saturating_mul(59) / total.max(1)).min(59) as u8)?;
            model_progress::switch_checkpoint(
                &task_id,
                Some(("switch-index", &target.model, indexed)),
            )?;
        }
        check()?;
        Ok::<(), anyhow::Error>(())
    })();
    worker.close()?;
    stage.safe_to_retire = true;
    attempt?;
    let mut prepared = prepared.into_inner()?;
    prepared.sync_all()?;
    prepared.rewind()?;
    Ok(Prepared {
        artifacts: BufReader::new(prepared),
        source,
        _stage: stage,
    })
}

struct BoundedFrame(Vec<u8>);
impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME.saturating_sub(self.0.len()) {
            return Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_frame(mut sink: impl Write, value: &impl Serialize, remaining: &mut usize) -> Result<()> {
    let mut encoded = BoundedFrame(Vec::new());
    serde_json::to_writer(&mut encoded, value).map_err(|error| {
        if error.io_error_kind() == Some(std::io::ErrorKind::FileTooLarge) {
            SpoolLimit.into()
        } else {
            anyhow::Error::from(error)
        }
    })?;
    anyhow::ensure!(!encoded.0.is_empty(), "model switch spool record missing");
    *remaining = remaining
        .checked_sub(encoded.0.len() + 4)
        .ok_or(SpoolLimit)?;
    sink.write_all(&(encoded.0.len() as u32).to_le_bytes())?;
    sink.write_all(&encoded.0)?;
    Ok(())
}

pub(super) fn read_frame<T: DeserializeOwned>(
    mut source: impl Read,
    maximum: usize,
) -> Result<Option<T>> {
    let mut length = [0; 4];
    if source.read(&mut length[..1])? == 0 {
        return Ok(None);
    }
    source.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    anyhow::ensure!(
        (1..=MAX_FRAME).contains(&length),
        "invalid model switch spool record length"
    );
    if length > maximum {
        return Err(SpoolLimit.into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .context("model switch spool record allocation failed")?;
    bytes.resize(length, 0);
    source.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    #[test]
    fn shared_spool_budget_checks_full_frame_before_any_partial_append() -> Result<()> {
        let mut staged = Vec::new();
        let mut budget = 12;
        write_frame(&mut staged, &"abc", &mut budget)?;
        assert_eq!(budget, 3);
        let written = staged.len();
        assert!(write_frame(&mut staged, &"def", &mut budget)
            .unwrap_err()
            .is::<SpoolLimit>());
        assert_eq!(staged.len(), written);
        assert_eq!(budget, 3);
        Ok(())
    }
}
