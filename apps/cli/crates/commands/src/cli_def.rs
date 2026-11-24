//! Shared clap definitions for the rsrs command surface.
//!
//! The CLI binary and the standalone runtime must parse the exact same
//! commands; the definitions live here so the two cannot drift. The bodies
//! are extracted verbatim from the CLI binary (runtime split, 2026-11-03);
//! only visibility was widened to `pub`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{autostart, classify, metadata, review};

#[derive(Parser)]
#[command(
    name = "rsrs",
    version = crate::app_version::clap_version(),
    about = "\u{8de8}\u{8bbe}\u{5907}\u{8de8}\u{8f6f}\u{4ef6}\u{7edf}\u{4e00} AI \u{8bb0}\u{5fc6}\u{7cfb}\u{7edf}\u{ff08}local-first\u{ff09}",
    after_help = "Sandbox: use --client-only or RSRS_CLIENT_ONLY=1 to connect to the host HTTP runtime without managing its lifecycle."
)]
pub struct Cli {
    /// Machine-readable output: JSON only, without human progress.
    #[arg(long, global = true)]
    pub json: bool,
    /// Correlates TUI progress and cancellation with this model task only.
    #[arg(long, global = true, hide = true)]
    pub model_task_id: Option<String>,
    /// Foreground request progress, independent from model cancellation.
    #[arg(long, global = true, hide = true)]
    pub progress_id: Option<String>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum ModelAction {
    /// List built-in/custom profiles and the library's immutable active selection.
    Catalog,
    /// Save model/backend drafts from JSON without changing the active library.
    Configure { file: PathBuf },
    /// Prepare, probe and rebuild a profile before atomic activation.
    /// Backends: onnx (default), mlx (Apple Silicon), host_transport.
    Switch {
        model: String,
        #[arg(long, default_value = "onnx")]
        backend: String,
        #[arg(long, default_value = "cpu", value_parser = ["cpu","gpu","npu"])]
        engine: String,
        #[arg(long)]
        variant: Option<String>,
    },
    /// Read the profile-switch task, cancellation ID and current operation.
    Progress,
    /// Resume the library's last pending immutable profile snapshot.
    Resume,
    /// Download or verify BGE-M3 without rebuilding the index.
    #[command(name = "install", visible_alias = "install-m3")]
    InstallM3 {
        #[arg(long)]
        mirror: Option<String>,
        /// Also fetch this onnx variant (quantized, fp16, int8, uint8, q4, q4f16).
        #[arg(long)]
        model: Option<String>,
        /// Predownload dependencies for this engine without changing the active selection.
        #[arg(long, value_parser = ["npu", "gpu", "cpu"])]
        for_engine: Option<String>,
    },
    /// Download or verify a pinned Granite embedding profile without activating it.
    #[command(name = "install-granite")]
    InstallGranite {
        /// 311m (quantized, default) or 97m (non-quantized).
        #[arg(long, value_parser = ["311m", "97m"], default_value = "311m")]
        model: String,
        #[arg(long)]
        mirror: Option<String>,
    },
    /// Build a resumable index generation and activate it after complete validation.
    Activate {
        #[arg(value_parser = ["m3"])]
        model: String,
    },
    /// Force CPU and terminate the runtime, even when inference or RPC is stuck.
    ResetCpu,
    /// Show or select inference engine. Failures do not switch engines; default CPU.
    Engine {
        #[arg(value_parser = ["npu", "gpu", "cpu"])]
        engine: Option<String>,
        /// Onnx model variant to pin (quantized, fp16, int8, uint8, q4, q4f16).
        #[arg(long)]
        model: Option<String>,
    },
    /// Validate installed weights, load and trial inference, then save selection. Never downloads.
    Load {
        #[arg(value_parser = ["npu", "gpu", "cpu"])]
        engine: String,
        #[arg(long)]
        model: String,
    },
    /// Install compatible NPU execution providers through Windows ML.
    InstallEngines,
    /// Run real inference with the active embedding profile and report its backend.
    Probe {
        #[arg(long)]
        model: Option<String>,
        /// Text to compare against the CPU baseline, including long-input checks.
        #[arg(
            long,
            default_value = "\u{672c}\u{5730}\u{63a8}\u{7406}\u{5f15}\u{64ce}\u{9a8c}\u{8bc1}\u{ff1a}\u{8bb0}\u{5fc6}\u{68c0}\u{7d22}"
        )]
        text: String,
    },
    /// Delete the user-installed BGE-M3 files.
    #[command(name = "uninstall", visible_alias = "uninstall-m3")]
    UninstallM3,
}

include!("cli_def/maintenance_and_host_commands.rs");
include!("cli_def/memory_commands.rs");

#[derive(Subcommand)]
pub enum GrantCommand {
    /// Create a grant; the token is shown only this once - keep it.
    Create {
        #[arg(long)]
        root: String,
        #[arg(long)]
        label: String,
    },
    List,
    Revoke {
        id: String,
    },
}

