/// Concurrent clients must preserve every successful event exactly once.
#[test]
fn concurrent_same_day_client_commands_preserve_each_successful_addition_once() -> anyhow::Result<()>
{
    let mut fixture = Fixture::new()?;
    fixture.start()?;
    let mut ids = Vec::new();
    for pair in 0..5 {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for index in 0..2 {
            let marker = format!("pair-{pair}-event-{index}");
            let mut command = fixture.business_command(&[
                "--client-only",
                "--json",
                "remember",
                &marker,
                "--importance",
                "trivial",
            ])?;
            let barrier = std::sync::Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                command.output()
            }));
        }
        for worker in workers {
            let output = worker
                .join()
                .map_err(|_| anyhow::anyhow!("synthetic client panicked"));
            fixture.verify_business_owner()?;
            let value = Fixture::result(output??)?;
            ids.push(
                value["summary"]["id"]
                    .as_str()
                    .context("synthetic response string missing")?
                    .to_owned(),
            );
        }
    }
    assert!(ids.iter().all(|id| id == &ids[0]));
    let saved = fixture.show(&ids[0])?;
    let content = saved["content"]
        .as_str()
        .context("synthetic response string missing")?;
    for pair in 0..5 {
        for index in 0..2 {
            assert_eq!(
                content
                    .matches(&format!("pair-{pair}-event-{index}"))
                    .count(),
                1
            );
        }
    }
    // Exercise the real admission ledger and encrypted business effects together.
    let endpoint = format!("http://127.0.0.1:{}/api/rpc", fixture.port);
    let health: Value = ureq::get(&format!("http://127.0.0.1:{}/api/health", fixture.port))
        .timeout(Duration::from_secs(5)).call()?.into_json()?;
    let scope = json!({"account_id": health["account_id"], "instance_id": health["instance_id"],
        "generation": health["generation"]});
    let original = json!({"v":1, "id":"same-key-first", "method":"cli.exec",
        "operation_id":"same-key-business-effect", "idempotency_key":"same-key-business-effect",
        "scope":scope, "args":["--json", "remember", "idempotent-rpc-marker",
            "--title", "Idempotent RPC effect", "--importance", "important", "--force"]});
    let send = |request: Value| -> anyhow::Result<(u16, Value)> {
        let response = match ureq::post(&endpoint).timeout(Duration::from_secs(120)).send_json(request) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(error.into()),
        };
        Ok((response.status(), response.into_json()?))
    };
    let post = |request: Value| -> anyhow::Result<Value> {
        let request_id = request["id"].clone();
        let (status, reply) = send(request)?;
        assert_eq!(status, 200);
        assert_eq!(reply["v"], 1);
        assert_eq!(reply["id"], request_id);
        assert_eq!(reply["pid"], health["pid"]);
        Ok(reply)
    };
    // Use an actually completed operation as the changed parent, not a made-up ID.
    let parent_operation = "fingerprint-known-parent";
    let mut parent = original.clone();
    parent["id"] = json!("fingerprint-parent-request");
    parent["operation_id"] = json!(parent_operation);
    parent["idempotency_key"] = json!(parent_operation);
    parent["args"][2] = json!("fingerprint-parent-marker");
    parent["args"][4] = json!("Fingerprint parent effect");
    let parent = post(parent)?;
    assert_eq!(parent["ok"], true, "{parent}");
    assert_eq!(parent["operation_id"], parent_operation);
    assert!(parent["envelope"]["summary"]["id"].as_str().is_some_and(|id| !id.is_empty()));
    let before = fixture.store()?.all(true)?.len();
    let barrier = std::sync::Barrier::new(2);
    let replies = std::thread::scope(|threads| -> anyhow::Result<Vec<Value>> {
        let mut workers = Vec::new();
        for index in 0..2 {
            let mut request = original.clone();
            request["id"] = json!(format!("same-key-concurrent-{index}"));
            let post = &post;
            let barrier = &barrier;
            workers.push(threads.spawn(move || {
                barrier.wait();
                post(request)
            }));
        }
        workers.into_iter().map(|worker| worker.join()
            .map_err(|_| anyhow::anyhow!("idempotent RPC worker panicked"))?)
            .collect()
    })?;
    assert!(replies.iter().any(|reply| reply["ok"] == true));
    for reply in &replies {
        assert!(reply["ok"] == true || reply["code"] == "request_in_progress", "{reply}");
    }
    let mut replay = original.clone();
    replay["id"] = json!("same-key-completed-replay");
    let replay = post(replay)?;
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["scope"], scope);
    let stored_id = replay["envelope"]["summary"]["id"].as_str()
        .context("idempotent business result ID missing")?;
    assert_eq!(fixture.show(stored_id)?["content"], "idempotent-rpc-marker");
    assert_eq!(fixture.store()?.all(true)?.len(), before + 1,
        "concurrent same-fingerprint requests committed more than one record");
    let mut conflict = original.clone();
    conflict["id"] = json!("same-key-changed-arguments");
    conflict["args"][2] = json!("conflicting-rpc-marker");
    let rejected = post(conflict)?;
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["code"], "idempotency_conflict");
    assert_eq!(fixture.store()?.all(true)?.len(), before + 1,
        "conflicting or replayed requests committed another record");
    assert_eq!(fixture.show(stored_id)?["content"], "idempotent-rpc-marker");
    let result = |operation: &str, id: &str| post(json!({"v":1,"id":id,
        "method":"runtime.result","scope":scope,"args":[operation]}));
    let receipt = result("same-key-business-effect", "fingerprint-original-receipt")?;
    assert_eq!(receipt["envelope"]["summary"]["state"], "completed");
    let retained = receipt["envelope"]["summary"]["response"].clone();
    let installation = PathBuf::from(health["installation_root"].as_str()
        .context("fingerprint installation root missing")?);
    let protected_files = [installation.join("embedding-profiles-v1.json"), installation.join("client.json")];
    let read_protected = || -> anyhow::Result<Vec<Option<Vec<u8>>>> {
        protected_files.iter().map(|path| match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }).collect()
    };
    let protected_before = read_protected()?;
    let unchanged = || -> anyhow::Result<()> {
        assert_eq!(fixture.store()?.all(true)?.len(), before + 1);
        assert_eq!(fixture.show(stored_id)?["content"], "idempotent-rpc-marker");
        assert_eq!(read_protected()?, protected_before);
        assert_eq!(result("same-key-business-effect", "fingerprint-receipt-check")?
            ["envelope"]["summary"]["response"], retained);
        Ok(())
    };
    let mut changes = Vec::new();
    for (field, value, expected) in [
        ("v", json!(2), "protocol_version_mismatch"),
        // index.prepare is supported, but admission must reject before kick_index.
        ("method", json!("index.prepare"), "idempotency_conflict"),
        ("parent_operation_id", json!(parent_operation), "idempotency_conflict"),
        ("lifecycle", json!("coordinator-v1"), "idempotency_conflict"),
    ] {
        let mut request = original.clone();
        request[field] = value;
        changes.push((request, expected));
    }
    for (field, value) in [
        ("account_id", json!("different-fingerprint-account")),
        ("instance_id", json!("different-fingerprint-instance")),
        ("generation", json!(scope["generation"].as_u64().context("fingerprint generation missing")? + 1)),
    ] {
        let mut request = original.clone();
        request["scope"][field] = value;
        changes.push((request, "scope_mismatch"));
    }
    // cli.exec fingerprints the argument vector, not CLI flag semantics.
    let mut reordered = original.clone();
    reordered["args"] = json!(["--json","remember","idempotent-rpc-marker",
        "--title","Idempotent RPC effect","--force","--importance","important"]);
    changes.push((reordered, "idempotency_conflict"));
    for (index, (mut request, expected)) in changes.into_iter().enumerate() {
        request["id"] = json!(format!("fingerprint-dimension-{index}"));
        let rejected = post(request)?;
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert_eq!(rejected["code"], expected);
        unchanged()?;
    }
    // Schema rejection has an HTTP error body, not a fabricated RPC envelope.
    for (index, mut malformed) in [original.clone(), original.clone()].into_iter().enumerate() {
        if index == 0 { malformed["schema_version"] = json!(1); }
        else { malformed["scope"]["generation"] = json!("not-an-integer"); }
        let (status, rejected) = send(malformed)?;
        assert_eq!(status, 400);
        assert!(rejected["error"].as_str().is_some());
        assert!(rejected.get("envelope").is_none());
        unchanged()?;
    }
    // This is failure replay: Configure rejects absent profiles before registry I/O.
    let canonical = json!({"v":1,"id":"canonical-json-first","method":"model.configure",
        "operation_id":"canonical-json-operation","idempotency_key":"canonical-json-operation",
        "scope":scope,"args":[r#"{"model_profile":null,"backend_profile":null}"#]});
    let failed = post(canonical.clone())?;
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["code"], "model_profile_failed");
    unchanged()?;
    let mut equivalent = canonical.clone();
    equivalent["id"] = json!("canonical-json-reordered");
    equivalent["args"][0] = json!("{ \"backend_profile\": null, \"model_profile\": null }");
    let replayed = post(equivalent)?;
    assert_eq!(replayed["ok"], false);
    assert_eq!(replayed["code"], "model_profile_failed");
    unchanged()?;
    let canonical_receipt = result("canonical-json-operation", "canonical-json-receipt")?;
    assert_eq!(canonical_receipt["envelope"]["summary"]["state"], "failed");
    assert_eq!(canonical_receipt["envelope"]["summary"]["response"], failed);
    assert_eq!(canonical_receipt["envelope"]["summary"]["response"]["id"], "canonical-json-first",
        "normalized JSON must replay without overwriting the first receipt");
    let mut different = canonical;
    different["id"] = json!("canonical-json-structurally-different");
    different["args"][0] = json!(r#"{"model_profile":null}"#);
    let rejected = post(different)?;
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["code"], "idempotency_conflict");
    unchanged()?;
    support::legacy_receipt("diary_identity::concurrent_same_day_client_commands_preserve_each_successful_addition_once", "candidate_runtime")?;
    Ok(())
}

/// Old appends copied the routing owner into the encrypted author without changing ID.
#[test]
fn old_owner_normalized_diary_keeps_its_author_after_public_title_edit() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let session_path = fixture.home.path().join("data/session.json");
    let mut session: Value = serde_json::from_slice(&std::fs::read(&session_path)?)?;
    session["user"] = json!("synthetic-account-owner");
    std::fs::write(session_path, serde_json::to_vec(&session)?)?;
    let day = day();
    let id = track_id("synthetic-diary", &day);
    // This is the payload produced by dev.7 after an owner-normalizing pull + append.
    let legacy = entry(
        &id,
        "synthetic-account-owner",
        &format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}"),
        "old-owner-event",
    );
    fixture.seed(&legacy, false)?;
    fixture.seed(
        &entry(
            "old-owner-legacy",
            "synthetic-account-owner",
            &format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}"),
            "old-owner-legacy-event",
        ),
        false,
    )?;
    fixture.start()?;
    fixture.run(&[
        "update",
        &id,
        "--title",
        "Renamed old owner-normalized diary",
    ])?;
    fixture.remember("new-owner-event")?;
    let saved = fixture.show(&id)?;
    assert_eq!(saved["user"], "synthetic-account-owner");
    assert_eq!(saved["title"], "Renamed old owner-normalized diary");
    let content = saved["content"]
        .as_str()
        .context("synthetic response string missing")?;
    for marker in [
        "old-owner-event",
        "old-owner-legacy-event",
        "new-owner-event",
    ] {
        assert_eq!(content.matches(marker).count(), 1);
    }
    support::legacy_receipt("diary_identity::old_owner_normalized_diary_keeps_its_author_after_public_title_edit", "candidate_runtime")?;
    Ok(())
}

