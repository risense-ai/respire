#[test]
fn public_empty_recall_returns_its_query_log_identity() -> Result<()> {
    if support::legacy_cli()?.is_some() {
        return legacy_snapshot_contract("snapshot_lifecycle::public_empty_recall_returns_its_query_log_identity","recall");
    }
    let fixture = Fixture::new()?;
    let (ok, result) = fixture.run(&[
        "--direct",
        "--json",
        "recall",
        "synthetic absent query",
        "--project",
        "not-in-fixture",
    ])?;
    anyhow::ensure!(ok, "empty recall failed: {}", result["errors"]);
    assert_eq!(result["summary"]["count"], 0);
    let request = result["summary"]["query_log_id"]
        .as_str()
        .context("empty recall query identity")?;
    assert!(uuid::Uuid::parse_str(request).is_ok());
    let store = LocalStore::open(&fixture.root.path().join("rsrs.db"))?;
    assert!(store
        .query_log_rows(20)?
        .iter()
        .any(|row| row.request_id.as_deref() == Some(request) && row.candidates.is_empty()));
    let rows = store.query_log_rows(20)?;
    let recorded = rows
        .iter()
        .find(|row| row.request_id.as_deref() == Some(request))
        .context("persisted empty recall record")?;
    assert_eq!(recorded.query, "synthetic absent query");
    assert_eq!(recorded.project, "not-in-fixture");
    let before: Vec<_> = rows.iter().map(|row| row.request_id.clone()).collect();
    drop(store);

    // A successful response must not advertise an identity whose INSERT failed.
    let database = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
    database.execute_batch(
        "CREATE TRIGGER reject_empty_query_log BEFORE INSERT ON query_log
         BEGIN SELECT RAISE(ABORT, 'synthetic empty query log refused'); END;",
    )?;
    drop(database);
    let (ok, denied) = fixture.run(&[
        "--direct",
        "--json",
        "recall",
        "synthetic rejected query",
        "--project",
        "not-in-fixture",
    ])?;
    assert!(!ok);
    assert!(denied["errors"].to_string().contains("synthetic empty query log refused"));
    assert!(denied["summary"]["query_log_id"].is_null());
    let database = rusqlite::Connection::open(fixture.root.path().join("rsrs.db"))?;
    database.execute_batch("DROP TRIGGER reject_empty_query_log;")?;
    drop(database);

    let (ok, mode) = fixture.run(&[
        "--direct",
        "--json",
        "agent-config",
        "--set",
        "workspace_mode=readonly",
    ])?;
    anyhow::ensure!(ok, "readonly configuration failed: {}", mode["errors"]);
    let (ok, readonly) = fixture.run(&[
        "--direct",
        "--json",
        "recall",
        "synthetic readonly query",
        "--project",
        "not-in-fixture",
    ])?;
    anyhow::ensure!(ok, "readonly recall failed: {}", readonly["errors"]);
    assert_eq!(readonly["summary"]["count"], 0);
    assert!(readonly["summary"]["query_log_id"].is_null());
    let store = LocalStore::open(&fixture.root.path().join("rsrs.db"))?;
    let after: Vec<_> = store
        .query_log_rows(20)?
        .into_iter()
        .map(|row| row.request_id)
        .collect();
    assert_eq!(after, before);
    Ok(())
}

