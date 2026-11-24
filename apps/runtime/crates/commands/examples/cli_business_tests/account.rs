//! Real CLI account/space calls with Runtime-owned business fixtures and decryption.
use super::support;
use std::process::{Command, Output};
use std::time::Duration;

fn runtime_bin() -> std::io::Result<std::path::PathBuf> {
    support::runtime_bin()
}

fn isolated_command(models: Option<&std::path::Path>) -> std::io::Result<Command> {
    let mut command = Command::new(match support::legacy_cli().map_err(std::io::Error::other)? { Some(binary)=>binary, None=>support::cli_bin()? });
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("RSRS_") || name.starts_with("ONEMEMORY_") || name.starts_with("RESPIRE_")
        }) {
            command.env_remove(name);
        }
    }
    command.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    if let Some(models) = models {
        command.env("RSRS_M3_DIR", models).env("RSRS_ENGINE", "cpu");
    }
    Ok(command)
}

fn runtime_health(port: u16, stage: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let url = format!("http://127.0.0.1:{port}/api/health");
    let health: serde_json::Value = ureq::get(&url).set("X-Respire-Health", "runtime-v1")
        .timeout(Duration::from_secs(5)).call()?.into_json()?;
    if std::env::var("RSRS_TEST_EXPECT_CLIENT2_HOST").as_deref() == Ok("1") {
        let legacy: serde_json::Value = ureq::get(&url)
            .timeout(Duration::from_secs(5)).call()?.into_json()?;
        use sha2::{Digest, Sha256};
        let root = health["installation_root"].as_str().ok_or("Runtime installation root missing")?;
        let expected = std::path::Path::new(root).join("bin")
            .join(if cfg!(windows) { "rsrs.exe" } else { "rsrs" }).canonicalize()?;
        let same_cli = Sha256::digest(std::fs::read(&expected)?)
            == Sha256::digest(std::fs::read(support::cli_bin()?)?);
        let pid = u32::try_from(legacy["pid"].as_u64().ok_or("legacy host PID missing")?)?;
        let actual_image: std::path::PathBuf = {
            #[cfg(target_os = "linux")]
            { std::fs::read_link(format!("/proc/{pid}/exe"))?.canonicalize()? }
            #[cfg(not(target_os = "linux"))]
            { return Err("this opt-in account host fixture requires native Linux".into()); }
        };
        let live = respire_commands::net_rpc::process_is_running(pid)?;
        let valid = same_cli && live && actual_image == expected
            && legacy["compatibility_host"] == "client2-v0256"
            && legacy["backend_instance_id"] == health["instance_id"]
            && legacy["pid"] != health["pid"]
            && legacy["data_dir"] == health["data_dir"]
            && legacy["installation_root"] == health["installation_root"]
            && legacy["exe"].as_str().is_some_and(|path| std::path::Path::new(path)
                .canonicalize().is_ok_and(|path| path == expected));
        eprintln!("client2-account-host {}", serde_json::json!({"stage":stage, "passed":valid}));
        if !valid {
            return Err("legacy Client2 host identity was not preserved across profile maintenance".into());
        }
    }
    Ok(health)
}

