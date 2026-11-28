use respire_app::core_sdk::{Core, Prepared};
use std::path::Path;

pub(super) struct ModelJobRoot(tempfile::TempDir);

impl ModelJobRoot {
    pub(super) fn create(base: &Path) -> std::io::Result<Self> {
        Ok(Self(tempfile::tempdir_in(base)?))
    }

    pub(super) fn root(&self) -> std::io::Result<&Path> {
        Ok(self.0.path())
    }

    pub(super) fn index_root(&self) -> std::io::Result<std::path::PathBuf> {
        Ok(self.0.path().join("index"))
    }

    pub(super) fn remove_empty(self) -> std::io::Result<()> {
        self.0.close()
    }
}

pub(super) struct CandidateWorker {
    core: Core,
    target: Selection,
}

impl CandidateWorker {
    pub(super) fn start(_: &Path, _: &Path, target: Selection) -> Result<Self> {
        target.verify()?;
        let mut core = Core::new()?;
        let weights = target
            .profile
            .weights
            .as_ref()
            .context("fixture weights missing")?;
        let tokenizer = target
            .profile
            .tokenizer
            .as_ref()
            .context("fixture tokenizer missing")?;
        core.load_model(
            target.config()?,
            &std::fs::read(&weights.path)?,
            &std::fs::read(&tokenizer.path)?,
        )?;
        Ok(Self { core, target })
    }

    pub(super) fn probe(&mut self, check: impl Fn() -> Result<()>) -> Result<Value> {
        check()?;
        self.core.call(
            "model_probe",
            json!({"model":self.target.model,"text":"Respire candidate fixture"}),
        )
    }

    pub(super) fn prepare(
        &mut self,
        entry: &MemoryEntry,
        check: impl Fn() -> Result<()>,
    ) -> Result<Prepared> {
        check()?;
        let artifact = self
            .core
            .call("prepare", json!({"model":self.target.model,"entry":entry}))?;
        check()?;
        Ok(serde_json::from_value(artifact)?)
    }

    pub(super) fn close(self) -> Result<()> {
        drop(self);
        Ok(())
    }
}

type Pause = (
    String,
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);
static CHECKPOINT: Mutex<Option<Pause>> = Mutex::new(None);

pub(crate) fn legacy_model_fixture(method: &str, args: Vec<String>) -> Result<RpcResponse> {
    let id = uuid::Uuid::new_v4().to_string();
    let request = serde_json::from_value(json!({
        "v": PROTOCOL_V, "id": id, "operation_id": id,
        "method": method, "args": args, "scope": current_scope(),
    }))?;
    let (tx, _rx) = std::sync::mpsc::channel();
    Ok(dispatch(request, &tx))
}

pub(crate) fn check_shadow_admission_fixture() -> Result<()> {
    for command in [
        "recall",
        "list",
        "show",
        "status",
        "diary-search",
        "diary-fragment",
        "memory-revision",
        "remember",
        "update",
        "delete",
        "classify",
        "sync",
    ] {
        assert!(
            !model_switch_blocks_args(&[command.to_owned()]),
            "{command}"
        );
    }
    for command in [
        "login",
        "logout",
        "register",
        "keygen",
        "config",
        "session",
        "agent-config",
        "sync-reset",
        "account",
        "space",
        "fivekeys",
        "super-reset",
        "model",
        "reembed",
    ] {
        assert!(model_switch_blocks_args(&[command.to_owned()]), "{command}");
    }
    let gate = shared_exclusive();
    let held = gate.acquire(true);
    let (reply, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        mark_worker();
        let _ = reply.send(capture_scoped(&current_scope(), vec!["status".to_owned()]));
    });
    let status = result.recv_timeout(Duration::from_secs(2));
    drop(held);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("status fixture panicked"))?;
    assert!(status?.is_ok());
    Ok(())
}

pub(crate) fn pause_after_checkpoint(
    model: &str,
) -> Result<(std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>)> {
    let (signal, arrived) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    *CHECKPOINT
        .lock()
        .map_err(|_| anyhow::anyhow!("checkpoint fixture poisoned"))? =
        Some((model.to_owned(), signal, resume));
    Ok((arrived, release))
}

pub(super) fn after_checkpoint(model: &str, check: &impl Fn() -> Result<()>) -> Result<()> {
    let pause = {
        let mut slot = CHECKPOINT
            .lock()
            .map_err(|_| anyhow::anyhow!("checkpoint fixture poisoned"))?;
        slot.take_if(|(target, _, _)| target == model)
    };
    if let Some((_, signal, resume)) = pause {
        signal.send(())?;
        loop {
            check()?;
            match resume.recv_timeout(Duration::from_millis(10)) {
                Ok(()) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}
