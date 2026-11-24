use anyhow::{anyhow, Result};
use respire_client_host::lock::LibraryLock;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{sync_channel, SyncSender},
    thread::{Builder, JoinHandle},
    time::{Duration, Instant},
};

/// Keep the SQLite lock on a thread that performs no pipe or endpoint I/O.
/// A blocked controller cannot lengthen the exclusion or earn a release receipt.
pub(super) struct BoundExclusion {
    locked_file: PathBuf,
    deadline: Instant,
    signal: Option<SyncSender<()>>,
    owner: Option<JoinHandle<bool>>,
}

impl BoundExclusion {
    pub(super) fn start(guard: LibraryLock, maximum: Duration) -> Result<Self> {
        let deadline = Instant::now() + maximum;
        let locked_file = guard.file_path()?.to_owned();
        let (signal, release) = sync_channel(1);
        let owner = Builder::new()
            .name("startup-lock-deadline".into())
            .spawn(move || {
                let released = release
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .is_ok()
                    && Instant::now() < deadline;
                drop(guard);
                released
            })?;
        Ok(Self {
            locked_file,
            deadline,
            signal: Some(signal),
            owner: Some(owner),
        })
    }

    pub(super) fn verify_directory(&self, directory: &Path) -> Result<()> {
        anyhow::ensure!(
            self.locked_file.canonicalize()? == directory.join("lock.db").canonicalize()?,
            "startup lock ownership changed"
        );
        Ok(())
    }

    pub(super) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub(super) fn release(mut self) -> Result<()> {
        if let Some(signal) = self.signal.take() {
            // Do not wait on a full channel past the deadline. Dropping the
            // sender on error also leaves the owner free to release the lock.
            signal
                .try_send(())
                .map_err(|_| anyhow!("startup exclusion timed out"))?;
        }
        let released = self.join()?;
        anyhow::ensure!(released, "startup exclusion timed out");
        Ok(())
    }

    fn join(&mut self) -> Result<bool> {
        self.owner.take().map_or(Ok(false), |owner| {
            owner
                .join()
                .map_err(|_| anyhow!("startup exclusion owner failed"))
        })
    }
}

impl Drop for BoundExclusion {
    fn drop(&mut self) {
        drop(self.signal.take());
        let _ = self.join();
    }
}
