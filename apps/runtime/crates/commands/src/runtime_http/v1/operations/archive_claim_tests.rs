use super::*;
use crate::runtime_http::v1::{operations::state, transport};
use respire_spawn::owner_credential::{
    archive_proof::{self, ArchiveClaim},
    read_owner_credential, ArchiveOperation, PendingBackupArchive,
};
use std::path::Path;

pub(super) const TEST:&str="runtime_http::v1::operations::tests::archive_claim_tests::native_claim_cancel_on_serving_router";
#[test]
fn native_claim_cancel_on_serving_router() -> Result<()> {
    production_router_fixture(TEST)
}

pub(super) async fn check(app: &Router, root: &Path, token: &Path) -> Result<()> {
    let scope = transport::authorize(
        &Request::get("/v1/operations")
            .header(header::HOST, BOUND.to_string())
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(())?
            .into_parts()
            .0,
        BOUND,
        BOUND,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let target = service::database_path(root)?.canonicalize()?;
    let target_hash: [u8; 32] =
        Sha256::digest(target.to_str().context("target")?.as_bytes()).into();
    let identity = respire_spawn::owner_credential::ArchiveTarget::pin(target.clone())?.identity_digest()?;
    let wire = b"synthetic authenticated worker payload";
    let hash: [u8; 32] = Sha256::digest(wire).into();
    let stage_at = |short, target: &std::path::Path| -> Result<_> {
        let job = state::registry()
            .begin(scope.clone(), "backup_import")
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let id = uuid::Uuid::parse_str(&job.id().map_err(|e| anyhow::anyhow!("{e:?}"))?)?;
        let op = ArchiveOperation::from_v4_bytes(*id.as_bytes())?;
        let pending =
            PendingBackupArchive::stage(root, op, wire.len() as u64, wire.as_slice(), || Ok(()))?;
        job.stage_fixture(respire_spawn::owner_credential::ArchiveTarget::pin(target.to_path_buf())?, hash, pending, short)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(id)
    };
    let stage = |short| stage_at(short, &target);
    let record = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(r) = read_owner_credential(
                &crate::rpc_worker::runtime_dir(),
                &scope.instance.to_string(),
            ) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let id = stage(false)?;
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
    let now = crate::runtime_http::v1::owner_control::wall_ms().context("fixture clock")?;
    let issue =
        |claim, time| archive_proof::issue(record.as_bytes(), claim, time).context("fixture proof");
    let send_bound = |identity: [u8;32], query_only: bool,
                      id: uuid::Uuid,
                      archive: [u8; 32],
                      target: [u8; 32],
                      proof: String| {
        let request=Request::post("/fixture/archive-claim").header(header::HOST,BOUND.to_string())
            .header("x-respire-archive-proof",proof).extension(ConnectInfo(crate::network::Peer(BOUND)))
            .body(Body::from(json!({"schema_version":2,"target_identity_sha256":hex::encode(identity),"query_only":query_only,"operation_id":id.to_string(),"archive_sha256":hex::encode(archive),"target_sha256":hex::encode(target)}).to_string()));
        let app = app.clone();
        async move { Ok::<_, anyhow::Error>(app.oneshot(request?).await?) }
    };
    let send_frame = |query, id, archive, target, proof| send_bound(identity, query, id, archive, target, proof);
    let post = |id, archive, target, proof| send_frame(false, id, archive, target, proof);
    let wrong_pin = ArchiveClaim { target_identity: [0xff;32], ..claim };
    assert_eq!(send_bound(wrong_pin.target_identity, false, id, hash, target_hash, issue(wrong_pin,now)?).await?.status(), StatusCode::UNAUTHORIZED);
    let name =
        |id: uuid::Uuid, ext: &str| root.join(format!(".backup-archive-{}.{ext}", id.simple()));
    for (bad, proof) in [
        (
            ArchiveClaim {
                archive: [9; 32],
                ..claim
            },
            issue(
                ArchiveClaim {
                    archive: [9; 32],
                    ..claim
                },
                now,
            )?,
        ),
        (
            ArchiveClaim {
                target: [8; 32],
                ..claim
            },
            issue(
                ArchiveClaim {
                    target: [8; 32],
                    ..claim
                },
                now,
            )?,
        ),
        (
            claim,
            issue(
                ArchiveClaim {
                    instance: *uuid::Uuid::new_v4().as_bytes(),
                    ..claim
                },
                now,
            )?,
        ),
        (
            claim,
            issue(
                ArchiveClaim {
                    generation: scope.generation + 1,
                    ..claim
                },
                now,
            )?,
        ),
        (claim, issue(claim, now - 60000)?),
        (claim, TOKEN.to_owned()),
    ] {
        assert_ne!(
            post(id, bad.archive, bad.target, proof).await?.status(),
            StatusCode::OK
        );
        assert!(name(id, "pending").exists() && !name(id, "archive").exists());
    }
    let foreign = uuid::Uuid::new_v4();
    assert_ne!(
        post(foreign, hash, target_hash, issue(claim, now)?)
            .await?
            .status(),
        StatusCode::OK
    );
    let cancelled = stage(false)?;
    let cancel = |id: uuid::Uuid| format!("/v1/operations/{id}");
    assert_eq!(
        request(app, "DELETE", &cancel(cancelled), Some(TOKEN))
            .await?
            .status(),
        StatusCode::ACCEPTED
    );
    assert!(!name(cancelled, "pending").exists());
    assert_ne!(
        post(
            cancelled,
            hash,
            target_hash,
            issue(
                ArchiveClaim {
                    query_only: false,
        inspect_only: false,
                    operation: *cancelled.as_bytes(),
                    ..claim
                },
                now
            )?
        )
        .await?
        .status(),
        StatusCode::OK
    );
    fs::write(token, NEXT_TOKEN)?;
    assert_ne!(
        post(id, hash, target_hash, issue(claim, now)?)
            .await?
            .status(),
        StatusCode::OK
    );
    fs::write(token, TOKEN)?;
    assert!(!name(id, "pending").exists());
    let expired = stage(true)?;
    tokio::time::timeout(WAIT, async {
        while name(expired, "pending").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert_eq!(
        json_response(
            request(app, "GET", &cancel(expired), Some(TOKEN)).await?,
            StatusCode::OK
        )
        .await?["state"],
        "failed"
    );
    assert_ne!(
        post(
            expired,
            hash,
            target_hash,
            issue(
                ArchiveClaim {
                    operation: *expired.as_bytes(),
                    ..claim
                },
                now
            )?
        )
        .await?
        .status(),
        StatusCode::OK
    );
    let id = stage(false)?;
    let claim = ArchiveClaim {
        operation: *id.as_bytes(),
        ..claim
    };
    let response = post(id, hash, target_hash, issue(claim, now)?).await?;
    let content = json_response(response, StatusCode::OK).await?;
    assert_eq!(content["state"], "claimed");
    assert_eq!(content["operation_id"], id.to_string());
    assert!(name(id, "archive").exists() && !name(id, "pending").exists());
    assert_eq!(
        post(id, hash, target_hash, issue(claim, now)?)
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(app, "DELETE", &cancel(id), Some(TOKEN))
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        json_response(
            request(app, "GET", &cancel(id), Some(TOKEN)).await?,
            StatusCode::OK
        )
        .await?["state"],
        "running"
    );
    assert_ne!(
        send_frame(true, id, hash, target_hash, issue(claim, now)?)
            .await?
            .status(),
        StatusCode::OK
    );
    let original = json_response(
        send_frame(
            true,
            id,
            hash,
            target_hash,
            issue(
                ArchiveClaim {
                    query_only: true,
                    ..claim
                },
                now,
            )?,
        )
        .await?,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(original, content);
    let substitute_path = root.join("target-substitution.db");
    fs::write(&substitute_path, b"first")?;
    let substituted = stage_at(false, &substitute_path)?;
    fs::rename(&substitute_path, root.join("original-target.db"))?;
    fs::write(&substitute_path, b"second")?;
    let substitute_claim = ArchiveClaim {
        operation: *substituted.as_bytes(),
        target: Sha256::digest(substitute_path.as_os_str().as_encoded_bytes()).into(),
        ..claim
    };
    assert_ne!(post(substituted, hash, substitute_claim.target, issue(substitute_claim, now)?).await?.status(), StatusCode::OK);
    assert!(!name(substituted, "archive").exists() && !name(substituted, "pending").exists());

    let swapped_after = stage_at(false, &substitute_path)?;
    let admitted = ArchiveClaim { operation: *swapped_after.as_bytes(), target_identity: respire_spawn::owner_credential::ArchiveTarget::pin(substitute_path.clone())?.identity_digest()?, ..substitute_claim };
    assert_eq!(send_bound(admitted.target_identity, false, swapped_after, hash, admitted.target, issue(admitted, now)?).await?.status(), StatusCode::OK);
    fs::rename(&substitute_path, root.join("admitted-target.db"))?;
    fs::write(&substitute_path, b"third")?;
    let swapped_query = ArchiveClaim { query_only: true, ..admitted };
    assert_ne!(send_bound(admitted.target_identity, true, swapped_after, hash, admitted.target, issue(swapped_query, now)?).await?.status(), StatusCode::OK);

    let refused = stage(false)?;
    fs::write(name(refused, "archive"), b"foreign")?;
    let refusal = ArchiveClaim { operation: *refused.as_bytes(), ..claim };
    assert_ne!(post(refused, hash, target_hash, issue(refusal, now)?).await?.status(), StatusCode::OK);
    assert_eq!(fs::read(name(refused, "archive"))?, b"foreign");
    assert!(!name(refused, "pending").exists() && !name(refused, "intent").exists());
    assert_eq!(json_response(request(app, "GET", &cancel(refused), Some(TOKEN)).await?, StatusCode::OK).await?["state"], "failed");
    assert_ne!(post(refused, hash, target_hash, issue(refusal, now)?).await?.status(), StatusCode::OK);

    // Withdraw authority after the actual wire rename. Only a fresh signed query
    // may recover the receipt for the already consumed operation.
    let late = stage(false)?;
    let late_claim = ArchiveClaim { operation: *late.as_bytes(), ..claim };
    let entry = state::registry().lookup(&scope, late).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    assert!(entry.claim_archive(late_claim, || !name(late, "archive").exists()).is_err());
    assert!(name(late, "archive").exists());
    assert_eq!(post(late, hash, target_hash, issue(late_claim, now)?).await?.status(), StatusCode::CONFLICT);
    assert_ne!(send_frame(true, late, hash, target_hash, issue(late_claim, now)?).await?.status(), StatusCode::OK);
    let query = ArchiveClaim { query_only: true, ..late_claim };
    assert_eq!(send_frame(true, late, hash, target_hash, issue(query, now)?).await?.status(), StatusCode::OK);
    let racing = stage(false)?;
    let racing_claim = ArchiveClaim {
        operation: *racing.as_bytes(),
        ..claim
    };
    let cancel_url = cancel(racing);
    let (claim_reply, cancel_reply) = tokio::join!(
        post(racing, hash, target_hash, issue(racing_claim, now)?),
        request(app, "DELETE", &cancel_url, Some(TOKEN))
    );
    let happened = claim_reply?.status() == StatusCode::OK;
    assert_eq!(
        cancel_reply?.status(),
        if happened {
            StatusCode::CONFLICT
        } else {
            StatusCode::ACCEPTED
        }
    );
    assert_eq!(name(racing, "archive").exists(), happened);
    assert!(!name(racing, "pending").exists());
    // Rotate the actual native owner credential. A signed frame for its previous
    // epoch cannot access an otherwise valid pending operation.
    let retired = stage(false)?;
    let stale = issue(
        ArchiveClaim {
            operation: *retired.as_bytes(),
            ..claim
        },
        now,
    )?;
    crate::runtime_http::v1::owner_control::revoke();
    tokio::time::timeout(WAIT, async {
        loop {
            if read_owner_credential(
                &crate::rpc_worker::runtime_dir(),
                &scope.instance.to_string(),
            )
            .is_ok_and(|next| next.as_bytes() != record.as_bytes())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_ne!(
        post(retired, hash, target_hash, stale).await?.status(),
        StatusCode::OK
    );
    assert!(name(retired, "pending").exists());
    state::archive_claim::retire_pending_archives();
    assert!(!name(retired, "pending").exists());
    assert!(state::Registry::default().lookup(&scope, retired).is_err());
    Ok(())
}

pub(super) const RESTART: &str = "runtime_http::v1::operations::tests::archive_claim_tests::restart_rejects_prior_registry_ticket";
const RESTART_PHASE: &str = "RSRS_ARCHIVE_RESTART_FIXTURE_PHASE";

pub(super) async fn check_case(test: &str, app: &Router, root: &Path, token: &Path) -> Result<()> {
    if test == RESTART {
        return check_restarted(app, root).await;
    }
    check(app, root, token).await
}

include!("archive_restart_tests.rs");
