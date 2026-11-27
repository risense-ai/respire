//! Coordinate create admission and writer deferral in an isolated Router fixture.
use std::{sync::{mpsc, Mutex}, time::Duration};

type Pause = (mpsc::SyncSender<()>, mpsc::Receiver<()>);
static PAUSE: Mutex<Option<Pause>> = Mutex::new(None);

pub(in crate::runtime_http::v1) fn install(pause: Pause) -> anyhow::Result<()> {
    let mut slot = PAUSE.lock().map_err(|_| anyhow::anyhow!("fixture pause poisoned"))?;
    anyhow::ensure!(slot.is_none(), "fixture pause already installed");
    *slot = Some(pause);
    Ok(())
}

pub(super) fn wait() {
    let pause = PAUSE.lock().ok().and_then(|mut slot| slot.take());
    if let Some((entered, resume)) = pause {
        if entered.send(()).is_ok() {
            let _ = resume.recv_timeout(Duration::from_secs(5));
        }
    }
}

static DEFERRED: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

pub(in crate::runtime_http::v1) fn watch_deferral(sender: tokio::sync::oneshot::Sender<()>) -> anyhow::Result<()> {
    let mut slot = DEFERRED.lock().map_err(|_| anyhow::anyhow!("fixture deferral watcher poisoned"))?;
    anyhow::ensure!(slot.is_none(), "fixture deferral watcher already installed");
    *slot = Some(sender);
    Ok(())
}

pub(in crate::runtime_http::v1) fn note_deferred() {
    if let Some(sender) = DEFERRED.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = sender.send(());
    }
}
