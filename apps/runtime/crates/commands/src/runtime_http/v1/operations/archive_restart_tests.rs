#[test]
fn restart_rejects_prior_registry_ticket() -> Result<()> {
    if std::env::var_os(RESTART_PHASE).is_some() && std::env::var_os(CHILD).is_some() {
        return production_router_fixture(RESTART);
    }
    let dir = tempfile::tempdir()?;
    for phase in ["stage", "restart"] {
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", RESTART, "--nocapture"])
            .env(RESTART_PHASE, phase)
            .env(CHILD, "1")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_DATA_DIR", dir.path())
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_ADDR")
            .env_remove("RSRS_TOKEN")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "restart fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    anyhow::ensure!(
        dir.path().join("restart-verified").is_file(),
        "restart fixture did not verify admission"
    );
    Ok(())
}

async fn check_restarted(app: &Router, root: &Path) -> Result<()> {
    let parts = Request::get("/v1/operations")
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?
        .into_parts()
        .0;
    let scope = transport::authorize(&parts, BOUND, BOUND)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let target = service::database_path(root)?.canonicalize()?;
    let identity = respire_spawn::owner_credential::ArchiveTarget::pin(target.clone())?.identity_digest()?;
    let wire = b"synthetic restart ticket";
    let hash: [u8; 32] = Sha256::digest(wire).into();
    let target_hash = Sha256::digest(target.to_str().context("target")?.as_bytes()).into();
    let record = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(record) = read_owner_credential(
                &crate::rpc_worker::runtime_dir(),
                &scope.instance.to_string(),
            ) {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let issue = |id: uuid::Uuid| {
        let claim = ArchiveClaim {
            query_only: false,
        inspect_only: false,
            operation: *id.as_bytes(),
            archive: hash,
            target: target_hash,
        target_identity: identity,
            instance: *scope.instance.as_bytes(),
            generation: scope.generation,
        };
        archive_proof::issue(
            record.as_bytes(),
            claim,
            crate::runtime_http::v1::owner_control::wall_ms()?,
        )
    };
    let note = root.join("synthetic-restart-receipt.json");
    if std::env::var(RESTART_PHASE)? == "stage" {
        let job = state::registry()
            .begin(scope.clone(), "backup_import")
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let id = uuid::Uuid::parse_str(&job.id().map_err(|e| anyhow::anyhow!("{e:?}"))?)?;
        let op = ArchiveOperation::from_v4_bytes(*id.as_bytes())?;
        let pending =
            PendingBackupArchive::stage(root, op, wire.len() as u64, wire.as_slice(), || Ok(()))?;
        job.stage_archive(respire_spawn::owner_credential::ArchiveTarget::pin(target)?, hash, pending)
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        fs::write(
            note,
            serde_json::to_vec(
                &json!({"id":id.to_string(),"proof":issue(id).context("old proof")?}),
            )?,
        )?;
        // End only the isolated fixture process, leaving its pending file behind.
        std::process::exit(0);
    }
    let old: Value = serde_json::from_slice(&fs::read(note)?)?;
    let id = uuid::Uuid::parse_str(old["id"].as_str().context("old ID")?)?;
    assert!(root
        .join(format!(".backup-archive-{}.pending", id.simple()))
        .exists());
    for (proof, status) in [
        (
            old["proof"].as_str().context("old proof")?.to_owned(),
            StatusCode::UNAUTHORIZED,
        ),
        (issue(id).context("current proof")?, StatusCode::NOT_FOUND),
    ] {
        let frame = json!({"schema_version":2,"target_identity_sha256":hex::encode(identity),"operation_id":id.to_string(),"archive_sha256":hex::encode(hash),"target_sha256":hex::encode(target_hash)});
        let req = Request::post("/api/runtime/private-backup/claim")
            .header(header::HOST, BOUND.to_string())
            .header("x-respire-archive-proof", proof)
            .extension(ConnectInfo(crate::network::Peer(BOUND)))
            .body(Body::from(frame.to_string()))?;
        assert_eq!(app.clone().oneshot(req).await?.status(), status);
    }
    assert!(!root
        .join(format!(".backup-archive-{}.archive", id.simple()))
        .exists());
    fs::write(root.join("restart-verified"), b"verified")?;
    Ok(())
}
