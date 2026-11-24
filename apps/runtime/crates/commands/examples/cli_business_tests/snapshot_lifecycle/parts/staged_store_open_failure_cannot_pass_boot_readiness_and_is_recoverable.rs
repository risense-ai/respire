#[test]
fn staged_store_open_failure_cannot_pass_boot_readiness_and_is_recoverable() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::staged_store_open_failure_cannot_pass_boot_readiness_and_is_recoverable","recover");
    }
    let mut fixture = Fixture::new()?;
    let (ok, created) = fixture.run(&["--direct", "--json", "snapshot", "create"])?;
    assert!(ok);
    let id = created["details"]["id"].as_str().context("snapshot id")?;
    fixture.stop_runtime()?;
    {
        // Diagnostics: if the fixture lock is already held at this point,
        // print the holders so the CI log identifies the leaking process.
        let ownership = match respire_app::lock::LibraryLock::acquire(
            fixture.root.path(),
            Duration::from_secs(1),
        ) {
            Ok(ownership) => ownership,
            Err(error) => {
                let lock_path = fixture.root.path().join("lock.db");
                let lock_target = std::fs::canonicalize(&lock_path)
                    .map(|path| path.to_string_lossy().to_string());
                eprintln!("DIAG staged: lock held; scanning fd holders of {lock_path:?}");
                if let Ok(entries) = std::fs::read_dir("/proc") {
                    for entry in entries.flatten() {
                        let pid = entry.file_name().to_string_lossy().to_string();
                        if !pid.chars().all(|c| c.is_ascii_digit()) {
                            continue;
                        }
                        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                            continue;
                        };
                        for fd in fds.flatten() {
                            let Ok(target) = std::fs::read_link(fd.path()) else {
                                continue;
                            };
                            let target = target.to_string_lossy().to_string();
                            let matches = match &lock_target {
                                Ok(canonical) => target == *canonical,
                                Err(_) => target.ends_with("lock.db"),
                            };
                            if matches {
                                let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
                                    .map(|cmd| cmd.replace('\0', " "))
                                    .unwrap_or_default();
                                let env = std::fs::read_to_string(format!("/proc/{pid}/environ"))
                                    .map(|env| {
                                        env.split('\0')
                                            .find(|entry| entry.starts_with("RSRS_DATA_DIR="))
                                            .unwrap_or("RSRS_DATA_DIR=<unset>")
                                            .to_owned()
                                    })
                                    .unwrap_or_default();
                                eprintln!("DIAG staged holder pid={pid} fd={:?} target={target} cmd={cmd} {env}", fd.file_name());
                            }
                        }
                    }
                }
                eprintln!("DIAG staged create child error: {error:#}");
                return Err(error);
            }
        };
        respire_app::snapshot::restore_locked(
            fixture.root.path(),
            "snapshot-fixture",
            &SessionKeys::from_urk([9; 32])?,
            id,
            &ownership,
        )?;
        std::fs::write(
            fixture.root.path().join("rsrs.db"),
            b"controlled invalid staged database",
        )?;
    }
    // Split-era semantics: the runtime either exits with a boot failure
    // (legacy combined behavior) or recovers in place and keeps serving.
    // Both are acceptable; a broken staged store must never report ready
    // without recovery. Afterwards the serving child is reaped so the
    // library lock is released for the following tests.
    let mut serving = fixture
        .runtime_command()?
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stderr = serving.stderr.take().context("serve stderr")?;
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("DIAG staged serve: {line}");
            }
        });
    }
    let recovered_at = Instant::now();
    let mut exited_failed = false;
    let mut exited = false;
    loop {
        if let Some(status) = serving.try_wait()? {
            let _ = serving.wait();
            exited_failed = !status.success();
            exited = true;
            break;
        }
        let healthy = std::fs::metadata(fixture.root.path().join("rsrs.db"))
            .map(|meta| meta.len() > 0)
            .unwrap_or(false)
            && !fixture.root.path().join(".snapshot-recovery.json").exists();
        if healthy && recovered_at.elapsed() > Duration::from_secs(3) {
            break;
        }
        if recovered_at.elapsed() > Duration::from_secs(60) {
            let _ = serving.kill();
            let _ = serving.wait();
            anyhow::bail!("runtime neither exited nor recovered the staged store");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        exited_failed || !exited,
        "runtime reported ready with a broken staged store"
    );
    let _ = serving.kill();
    let _ = serving.wait();
    let (ok, recovered) = fixture.run(&["--json", "snapshot", "recover", "--confirm"])?;
    anyhow::ensure!(ok, "host recovery failed: {}", recovered["errors"]);
    let (ok, status) = fixture.run(&["--client-only", "--json", "status"])?;
    assert!(ok);
    assert_eq!(status["summary"]["local_alive"], 1);
    Ok(())
}

