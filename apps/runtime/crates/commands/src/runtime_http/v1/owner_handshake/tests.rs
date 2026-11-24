use super::*;
use axum::http::Request;

fn parts(bound: SocketAddr, proof: Option<&str>) -> Parts {
    let request = Request::post("/v1/auth/handshake")
        .header(header::HOST, bound.to_string())
        .header(header::CONTENT_TYPE, "application/json");
    let request = if let Some(proof) = proof {
        request.header(PROOF, proof)
    } else {
        request
    };
    request.body(()).expect("fixture request").into_parts().0
}

#[test]
fn redeemed_token_is_exact_and_never_issued_after_revocation() {
    let peer = SocketAddr::from(([127, 0, 0, 1], 4567));
    let bound = SocketAddr::from(([127, 0, 0, 1], 3434));
    let live = OwnerBinding {
        account: "synthetic".to_owned(),
        canonical_profile: std::env::temp_dir().join("respire-test-scope"),
        instance: uuid::Uuid::new_v4(),
        generation: 7,
        vault_epoch: uuid::Uuid::new_v4(),
    };
    let now = Instant::now();
    let scopes = ActionSet::try_from_unique(&[OwnerAction::ReadGrants]).expect("fixture scope");
    let mut registry =
        OwnerLeaseRegistry::for_verified_connection(live.clone(), scopes).expect("fixture owner");
    let proof = registry
        .issue(Some(&live), scopes, now)
        .expect("fixture proof");
    let input = br#"{"scopes":["grants:read"]}"#;
    let missing = exchange(
        &parts(bound, None),
        peer,
        bound,
        input,
        &mut registry,
        Some(&live),
        now,
    )
    .err()
    .expect("missing proof denied")
    .into_response();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let allowed = exchange(
        &parts(bound, Some(&proof)),
        peer,
        bound,
        input,
        &mut registry,
        Some(&live),
        now,
    )
    .expect("valid synthetic exchange");
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(allowed.headers()[header::CACHE_CONTROL], "no-store");
    let confirmed = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime")
        .block_on(axum::body::to_bytes(allowed.into_body(), 2048))
        .expect("fixture body");
    let value: serde_json::Value = serde_json::from_slice(&confirmed).expect("contract shape");
    assert_eq!(value["scopes"], serde_json::json!(["grants:read"]));
    assert_eq!(value["expires_in"], 300);
    assert_eq!(value["token_type"], "Bearer");
    assert_eq!(value.as_object().map(|object| object.len()), Some(4));
    let token = value["access_token"]
        .as_str()
        .expect("fixture token exists");
    assert!(registry
        .authorize(Some(&live), token, OwnerAction::ReadGrants, now)
        .is_ok());
    assert!(registry
        .authorize(Some(&live), token, OwnerAction::RevokeGrant, now)
        .is_err());

    let still_pending = registry
        .issue(Some(&live), scopes, now)
        .expect("fixture proof");
    registry.revoke();
    assert!(registry
        .authorize(Some(&live), token, OwnerAction::ReadGrants, now)
        .is_err());
    let denied = exchange(
        &parts(bound, Some(&still_pending)),
        peer,
        bound,
        input,
        &mut registry,
        Some(&live),
        now,
    )
    .err()
    .expect("revoked proof denied")
    .into_response();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn bounded_body_is_first_read_after_live_proof_on_test_router() -> anyhow::Result<()> {
    use axum::{
        extract::{ConnectInfo, Request},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let local = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    local.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bound = listener.local_addr()?;
        let live = OwnerBinding {
            account: "synthetic".to_owned(),
            canonical_profile: std::env::temp_dir().join("respire-test-scope"),
            instance: uuid::Uuid::new_v4(),
            generation: 1,
            vault_epoch: uuid::Uuid::new_v4(),
        };
        let scopes = ActionSet::try_from_unique(&[OwnerAction::ReadGrants]).expect("fixture scope");
        let mut state = OwnerLeaseRegistry::for_verified_connection(live.clone(), scopes).expect("fixture owner");
        let proof = state.issue(Some(&live), scopes, Instant::now()).expect("fixture proof");
        let state = Arc::new(Mutex::new(state));
        let app = Router::new().route(
            "/v1/auth/handshake",
            post(move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>, request: Request| {
                let state = Arc::clone(&state);
                let live = live.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    match collect_guarded(&parts, peer, bound, body, |candidate| {
                        state.lock().is_ok_and(|mut state| state.screen_proof(Some(&live), candidate, Instant::now()).is_ok())
                    }).await {
                        Ok(_) => StatusCode::NO_CONTENT.into_response(),
                        Err(response) => response,
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(crate::network::Listener(listener), app.into_make_service_with_connect_info::<crate::network::Peer>()).await
        });

        for (proof_header, chunk, terminal, status) in [
            (false, 0, false, 401),
            (true, MAX_HANDSHAKE_BODY, true, 204),
            (true, MAX_HANDSHAKE_BODY + 1, true, 413),
            (true, 1, false, 408),
        ] {
            let mut socket = tokio::net::TcpStream::connect(bound).await?;
            let field = if proof_header { format!("{PROOF}: {proof}\r\n") } else { String::new() };
            let mut wire = format!("POST /v1/auth/handshake HTTP/1.1\r\nHost: {bound}\r\n{field}Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            if chunk > 0 { wire.push_str(&format!("{chunk:x}\r\n{}\r\n", "x".repeat(chunk))); }
            if terminal { wire.push_str("0\r\n\r\n"); }
            socket.write_all(wire.as_bytes()).await?;
            let mut received = Vec::new();
            tokio::time::timeout(Duration::from_secs(4), socket.read_to_end(&mut received)).await??;
            let response = std::str::from_utf8(&received)?;
            assert_eq!(response.split_whitespace().nth(1).and_then(|word| word.parse().ok()), Some(status));
        }
        server.abort();
        Ok(())
    })
}

#[test]
fn guarded_socket_redeems_once_and_rechecks_owner_after_streaming() -> anyhow::Result<()> {
    use axum::{
        extract::{ConnectInfo, Request},
        routing::post,
        Router,
    };
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bound = listener.local_addr()?;
        let live = OwnerBinding {
            account: "synthetic-account".to_owned(),
            canonical_profile: std::env::temp_dir().join("respire-test-scope"),
            instance: uuid::Uuid::new_v4(),
            generation: 1,
            vault_epoch: uuid::Uuid::new_v4(),
        };
        let scopes = ActionSet::try_from_unique(&[OwnerAction::ReadGrants]).expect("fixture scope");
        let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), scopes).expect("fixture owner");
        let first = registry.issue(Some(&live), scopes, Instant::now()).expect("fixture proof");
        let second = registry.issue(Some(&live), scopes, Instant::now()).expect("fixture proof");
        let registry = Arc::new(Mutex::new(registry));
        let saw_second_preflight = Arc::new(tokio::sync::Notify::new());
        let owned_registry = Arc::clone(&registry);
        let notified = Arc::clone(&saw_second_preflight);
        let second_expected = second.clone();
        let app = Router::new().route("/v1/auth/handshake", post(
            move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>, request: Request| {
                let live = live.clone();
                let registry = Arc::clone(&owned_registry);
                let signal = Arc::clone(&notified);
                let expected = second_expected.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    exchange_after_preflight(&parts, peer, bound, body, |candidate| {
                        let accepted = registry.lock().is_ok_and(|mut registry| {
                            registry.screen_proof(Some(&live), candidate, Instant::now()).is_ok()
                        });
                        if accepted && candidate == expected {
                            signal.notify_one();
                        }
                        accepted
                    }, |bytes| {
                        let mut registry = registry.lock().map_err(|_| unauthorized())?;
                        exchange(&parts, peer, bound, bytes, &mut registry, Some(&live), Instant::now())
                    }).await
                }
            }
        ));
        let server = tokio::spawn(async move {
            axum::serve(crate::network::Listener(listener), app.into_make_service_with_connect_info::<crate::network::Peer>()).await
        });

        let body = br#"{"scopes":["grants:read"]}"#;
        let head = |proof: &str| format!(
            "POST /v1/auth/handshake HTTP/1.1\r\nHost: {bound}\r\n{PROOF}: {proof}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()
        );
        for (proof, expected, interrupt) in [(&first, 200, false), (&first, 401, false), (&second, 401, true)] {
            let mut socket = tokio::net::TcpStream::connect(bound).await?;
            socket.write_all(head(proof).as_bytes()).await?;
            if interrupt {
                tokio::time::timeout(Duration::from_millis(750), saw_second_preflight.notified()).await?;
                registry.lock().expect("fixture registry").revoke();
            }
            socket.write_all(body).await?;
            let mut received = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut received)).await??;
            let reply = std::str::from_utf8(&received)?;
            let (headers, json) = reply.split_once("\r\n\r\n").ok_or_else(|| anyhow::anyhow!("missing reply body"))?;
            assert_eq!(headers.split_whitespace().nth(1).and_then(|word| word.parse().ok()), Some(expected));
            let value: serde_json::Value = serde_json::from_str(json)?;
            assert_eq!(value.get("access_token").is_some(), expected == 200);
            if expected == 200 { assert!(headers.contains("cache-control: no-store")); }
        }
        server.abort();
        Ok(())
    })
}

