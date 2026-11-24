fn contract_root() -> Result<PathBuf> {
    let fixtures = std::env::var("RSRS_CONTRACT_FIXTURES").context("RSRS_CONTRACT_FIXTURES must name the frozen runtime-control fixtures")?;
    Ok(Path::new(&fixtures)
        .canonicalize()
        .context("runtime control contract exists")?)
}

fn read_fixture(name: &str) -> Result<Value> {
    let path = contract_root()?.join("fixtures").join(name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read fixture {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse fixture {}", path.display()))
}

fn require_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string field {key}"))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_release_metadata(fixture: &Value) -> Result<()> {
    let payload = fixture
        .get("payload")
        .and_then(Value::as_object)
        .context("metadata payload must be an object")?;
    let version = payload
        .get("schema_version")
        .and_then(Value::as_u64)
        .context("schema_version must be an integer")?;
    let runtime_version = require_string(&fixture["payload"], "runtime_version")?;
    let tag = require_string(&fixture["payload"], "tag")?;
    if tag.strip_prefix('v') != Some(runtime_version) {
        bail!("tag and runtime_version disagree");
    }

    let target = payload
        .get("target_triple")
        .and_then(Value::as_str)
        .or_else(|| payload.get("triple").and_then(Value::as_str))
        .context("target_triple or legacy triple is required")?;
    if target.is_empty() {
        bail!("target cannot be empty");
    }

    let archive = payload
        .get("archive_sha256")
        .and_then(Value::as_str)
        .or_else(|| payload.get("sha256").and_then(Value::as_str))
        .context("archive_sha256 or legacy sha256 is required")?;
    if !valid_sha256(archive) {
        bail!("archive digest is invalid");
    }

    let binary = payload.get("binary_sha256").and_then(Value::as_str);
    if version >= 2 {
        let binary = binary.context("schema 2 requires binary_sha256")?;
        if !valid_sha256(binary) {
            bail!("binary digest is invalid");
        }
    }

    let legacy_archive = payload.get("sha256").and_then(Value::as_str);
    let legacy_target = payload.get("triple").and_then(Value::as_str);
    if let Some(value) = legacy_archive {
        if value != archive {
            bail!("legacy archive alias conflicts with canonical value");
        }
    }
    if let Some(value) = legacy_target {
        if value != target {
            bail!("legacy target alias conflicts with canonical value");
        }
    }
    Ok(())
}

fn validate_identity(fixture: &Value, complete: bool) -> Result<()> {
    let payload = fixture["payload"].as_object().context("identity object")?;
    if !complete {
        assert_eq!(fixture["error_code"], "identity_incomplete");
        assert!(payload.get("instance_id").is_none_or(Value::is_null));
        return Ok(());
    }
    for key in [
        "installation_root",
        "installation_id",
        "instance_id",
        "pid",
        "process_start_id",
        "image_path",
        "image_sha256",
        "runtime_version",
        "account_id",
        "generation",
        "protocol_range",
        "capabilities",
        "store_ready",
        "state",
    ] {
        if !payload.contains_key(key) {
            bail!("health identity requires {key}");
        }
    }
    Ok(())
}

fn validate_stop(fixture: &Value, valid: bool) -> Result<()> {
    let payload = fixture["payload"].as_object().context("stop object")?;
    for key in ["request_id", "operation_id", "state"] {
        if !payload.contains_key(key) {
            bail!("stop state requires {key}");
        }
    }
    let drained_unfinished = payload.get("state").and_then(Value::as_str) == Some("drained")
        && payload
            .get("unfinished")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
    if !valid {
        assert_eq!(fixture["error_code"], "ambiguous_stop_result");
        assert!(drained_unfinished);
        return Ok(());
    }
    anyhow::ensure!(
        !drained_unfinished,
        "drained cannot contain unfinished operations"
    );
    Ok(())
}

fn validate_operation(fixture: &Value) -> Result<()> {
    let payload = fixture["payload"].as_object().context("operation object")?;
    for key in ["request_id", "operation_id", "method", "result", "scope"] {
        if !payload.contains_key(key) {
            bail!("operation result requires {key}");
        }
    }
    let scope: respire_protocol::RuntimeRequestScope =
        serde_json::from_value(fixture["payload"]["scope"].clone())?;
    anyhow::ensure!(
        !scope.account_id.trim().is_empty()
            && !scope.instance_id.trim().is_empty()
            && scope.generation > 0,
        "operation receipt requires a complete scope"
    );
    if fixture.get("expected").and_then(Value::as_str) == Some("reject") {
        if require_string(&fixture, "error_code")? != "idempotency_conflict" {
            bail!("unexpected operation rejection");
        }
        if !payload.contains_key("idempotency_key") {
            bail!("idempotency conflict requires the key");
        }
    }
    Ok(())
}

fn validate_roots(fixture: &Value) -> Result<()> {
    let installation = require_string(&fixture["payload"], "installation_root")?;
    for key in [
        "device_config_root",
        "account_data_root",
        "asset_root",
        "log_root",
    ] {
        let value = require_string(&fixture["payload"], key)?;
        if fixture.get("expected").and_then(Value::as_str) == Some("accept")
            && !value.starts_with(installation)
        {
            bail!("accepted legacy isolation must keep all roots under installation root");
        }
    }
    Ok(())
}

fn validate_route(fixture: &Value) -> Result<()> {
    let payload = fixture["payload"].as_object().context("route object")?;
    let operation = payload
        .get("operation")
        .and_then(Value::as_str)
        .context("route operation")?;
    let executor = payload
        .get("executor")
        .and_then(Value::as_str)
        .context("route executor")?;
    if operation.starts_with("auth.") && operation != "auth.status" && executor != "cli" {
        bail!("identity operations route to CLI");
    }
    Ok(())
}
