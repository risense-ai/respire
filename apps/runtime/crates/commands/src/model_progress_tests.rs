use super::*;
use anyhow::Context;

#[test]
fn foreground_preempts_background_without_releasing_its_replacement() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let background = std::thread::spawn(move || -> Result<()> {
        let operation =
            Operation::try_begin_background_index()?.context("background not admitted")?;
        assert_eq!(status()?["background"], true);
        ready_tx.send(())?;
        resume_rx.recv()?;
        assert!(check().is_err());
        assert!(update("index", "stale", 1, None).is_err());
        let mut wrote = false;
        assert!(commit(|| {
            wrote = true;
            Ok(())
        })
        .is_err());
        assert!(!wrote);
        drop(operation);
        Ok(())
    });
    ready_rx.recv()?;
    let _scope = TaskScope::new(Some("foreground-preemption".into()));
    let foreground = Operation::begin_load()?;
    assert_eq!(status()?["background"], false);
    assert!(Operation::try_begin_background_index()?.is_none());
    assert!(Operation::try_begin("second-foreground")?.is_none());
    resume_tx.send(())?;
    background.join().map_err(|_| anyhow::anyhow!("background worker panicked"))??;
    assert_eq!(status()?["id"], "foreground-preemption");
    check()?;
    drop(foreground);
    assert_eq!(status()?["active"], false);
    // A native background result can arrive after the foreground has already ended.
    in_model_operation("retired-background", || {
        let error = commit(|| Ok(())).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OperationStopped>(),
            Some(OperationStopped::Preempted)
        ));
    });
    Ok(())
}