/// Different payload authors cannot borrow a canonical ID or a matching wire owner.
#[test]
fn unrelated_canonical_author_fails_without_writes_or_private_output() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let day = day();
    fixture.seed_with_owner(
        &entry(
            &track_id("synthetic-diary", &day),
            "foreign-private-author",
            "Renamed foreign track",
            "foreign-private-event",
        ),
        false,
        "synthetic-diary",
    )?;
    fixture.seed(
        &entry(
            "legacy-must-survive",
            "synthetic-diary",
            &format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}"),
            "legacy-live-event",
        ),
        false,
    )?;
    fixture.start()?;
    fixture.assert_author_rejection()?;
    support::legacy_receipt("diary_identity::unrelated_canonical_author_fails_without_writes_or_private_output", "candidate_runtime")?;
    Ok(())
}

/// Environment fallback keys must not inherit the failed local session's owner alias.
#[test]
fn environment_key_fallback_does_not_adopt_unlocked_profile_alias() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let session_path = fixture.home.path().join("data/session.json");
    let mut session: Value = serde_json::from_slice(&std::fs::read(&session_path)?)?;
    session["user"] = json!("foreign-private-author");
    // Preserve a readable profile, but force local unlock to fail before env fallback.
    session["wrapped_urk"] = json!("rsrs:v1:00");
    std::fs::write(session_path, serde_json::to_vec(&session)?)?;
    let salt = "13".repeat(16);
    let password = "synthetic-fallback-password";
    let account_secret = "synthetic-fallback-secret";
    let kek = crypto::derive_rsrs_password_kek(password, account_secret, &salt)?;
    let (nonce, wrapped) = crypto::wrap_key(&fixture.keys.urk, &kek)?;
    fixture.extra_env = vec![
        ("RSRS_PASS", password.into()),
        ("RSRS_SECRET", account_secret.into()),
        ("RSRS_KDF_SALT", salt),
        (
            "RSRS_WRAPPED_URK",
            format!("{}{wrapped}", crypto::RSRS_PREFIX),
        ),
        ("RSRS_URK_NONCE", nonce),
    ];
    let day = day();
    let id = track_id("synthetic-diary", &day);
    fixture.seed(
        &entry(
            &id,
            "foreign-private-author",
            "Renamed unrelated profile",
            "foreign-private-event",
        ),
        false,
    )?;
    fixture.start()?;
    // A successful full read proves the environment keys can actually decrypt this vault.
    assert_eq!(fixture.show(&id)?["user"], "foreign-private-author");
    fixture.assert_author_rejection()?;
    support::legacy_receipt("diary_identity::environment_key_fallback_does_not_adopt_unlocked_profile_alias", "candidate_runtime")?;
    Ok(())
}

