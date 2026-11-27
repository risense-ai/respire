use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(in crate::autostart) enum Phase {
    Paused,
    Starting,
    Verified,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    pub(super) schema: u32,
    pub(super) id: String,
    pub(super) token: String,
    pub(super) phase: Phase,
    pub(super) expected_profile: Option<PathBuf>,
    pub(super) expected_version: String,
    pub(super) owner_pid: u32,
    pub(super) owner_birth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) snapshot: Option<snapshot_control::SnapshotMarkerBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_config: Option<Value>,
}

pub(super) fn marker_path(root: &Path) -> PathBuf {
    control_path(root).with_file_name("maintenance.json")
}
pub(super) fn marker_bytes(root: &Path) -> Result<Option<Vec<u8>>> {
    let path = marker_path(root);
    let meta = match fs::symlink_metadata(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink()
            && meta.len() <= MAINTENANCE_MARKER_MAX_BYTES as u64,
        "invalid runtime maintenance marker"
    );
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(MAINTENANCE_MARKER_MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAINTENANCE_MARKER_MAX_BYTES, "runtime maintenance marker too large");
    Ok(Some(bytes))
}
pub(super) fn marker(root: &Path) -> Result<Option<Marker>> {
    marker_bytes(root)?.as_deref().map(parse_marker).transpose()
}

pub(super) fn parse_marker(bytes: &[u8]) -> Result<Marker> {
    let value: Marker = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        value.schema == 1 && uuid::Uuid::parse_str(&value.token).is_ok(),
        "invalid runtime maintenance marker"
    );
    Ok(value)
}

pub(super) fn controller_alive(value: &Marker) -> Result<bool> {
    Ok(manager::process_birth(value.owner_pid)?.as_deref() == Some(value.owner_birth.as_str()))
}
