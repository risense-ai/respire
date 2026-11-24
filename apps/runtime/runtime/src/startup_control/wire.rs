//! Private inherited-stdio startup exclusion, independent of public HTTP contracts.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(super) const SCHEMA: u32 = 1;
pub(super) const MAX_FRAME: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Acquire {
    pub schema_version: u32,
    pub installation_root: PathBuf,
    pub profile: PathBuf,
    pub controller_pid: u32,
    pub controller_birth: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Held {
    pub schema_version: u32,
    pub guard_id: String,
    pub installation_root: PathBuf,
    pub profile: PathBuf,
    pub account_id: String,
    pub helper_pid: u32,
    pub helper_birth: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Endpoint {
    pub pid: u32,
    pub process_start_id: String,
    pub image_path: PathBuf,
    pub image_sha256: String,
    pub instance_id: String,
    pub account_id: String,
    pub generation: u64,
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Finish {
    pub guard_id: String,
    // None explicitly aborts startup; it never claims a ready Runtime.
    pub endpoint: Option<Endpoint>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Released {
    pub schema_version: u32,
    pub guard_id: String,
    pub endpoint: Option<Endpoint>,
}