// The candidate guard remains live in a separate installation throughout old
// maintenance. These substeps exercise the old supported contract, not the new
// fault-injection or scoped-stop features in the first current31 pass.
fn legacy_snapshot_contract(case: &str, feature: &str) -> Result<()> {
    // Fixed stage names only: no response bodies, paths, credentials or raw errors.
    let checkpoint = |stage: &str| eprintln!("legacy-snapshot-stage {}",
        json!({"case":case,"stage":stage}));
    checkpoint("candidate_guard");
    let guard = Fixture::new()?;
    let sender=support::legacy_cli()?.context("old remote sender missing")?;
    let remote=|args:&[&str]| -> Result<Value> {
        let output=guard.command_for(&sender,args).output()?;
        let value:Value=serde_json::from_slice(&output.stdout)?;
        anyhow::ensure!(output.status.success(),"official stable remote API request failed");
        Ok(value)
    };
    let api_receipt=|feature:&str| {
        eprintln!("legacy-stable-api {}",json!({"feature":feature,"sender":"old_cli",
            "execution":"candidate_runtime","passed":true}));
    };
    if feature=="feedback" {
        let request=uuid::Uuid::new_v4().to_string();
        let id="12345678-1234-4234-8234-123456789abc";
        LocalStore::open(&guard.root.path().join("rsrs.db"))?.log_query_request(
            &request,"old remote feedback","fixture","",&[id.into()],&[0.8])?;
        let value=remote(&["--client-only","--json","query-log","mark",id,"--good",
            "--query-log-id",&request,"--source","human"])?;
        assert_eq!(value["summary"]["query_log_id"],request);
        assert_eq!(LocalStore::open(&guard.root.path().join("rsrs.db"))?.query_log_rows(1)?[0].good,[id]);
        api_receipt("feedback.mark");
    } else if feature=="recall" {
        let value=remote(&["--client-only","--json","recall","old remote absent query",
            "--project","not-in-fixture"])?;
        assert_eq!(value["summary"]["count"],0);
        let id=value["summary"]["query_log_id"].as_str().context("old remote query ID missing")?;
        uuid::Uuid::parse_str(id)?;
        assert!(LocalStore::open(&guard.root.path().join("rsrs.db"))?.query_log_rows(20)?
            .iter().any(|row|row.request_id.as_deref()==Some(id)&&row.candidates.is_empty()));
        api_receipt("recall.empty");
    } else {
        checkpoint("remote_create");
        let created=remote(&["--client-only","--json","snapshot","create"])?;
        let id=created["details"]["id"].as_str().context("old remote snapshot ID missing")?;
        checkpoint("remote_list");
        let listed=remote(&["--client-only","--json","snapshot","list"])?;
        assert!(listed["details"]["snapshots"].as_array().context("old remote snapshots missing")?
            .iter().any(|snapshot|snapshot["id"]==id));
        api_receipt("snapshot.create");
        api_receipt("snapshot.list");
        if feature=="witness" || feature=="rescue" {
            remote(&["--client-only","--json","witness","--verify"])?;
            api_receipt("witness.verify");
        }
        let before=guard.protected_state()?;
        let refused=guard.command_for(&sender,&["--client-only","--json","snapshot","restore",id,"--confirm"])
            .output()?;
        assert!(!refused.status.success(),"old client-only sender bypassed its host restore boundary");
        assert_eq!(guard.protected_state()?,before);
    }
    checkpoint("old_install");
    let protected = guard.protected_state()?;
    let root = tempfile::tempdir()?;
    let cli = support::stage_legacy_cli(root.path())?;
    std::fs::write(root.path().join("session.json"),
        std::fs::read(guard.root.path().join("session.json"))?)?;
    {
        let original = LocalStore::open(&guard.root.path().join("rsrs.db"))?;
        let target = LocalStore::open(&root.path().join("rsrs.db"))?;
        for row in original.all(true)? { target.put(&row)?; }
    }
    let port = std::net::TcpListener::bind(("127.0.0.1",0))?.local_addr()?.port();
    let mut old = Fixture { cli:cli.clone(), runtime:None, runtime_bin:cli,
        root, port, secret:guard.secret.clone() };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        checkpoint("old_start");
        old.start_runtime()?;
        let first = support::legacy_owner(old.root.path(), old.root.path(), old.port)?;
        let rows = serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?;
        assert!(respire::lock::LibraryLock::acquire(old.root.path(), Duration::from_millis(80)).is_err(),
            "old runtime failed to retain exclusive library ownership");
        let (ok, denied) = old.run(&["--direct","--json","show","12345678-1234-4234-8234-123456789abc"])?;
        assert!(!ok);
        assert!(denied["errors"].to_string().contains("the local runtime is running"));
        assert_eq!(support::legacy_owner(old.root.path(), old.root.path(), old.port)?["pid"], first["pid"]);
        assert_eq!(serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?, rows);
        let pid = u32::try_from(first["pid"].as_u64().context("old PID missing")?)?;
        let (ok, _) = old.run(&["--json","--runtime-internal","--stop"])?;
        assert!(ok);
        old.stop_runtime()?; // Reap our initial foreground child before /proc checks.
        support::legacy_drained(old.root.path(), old.port, pid)?;

        // Explicit direct requests retain their original local semantics. The old
        // owner is proven down; no --direct flag is stripped or rewritten.
        checkpoint("old_create");
        let (ok, created) = old.run(&["--direct","--json","snapshot","create","--note","legacy contract"])?;
        assert!(ok, "old local snapshot creation failed");
        let id = created["details"]["id"].as_str().context("old snapshot identity missing")?.to_owned();
        let (ok, listed) = old.run(&["--direct","--json","snapshot","list"])?;
        assert!(ok);
        assert_eq!(listed["details"]["snapshots"].as_array().context("old snapshot list missing")?.len(),1);
        if feature == "feedback" || feature == "recall" {
            let memory = "12345678-1234-4234-8234-123456789abc";
            let request = uuid::Uuid::new_v4().to_string();
            if feature == "feedback" {
                LocalStore::open(&old.root.path().join("rsrs.db"))?.log_query_request(
                    &request,"old feedback","fixture","",&[memory.into()],&[0.8])?;
                let args = ["--direct","--json","query-log","mark",memory,"--good",
                    "--query-log-id",&request,"--source","human"];
                let (ok, marked) = old.run(&args)?;
                assert!(ok);
                assert_eq!(marked["summary"]["query_log_id"],request);
                let revision=marked["summary"]["revision"].clone();
                let (ok,repeated)=old.run(&args)?;
                assert!(ok);
                assert_eq!(repeated["summary"]["revision"],revision);
                let saved=LocalStore::open(&old.root.path().join("rsrs.db"))?.query_log_rows(1)?;
                assert_eq!(saved[0].good,[memory]);
                assert!(saved[0].bad.is_empty());
            } else {
                let (ok, recalled)=old.run(&["--direct","--json","recall","old absent query",
                    "--project","not-in-fixture"])?;
                assert!(ok);
                assert_eq!(recalled["summary"]["count"],0);
                let request=recalled["summary"]["query_log_id"].as_str().context("old empty query ID missing")?;
                uuid::Uuid::parse_str(request)?;
                let logs=LocalStore::open(&old.root.path().join("rsrs.db"))?.query_log_rows(20)?;
                assert!(logs.iter().any(|row|row.request_id.as_deref()==Some(request)&&row.candidates.is_empty()));
            }
            let (ok,mode)=old.run(&["--direct","--json","agent-config","--set","workspace_mode=readonly"])?;
            assert!(ok, "old local readonly configuration failed: {}",mode["status"]);
            let before=serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?;
            let (ok,_)=old.run(&["--direct","--json","remember","must not be written","--force"])?;
            assert!(!ok);
            assert_eq!(serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?,before);
        } else {
            checkpoint("old_update");
            let (ok, changed)=old.run(&["--direct","--json","update",
                "12345678-1234-4234-8234-123456789abc","--title","old temporary title"])?;
            assert!(ok, "old local update failed: {}",changed["status"]);
            checkpoint("old_restart");
            let (ok,_)=old.run(&["--json","runtime","start"])?;
            assert!(ok);
            support::legacy_owner(old.root.path(),old.root.path(),old.port)?;
            checkpoint("old_restore");
            let (ok, restored)=old.run(&["--json","snapshot","restore",&id,"--confirm"])?;
            assert!(ok, "old host restore failed: {}",restored["status"]);
            assert_eq!(restored["summary"]["runtime_verified"],true);
            assert_eq!(restored["summary"]["sync_quarantined"],true);
            let restored_owner=support::legacy_owner(old.root.path(),old.root.path(),old.port)?;
            assert_eq!(restored_owner["bin"],"1.0.20");
            assert_eq!(serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?,rows);
            if feature == "witness" || feature == "rescue" {
                let (ok,proof)=old.run(&["--client-only","--json","witness","--verify"])?;
                assert!(ok, "old restored witness verification failed: {}",proof["status"]);
            }
            if feature == "recover" || feature == "concurrent_recover" {
                checkpoint("old_recovery");
                // The original binary has no public interruption hook. The
                // harness stages its original schema using the real snapshot
                // bytes, then the old binary alone executes recovery.
                let owner=support::legacy_owner(old.root.path(),old.root.path(),old.port)?;
                let pid=u32::try_from(owner["pid"].as_u64().context("old recovery PID missing")?)?;
                assert!(old.run(&["--json","--runtime-internal","--stop"])?.0);
                old.stop_runtime()?;
                support::legacy_drained(old.root.path(),old.port,pid)?;
                assert!(old.run(&["--direct","--json","update",
                    "12345678-1234-4234-8234-123456789abc","--title","old interrupted state"])?.0);
                {
                    use sha2::Digest;
                    let _ownership=respire::lock::LibraryLock::acquire(old.root.path(),Duration::from_secs(2))?;
                    let manifest:Value=serde_json::from_slice(&std::fs::read(
                        old.root.path().join("snapshots").join(format!("{id}.json")))?)?;
                    let bytes=std::fs::read(old.root.path().join("snapshots").join(format!("{id}.db")))?;
                    let digest=format!("{:x}",sha2::Sha256::digest(&bytes));
                    assert_eq!(manifest["database_sha256"],digest);
                    let rollback=format!(".snapshot-rollback-{}.db",uuid::Uuid::new_v4().simple());
                    std::fs::write(old.root.path().join(&rollback),bytes)?;
                    let journal=json!({"schema_version":1,"phase":"installed","receipt":{
                        "restored":id,"before_restore":id,"rollback_file":rollback,
                        "rollback_sha256":digest,"account_fingerprint":manifest["account_fingerprint"]}});
                    for name in [".snapshot-recovery.json",".snapshot-maintenance.json"] {
                        std::fs::write(old.root.path().join(name),serde_json::to_vec(&journal)?)?;
                    }
                }
                let mut commands=Vec::new();
                let count=if feature=="concurrent_recover" {2} else {1};
                for _ in 0..count {
                    commands.push(old.command(&["--json","snapshot","recover","--confirm"])
                        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?);
                }
                let mut successful=0;
                for command in commands {
                    let output=wait_bounded(command)?;
                    if output.status.success() {successful+=1;} else {
                        let value:Value=serde_json::from_slice(&output.stdout)?;
                        assert!(value["errors"].to_string().contains("no interrupted snapshot operation exists"));
                    }
                }
                assert_eq!(successful,1,"the original recovery journal must be consumed exactly once");
                assert!(!old.root.path().join(".snapshot-recovery.json").exists());
                assert!(!old.root.path().join(".snapshot-maintenance.json").exists());
                support::legacy_owner(old.root.path(),old.root.path(),old.port)?;
                assert_eq!(serde_json::to_value(LocalStore::open(&old.root.path().join("rsrs.db"))?.all(true)?)?,rows);
                eprintln!("legacy-stable-recovery input=harness_original_schema image=old_snapshot execution=old_embedded");
            }
        }
        assert_eq!(guard.protected_state()?,protected,
            "old maintenance changed the separate candidate owner or its records");
        Ok(())
    }));
    checkpoint("old_cleanup");
    let cleanup=(|| -> Result<()> {
        if respire_commands::net_rpc::port_is_open_on(old.port) {
            let owner=support::legacy_owner(old.root.path(),old.root.path(),old.port)?;
            let pid=u32::try_from(owner["pid"].as_u64().context("old cleanup PID missing")?)?;
            let (ok,_)=old.run(&["--json","--runtime-internal","--stop"])?;
            assert!(ok);
            old.stop_runtime()?;
            support::legacy_drained(old.root.path(),old.port,pid)?;
        }
        assert_eq!(guard.protected_state()?,protected);
        Ok(())
    })();
    let result=match outcome { Ok(result)=>result, Err(panic)=>std::panic::resume_unwind(panic) };
    result?;
    cleanup?;
    support::legacy_receipt(case,"old_embedded")
}
