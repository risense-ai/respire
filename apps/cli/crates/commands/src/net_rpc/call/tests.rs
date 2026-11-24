use super::*;
use crate::loopback_http;

fn valid_timeout_args(args: &[&str]) -> Vec<String> {
    use clap::Parser;
    crate::cli_def::Cli::try_parse_from(std::iter::once("rsrs").chain(args.iter().copied()))
        .expect("timeout regression must exercise valid CLI argv");
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn rpc_timeout_recognizes_commands_after_global_values_and_equals_forms() {
    for args in [
        vec!["sync"],
        vec!["--json", "--progress-id", "request-one", "sync"],
        vec!["--progress-id=import", "--model-task-id=task-one", "sync"],
        vec!["--model-task-id", "task-one", "import", "synthetic.json"],
        vec!["import", "synthetic.json"],
        vec!["reembed"],
        vec!["model", "install"],
        vec!["--json", "--progress-id", "model", "model", "install-m3"],
        vec!["model", "--model-task-id", "install", "activate", "m3"],
    ] {
        let parsed = valid_timeout_args(&args);
        assert_eq!(
            rpc_timeout("cli.exec", &parsed, None),
            Duration::from_secs(30 * 60),
            "{args:?}"
        );
    }
}

#[test]
fn rpc_timeout_does_not_treat_tracking_values_or_other_methods_as_commands() {
    for args in [
        vec!["--progress-id", "sync", "list"],
        vec!["--model-task-id", "import", "status"],
        vec!["model", "--progress-id", "install", "engine"],
        vec!["show", "synthetic-id"],
    ] {
        let parsed = valid_timeout_args(&args);
        assert_eq!(
            rpc_timeout("cli.exec", &parsed, None),
            HTTP_TIMEOUT,
            "{args:?}"
        );
    }
    let long = valid_timeout_args(&["--progress-id", "request-one", "sync"]);
    assert_eq!(
        rpc_timeout("cli.progress", &long, None),
        Duration::from_secs(1)
    );
    assert_eq!(
        rpc_timeout("model.control", &long, None),
        Duration::from_secs(3)
    );
    assert_eq!(
        rpc_timeout("runtime.result", &["sync".into()], None),
        HTTP_TIMEOUT
    );
    assert_eq!(
        rpc_timeout("model.control", &long, Some(Duration::from_millis(7))),
        Duration::from_millis(7)
    );
}

#[test]
fn response_after_client_deadline_reports_unknown_and_can_be_inspected_without_replay(
) -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(concat!(env!("RSRS_CONTRACT_FIXTURES"),
        "/fixtures/positive/operation-result-completed.json"
    )))?;
    let receipt = &fixture["payload"];
    let operation = OperationContext {
        scope: serde_json::from_value(receipt["scope"].clone())?,
        operation_id: receipt["operation_id"]
            .as_str()
            .context("fixture operation ID")?
            .to_owned(),
        idempotency_key: receipt["idempotency_key"]
            .as_str()
            .context("fixture idempotency key")?
            .to_owned(),
    };
    let request = json!({"v":1,"id":"request-submit-1","method":"model.configure",
        "operation_id":operation.operation_id,"idempotency_key":operation.idempotency_key,"scope":operation.scope});
    let response = json!({"v":1,"id":request["id"],"ok":true,"exit":0,
        "operation_id":operation.operation_id,"idempotency_key":operation.idempotency_key,"scope":operation.scope});
    assert_eq!(
        validate_rpc_response(&request, response.clone(), Some(&operation))?,
        response
    );
    for (pointer, value) in [
        ("/id", json!("foreign-request")),
        ("/id", Value::Null),
        ("/v", json!(2)),
        ("/v", json!("1")),
        ("/ok", json!("true")),
        ("/exit", Value::Null),
        ("/exit", json!(-1)),
        ("/exit", json!(1)),
        ("/operation_id", json!("foreign-operation")),
        ("/operation_id", Value::Null),
        ("/idempotency_key", json!("foreign-key")),
        ("/scope", Value::Null),
        ("/scope", json!({})),
        ("/scope/account_id", json!("foreign-account")),
        ("/scope/instance_id", json!("foreign-instance")),
        ("/scope/generation", json!(8)),
        ("/scope/generation", json!("7")),
    ] {
        let mut invalid = response.clone();
        *invalid.pointer_mut(pointer).context("mutation pointer")? = value;
        let error = validate_rpc_response(&request, invalid, Some(&operation))
            .err()
            .context("unbound write response was accepted")?;
        assert!(matches!(error.downcast_ref::<RuntimeError>(),
            Some(RuntimeError::OutcomeUnknown { request_id, operation: original, .. })
                if request_id == "request-submit-1" && original == &operation));
    }
    for key in ["id", "v", "ok", "exit", "operation_id", "scope"] {
        let mut invalid = response.clone();
        invalid
            .as_object_mut()
            .context("response object")?
            .remove(key);
        assert!(
            matches!(validate_rpc_response(&request, invalid, Some(&operation))
            .err().context("missing response field accepted")?.downcast_ref::<RuntimeError>(),
            Some(RuntimeError::OutcomeUnknown { operation: original, .. }) if original == &operation)
        );
    }
    let mut extra = response.clone();
    extra["scope"]["extra"] = json!(true);
    assert!(validate_rpc_response(&request, extra, Some(&operation)).is_err());
    for code in ["scope_required", "scope_mismatch"] {
        let mut rejection = json!({"v":1,"id":request["id"],"ok":false,"exit":1,
            "code":code,"error":"scope rejected before admission"});
        assert_eq!(
            validate_rpc_response(&request, rejection.clone(), Some(&operation))?,
            rejection
        );
        rejection["scope"] = json!({"account_id":"current-account","instance_id":"current-instance","generation":8});
        assert_eq!(
            validate_rpc_response(&request, rejection.clone(), Some(&operation))?,
            rejection
        );
        for (key, value) in [
            ("id", json!("foreign-request")),
            ("v", json!(2)),
            ("exit", json!(0)),
            ("exit", json!(-1)),
            ("scope", json!({})),
            ("operation_id", json!("foreign-operation")),
        ] {
            let mut invalid = rejection.clone();
            invalid[key] = value;
            assert!(
                matches!(validate_rpc_response(&request, invalid, Some(&operation))
                .err().context("malformed scope rejection accepted")?.downcast_ref::<RuntimeError>(),
                Some(RuntimeError::OutcomeUnknown { operation: original, .. }) if original == &operation)
            );
        }
        rejection["ok"] = json!(true);
        assert!(validate_rpc_response(&request, rejection, Some(&operation)).is_err());
    }
    let mut query = request.clone();
    query["id"] = receipt["request_id"].clone();
    query["method"] = json!("runtime.result");
    let envelope = crate::output::ResultEnvelope::new(
        "runtime.result",
        crate::output::Status::Ok,
        receipt.clone(),
        Vec::new(),
    );
    let result = json!({"v":1,"id":query["id"],"ok":true,"exit":0,"envelope":envelope});
    for state in ["pending", "completed", "failed", "aborted", "unknown"] {
        let mut result = result.clone();
        result["envelope"]["summary"]["result"] = json!(state);
        assert_eq!(
            validate_rpc_response(&query, result.clone(), Some(&operation))?,
            result
        );
    }
    for (pointer, value) in [
        ("/id", json!("foreign-query")),
        ("/v", json!(2)),
        ("/envelope/summary/request_id", json!("foreign-query")),
        ("/envelope/summary/operation_id", json!("foreign-operation")),
        ("/envelope/summary/idempotency_key", json!("foreign-key")),
        ("/envelope/summary/scope", Value::Null),
        (
            "/envelope/summary/scope/account_id",
            json!("foreign-account"),
        ),
        (
            "/envelope/summary/scope/instance_id",
            json!("foreign-instance"),
        ),
        ("/envelope/summary/scope/generation", json!(8)),
    ] {
        let mut invalid = result.clone();
        *invalid
            .pointer_mut(pointer)
            .context("result mutation pointer")? = value;
        let error = validate_rpc_response(&query, invalid, Some(&operation))
            .err()
            .context("foreign result was accepted")?;
        assert!(!matches!(
            error.downcast_ref::<RuntimeError>(),
            Some(RuntimeError::OutcomeUnknown { .. })
        ));
    }
    for (key, value) in [
        ("scope", json!({})),
        ("operation_id", json!("foreign-operation")),
    ] {
        let mut invalid = result.clone();
        invalid[key] = value;
        assert!(validate_rpc_response(&query, invalid, Some(&operation)).is_err());
    }
    for key in ["request_id", "operation_id", "scope"] {
        let mut invalid = result.clone();
        invalid["envelope"]["summary"]
            .as_object_mut()
            .context("result receipt object")?
            .remove(key);
        assert!(validate_rpc_response(&query, invalid, Some(&operation)).is_err());
    }
    for code in ["scope_required", "scope_mismatch"] {
        let rejection = json!({"v":1,"id":query["id"],"ok":false,"exit":1,"code":code,"error":"scope rejected"});
        assert_eq!(
            validate_rpc_response(&query, rejection.clone(), Some(&operation))?,
            rejection
        );
    }
    let mut read = request.clone();
    read["method"] = json!("model.catalog");
    let read_response = json!({"v":1,"id":read["id"],"ok":true,"exit":0});
    assert_eq!(
        validate_rpc_response(&read, read_response.clone(), None)?,
        read_response
    );
    let mut foreign = read_response;
    foreign["id"] = json!("foreign-read");
    assert!(validate_rpc_response(&read, foreign, None).is_err());
    let _guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for method in [
        "cli.exec",
        "model.control",
        "model.prepare",
        "index.prepare",
        "model.configure",
        "model.switch",
        "model.resume",
    ] {
        let directory = tempfile::tempdir()?;
        let server = loopback_http::Server::bind()?;
        let port = server
            .addr()
            .port();
        let previous: Vec<_> = ["RSRS_RPC_PORT", "RSRS_DATA_DIR", "RSRS_RPC_TOKEN"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        std::env::set_var("RSRS_RPC_PORT", port.to_string());
        std::env::set_var("RSRS_DATA_DIR", directory.path());
        std::env::remove_var("RSRS_RPC_TOKEN");
        let test_root = directory.path().to_path_buf();
        let actor = std::thread::spawn(move || -> Result<String> {
            fn receive_rpc(
                server: &loopback_http::Server,
                root: &std::path::Path,
            ) -> Result<loopback_http::Request> {
                loop {
                    let request = server
                        .recv_timeout(Duration::from_secs(2))?
                        .ok_or_else(|| anyhow::anyhow!("RPC never arrived"))?;
                    if request.url() == "/api/health" {
                        request.reply(json!({
                        "server":"respire", "bin":"fixture", "pid":4321, "v":1,
                        "installation_root":root, "account_id":"account-1", "instance_id":"instance-1", "generation":7,
                    }).to_string())?;
                    } else {
                        return Ok(request);
                    }
                }
            }
            let request = receive_rpc(&server, &test_root)?;
            let write: Value = serde_json::from_slice(request.body())?;
            anyhow::ensure!(write["method"] == method, "unexpected first request");
            anyhow::ensure!(
                write["id"].is_string()
                    && write["operation_id"].is_string()
                    && write["id"] != write["operation_id"],
                "request and operation identities must be distinct"
            );
            let id = write["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing write ID"))?
                .to_owned();
            let operation_id = write["operation_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing operation ID"))?
                .to_owned();
            // Simulated committed write returns after the shortened client deadline.
            std::thread::sleep(Duration::from_millis(150));
            let _ = request.reply(
                json!({"v":1,"id":id,"ok":true}).to_string());
            let inspect = server
                .recv_timeout(Duration::from_secs(2))?
                .ok_or_else(|| anyhow::anyhow!("result query never arrived"))?;
            anyhow::ensure!(
                inspect.url() == "/api/rpc",
                "result query must not refresh health"
            );
            let query: Value = serde_json::from_slice(inspect.body())?;
            anyhow::ensure!(
                write["scope"]
                    == json!({"account_id":"account-1","instance_id":"instance-1","generation":7})
                    && query["scope"] == write["scope"],
                "request scope changed or was omitted"
            );
            anyhow::ensure!(
                query["method"] == "runtime.result"
                    && query["args"][0] == operation_id
                    && query["operation_id"] == write["operation_id"]
                    && query["idempotency_key"] == write["idempotency_key"]
                    && query["id"] != write["id"],
                "client replayed or inspected the wrong write"
            );
            let envelope = crate::output::ResultEnvelope::new(
                "runtime.result",
                crate::output::Status::Ok,
                json!({"state":"completed","request_id":query["id"],"operation_id":operation_id,
                    "scope":query["scope"],"idempotency_key":query["idempotency_key"],
                    "response":{"ok":true,"committed":true,"id":id}}),
                Vec::new(),
            );
            inspect.reply(
                json!({"v":1,"id":query["id"],"ok":true,"exit":0,"envelope":envelope})
                    .to_string())?;
            Ok(operation_id)
        });
        let result = (|| -> Result<()> {
            let error = rpc_method_with_timeout(
                method,
                vec!["remember".into(), "synthetic write".into()],
                Some(Duration::from_millis(50)),
            )
            .err()
            .ok_or_else(|| {
                anyhow::anyhow!("delayed write unexpectedly completed within deadline")
            })?;
            let operation = match error.downcast_ref::<RuntimeError>() {
                Some(RuntimeError::OutcomeUnknown { operation, .. }) => operation.clone(),
                _ => anyhow::bail!(
                    "ambiguous transport failure was not marked outcome_unknown: {error}"
                ),
            };
            {
                let (failure, _) = crate::output_emit::command_failure(&error);
                anyhow::ensure!(
                    failure.details["scope"] == serde_json::to_value(&operation.scope)?
                        && failure.details["operation_id"] == operation.operation_id
                        && failure.details["idempotency_key"] == operation.idempotency_key
                        && failure.details["replayed"] == false,
                    "outcome_unknown output lost the original operation context"
                );
            }
            anyhow::ensure!(
                rpc_method("runtime.result", vec![operation.operation_id.clone()]).is_err(),
                "result query without original context must fail before contacting runtime"
            );
            let inspected = rpc_result(&operation)?;
            anyhow::ensure!(
                inspected["envelope"]["summary"]["state"] == "completed",
                "inspection failed: {inspected}"
            );
            let seen = actor
                .join()
                .map_err(|_| anyhow::anyhow!("test transport actor panicked"))??;
            anyhow::ensure!(seen == operation.operation_id, "operation ID changed");
            Ok(())
        })();
        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        result?;
    }
    Ok(())
}