#[test]
fn space_create_list_use_remove() -> Result<(), Box<dyn std::error::Error>> {
    if support::legacy_cli()?.is_some() {
        return legacy_space_contract("account::space_create_list_use_remove");
    }
    use serde_json::Value;

    let home = tempfile::tempdir()?;
    let root = home.path().join(".rsrs");
    std::fs::create_dir(&root)?;
    if support::legacy_cli()?.is_some() {
        support::stage_legacy_cli(&root)?;
    } else {
        support::install_runtime_fixture(&root)?;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let invoke = |args: &[&str]| -> std::io::Result<Output> {
        isolated_command(None)?
            .arg("--json")
            .args(args)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("RSRS_DATA_DIR", &root)
            .env("RSRS_BIN_DIR", root.join("bin"))
            .env("RSRS_M3_DIR", root.join("absent-model"))
            .env("RSRS_RPC_PORT", port.to_string())
            .env("RSRS_NO_AUTOSYNC", "1")
            .env("RSRS_UPDATE_CHECK", "0")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env_remove("RSRS_RUNTIME_BIN")
            .output()
    };
    let success = |args: &[&str]| -> Result<Value, Box<dyn std::error::Error>> {
        let output = invoke(args)?;
        assert!(
            output.status.success(),
            "space command {args:?} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };

    assert!(!invoke(&["space", "create", "main"])?.status.success());
    let created = success(&["space", "create", "team_a"])?;
    assert_eq!(created["summary"]["name"], "team_a");
    let listed = success(&["space", "list"])?;
    let spaces = listed["details"]["spaces"]
        .as_array()
        .ok_or("space listing must contain its profile array")?;
    assert!(spaces.iter().any(|space| space["name"] == "team_a"));
    assert!(!invoke(&["space", "use", "missing"])?.status.success());
    let used = success(&["space", "use", "main"])?;
    assert_eq!(used["summary"]["name"], "main");
    success(&["space", "use", "team_a"])?;
    assert!(
        !invoke(&["space", "remove", "team_a", "--yes"])?.status.success(),
        "cannot remove current space"
    );
    success(&["space", "use", "main"])?;
    success(&["space", "remove", "team_a", "--yes"])?;
    Ok(())
}

#[test]
fn spaces_coordinate_with_the_owned_runtime() -> Result<(), Box<dyn std::error::Error>> {
    if support::legacy_cli()?.is_some() {
        return legacy_space_contract("account::spaces_coordinate_with_the_owned_runtime");
    }
    use serde_json::{json, Value};
    let home = tempfile::tempdir()?;
    let library = home.path().join(".rsrs");
    std::fs::create_dir(&library)?;
    if support::legacy_cli()?.is_some() {
        support::stage_legacy_cli(&library)?;
    } else {
        support::install_runtime_fixture(&library)?;
    }
    let source_runtime = runtime_bin()?;
    let version = source_runtime.parent().and_then(std::path::Path::file_name)
        .ok_or("Runtime fixture version directory missing")?;
    let installed_runtime = library.join("bin/runtime").join(version)
        .join(source_runtime.file_name().ok_or("Runtime fixture file name missing")?);
    let config = library.join("client.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({
            "data_dir":library,"api_base":"https://fixture.invalid","custom":"preserve"
        }))?,
    )?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let invoke = |args: &[&str], expected: i32| -> Result<Value, Box<dyn std::error::Error>> {
        let output = isolated_command(None)?
            .arg("--json")
            .args(args)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("RSRS_DATA_DIR", &library)
            .env("RSRS_BIN_DIR", library.join("bin"))
            .env("RSRS_RPC_PORT", port.to_string())
            .env("RSRS_NO_AUTOSYNC", "1")
            .env("RSRS_UPDATE_CHECK", "0")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env_remove("RSRS_RPC_URL")
            .env_remove("RSRS_CLIENT_ONLY")
            .env_remove("RSRS_NO_AUTOSTART")
            .env_remove("RSRS_RUNTIME_WORKER")
            .output()?;
        let stdout = String::from_utf8(output.stdout)?;
        let value: Value = serde_json::from_str(stdout.trim())?;
        // Headless systems can join successfully without an available OS keyring.
        let keyring_warning = expected == 0
            && args.first() == Some(&"space")
            && args.get(1) == Some(&"join")
            && output.status.code() == Some(2)
            && value["status"] == "warn"
            && value["details"]["keyring"] == false;
        if output.status.code() != Some(expected) && !keyring_warning {
            eprintln!("account-response-evidence {}", json!({
                "stage": "space_command", "reason_code": value["summary"]["reason"],
                "error_type": value["details"]["error_type"]
            }));
            return Err(format!(
                "owned space command {args:?} exited {:?}: {stdout} {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(value)
    };
    let health = |profile: &std::path::Path, stage: &str| -> Result<u64, Box<dyn std::error::Error>> {
        let status = runtime_health(port, "space-profile")
            .map_err(|error| format!("owned space health stage={stage}: {error}"))?;
        let executable = status["exe"].as_str().ok_or("Runtime executable missing")?;
        if status["data_dir"].as_str().map(std::path::Path::new) != Some(profile)
            || std::path::Path::new(executable).canonicalize()? != installed_runtime.canonicalize()?
        {
            return Err(format!("runtime profile readback failed: {status}").into());
        }
        // Exercise request scope without autostart masking a missed restart.
        invoke(&["--client-only", "status"], 0)?;
        status["pid"]
            .as_u64()
            .ok_or_else(|| "runtime PID missing".into())
    };
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // Start the copied pinned managed installation, not a shared Runtime override.
        invoke(&["runtime", "start"], 0)?;
        invoke(&["status"], 0)?;
        let original_pid = health(&library, "initial-status")?;
        invoke(&["space", "list"], 0)?;
        assert_eq!(
            health(&library, "after-list")?,
            original_pid,
            "listing host profiles must preserve the owned runtime"
        );
        let before = std::fs::read(&config)?;
        for args in [
            vec!["space", "join", "BADCODE"],
            vec!["space", "join", "--code", "BADCODE"],
        ] {
            let rejected = invoke(&args, 2)?;
            assert_eq!(rejected["summary"]["reason"], "invalid_input");
            assert_eq!(rejected["details"]["error_type"], "user");
            assert_eq!(
                health(&library, "after-invalid-invite")?,
                original_pid,
                "bad invite must not stop the current runtime"
            );
            assert_eq!(std::fs::read(&config)?, before);
        }
        for readonly in [false, true] {
            let server =
                tiny_http::Server::http("127.0.0.1:0").map_err(|error| error.to_string())?;
            let name = if readonly {
                "joined-readonly"
            } else {
                "joined-writable"
            };
            let user = format!("join-fixture-{}", uuid::Uuid::new_v4());
            let super_password = respire_app::memory::crypto::generate_secret_key();
            let salt = respire_app::memory::crypto::random_hex(16);
            let kek = respire_app::memory::crypto::derive_kek_v4(&super_password, &salt)?;
            let (nonce, wrapped) = respire_app::memory::crypto::wrap_key(&[42_u8; 32], &kek)?;
            let vault = json!({"version":4,"kdf_salt":salt,"wrapped_urk":wrapped,"urk_nonce":nonce});
            let member_token = uuid::Uuid::new_v4().to_string();
            let payload = json!({"space":name,"addr":format!("http://{}",server.server_addr()),"user":user,"token":member_token,"super":super_password,"readonly":readonly});
            let code = format!(
                "1mem-invite:{}",
                serde_json::to_vec(&payload)?
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let expected_user = user.clone();
            let expected_authorization = format!("Bearer {member_token}");
            let worker = std::thread::spawn(move || -> Result<(), String> {
                for (path, body) in [("/api/self", json!({"user":expected_user})), ("/api/self/vault", vault)] {
                    let request = server
                        .recv_timeout(Duration::from_secs(20))
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| format!("join never fetched {path}"))?;
                    if request.method() != &tiny_http::Method::Get || request.url() != path
                        || !request.headers().iter().any(|header| {
                            header.field.equiv("Authorization")
                                && header.value.as_str() == expected_authorization.as_str()
                        })
                    {
                        return Err(format!("join did not authenticate its ordered GET {path}"));
                    }
                    request.respond(tiny_http::Response::from_string(body.to_string()))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            });
            let joined = invoke(&["space", "join", &code], if readonly { 2 } else { 0 });
            let served = worker.join().map_err(|_| "join fixture panicked".to_owned())
                .and_then(|result| result).map_err(|error| format!("join server: {error}"));
            respire_app::keystore::delete_super(&user);
            let joined = match (joined, served) {
                (Ok(joined), Ok(())) => joined,
                (Err(error), Ok(())) => return Err(error),
                (Ok(_), Err(error)) => return Err(error.into()),
                (Err(error), Err(server)) => return Err(format!("{error}; {server}").into()),
            };
            assert_eq!(
                joined["status"],
                if readonly || joined["details"]["keyring"] == false {
                    "warn"
                } else {
                    "ok"
                }
            );
            assert_eq!(joined["details"]["readonly"], readonly);
            assert_eq!(std::fs::read(&config)?, before);
            assert_ne!(health(&library, if readonly { "after-readonly-join" } else { "after-writable-join" })?, original_pid);
            let target = library.join("accounts").join(name);
            let session: Value =
                serde_json::from_slice(&std::fs::read(target.join("session.json"))?)?;
            assert_eq!(session["user"], user);
            if readonly {
                let agent: Value =
                    serde_json::from_slice(&std::fs::read(target.join("agent.json"))?)?;
                assert_eq!(agent["readonly_team"], true);
            }
            let pid = health(&library, "before-duplicate-join")?;
            invoke(&["space", "join", "--code", &code], 2)?;
            assert_eq!(
                health(&library, "after-duplicate-join")?,
                pid,
                "existing target must fail before stopping the runtime"
            );
        }
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| error.to_string())?;
        let payload = json!({"space":"rejected-join","addr":format!("http://{}",server.server_addr()),"user":"fixture-rejected","token":"revoked-token","super":"synthetic-super"});
        let code = format!(
            "1mem-invite:{}",
            serde_json::to_vec(&payload)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let worker = std::thread::spawn(move || -> Result<(), String> {
            let request = server
                .recv_timeout(Duration::from_secs(20))
                .map_err(|error| error.to_string())?
                .ok_or("rejected join never fetched the vault")?;
            request
                .respond(tiny_http::Response::empty(401))
                .map_err(|error| error.to_string())
        });
        let rejected = invoke(&["space", "join", &code], 1);
        let served = worker.join().map_err(|_| "rejected join fixture panicked".to_owned())
            .and_then(|result| result).map_err(|error| format!("join server: {error}"));
        let rejected = match (rejected, served) {
            (Ok(rejected), Ok(())) => rejected,
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(error)) => return Err(error.into()),
            (Err(error), Err(server)) => return Err(format!("{error}; {server}").into()),
        };
        assert_eq!(rejected["details"]["error_type"], "runtime");
        assert_eq!(std::fs::read(&config)?, before);
        assert!(!library.join("accounts/rejected-join").exists());
        let _before_create_pid = u32::try_from(health(&library, "after-rejected-join")?)?;
        #[cfg(windows)]
        let before_create_birth = respire_spawn::process_birth(_before_create_pid)?
            .ok_or("space create owner birth missing")?;
        let read_optional_session = |path: &std::path::Path| -> std::io::Result<Option<Vec<u8>>> {
            match std::fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        };
        let original_session = read_optional_session(&library.join("session.json"))?;
        let joined_writable = library.join("accounts/joined-writable");
        let joined_session = std::fs::read(joined_writable.join("session.json"))?;
        let mut original_config: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
        original_config.as_object_mut().ok_or("space config must be an object")?.remove("data_dir");
        invoke(&["space", "create", "probe"], 0)?;
        let probe = library.join("accounts/probe");
        assert!(probe.join("space_owner.json").is_file());
        let assert_idle_probe = || -> Result<(), Box<dyn std::error::Error>> {
            let mut selected: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
            let directory = selected["data_dir"].as_str().ok_or("space selected directory missing")?;
            if std::path::Path::new(directory).canonicalize()? != probe.canonicalize()? {
                return Err("empty space was not selected".into());
            }
            selected.as_object_mut().ok_or("space config must be an object")?.remove("data_dir");
            if selected != original_config {
                return Err("empty space changed unrelated configuration".into());
            }
            respire_app::login_transaction::verify_auth_identity(
                &library, &probe, &respire_protocol::HostAuthIdentity::Absent,
            )?;
            #[cfg(windows)]
            if respire_spawn::process_birth(_before_create_pid)? == Some(before_create_birth) {
                return Err("empty space retained the previous Runtime owner".into());
            }
            if probe.join("session.json").try_exists()?
                || read_optional_session(&library.join("session.json"))? != original_session
                || std::fs::read(joined_writable.join("session.json"))? != joined_session
                || std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200),
                ).is_ok()
                || library.join("runtime/endpoint.json").try_exists()?
            {
                return Err("empty space did not preserve idle ownership and session boundaries".into());
            }
            Ok(())
        };
        assert_idle_probe()?;
        let before = std::fs::read(&config)?;
        let blocked = invoke(&["space", "remove", "probe"], 2)?;
        assert_eq!(blocked["summary"]["reason"], "invalid_input");
        assert_eq!(std::fs::read(&config)?, before);
        let blocked = invoke(&["space", "remove", "probe", "--yes"], 2)?;
        assert_eq!(blocked["details"]["error_type"], "user");
        assert!(probe.is_dir());
        assert_idle_probe()?;
        invoke(&["space", "use", "joined-writable"], 0)?;
        invoke(&["runtime", "start"], 0)?;
        let resumed_pid = health(&joined_writable, "after-idle-create-return-authenticated")?;
        assert_ne!(resumed_pid, original_pid);
        use sha2::{Digest, Sha256};
        let joined: Value = serde_json::from_slice(&joined_session)?;
        let joined_account = format!("{:x}", Sha256::digest(
            joined["user"].as_str().ok_or("joined account missing")?.as_bytes(),
        ));
        respire_app::login_transaction::verify_auth_identity(&library, &joined_writable,
            &respire_protocol::HostAuthIdentity::Bound {
                account_id: joined_account.clone(),
                session_sha256: format!("{:x}", Sha256::digest(&joined_session)),
            })?;
        let resumed = runtime_health(port, "space-profile")?;
        if resumed["pid"] != resumed_pid || resumed["account_id"] != joined_account {
            return Err("resumed space health differs from its authenticated session".into());
        }
        invoke(&["space", "use", "main"], 0)?;
        health(&library, "after-use-main")?;
        let before = std::fs::read(&config)?;
        invoke(&["space", "use", "missing"], 2)?;
        assert_eq!(std::fs::read(&config)?, before);
        health(&library, "after-use-missing")?;
        invoke(&["space", "remove", "probe", "--yes"], 0)?;
        assert!(!probe.exists());
        health(&library, "after-remove")?;
        let preserved: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
        assert_eq!(preserved["api_base"], "https://fixture.invalid");
        assert_eq!(preserved["custom"], "preserve");
        Ok(())
    })();
    let cleanup = invoke(&["--runtime-internal", "--stop"], 0);
    result?;
    cleanup?;
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    Ok(())
}

