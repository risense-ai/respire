#[test]
fn feedback_capability_is_explicit_without_model_or_transport() -> anyhow::Result<()> {
    let mut core=super::Core::default();
    let (code,bytes)=core.call(&serde_json::to_vec(&serde_json::json!({"schema_version":1,"request_id":"feedback-capability","operation":"capabilities","payload":{}}))?);
    assert_eq!(code,super::OK);
    let value:serde_json::Value=serde_json::from_slice(&bytes)?;
    assert_eq!(value["result"]["feedback_contract"],1);
    assert_eq!(value["result"]["abi_version"],super::ABI_VERSION);
    assert_eq!(value["result"]["backend_load_probe_contract"],1);
    assert!(value["result"]["operations"].as_array().is_some_and(|operations| operations.iter().any(|operation| operation == "backend_load_probe")));
    struct UnavailableHost { calls: usize }
    impl super::Transport for UnavailableHost {
        fn request(&mut self, request: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
            assert_eq!(request["operation"], "backend_load_probe");
            assert_eq!(request["phase"], "load");
            self.calls += 1;
            anyhow::bail!("probe-host-unavailable-sentinel")
        }
    }
    let mut host = UnavailableHost { calls: 0 };
    let payload = serde_json::json!({"config":{"model":"m3","engine":"cpu","execution_timeout_secs":120,"asset_id":"probe-fixture"},
        "model_base64":"bW9kZWw=","tokenizer_base64":"e30=","text":"probe"});
    for field in ["reuse_current", "path"] {
        let mut invalid = payload.clone();
        invalid["config"][field] = if field == "reuse_current" { serde_json::json!(true) } else { serde_json::json!("forbidden") };
        assert!(core.dispatch("backend_load_probe", invalid, &mut host).is_err());
    }
    for invalid_reuse in [serde_json::Value::Null, serde_json::json!("false"), serde_json::json!(0)] {
        let mut invalid = payload.clone();
        invalid["config"]["reuse_current"] = invalid_reuse;
        assert!(core.dispatch("backend_load_probe", invalid, &mut host).is_err());
    }
    for invalid_asset in [serde_json::Value::Null, serde_json::json!(""), serde_json::json!(17)] {
        let mut invalid = payload.clone();
        invalid["config"]["asset_id"] = invalid_asset;
        assert!(core.dispatch("backend_load_probe", invalid, &mut host).is_err());
    }
    let mut missing_asset = payload.clone();
    missing_asset["config"].as_object_mut().ok_or_else(|| anyhow::anyhow!("probe config must be an object"))?.remove("asset_id");
    assert!(core.dispatch("backend_load_probe", missing_asset, &mut host).is_err());
    assert_eq!(host.calls, 0, "invalid probes must fail before calling the host");
    let failure = core.dispatch("backend_load_probe", payload, &mut host).err().ok_or_else(|| anyhow::anyhow!("unavailable host must fail probe"))?;
    assert_eq!(failure.code, super::ENGINE_FAILURE);
    assert!(failure.message.contains("probe-host-unavailable-sentinel"));
    assert_eq!(host.calls, 1, "cold probe must request loading without a resident model");
    let observation = serde_json::json!({"selected":"CPU", "runtime":{"kind":"onnxruntime","api_version":20,"api_address":"0x3456",
        "symbols":{"CreateSessionFromArray":"0x2345","Run":"0x1234"}}});
    let mut evidence = serde_json::json!({"verified":true,"resources_verified":true,"provider":"CPUExecutionProvider","runtime_api_version":20,
        "modules":[{"id":"ort","sha256":"a".repeat(64),"symbols":{"CreateSessionFromArray":"0x2345","Run":"0x1234"}}]});
    assert!(super::backend_probe::verify_modules(evidence.clone(), &observation).is_ok());
    evidence["modules"][0]["symbols"]["Run"] = serde_json::json!("0x5678");
    assert!(super::backend_probe::verify_modules(evidence.clone(), &observation).is_err());
    evidence["modules"][0]["symbols"]["Run"] = serde_json::json!("0x1234");
    evidence["resources_verified"] = serde_json::json!(false);
    assert!(super::backend_probe::verify_modules(evidence.clone(), &observation).is_err());
    evidence["resources_verified"] = serde_json::json!(true);
    for (pointer, value) in [
        ("/selected", serde_json::json!("UnknownExecutionProvider")),
        ("/selected", serde_json::json!("CoreML unknown")),
        ("/selected", serde_json::json!("MLX CPU")),
        ("/runtime/kind", serde_json::Value::Null),
        ("/runtime/kind", serde_json::json!("mlx")),
        ("/runtime/api_address", serde_json::json!("0x0")),
        ("/runtime/api_version", serde_json::Value::Null),
        ("/runtime/symbols", serde_json::Value::Null),
        ("/runtime/symbols/CreateSessionFromArray", serde_json::Value::Null),
    ] {
        let mut invalid = observation.clone();
        *invalid.pointer_mut(pointer).ok_or_else(|| anyhow::anyhow!("missing probe observation field {pointer}"))? = value;
        assert!(super::backend_probe::verify_modules(evidence.clone(), &invalid).is_err(), "invalid observation accepted: {pointer}");
    }
    for address in ["0x0", "0x", "invalid", "0x+1234", "0xffffffffffffffffffffffffffffffff"] {
        let mut invalid = observation.clone();
        invalid["runtime"]["symbols"]["Run"] = serde_json::json!(address);
        let mut matching = evidence.clone();
        matching["modules"][0]["symbols"]["Run"] = serde_json::json!(address);
        assert!(super::backend_probe::verify_modules(matching, &invalid).is_err(), "invalid address accepted: {address}");
    }
    let mut missing_symbol = observation.clone();
    missing_symbol["runtime"]["symbols"].as_object_mut().ok_or_else(|| anyhow::anyhow!("probe symbols must be an object"))?.remove("CreateSessionFromArray");
    assert!(super::backend_probe::verify_modules(evidence.clone(), &missing_symbol).is_err());
    let mut custom = observation.clone();
    custom["selected"] = serde_json::json!("Host");
    let mut custom_evidence = evidence.clone();
    custom_evidence["provider"] = serde_json::json!("ExplicitCustomExecutionProvider");
    assert!(super::backend_probe::verify_modules(custom_evidence, &custom).is_ok());
    let mlx = serde_json::json!({"selected":"MLX GPU","runtime":{"kind":"mlx"}});
    let mlx_evidence = serde_json::json!({"verified":true,"resources_verified":true,"provider":"MLX GPU",
        "modules":[{"id":"mlx","sha256":"b".repeat(64)}]});
    assert!(super::backend_probe::verify_modules(mlx_evidence.clone(), &mlx).is_ok());
    let mut invalid_mlx = mlx_evidence;
    invalid_mlx["runtime_api_version"] = serde_json::json!(20);
    assert!(super::backend_probe::verify_modules(invalid_mlx, &mlx).is_err());
    Ok(())
}

#[test]
fn inference_error_keeps_the_underlying_cause_in_the_abi_response() -> anyhow::Result<()> {
    let failure = super::Failure::engine(anyhow::anyhow!("ONNX allocation failed").context("ort run failed"));
    let (code, bytes) = super::response("probe", Err(failure));
    assert_eq!(code, super::ENGINE_FAILURE);
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let message = value["error"]["message"].as_str().ok_or_else(|| anyhow::anyhow!("error message missing"))?;
    assert!(message.contains("ort run failed"));
    assert!(message.contains("ONNX allocation failed"));
    Ok(())
}