/// Important remember retains full bodies, synchronized fields, and outbox bytes.
#[test]
fn important_remember_pending_candidates_preserve_ciphertext_and_outbox() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let content = "synthetic orchard irrigation moisture sensor maintenance ".repeat(48);
    let mut original = entry(
        "important-candidate",
        "synthetic-diary",
        "Orchard maintenance",
        &content,
    );
    original.importance = "important".into();
    original.parent_id.clear();
    fixture.seed(&original, false)?;
    fixture.start()?;
    let before = serde_json::to_value(fixture.store()?.all(true)?)?;
    let boundary = fixture.store()?.outgoing_boundary()?;
    let outgoing = serde_json::to_value(fixture.store()?.outgoing()?)?;
    anyhow::ensure!(fixture.owns_runtime()?, "synthetic runtime is not ready");
    let output = fixture.business_output(&[
        "--client-only",
        "--json",
        "remember",
        &content,
        "--importance",
        "important",
    ])?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "pending candidates use the public pending exit code"
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["errors"], serde_json::json!([]));
    assert_eq!(result["status"], "pending");
    assert_eq!(result["summary"]["written"], false);
    assert_eq!(result["summary"]["state"], "candidates");
    assert_eq!(result["summary"]["merge_count"], 1);
    assert!(result["actions"]
        .as_array()
        .context("candidate actions missing")?
        .iter()
        .any(|action| action
            .as_str()
            .is_some_and(|text| text.contains("--merge-ids \"important-candidate\""))));
    assert_eq!(fixture.show("important-candidate")?["content"], content);
    assert!(
        before == serde_json::to_value(fixture.store()?.all(true)?)?,
        "candidate lookup mutated synchronized record fields"
    );
    assert_eq!(boundary, fixture.store()?.outgoing_boundary()?);
    assert!(
        outgoing == serde_json::to_value(fixture.store()?.outgoing()?)?,
        "candidate lookup mutated outbox"
    );
    support::legacy_receipt("diary_identity::important_remember_pending_candidates_preserve_ciphertext_and_outbox", "candidate_runtime")?;
    Ok(())
}