/// query-log subcommand: model self-grade report.
#[derive(Subcommand, Debug)]
pub enum SnapshotCmd {
    /// Create a consistent private database snapshot, excluding access grants
    Create {
        #[arg(long, default_value = "")]
        note: String,
    },
    /// List snapshots after validating file digests and SQLite integrity
    List,
    /// Restore one local snapshot after a mandatory pre-backup; cloud state is unchanged
    Restore {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Recover an interrupted local snapshot transaction (host only)
    Recover {
        #[arg(long)]
        confirm: bool,
    },
    /// Resume sync only after reviewing local/remote differences at this exact local revision
    ResumeSync {
        #[arg(long)]
        reviewed_revision: String,
    },
}

#[derive(Subcommand)]
pub enum QueryLogCmd {
    /// Report a self-grade: true hit --good (chosen) / misleading --bad (rejected).
    /// ids are comma-separated; 8-char prefixes work; matches recall candidates from the last 10 minutes.
    Mark {
        /// Candidate entry ids (comma-separated; prefixes ok)
        ids: String,
        /// Mark as a true hit (post-training chosen)
        #[arg(long)]
        good: bool,
        /// Mark as misleading (post-training rejected)
        #[arg(long)]
        bad: bool,
        /// Explicit request identity from recall.summary.query_log_id (required with feedback enabled).
        #[arg(long)]
        query_log_id: Option<String>,
        /// Origin of this explicit verdict; never inferred from a successful command.
        #[arg(long,default_value="model",value_parser=["human","model","task_outcome"])]
        source: String,
    },
}

/// bench subcommand: retrieval quality suite (added 2026-09-19).
#[derive(Subcommand)]
pub enum BenchCmd {
    /// Run an evalset: JSONL one case per line {"query","expect":[8-char prefix or full id...],"project?","note?"}; empty expect = negative
    Run {
        /// Evalset file (JSONL)
        file: String,
        /// Eval depth: whether the expected hit is in the top K (default 5)
        #[arg(long, default_value_t = 5)]
        topk: usize,
        /// Archive this run as JSON (for a later --baseline compare)
        #[arg(long)]
        save: Option<String>,
        /// Compare against a baseline (JSON from a previous --save): per-case up/down/same
        #[arg(long)]
        baseline: Option<String>,
        /// Per-case detail (default lists only misses)
        #[arg(long)]
        verbose: bool,
    },
    /// Mine an evalset draft from query-log (self-grade good is the main signal, adopted is weak)
    Mine {
        /// Output JSONL file
        #[arg(long)]
        out: String,
        /// Max cases to export
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Strict: use self-grade good only, ignore adopted as a weak signal
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List independent sessions for this account (the current one is marked current).
    List,
    /// Revoke cloud access for a session; the full UUID comes from session list.
    Revoke { id: String },
}

/// plugin subcommand: inspect hook config and fire events by hand.
#[derive(Subcommand)]
pub enum PluginCommand {
    /// List hooks configured in plugins.json (event -> command/timeout/failure policy)
    List,
    /// Fire one event to verify a plugin (--payload is business JSON; a test hook need not actually write the store)
    Test {
        /// Event name: pre-remember | post-remember | post-recall | post-forget | post-sync
        event: String,
        /// Business payload JSON (default {}); combined with event name/timestamp into the full event on plugin stdin
        #[arg(long, default_value = "{}")]
        payload: String,
    },
}

pub fn changes_profile(command: Option<&Command>) -> bool {
    match command {
        Some(Command::Account { action, .. }) => !matches!(action.as_str(), "list" | "remove"),
        Some(Command::Space { action, yes, .. }) => {
            matches!(action.as_str(), "create" | "use" | "join") || (action == "remove" && *yes)
        }
        Some(Command::Config { data_dir, .. }) => data_dir.is_some(),
        _ => false,
    }
}

/// Classify host UI dispatch without executing extraction business.
pub fn extract_uses_tui(command: &Command) -> anyhow::Result<bool> {
    let Command::Extract {
        targets,
        tui,
        all,
        id,
        dry_run,
        since,
        limit,
        cwd,
        model,
        session,
        kind,
        importance,
        parent,
        include_subagents,
        force,
    } = command
    else {
        return Ok(false);
    };
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());
    let filters_absent = since.is_none()
        && limit.is_none()
        && cwd.is_none()
        && model.is_none()
        && session.is_none()
        && kind.as_str() == "context"
        && importance.as_str() == "important"
        && parent.is_empty()
        && !*include_subagents
        && !*force;
    let bare = !*targets && id.is_none() && !*all && !*dry_run && filters_absent && !crate::output_state::json_mode();
    if *tui {
        if !bare {
            anyhow::bail!(
                "--tui opens the interactive picker and cannot be combined with extraction options \
                 (--since/--limit/--cwd/--model/--session/--kind/--importance/--parent/--include-subagents/--force) \
                 or --json; drop --tui to apply them."
            );
        }
        return Ok(true);
    }
    Ok(bare && interactive)
}
