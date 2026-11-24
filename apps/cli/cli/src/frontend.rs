// rsrs CLI: parse commands, manage components, and render Runtime replies.
// Business, database, credential and inference work stays in the independent
// Runtime. Host code prepares its assets, configuration and private controls.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use clap::Parser;

pub mod autostart;
mod classify_config;
mod client_config;
#[cfg(any(test, feature = "client2-host-prototype"))]
mod client2_host;
mod doctor;
mod host_rpc;
mod login;
mod mcp;
mod profile_control;
mod profile_discovery;
mod rpc;
mod runtime_fetch;
mod shell;
mod web;

// Shared command surface extracted into the respire_commands crate; the bin
// re-binds the module names so existing `crate::` paths keep resolving.
pub use respire_commands::app_version;
use respire_commands::capabilities::run_capabilities;
pub use respire_commands::classify;
use respire_commands::cli_def::{
    Cli, Command, ModelAction, PluginCommand, SessionCommand, SnapshotCmd,
};
use respire_commands::gates::{
    is_off_allowed, is_readonly_switch_off, is_write_command, is_write_command_fine,
};
pub use respire_commands::i18n;
pub use respire_commands::metadata;
pub use respire_commands::model_progress;
pub use respire_commands::net_rpc;
pub use respire_commands::output::{
    self, Item as OutputItem, ResultEnvelope, Status as OutputStatus,
};
use respire_commands::output_emit::{emit_details, emit_result};
use respire_commands::output_state::DIRECT_MODE;
use respire_commands::output_state::{
    exit_code, json_mode, mark_emitted, output_emitted, parse_failure, progress_enabled,
    set_exit_code, set_json_mode, status_exit_code, Captured,
};
use respire_commands::progress;
pub use respire_commands::runtime_error;
pub use respire_commands::runtime_policy;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[path = "frontend/bootstrap.rs"]
mod bootstrap;
pub use bootstrap::run_frontend;
use bootstrap::{command_failure, install_cli_host_hooks, install_error_logging, prepare_command_runtime};
#[path = "frontend/dispatch.rs"]
mod dispatch;
use dispatch::run;
#[path = "frontend/host_commands.rs"]
mod host_commands;
use host_commands::{host_command, run_agent_config_read, run_extract_tui, run_super_reset};
#[path = "frontend/host_actions.rs"]
mod host_actions;
use host_actions::*;

pub(crate) fn capture_run(args: Vec<String>) -> Captured {
    // Host capture parses and collects output once; Runtime owns business retries.
    respire_commands::capture::capture_exec(args, run)
}

#[cfg(test)]
#[path = "frontend/capture_tests.rs"]
mod capture_tests;
