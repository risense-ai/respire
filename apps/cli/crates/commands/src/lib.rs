#![forbid(unsafe_code)]

//! CLI command parsing, output, host hooks and Runtime transport.
//! Business execution lives in the separate Runtime process.

pub(crate) use respire_client_host::env;

use respire_client_host::paths as client_paths;

pub mod app_version;
pub mod capabilities;
pub mod autostart;
pub mod capture;
pub mod classify;
pub mod cli_def;
pub mod gates;
pub mod hooks;
pub mod host_hooks;
pub mod i18n;
pub mod metadata;
pub mod model_asset_files;
pub mod model_download;
pub mod model_granite;
pub mod model_progress;
pub mod net_rpc;
pub mod output;
pub mod output_emit;
pub mod output_state;
pub mod progress;
pub mod review;
pub mod rpc_client;
pub mod runtime_error;
pub mod runtime_policy;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[path = "../../../test-support/loopback_http.rs"]
mod loopback_http;
