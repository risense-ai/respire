//! Host-owned component planning, configuration publication and file verification.
//! Only fresh core backend evidence comes from Runtime's preparation control.
use anyhow::{Context, Result};
use respire_client_host::lock::LibraryLock;
use respire_commands::output::{Item, ResultEnvelope, Status};
use respire_protocol::model_maintenance::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[cfg(windows)]
const PACKAGE_SOURCE: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.ai.machinelearning/2.4.89/microsoft.windows.ai.machinelearning.2.4.89.nupkg";
#[cfg(windows)]
const PACKAGE_SHA256: &str = "5c68ecfb947223267abf159a023f5192ad42725e4e9cc995e7c1470dd54dff63";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

include!("model_maintenance/review.rs");

include!("model_maintenance/operation.rs");

include!("model_maintenance/local_assets.rs");

#[cfg(test)]
#[path = "control_tests/model_maintenance.rs"]
mod tests;
