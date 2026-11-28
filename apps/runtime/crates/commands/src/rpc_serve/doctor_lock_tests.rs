use super::*;

#[test]
fn doctor_routes_only_mutating_and_update_check_options_to_the_writer() {
    for (args, readonly) in [
        (&["doctor"][..], true),
        (&["--json", "doctor"][..], true),
        (&["--json", "doctor", "--remote"][..], true),
        (&["--json", "doctor", "--fix"][..], false),
        (&["--json", "doctor", "--check-update"][..], false),
        (&["doctor", "--fix=true"][..], false),
        (&["doctor", "--check-update=true"][..], false),
        (&["doctor", "--", "--fix", "--check-update"][..], true),
    ] {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        assert_eq!(is_readonly_doctor(&args), readonly, "{args:?}");
        assert_eq!(is_exclusive(&args), !readonly, "{args:?}");
    }
    assert!(!is_readonly_doctor(&["status".to_owned()]));
}

#[test]
fn readonly_doctor_validates_scope_without_waiting_for_the_shared_gate() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    let scope = fixture_scope("", 0);
    let rejected = validate_job_scope(&scope).expect_err("invalid scope must be rejected");
    let args = vec!["--json".to_owned(), "doctor".to_owned()];
    assert!(!is_exclusive(&args));

    let gate = shared_exclusive();
    let foreground = {
        let _scope = SCOPE_EXECUTION
            .read()
            .map_err(|_| anyhow::anyhow!("test scope fence poisoned"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match gate.try_owner_foreground() {
                Ok(Some(guard)) => break guard,
                Err(()) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    anyhow::bail!("foreground gate unavailable while the scope read fence is held")
                }
            }
        }
    };

    let (result, receipt) = mpsc::channel();
    let attempt = std::thread::spawn(move || {
        let _ = result.send(capture_scoped(&scope, args).err());
    });
    let observed = receipt.recv_timeout(Duration::from_secs(2));
    drop(foreground);
    attempt
        .join()
        .map_err(|_| anyhow::anyhow!("doctor capture panicked"))?;
    assert_eq!(observed?, Some(rejected));
    Ok(())
}
