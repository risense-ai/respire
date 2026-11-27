use super::*;

pub fn final_stop_receipt(health: &Health, operation_id: Option<&str>) -> Result<Value> {
    read_final_stop_receipt(health, operation_id)?.context("final runtime stop receipt is missing")
}

pub fn read_final_stop_receipt(health: &Health, operation_id: Option<&str>) -> Result<Option<Value>> {
    anyhow::ensure!(
        !health.instance_id.is_empty()
            && health.instance_id.len() <= 128
            && health
                .instance_id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_'),
        "invalid runtime stop instance ID"
    );
    let root = respire_client_host::paths::main_data_dir();
    anyhow::ensure!(
        !health.installation_root.is_empty()
            && fs::canonicalize(&health.installation_root)? == fs::canonicalize(&root)?,
        "stopped runtime belongs to another installation"
    );
    let path = root
        .join("runtime")
        .join(format!("stop-{}.json", health.instance_id));
    let entry = match fs::symlink_metadata(&path) {
        Ok(entry) => entry,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read final runtime stop receipt"),
    };
    anyhow::ensure!(entry.is_file(), "invalid stop receipt file");
    let file = fs::File::open(&path).context("cannot read final runtime stop receipt")?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "invalid stop receipt file"
    );
    let mut data = Vec::new();
    use std::io::Read;
    file.take(1024 * 1024 + 1).read_to_end(&mut data)?;
    anyhow::ensure!(data.len() <= 1024 * 1024, "stop receipt exceeds 1 MiB");
    let receipt: Value = serde_json::from_slice(&data).context("invalid final stop receipt")?;
    validate_final_stop_receipt(health, operation_id, &receipt)?;
    Ok(Some(receipt))
}

pub fn validate_final_stop_receipt(
    health: &Health,
    operation_id: Option<&str>,
    receipt: &Value,
) -> Result<()> {
    let expected = serde_json::to_value(health)?;
    for key in [
        "installation_id",
        "instance_id",
        "process_start_id",
        "image_path",
        "image_sha256",
        "runtime_version",
        "account_id",
    ] {
        anyhow::ensure!(
            expected[key]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                && receipt[key] == expected[key],
            "stop receipt identity mismatch: {key}"
        );
    }
    anyhow::ensure!(
        health.pid > 0
            && health.generation > 0
            && receipt["schema_version"] == 1
            && receipt["protocol_version"] == health.v
            && receipt["pid"] == health.pid
            && receipt["generation"] == health.generation
            && receipt["operation_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
            && operation_id.is_none_or(|id| receipt["operation_id"] == id),
        "stop receipt does not match the stopped instance and operation"
    );
    anyhow::ensure!(
        receipt["state"] == "drained"
            && receipt["unfinished"].as_array().is_some_and(Vec::is_empty),
        "runtime stopped without a proven complete drain"
    );
    Ok(())
}