#[test]
fn held_owner_lock_cannot_extend_proof_or_request_deadlines() {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex,
    };
    use std::time::Duration;

    for request_expired in [false, true] {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);
        let live = OwnerBinding {
            account: "synthetic".to_owned(),
            canonical_profile: std::env::temp_dir().join("respire-owner-deadline"),
            instance: uuid::Uuid::new_v4(),
            generation: 1,
            vault_epoch: uuid::Uuid::new_v4(),
        };
        let scopes = ActionSet::try_from_unique(&[OwnerAction::ReadGrants]).expect("fixture scope");
        let record = [1_u8; 48];
        let proof =
            respire_spawn::owner_credential::proof::issue(&record, 1, 1000).expect("fixture proof");
        let mut state = OwnerLeaseRegistry::for_verified_connection(live.clone(), scopes)
            .expect("fixture owner");
        assert!(state
            .screen_signed(Some(&live), &record, proof.as_str(), 31_000)
            .is_ok());
        let state = Mutex::new(state);
        let hold = state.lock().expect("fixture lock");
        let wall = AtomicU64::new(31_000);
        let offset = AtomicU64::new(0);
        let (attempt, waiting) = mpsc::sync_channel::<()>(0);
        let denied = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                attempt.send(()).expect("fixture wait notification");
                let mut state = state.lock().expect("fixture lock released");
                let (utc, now) = fenced_sample(
                    deadline,
                    || started + Duration::from_millis(offset.load(Ordering::Acquire)),
                    || Some(wall.load(Ordering::Acquire)),
                )?;
                state
                    .exchange_signed(Some(&live), &record, proof.as_str(), scopes, utc, now)
                    .map_err(|_| unauthorized())
            });
            waiting.recv().expect("fixture wait notification");
            if request_expired {
                offset.store(100, Ordering::Release);
            } else {
                wall.store(31_001, Ordering::Release);
            }
            drop(hold);
            worker
                .join()
                .expect("fixture worker")
                .err()
                .expect("no late lease")
        });
        let expected = if request_expired {
            StatusCode::REQUEST_TIMEOUT
        } else {
            StatusCode::UNAUTHORIZED
        };
        assert_eq!(denied.into_response().status(), expected);
        if request_expired {
            // The detached request did not consume an otherwise live proof.
            let mut state = state.lock().expect("fixture lock");
            assert!(state
                .exchange_signed(
                    Some(&live),
                    &record,
                    proof.as_str(),
                    scopes,
                    31_000,
                    started
                )
                .is_ok());
        }
    }
}