fn authorization_failure_evidence(root: &std::path::Path, source: &std::path::Path, output: &Output) {
    use serde_json::{json, Value};

    fn allowed(value: &Value, choices: &[&str]) -> Value {
        match value.as_str().filter(|value| choices.contains(value)) {
            Some(value) => json!(value),
            None if value.is_null() => Value::Null,
            None => json!("redacted"),
        }
    }

    fn operation_id(value: &Value) -> Option<String> {
        let text = value.as_str()?;
        let id = uuid::Uuid::parse_str(text).ok()?;
        (id.get_version_num() == 4 && id.to_string() == text).then(|| id.to_string())
    }

    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let mut evidence = json!({
        "schema_version": 1, "event": "authorization_failure_evidence",
        "reason_code": allowed(&response["summary"]["reason"], &["account_outcome_unknown", "runtime_error"]),
        "error_type": allowed(&response["details"]["error_type"], &["outcome_unknown", "runtime"]),
        "operation_id": operation_id(&response["details"]["operation_id"]),
        "runtime_log_reason_code": "not_captured_by_fixture",
    });
    let journal = match std::fs::read(root.join(".login-recovery.json")) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(_) => {
                evidence["journal_reason_code"] = json!("invalid_json");
                eprintln!("{evidence}");
                return;
            }
        },
        Err(error) => {
            evidence["journal_reason_code"] = json!(match error.kind() {
                std::io::ErrorKind::NotFound => "not_found",
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                _ => "io_error",
            });
            evidence["journal_os_error_code"] = json!(error.raw_os_error());
            eprintln!("{evidence}");
            return;
        }
    };
    let active = &journal["active"];
    let id = operation_id(&response["details"]["operation_id"]);
    evidence["operation_id"] = json!(id);
    evidence["journal_reason_code"] = json!("read");
    evidence["journal_schema_v2"] = json!(journal["schema_version"] == 2);
    evidence["active_present"] = json!(active.is_object());
    evidence["active_matches_operation"] = json!(id.is_some() && operation_id(&active["operation_id"]) == id);
    let records: Vec<&Value> = journal["operations"].as_array().into_iter().flatten()
        .filter(|record| id.is_some() && operation_id(&record["operation_id"]) == id).collect();
    evidence["matching_records"] = json!(records.len());
    if let [record] = records.as_slice() {
        let account = &record["account"];
        let login = &account["login"];
        evidence["phase"] = allowed(&record["phase"], &["admitted", "login_input", "remote_pending",
            "remote_unknown", "authorized", "prepared", "awaiting_verification", "rolling_back", "committed", "rolled_back"]);
        evidence["backend"] = allowed(&record["backend"], &["native", "secret_service", "kernel"]);
        evidence["root_matches_fixture"] = json!(root.canonicalize().ok().is_some_and(|path|
            record["root"].as_str().map(std::path::Path::new) == Some(path.as_path())));
        evidence["source_matches_fixture"] = json!(source.canonicalize().ok().is_some_and(|path|
            record["profile"].as_str().map(std::path::Path::new) == Some(path.as_path())));
        for field in ["candidate", "candidate_sha256"] {
            evidence[format!("{field}_present")] = json!(!record[field].is_null());
        }
        for field in ["authorization", "vault", "target"] {
            evidence[format!("{field}_present")] = json!(!account[field].is_null());
        }
        for field in ["published_vault", "vault_pending"] {
            evidence[field] = json!(account[field].as_bool());
        }
        for field in ["import_started", "sync_started", "publication_pending", "publication_done", "vault_done", "password_done"] {
            evidence[field] = json!(login[field].as_bool());
        }
        evidence["owns_config"] = json!(active["owns_config"].as_bool());
        for field in ["backends", "pass_backends", "totp_backends"] {
            evidence[field] = json!(active[field].as_array().map(|values| values.iter()
                .map(|value| allowed(value, &["native", "secret_service", "kernel"])).collect::<Vec<_>>()));
        }
    }
    // Never serialize journal material, response text, credentials or arbitrary errors.
    eprintln!("{evidence}");
}


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/cli_business_tests/account/parts/authorization_preserves_and_commits_profiles.rs"));
