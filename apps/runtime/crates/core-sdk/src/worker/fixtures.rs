use super::*;

/// Non-default, non-releasable fixture controls. Never calls an external API.
#[cfg(feature = "native-fault-tests")]
pub fn fixture_control(action: &str) -> Result<Value> {
    let owned = || -> Result<Option<Arc<Process>>> {
        Ok(PROCESS
            .lock()
            .map_err(|_| anyhow::anyhow!("worker state poisoned"))?
            .clone())
    };
    match action {
        "clear" => {
            TEST_START_FAILURE.store(0, Ordering::Release);
            TEST_CLEANUP_FAILURE.store(0, Ordering::Release);
            if let Some(worker) = owned()? {
                worker.retire("fixture clear")?;
            }
        }
        "retire" => {
            if let Some(worker) = owned()? {
                worker.retire("fixture retire")?;
            }
        }
        "fail-cleanup-kill" => TEST_CLEANUP_FAILURE.store(1, Ordering::Release),
        "fail-cleanup-reap" => TEST_CLEANUP_FAILURE.store(2, Ordering::Release),
        "spawn" => {
            process()?;
        }
        "spawn-reader-kill"
        | "spawn-reader-reap"
        | "spawn-supervisor-kill"
        | "spawn-supervisor-reap" => {
            if let Some(worker) = owned()? {
                worker.retire("fixture before start failure")?;
            }
            TEST_START_FAILURE.store(
                if action.contains("reader") { 1 } else { 2 },
                Ordering::Release,
            );
            TEST_CLEANUP_FAILURE.store(
                if action.ends_with("kill") { 1 } else { 2 },
                Ordering::Release,
            );
            let error = process()
                .err()
                .context("fixture start failure was not injected")?;
            return Ok(json!({"injected_error":format!("{error:#}"),"worker":status()?}));
        }
        "callback-queued" | "callback-started" => {
            let mut calls = 0usize;
            let mut observed_active = 0usize;
            TEST_RETIRE_QUEUED_CALLBACK.store(action == "callback-queued", Ordering::Release);
            let mut callback = |_request: &Value| -> Result<Value> {
                calls += 1;
                observed_active = ACTIVE_CALLBACKS.load(Ordering::Acquire);
                if let Some(worker) = owned()? {
                    worker.retire("fixture started host callback")?;
                }
                std::thread::sleep(Duration::from_millis(300));
                Ok(json!({"synthetic":true}))
            };
            let error = execute("test_worker_callback", &json!({}), Some(&mut callback))
                .err()
                .context("fixture callback retirement did not fail")?;
            return Ok(json!({"calls":calls,"observed_active":observed_active,
                "error":format!("{error:#}"),"worker":status()?}));
        }
        _ => bail!("unknown worker fixture action"),
    }
    status()
}