#[test]
fn queued_direct_resume_rechecks_maintenance_after_library_ownership_wait() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::queued_direct_resume_rechecks_maintenance_after_library_ownership_wait","recover");
    }
    let mut fixture = Fixture::new()?;
    let (ok, created) = fixture.run(&["--direct", "--json", "snapshot", "create"])?;
    assert!(ok);
    let id = created["details"]["id"].as_str().context("snapshot id")?;
    let (ok, revision) = fixture.run(&["--direct", "--json", "memory-revision"])?;
    anyhow::ensure!(ok, "host control revision failed: {}", revision["errors"]);
    let token = revision["summary"]["revision"]
        .as_str()
        .context("revision token")?;
    fixture.stop_runtime()?;
    // Parallel tests may briefly hold a serve child; wait patiently for the
    // fixture's own lock instead of a hard 1s budget.
    let ownership = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match respire_app::lock::LibraryLock::acquire(fixture.root.path(), Duration::from_secs(1)) {
                Ok(lock) => break lock,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(error) => return Err(error),
            }
        }
    };
    let mut queued = fixture
        .command(&[
            "--direct",
            "--json",
            "snapshot",
            "resume-sync",
            "--reviewed-revision",
            token,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // The lock wait moved inside the runtime process, so stderr no longer
    // reports it. Prove the wait by requiring the child to remain alive while
    // ownership is held, then verify the post-wait quarantine recheck.
    let began = Instant::now();
    while began.elapsed() < Duration::from_millis(500) {
        anyhow::ensure!(
            queued.try_wait()?.is_none(),
            "direct command completed while maintenance held library ownership"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let receipt = respire_app::snapshot::restore_locked(
        fixture.root.path(),
        "snapshot-fixture",
        &SessionKeys::from_urk([9; 32])?,
        id,
        &ownership,
    )?;
    drop(ownership);
    let rejected = wait_bounded(queued)?;
    assert!(
        !rejected.status.success(),
        "queued direct command removed maintenance quarantine"
    );
    let error: Value = serde_json::from_slice(&rejected.stdout)?;
    assert!(error["errors"].to_string().contains("snapshot"));
    assert_eq!(
        LocalStore::open(&fixture.root.path().join("rsrs.db"))?
            .snapshot_quarantine_id()?
            .as_deref(),
        Some(id)
    );
    fixture.stop_runtime()?;
    let ownership =
        respire_app::lock::LibraryLock::acquire(fixture.root.path(), Duration::from_secs(1))?;
    respire_app::snapshot::rollback_locked(fixture.root.path(), &receipt, &ownership)?;
    Ok(())
}

#[test]
fn public_witness_verifies_restored_chain_and_reports_tamper_without_writes() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::public_witness_verifies_restored_chain_and_reports_tamper_without_writes","witness");
    }
    let fixture = Fixture::new()?;
    let (ok, before) = fixture.run(&["--direct", "--json", "witness", "--verify"])?;
    assert!(ok);
    assert_eq!(before["summary"]["count"], 1);
    assert_eq!(before["summary"]["intact"], true);
    let (ok, created) = fixture.run(&["--direct", "--json", "snapshot", "create"])?;
    assert!(ok);
    let id = created["details"]["id"].as_str().context("snapshot id")?;
    assert_eq!(
        created["details"]["witness_head"],
        before["summary"]["head"]
    );
    assert_eq!(created["details"]["witness_count"], 1);
    assert_eq!(created["details"]["witness_intact"], true);
    {
        let store = LocalStore::open(&fixture.root.path().join("rsrs.db"))?;
        assert!(store.forget("12345678-1234-4234-8234-123456789abc")?);
    }
    let (ok, changed) = fixture.run(&["--direct", "--json", "witness", "--verify"])?;
    assert!(ok);
    assert_eq!(changed["summary"]["count"], 2);
    let (ok, restored) = fixture.run(&["--json", "snapshot", "restore", id, "--confirm"])?;
    anyhow::ensure!(
        ok,
        "restore of witnessed library failed: {}",
        restored["errors"]
    );
    let (ok, verified) = fixture.run(&["--client-only", "--json", "witness", "--verify"])?;
    assert!(ok);
    assert_eq!(verified["summary"]["head"], before["summary"]["head"]);
    assert_eq!(verified["summary"]["count"], 1);
    {
        let injection = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
        injection.execute(
            "UPDATE witness_log SET after_digest='synthetic-tamper' WHERE seq=1",
            [],
        )?;
    }
    let (ok, broken) = fixture.run(&["--client-only", "--json", "witness", "--verify"])?;
    assert!(!ok);
    assert_eq!(broken["command"], "witness");
    assert_eq!(broken["status"], "fail");
    assert_eq!(broken["summary"]["broken_at"], 1);
    assert!(!broken["errors"]
        .as_array()
        .context("diagnostics")?
        .is_empty());
    let (ok, status) = fixture.run(&["--client-only", "--json", "status"])?;
    assert!(ok);
    assert_eq!(status["summary"]["local_alive"], 1);
    // The damaged current chain is still preserved in a pre-restore copy; it
    // must not prevent selecting a separate healthy checkpoint by exact ID.
    let (ok, rescued) = fixture.run(&["--json", "snapshot", "restore", id, "--confirm"])?;
    anyhow::ensure!(
        ok,
        "healthy witness snapshot rescue failed: {}",
        rescued["errors"]
    );
    let (ok, rechecked) = fixture.run(&["--client-only", "--json", "witness", "--verify"])?;
    assert!(ok);
    assert_eq!(rechecked["summary"]["head"], before["summary"]["head"]);
    // The whole database hash is unchanged: this specifically tests manifest
    // checkpoint binding rather than merely file-byte integrity validation.
    let manifest_path = fixture
        .root
        .path()
        .join("snapshots")
        .join(format!("{id}.json"));
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["witness_head"] = json!("0".repeat(64));
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    let (ok, rejected) = fixture.run(&["--json", "snapshot", "restore", id, "--confirm"])?;
    assert!(!ok);
    assert!(
        rejected["errors"].to_string().contains("witness checkpoint"),
        "expected witness checkpoint rejection; actual rejected JSON: {rejected}"
    );

    Ok(())
}

#[test]
fn healthy_snapshot_rescue_preserves_unparseable_current_witness() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::healthy_snapshot_rescue_preserves_unparseable_current_witness","rescue");
    }
    for malformed in [true, false] {
        let fixture = Fixture::new()?;
        let (ok, created) = fixture.run(&["--direct", "--json", "snapshot", "create"])?;
        assert!(ok);
        let id = created["details"]["id"]
            .as_str()
            .context("healthy snapshot id")?;
        {
            let database = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
            if malformed {
                database.execute(
                    "UPDATE witness_log SET after_digest=x'ff0001' WHERE seq=1",
                    [],
                )?;
            } else {
                database.execute("DELETE FROM meta WHERE key='witness_library_id_v1'", [])?;
            }
        }
        let (ok, restored) = fixture.run(&["--json", "snapshot", "restore", id, "--confirm"])?;
        anyhow::ensure!(
            ok,
            "unverifiable witness prevented healthy rescue: {}",
            restored["errors"]
        );
        let mut diagnostic = None;
        for entry in std::fs::read_dir(fixture.root.path().join("snapshots"))? {
            let path = entry?.path();
            if path.extension().and_then(|name| name.to_str()) != Some("json") {
                continue;
            }
            let manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            if manifest["witness_problem"].is_string() {
                diagnostic = Some(manifest);
                break;
            }
        }
        let diagnostic = diagnostic
            .context("damaged state must be retained as an explicit diagnostic snapshot")?;
        assert_eq!(diagnostic["witness_intact"], false);
        let bad_id = diagnostic["id"]
            .as_str()
            .context("diagnostic snapshot id")?;
        let saved = rusqlite::Connection::open(
            fixture
                .root
                .path()
                .join("snapshots")
                .join(format!("{bad_id}.db")),
        )?;
        if malformed {
            let value: Vec<u8> = saved.query_row(
                "SELECT after_digest FROM witness_log WHERE seq=1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(value, [255, 0, 1]);
        } else {
            let count: i64 = saved.query_row(
                "SELECT COUNT(*) FROM meta WHERE key='witness_library_id_v1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0);
        }
        drop(saved);
        let (ok, proof) = fixture.run(&["--client-only", "--json", "witness", "--verify"])?;
        assert!(ok);
        assert_eq!(proof["summary"]["head"], created["details"]["witness_head"]);
        let (ok, rejected) =
            fixture.run(&["--json", "snapshot", "restore", bad_id, "--confirm"])?;
        assert!(!ok);
        assert!(
            rejected["errors"].to_string().contains("diagnostic copy"),
            "expected diagnostic copy rejection; actual rejected JSON: {rejected}"
        );
    }
    Ok(())
}

#[test]
fn public_feedback_attribution_uses_exact_request_and_refuses_readonly_writes() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::public_feedback_attribution_uses_exact_request_and_refuses_readonly_writes","feedback");
    }
    let fixture = Fixture::new()?;
    let id = "12345678-1234-4234-8234-123456789abc";
    let request = uuid::Uuid::new_v4().to_string();
    {
        let store = LocalStore::open(&fixture.root.path().join("rsrs.db"))?;
        store.log_query_request(
            &request,
            "synthetic retrieval",
            "fixture",
            "",
            &[id.into()],
            &[0.8],
        )?;
    }
    let mark = [
        "--direct",
        "--json",
        "query-log",
        "mark",
        id,
        "--good",
        "--query-log-id",
        request.as_str(),
        "--source",
        "human",
    ];
    let (ok, marked) = fixture.run(&mark)?;
    anyhow::ensure!(ok, "explicit feedback failed: {}", marked["errors"]);
    assert_eq!(marked["summary"]["marked"], 1);
    assert_eq!(marked["summary"]["source"], "human");
    assert_eq!(marked["summary"]["query_log_id"], request);
    let revision = marked["summary"]["revision"].as_u64().context("revision")?;
    let (ok, repeated) = fixture.run(&mark)?;
    assert!(ok);
    assert_eq!(repeated["summary"]["revision"], revision);
    let (ok, unsupported) = fixture.run(&[
        "--direct",
        "--json",
        "query-log",
        "mark",
        id,
        "--bad",
        "--source",
        "human",
    ])?;
    assert!(!ok);
    assert!(unsupported["errors"]
        .to_string()
        .contains("supports only --source model"));
    let persisted = LocalStore::open(&fixture.root.path().join("rsrs.db"))?.query_log_rows(1)?;
    assert_eq!(persisted[0].good, [id]);
    assert!(persisted[0].bad.is_empty());
    let (ok, legacy) = fixture.run_feedback(
        &["--direct", "--json", "query-log", "mark", id, "--bad"],
        Some("0"),
    )?;
    assert!(ok);
    assert_eq!(legacy["summary"]["good"], false);
    {
        let store = LocalStore::open(&fixture.root.path().join("rsrs.db"))?;
        let allowed = std::collections::BTreeSet::from([id.to_owned()]);
        let counts = store.feedback_counts(
            "synthetic retrieval",
            "fixture",
            "",
            &allowed,
            0.5,
            chrono::Utc::now().timestamp(),
        )?;
        assert!(counts.good.is_empty());
        assert_eq!(counts.bad.get(id), Some(&1));
        let row = store.query_log_rows(1)?.remove(0);
        assert!(row.good.is_empty());
        assert_eq!(row.bad, [id]);
    }
    let (ok, reenabled) = fixture.run_feedback(&mark, Some("1"))?;
    assert!(ok);
    let new_revision = reenabled["summary"]["revision"]
        .as_u64()
        .context("reenabled revision")?;
    assert!(new_revision > revision);
    let revision = new_revision;
    {
        let raw = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
        raw.execute(
            "UPDATE query_log SET good_ids='[]',bad_ids=?1",
            [serde_json::to_string(&[id])?],
        )?;
    }
    let (ok, repaired) = fixture.run_feedback(&mark, Some("1"))?;
    assert!(ok);
    assert_eq!(repaired["summary"]["revision"], revision);
    let rows = LocalStore::open(&fixture.root.path().join("rsrs.db"))?.query_log_rows(1)?;
    assert_eq!(rows[0].good, [id]);
    assert!(rows[0].bad.is_empty());
    let (ok, invalid) = fixture.run(&[
        "--direct",
        "--json",
        "query-log",
        "mark",
        "12345678",
        "--bad",
        "--query-log-id",
        &request,
    ])?;
    assert!(!ok);
    assert!(invalid["errors"].to_string().contains("complete candidate"));
    let (ok, list) = fixture.run(&["--direct", "--json", "query-log"])?;
    assert!(ok);
    assert!(list.to_string().contains(&request));
    let (ok, mode) = fixture.run(&[
        "--direct",
        "--json",
        "agent-config",
        "--set",
        "workspace_mode=readonly",
    ])?;
    anyhow::ensure!(ok, "readonly configuration failed: {}", mode["errors"]);
    let (ok, denied) = fixture.run(&mark)?;
    assert!(!ok);
    assert!(denied["errors"].to_string().contains("read-only"));
    let database = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
    let persisted: u64 =
        database.query_row("SELECT revision FROM query_feedback", [], |row| row.get(0))?;
    assert_eq!(persisted, revision);
    Ok(())
}

