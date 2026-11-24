//! Business command definitions for Runtime RPC. Host installers, startup
//! registration, injection and component management are parsed by the CLI.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{classify, metadata, review};

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
    /// Build a resumable index generation and activate it after complete validation.
    Activate {
        #[arg(value_parser = ["m3"])]
        model: String,
    },
    /// Show or select inference engine. Failures do not switch engines; default CPU.
    Engine {
        #[arg(value_parser = ["npu", "gpu", "cpu"])]
        engine: Option<String>,
        /// Onnx model variant to pin (quantized, fp16, int8, uint8, q4, q4f16).
        #[arg(long)]
        model: Option<String>,
        /// Apply only if inference.json still has this SHA-256 revision.
        #[arg(long, requires = "engine")]
        expected_revision: Option<String>,
    },
    /// Validate installed weights, load and trial inference, then save selection. Never downloads.
    Load {
        #[arg(value_parser = ["npu", "gpu", "cpu"])]
        engine: String,
        #[arg(long)]
        model: String,
    },
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
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli_def/parts/command_host_variants.rs"));
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli_def/parts/command_memory_variants.rs"));

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
