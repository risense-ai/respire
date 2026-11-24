//! Default screen when `rsrs` is started with no command in a terminal.
//!
//! The home page polls the resident runtime. Keys are only arrows, digits,
//! Enter, and Space. Backspace is accepted only while a server address is
//! being typed, because a typo otherwise cannot be corrected.

use std::io::{stdout, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use serde_json::Value;

use crate::i18n::{self, Lang};

mod model_task;
mod model_weights;
mod recall_api;
mod recall_fusion;
mod recall_test;
mod text_input;

const FAST: Duration = Duration::from_millis(250);
const SLOW: Duration = Duration::from_secs(5);

enum Page {
    Home,
    Accounts,
    Migration,
    Server,
    Inject,
    Sync,
    Model,
    Workspace,
    Lang,
    Version,
    Web,
}

enum ConfirmKind {
    Switch(String),
    Migrate(String, String),
    Sync,
    InstallM3,
    UninstallM3,
    CancelModel(String),
    RestartM3(String, String),
    Engine(String),
    Weights(String, String),
    InstallVariant(String, String),
    InstallEngines,
    ProbeEngine,
    RecallMode(String),
    Workspace(String),
    Autosync(bool),
    Update(String, String),
}

enum Overlay {
    None,
    RecallApi(recall_api::Form),
    RecallFusion(recall_fusion::Form),
    ModelMirror(text_input::Input),
    ModelWeights(model_weights::Form),
    SavingWeights,
    HostModel,
    RecallTest(recall_test::Form),
    ModelTask(model_task::Task),
    Confirm {
        text: String,
        cursor: usize,
        kind: ConfirmKind,
    },
    Edit {
        buf: String,
        cursor: usize,
    },
}

struct InjectRow {
    id: String,
    name: String,
    state: String,
    seen: bool,
    checked: bool,
}

struct AccountRow {
    name: String,
    user: String,
    current: bool,
}

struct Live {
    inference: Value,
    index: Value,
    model_progress: Value,
    engine: String,
    model_settings: Value,
    connected: bool,
    error: String,
    notice: String,
    user: String,
    addr: String,
    autosync: bool,
    workspace: String,
    local_alive: u64,
    local_total: u64,
    phase: String,
    pulled: u64,
    pushed: u64,
    remote_alive: u64,
    conflicts: i64,
    pending: i64,
    pid: u32,
    url: String,
    runtime_version: String,
    doctor: Vec<(String, String, String)>,
    inject: Vec<InjectRow>,
    accounts: Vec<AccountRow>,
    fresh: usize,
    stale: usize,
    missing: usize,
    ver_line: String,
    update_spec: String,
}

struct App {
    login_requested: bool,
    migration_profiles: Vec<Value>,
    page: Page,
    cursor: usize,
    overlay: Overlay,
    live: Live,
    workspace_draft: usize,
    autosync_draft: bool,
    manager_pick: usize,
    shared: Arc<Mutex<Live>>,
    outcome: Arc<Mutex<Option<String>>>,
    model_outcome: Arc<Mutex<Option<String>>>,
}

// Odd revisions mean a write is in flight; changed revisions invalidate old reads.
static MODEL_REVISION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

static INJECT_REVISION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

include!("shell/session.rs");

include!("shell/live_status.rs");

include!("shell/navigation.rs");

include!("shell/model_actions.rs");

include!("shell/workspace_updates.rs");

include!("shell/render.rs");

include!("shell/page_views.rs");

include!("shell/model_and_settings_views.rs");

include!("shell/model_menu_tests.rs");

include!("shell/terminal.rs");
