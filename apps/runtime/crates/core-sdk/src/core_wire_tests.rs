    use super::*;

    #[test]
    fn explicit_feedback_cannot_bypass_old_sdk_checks_in_public_call_paths() -> Result<()> {
        if FEEDBACK_CONTRACT == 1 {
            return Ok(());
        } // New-SDK actual scoring is verified separately.
        let payload = json!({"query_settings":{"feedback":{"enabled":true}},"model":"m3","snapshots":[],"query":{"text":"synthetic","limit":1},"mode":"scored"});
        let mut core = Core::new()?;
        let direct = core
            .call("query", payload.clone())
            .err()
            .context("old native SDK accepted feedback")?;
        assert!(direct.to_string().contains("feedback contract 1"));
        let high = execute::<Value>("query", payload.clone())
            .err()
            .context("high-level execute accepted feedback")?;
        assert!(high.to_string().contains("feedback contract 1"));
        let mut calls = 0;
        let mut transport = |_: &Value| -> Result<Value> {
            calls += 1;
            Ok(Value::Null)
        };
        let transport_error = core
            .call_with_transport("query_business", payload, &mut transport)
            .err()
            .context("transport path accepted feedback")?;
        assert!(transport_error.to_string().contains("feedback contract 1"));
        assert_eq!(calls, 0);
        let disabled = core.call(
            "capabilities",
            json!({"query_settings":{"feedback":{"enabled":false}}}),
        )?;
        assert_eq!(disabled["abi_version"].as_u64(), Some(ABI_VERSION as u64));
        Ok(())
    }

    #[test]
    fn worker_stdout_keeps_protocol_private() -> anyhow::Result<()> {
        use std::io::Write;
        const CHILD: &str = "RSRS_TEST_WORKER_STDOUT_CHILD";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
            let mut protocol = super::ffi::worker_stdout()?;
            super::ffi::worker_stdout_test_diagnostic()?;
            std::io::stdout().write_all(b"worker-rust-diagnostic\n")?;
            std::io::stdout().flush()?;
            protocol.write_all(b"worker-protocol-private\n")?;
            protocol.flush()?;
            return Ok(());
        }
        // Redirection is process-wide, so exercise it outside the test runner.
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "core_wire_tests::worker_stdout_keeps_protocol_private",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()?;
        anyhow::ensure!(output.status.success(), "stdout isolation child failed");
        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8(output.stderr)?;
        anyhow::ensure!(stdout.contains("worker-protocol-private"));
        for diagnostic in ["worker-native-diagnostic", "worker-rust-diagnostic"] {
            anyhow::ensure!(
                !stdout.contains(diagnostic),
                "diagnostic entered IPC output"
            );
            anyhow::ensure!(
                stderr.contains(diagnostic),
                "diagnostic did not reach stderr"
            );
        }
        anyhow::ensure!(!stderr.contains("worker-protocol-private"));
        Ok(())
    }

    #[test]
    fn borrowed_request_and_moved_result_preserve_protocol() -> Result<()> {
        for payload in [
            Value::Null,
            json!({"nested":[1,"\u{4f60}\u{597d}",{"flag":true}]}),
            json!([1, 2, 3]),
        ] {
            let bytes = serde_json::to_vec(&CoreRequest {
                schema_version: 1,
                request_id: "42",
                operation: "query",
                payload: &payload,
            })?;
            let request: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                request,
                json!({"schema_version":1,"request_id":"42",
                "operation":"query","payload":payload})
            );
            let mut response = json!({"schema_version":1,"request_id":"42","result":payload});
            assert_eq!(response_result(&mut response)?, payload);
            assert!(response.get("result").is_none());
            assert_eq!(response["request_id"], "42");
        }
        assert!(response_result(&mut json!({"error":{"message":"failure"}})).is_err());
        assert!(response_result(&mut Value::Null).is_err());
        Ok(())
    }