// Hold a real provider request until the native stop assertions release it.
struct HeldClassifyProvider {
    endpoint: String,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    received: std::sync::mpsc::Receiver<()>,
    worker: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl HeldClassifyProvider {
    fn new() -> anyhow::Result<Self> {
        Self::with_response(None)
    }

    fn with_response(response: Option<Value>) -> anyhow::Result<Self> {
        use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc};
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|error| anyhow::anyhow!("loopback provider bind: {error}"))?;
        let endpoint = format!("http://{}/classify", server.server_addr());
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (send, received) = std::sync::mpsc::channel();
        let (count, released, stopped) = (calls.clone(), release.clone(), stop.clone());
        let worker = std::thread::spawn(move || -> anyhow::Result<()> {
            let mut held = Vec::<tiny_http::Request>::new();
            loop {
                if released.load(Ordering::Acquire) || stopped.load(Ordering::Acquire) {
                    for request in held.drain(..) {
                        if let Some(response) = response.as_ref() {
                            request.respond(tiny_http::Response::from_string(serde_json::to_string(response)?)
                                .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json")
                                    .map_err(|_| anyhow::anyhow!("provider content-type header"))?))
                                .context("send held loopback HTTP 200")?;
                        } else {
                            // Keep the existing terminal failure cases unchanged.
                            request.respond(tiny_http::Response::empty(401))
                                .context("send terminal loopback HTTP 401")?;
                        }
                    }
                }
                if stopped.load(Ordering::Acquire) { break; }
                let Some(request) = server.recv_timeout(Duration::from_millis(100))? else { continue };
                count.fetch_add(1, Ordering::AcqRel);
                anyhow::ensure!(request.method() == &tiny_http::Method::Post
                    && request.url() == "/classify", "unexpected provider request");
                anyhow::ensure!(request.headers().iter().any(|header|
                    header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer synthetic-native-stop-key"),
                    "synthetic provider authentication missing");
                held.push(request);
                let _ = send.send(());
            }
            Ok(())
        });
        Ok(Self { endpoint, calls, release, stop, received, worker: Some(worker) })
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.release.store(true, std::sync::atomic::Ordering::Release);
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| anyhow::anyhow!("loopback provider panicked"))??;
        }
        Ok(())
    }
}

impl Drop for HeldClassifyProvider {
    fn drop(&mut self) {
        self.release.store(true, std::sync::atomic::Ordering::Release);
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {},
                other => eprintln!("loopback provider cleanup failed: {other:?}"),
            }
        }
    }
}

fn stop_native_listener_open(port: u16, _timeout: Duration) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        // Windows can time out connecting to a closed loopback port; query the OS table instead.
        respire_commands::net_rpc::windows_listener_pid_on(port).map(|pid| pid.is_some())
    }
    #[cfg(not(windows))]
    {
        match std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), _timeout) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn stop_native_post(url: &str, request: &Value) -> anyhow::Result<(u16, Value)> {
    let response = match ureq::AgentBuilder::new().try_proxy_from_env(false).build()
        .post(url).timeout(Duration::from_secs(90))
        .set("X-Respire-Lifecycle", "coordinator-v1").send_json(request.clone()) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(error.into()),
    };
    Ok((response.status(), response.into_json()?))
}

