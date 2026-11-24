use crate::loopback_http;
use super::*;
use respire_commands::output_state;
use serde_json::{json, Value};
use std::{ffi::OsString, path::Path, time::Duration};

fn isolated(test: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    struct Restore {
        env: Vec<(&'static str, Option<OsString>)>,
        captured: Option<ResultEnvelope>,
        host_output: bool,
        emitted: bool,
        exit: u8,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.env {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            output_state::CAPTURED.with(|slot| *slot.borrow_mut() = self.captured.take());
            output_state::HOST_RPC_OUTPUT.set(self.host_output);
            output_state::OUTPUT_EMITTED.set(self.emitted);
            output_state::OUTPUT_EXIT_CODE.set(self.exit);
        }
    }
    let keys = [
        "RSRS_DATA_DIR",
        "RSRS_BIN_DIR",
        "RSRS_M3_DIR",
        "RSRS_NO_AUTOSYNC",
        "RSRS_SUPER",
        "HOME",
        "USERPROFILE",
        "RSRS_CLIENT_ONLY",
        "RSRS_NO_AUTOSTART",
        "RSRS_UPDATE_CHECK",
        "RSRS_REGISTRY",
        "RSRS_RUNTIME_BIN",
        "RSRS_CORE_TEST_MODE",
        "RSRS_RPC_TOKEN",
        "RSRS_RPC_PORT",
    ];
    let _restore = Restore {
        env: keys
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
        captured: output_state::CAPTURED.with(|slot| slot.borrow_mut().take()),
        host_output: output_state::HOST_RPC_OUTPUT.get(),
        emitted: output_state::OUTPUT_EMITTED.get(),
        exit: output_state::OUTPUT_EXIT_CODE.get(),
    };
    std::env::set_var("RSRS_DATA_DIR", root);
    std::env::set_var("RSRS_BIN_DIR", root.join("bin"));
    std::env::set_var("RSRS_M3_DIR", root.join("absent-model"));
    std::env::set_var("RSRS_NO_AUTOSYNC", "1");
    std::env::set_var("RSRS_CORE_TEST_MODE", "1");
    std::env::remove_var("RSRS_SUPER");
    std::env::remove_var("RSRS_RPC_PORT");
    let home = root.join("home");
    std::fs::create_dir(&home)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("RSRS_CLIENT_ONLY", "0");
    std::env::set_var("RSRS_NO_AUTOSTART", "0");
    std::env::set_var("RSRS_UPDATE_CHECK", "0");
    std::env::remove_var("RSRS_RPC_TOKEN");
    // This file supplies an identity to the HTTP fixture; it is never executed.
    let image = root.join("doctor-runtime-fixture");
    std::fs::write(&image, b"doctor HTTP fixture identity")?;
    std::env::set_var("RSRS_RUNTIME_BIN", image);
    output_state::HOST_RPC_OUTPUT.set(true);
    test(root)
}

fn diagnostic(status: Status) -> ResultEnvelope {
    ResultEnvelope::new(
        "doctor",
        status,
        json!({"scope":"runtime","runtime_version":"1.0.20"}),
        vec![Item::new("Runtime version", Status::Ok, "1.0.20")],
    )
}

fn with_runtime(
    replies: Vec<(&'static str, Value)>,
    test: impl FnOnce() -> Result<()>,
) -> Result<Vec<Value>> {
    let server =
        loopback_http::Server::bind()?;
    let port = server
        .addr()
        .port();
    std::env::set_var("RSRS_RPC_PORT", port.to_string());
    let health = json!({
        "server":"respire", "bin":"doctor-fixture", "v":1, "pid":std::process::id(),
        "runtime_version":crate::runtime_fetch::pinned()?.required,
        "installation_root":respire_client_host::paths::main_data_dir().canonicalize()?,
        "data_dir":respire_client_host::config::control_roots()?.1.canonicalize()?,
        "exe":std::path::PathBuf::from(std::env::var_os("RSRS_RUNTIME_BIN").context("fixture image")?),
        "account_id":"doctor-account", "instance_id":"doctor-instance", "generation":1,
        "store_ready":true,
    });
    let actor = std::thread::spawn(move || -> Result<Vec<Value>> {
        let mut requests = Vec::new();
        for (method, mut reply) in replies {
            loop {
                let request = server
                    .recv_timeout(Duration::from_secs(3))?
                    .context("doctor fixture request did not arrive")?;
                if request.url() == "/api/health" {
                    request.reply(health.to_string())?;
                    continue;
                }
                ensure!(
                    request.url() == "/api/rpc",
                    "unexpected doctor fixture route"
                );
                let body: Value = serde_json::from_slice(request.body())?;
                // This diagnostic fixture represents an already configured owner.
                // Startup must observe its nonzero revision without applying over it.
                if body["method"] == "core_config.read" {
                    let mut current = json!({"v":1,"ok":true,"exit":0,
                        "pid":health["pid"],"data_dir":health["data_dir"],
                        "envelope":ResultEnvelope::new("core-config", Status::Ok, json!({
                            "state":"current","revision":1,
                            "candidate":{"read_heat":null,"recall_fusion":null},
                            "effective":{"read_heat":true,"recall_fusion":{}},
                            "capabilities":{"read_heat_contract":1,"recall_fusion_contract":1},
                            "applies":"next_query"}), Vec::new())});
                    for key in ["id", "operation_id", "idempotency_key", "scope"] {
                        current[key] = body[key].clone();
                    }
                    request.reply(current.to_string())?;
                    continue;
                }
                ensure!(
                    body["method"] == method,
                    "unexpected doctor RPC method: {body}"
                );
                for key in ["id", "operation_id", "idempotency_key", "scope"] {
                    reply[key] = body[key].clone();
                }
                reply["v"] = json!(1);
                reply["bin"] = json!("2.0.0-dev.25");
                request.reply(reply.to_string())?;
                requests.push(body);
                break;
            }
        }
        Ok(requests)
    });
    let result = test();
    let requests = actor
        .join()
        .map_err(|_| anyhow::anyhow!("doctor fixture panicked"))?;
    result?;
    requests
}

fn reply(result: ResultEnvelope) -> Value {
    json!({"ok":true,"exit":0,"envelope":result})
}

#[test]
fn host_rows_keep_configuration_failures_and_local_update_results() -> Result<()> {
    isolated(|root| {
        let row = |rows: Vec<Item>, name: &str| -> Result<Item> {
            rows.into_iter()
                .find(|item| item.name == name)
                .context("missing host row")
        };
        assert!(!host_rows(false).iter().any(|item| item.name == "session"));
        assert!(matches!(
            row(host_rows(false), "inject")?.status,
            Status::Fail
        ));
        assert!(row(host_rows(false), "CLI version")?
            .value
            .contains("disabled"));
        let home = root.join("home");
        respire::inject::inject_one("generic")?;
        assert!(matches!(
            row(host_rows(false), "inject")?.status,
            Status::Ok
        ));
        std::fs::create_dir(home.join(".codex"))?;
        assert!(matches!(
            row(host_rows(false), "inject")?.status,
            Status::Warn
        ));
        let binary = crate::mcp::stable_bin_path();
        std::fs::create_dir_all(&binary)?;
        assert!(row(host_rows(false), "mcp bin")?
            .value
            .contains("not a file"));
        std::fs::remove_dir(&binary)?;
        std::fs::write(&binary, b"not executed")?;
        assert!(matches!(
            row(host_rows(false), "mcp bin")?.status,
            Status::Ok
        ));
        let legacy = home.join(".respire");
        std::fs::create_dir(&legacy)?;
        std::fs::write(legacy.join("session.json"), b"{}")?;
        assert!(matches!(
            row(host_rows(false), "migration")?.status,
            Status::Warn
        ));
        std::fs::write(legacy.join("session.json"), b"{")?;
        assert!(matches!(
            row(host_rows(false), "migration")?.status,
            Status::Fail
        ));
        std::env::set_var("RSRS_UPDATE_CHECK", "1");
        assert!(row(host_rows(false), "CLI version")?
            .value
            .contains("not requested"));
        for (body, expected) in [
            (json!({"version":"999.0.0"}), Status::Warn),
            (json!({"version":respire::VERSION}), Status::Ok),
            (json!({}), Status::Skip),
        ] {
            let server = loopback_http::Server::bind()?;
            std::env::set_var("RSRS_REGISTRY", format!("http://{}", server.addr()));
            let actor = std::thread::spawn(move || -> Result<()> {
                let request = server
                    .recv_timeout(Duration::from_secs(3))?
                    .context("local version request missing")?;
                ensure!(
                    request.url() == "/rsrs-cli/latest",
                    "unexpected registry route"
                );
                request.reply(body.to_string())?;
                Ok(())
            });
            let version = row(host_rows(true), "CLI version")?;
            actor
                .join()
                .map_err(|_| anyhow::anyhow!("registry fixture panicked"))??;
            assert_eq!(version.status.as_str(), expected.as_str());
            assert!(
                version.value.contains("just checked") || version.value.contains("unavailable")
            );
        }
        Ok(())
    })
}

#[test]
fn runtime_scope_and_paused_contracts_are_not_inferred_from_success() -> Result<()> {
    isolated(|_| {
        std::env::set_var("RSRS_CLIENT_ONLY", "1");
        for (command, summary, accepted) in [
            ("doctor", json!({"scope":"runtime"}), true),
            ("doctor", json!({"scope":"host"}), false),
            ("model", json!({"scope":"runtime"}), false),
            ("memory", json!({"mode":"off","skipped":true}), true),
            ("memory", json!({"mode":"off","skipped":false}), false),
        ] {
            let mut result = diagnostic(Status::Ok);
            result.command = command.to_owned();
            result.summary = summary;
            let requests = with_runtime(vec![("cli.exec", reply(result))], || {
                let actual = runtime_doctor(true);
                assert_eq!(actual.is_ok(), accepted);
                if !accepted {
                    assert!(actual
                        .err()
                        .context("expected scope error")?
                        .to_string()
                        .contains("scope=runtime"));
                }
                Ok(())
            })?;
            assert_eq!(requests[0]["args"], json!(["--json", "doctor", "--remote"]));
        }
        with_runtime(
            vec![(
                "cli.exec",
                json!({"ok":false,"exit":1,"error":"fixture diagnostic unavailable"}),
            )],
            || {
                let error = runtime_doctor(false).err().context("missing RPC error")?;
                assert!(format!("{error:#}").contains("fixture diagnostic unavailable"));
                Ok(())
            },
        )?;
        for (update, fix) in [(true, false), (false, true)] {
            assert!(run(false, update, fix)
                .err()
                .context("sandbox accepted host action")?
                .to_string()
                .contains("client_only"));
        }
        with_runtime(vec![("cli.exec", reply(diagnostic(Status::Warn)))], || {
            run(false, false, false)?;
            let result = output_state::CAPTURED
                .with(|slot| slot.borrow_mut().take())
                .context("client result missing")?;
            assert_eq!(result.summary["scope"], "runtime");
            assert_eq!(result.summary["runtime_version"], "2.0.0-dev.25");
            assert_eq!(result.items[0].value, "2.0.0-dev.25");
            assert!(result.summary.get("host_checks").is_none());
            Ok(())
        })?;
        Ok(())
    })
}

#[test]
fn repair_defers_or_preserves_the_actual_runtime_failure() -> Result<()> {
    isolated(|root| {
        let mut result = diagnostic(Status::Fail);
        result
            .items
            .push(Item::new("embedder", Status::Fail, "missing"));
        let database = root.join("rsrs.db");
        std::fs::write(
            &database,
            b"invalid database must not be opened by deferred repair",
        )?;
        let before = std::fs::read(&database)?;
        result.summary = json!({"mode":"off","skipped":true});
        assert!(matches!(repair(&result)?.status, Status::Skip));
        result.summary = json!({"scope":"runtime"});
        let config = respire_client_host::paths::client_config_path();
        std::fs::write(&config, br#"{"service_mode":"off"}"#)?;
        assert!(matches!(repair(&result)?.status, Status::Skip));
        std::fs::write(&config, b"{}")?;
        let operation = respire::model_progress::Operation::begin("download")?;
        result.details = json!({"model_operation":{"active":true}});
        assert!(matches!(repair(&result)?.status, Status::Warn));
        assert_eq!(respire::model_progress::status()?["phase"], "download");
        drop(operation);
        assert_eq!(std::fs::read(&database)?, before);
        assert!(!root.join("models").exists());
        result.details = Value::Null;
        result.items.clear();
        assert!(repair(&result)?.value.contains("No asset or index repair"));
        result
            .items
            .push(Item::new("model index", Status::Fail, "pending"));
        std::fs::write(&config, br#"{"service_mode":"readonly"}"#)?;
        assert!(repair(&result)
            .err()
            .context("readonly repair accepted")?
            .to_string()
            .contains("read-only"));
        std::fs::write(&config, b"{")?;
        assert!(respire_client_host::settings::ensure_writable().is_err());
        assert_eq!(std::fs::read(&database)?, before);
        std::fs::write(&config, b"{}")?;
        for error in [Some("fixture index refused"), None] {
            with_runtime(
                vec![("index.prepare", json!({"ok":false,"exit":1,"error":error}))],
                || {
                    let failure = repair(&result).err().context("index error swallowed")?;
                    assert!(failure
                        .to_string()
                        .contains(error.unwrap_or("Runtime index preparation failed")));
                    Ok(())
                },
            )?;
        }
        with_runtime(vec![("index.prepare", json!({"ok":true,"exit":0}))], || {
            assert!(matches!(repair(&result)?.status, Status::Ok));
            Ok(())
        })?;
        // Package preparation reaches the download boundary without opening the store.
        let endpoint = root.join("runtime/endpoint.json");
        std::fs::create_dir_all(endpoint.parent().context("legacy endpoint parent missing")?)?;
        let owner = br#"{"v":1,"pid":0}"#;
        std::fs::write(&endpoint, owner)?;
        let unused = std::net::TcpListener::bind("127.0.0.1:0")?;
        std::env::set_var("RSRS_RPC_PORT", unused.local_addr()?.port().to_string());
        drop(unused);
        std::env::set_var("RSRS_CORE_TEST_MODE", "1");
        result.items = vec![Item::new("embedder", Status::Fail, "invalid selection")];
        let failure = repair(&result)
            .err()
            .context("invalid selection was accepted")?;
        assert!(format!("{failure:#}").contains("runtime downloads are disabled in core test mode"));
        assert_eq!(std::fs::read(&endpoint)?, owner);
        assert_eq!(std::fs::read(&database)?, before);
        assert!(!root.join("models").exists());
        Ok(())
    })
}

#[test]
fn host_output_retains_runtime_severity_counts_and_repair_results() -> Result<()> {
    isolated(|_| {
        respire::inject::inject_one("generic")?;
        let binary = crate::mcp::stable_bin_path();
        std::fs::create_dir_all(binary.parent().context("fixture binary parent")?)?;
        std::fs::write(binary, b"not executed")?;
        for (status, errors, expected) in [
            (Status::Warn, Vec::new(), Status::Warn),
            (Status::Pending, Vec::new(), Status::Warn),
            (Status::Fail, Vec::new(), Status::Fail),
            (
                Status::Ok,
                vec!["runtime diagnostic failure".to_owned()],
                Status::Fail,
            ),
        ] {
            let mut result = diagnostic(status);
            // Session diagnostics moved from the host to the Runtime response.
            result.items.push(Item::new(
                "session",
                Status::Ok,
                "five-keys complete, can unlock",
            ));
            result.errors = errors;
            with_runtime(vec![("cli.exec", reply(result))], || {
                run(false, false, false)?;
                let result = output_state::CAPTURED
                    .with(|slot| slot.borrow_mut().take())
                    .context("host result missing")?;
                assert_eq!(result.status.as_str(), expected.as_str());
                assert_eq!(result.summary["scope"], "host+runtime");
                assert_eq!(result.summary["version"], respire::VERSION);
                assert_eq!(result.summary["host_checks"], "completed");
                assert_eq!(result.summary["pass"], 4);
                assert_eq!(result.summary["skip"], 1);
                assert_eq!(result.summary["total"], result.items.len());
                Ok(())
            })?;
        }
        let mut paused = diagnostic(Status::Skip);
        paused.command = "memory".into();
        paused.summary = json!({"mode":"off","skipped":true});
        paused.items.clear();
        with_runtime(vec![("cli.exec", reply(paused))], || {
            run(false, false, true)?;
            let result = output_state::CAPTURED
                .with(|slot| slot.borrow_mut().take())
                .context("paused result missing")?;
            assert_eq!(result.command, "doctor");
            assert!(matches!(result.status, Status::Warn));
            assert_eq!(result.summary["warn"], 1);
            assert_eq!(result.summary["skip"], 2);
            assert!(result
                .items
                .iter()
                .any(|row| row.name == "host repair" && matches!(row.status, Status::Skip)));
            Ok(())
        })?;
        let mut refreshed = diagnostic(Status::Ok);
        refreshed.summary["refreshed"] = json!(true);
        with_runtime(
            vec![
                ("cli.exec", reply(diagnostic(Status::Ok))),
                ("cli.exec", reply(refreshed)),
            ],
            || {
                run(false, false, true)?;
                let result = output_state::CAPTURED
                    .with(|slot| slot.borrow_mut().take())
                    .context("repaired result missing")?;
                assert_eq!(result.summary["refreshed"], true);
                assert!(result
                    .items
                    .iter()
                    .any(|row| row.name == "host repair" && matches!(row.status, Status::Ok)));
                Ok(())
            },
        )?;
        Ok(())
    })
}
