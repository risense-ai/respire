define_command! {
    /// Review evidence, due records and explicit approval-backed confirmations.
    Review {
        #[command(flatten)]
        args: review::ReviewArgs,
    },
    /// Host-owned foreground runtime and opt-in current-user sign-in startup.
    Runtime {
        #[command(subcommand)]
        command: autostart::RuntimeCommand,
    },
    /// Restart the resident runtime; kill the verified process if graceful shutdown times out.
    Restart {
        /// Seconds to wait for graceful shutdown before force termination.
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// Read the full memory workflow before storing or maintaining memories.
    Prompt,
    /// Print the embedded CLI version (same as `--version` / `-v`)
    #[command(name = "v", visible_alias = "version")]
    V,
    /// Manage independent read-only subtree grants for local tools.
    Grant {
        #[command(subcommand)]
        command: GrantCommand,
    },
    /// List or revoke independent cloud login sessions; does not change local decrypt keys.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Generate local key material (offline; writes local session/keyfile - no server, no auth)
    Keygen {
        /// Compatibility flag: unused for encryption since v4 (the super password is system-generated)
        #[arg(long)]
        pass: Option<String>,
        /// Force overwrite with offline keys even if this machine already has cloud key material (drops the cloud session)
        #[arg(long)]
        force: bool,
    },
    /// Store a memory (local authority store; encrypts + embeds automatically).
    /// Judge-then-store (dedup first, then choose): without --force, an internal recall runs first -
    /// high similarity -> suggest merge (`--merge-ids "id1,id2"` deletes old, writes a combined entry);
    /// medium -> suggest attach (`--parent` as cause); no candidate or --force writes directly.
    /// `--parent` = causal attach; parent/child is cause/effect.
    Remember {
        content: String,
        /// Kind; defaults to context for creation and the first source's kind for merge.
        #[arg(long, value_parser = metadata::kind)]
        r#type: Option<String>,
        #[command(flatten)]
        metadata: metadata::Flags,
        #[arg(long, default_value = "")]
        tags: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        project: String,
        #[arg(long, default_value = "")]
        computer: String,
        #[arg(long, default_value = "-1")]
        emotion: f32,
        /// Importance (AI-judged): important (reusable, recall primary zone) | trivial (diary; default). The normal tier is retired.
        #[arg(long, default_value = "trivial")]
        importance: String,
        #[arg(long, default_value = "")]
        parent: String,
        /// Skip judge-then-store and write immediately
        #[arg(long)]
        force: bool,
        /// Merge: delete the listed old memories (comma-separated ids) and store this content as the combined entry (inherits the first cause chain)
        #[arg(long)]
        merge_ids: Option<String>,
        /// This new conclusion replaces a live important memory.
        #[arg(long, conflicts_with = "merge_ids")]
        supersedes: Option<String>,
        /// Bidirectional links to live important memories.
        #[arg(long, value_delimiter = ',', conflicts_with = "merge_ids")]
        see_also: Vec<String>,
    },
    /// Import a JSON-array dump: keep fields, rebuild parent links, sync once after the batch
    Import { file: String },
    /// Semantic recall through the private Core
    Recall {
        query: String,
        /// Override the recall mode for this request without changing saved settings.
        #[arg(long, value_parser = ["fast", "quality"])]
        mode: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        r#type: Option<String>,
        /// Restrict to this project (use when other projects interfere)
        #[arg(long)]
        project: Option<String>,
        /// Recall trace: stderr dumps the full routing path (candidates -> scope -> two zones -> per-axis Top10)
        #[arg(long)]
        trace: bool,
        /// Return compact IDs and titles in JSON; use show to read full memory content.
        #[arg(long)]
        titles: bool,
        /// Opt-in complete representations selected by Core under the 1500-char budget.
        #[arg(long, conflicts_with = "titles")]
        brief: bool,
        /// Base64-encoded RenderBudget JSON wrappers; items must be empty (host supplies catalog titles).
        #[arg(long)]
        render_budget_base64: Option<String>,
        /// Filter final Core scores before the render budget (does not change retrieval admission).
        #[arg(long)]
        render_min_score: Option<f32>,
        /// Disable associations for this request (original recall results only).
        #[arg(long)]
        no_related: bool,
        /// Recall intent: auto (keyword), howto (extra skill pass, keeps one procedure visible), fact (single pass)
        #[arg(long, value_parser = ["auto", "howto", "fact"], default_value = "auto")]
        intent: String,
    },
    /// Query log: each recall's candidates + adoption + model self-grade (local DPO/SFT raw material)
    QueryLog {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Stats only: query count, adoption rate, empty-candidate rate, good/bad self-grade, top entries
        #[arg(long)]
        stats: bool,
        /// JSON output
        #[arg(long)]
        json: bool,
        /// mark: model self-grade (true hit --good / misleading --bad); the main post-training signal
        #[command(subcommand)]
        cmd: Option<QueryLogCmd>,
    },
    /// Retrieval quality benchmark: run an evalset for hit-rate/MRR, optional baseline compare; mine drafts an evalset from query-log
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Record a procedure run verdict (ok|fail|confounded|no_effect) for a skill/knowledge entry; local-only, feeds how-to ordering and audit
    RunLog {
        /// Skill/knowledge entry id (full id or unique prefix)
        id: String,
        /// Outcome: ok | fail (plain success/failure) | confounded (other changes interfered, cannot attribute) | no_effect (worked, but changed nothing observable)
        #[arg(long, value_parser = ["ok", "fail", "confounded", "no_effect"], default_value = "ok")]
        status: String,
        /// Short note: what deviated or which command failed (local-only, never synced)
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Batch classify via JEV (TypeSafe): ask which top-level category each entry belongs to, compare with the current parent (read-only)
    Classify {
        /// Process the latest N entries (default 50, so a full-store run is not accidental; --all overrides)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Whole store (overrides --limit)
        #[arg(long)]
        all: bool,
        /// Restrict to a subtree (id or 8-char prefix; includes the node itself and descendants)
        #[arg(long)]
        root: Option<String>,
        /// Truncate each entry body to this many chars (API payload cap)
        #[arg(long, default_value_t = 600)]
        max_chars: usize,
        /// Below this confidence, list as "low confidence, needs a human" (default 0.5)
        #[arg(long, default_value_t = 0.5)]
        min_confidence: f32,
        /// Save suggestion JSON (audit / later apply)
        #[arg(long)]
        save: Option<String>,
        /// Dry run: preview final actions without network access or local writes
        #[arg(long)]
        dry_run: bool,
        /// Use the DS (OpenAI-compatible) backend to emulate JEV: `--ds` uses a stored key; `--ds <key>` takes the key and stores it
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        ds: Option<String>,
        /// Classify backend: jev (TypeSafe official Jev, default) | ds (DeepSeek-compatible emulate) - same as --ds; pick one
        #[arg(long)]
        backend: Option<String>,
        /// Requested classification sample count
        #[arg(long, default_value_t = 5, hide = true)]
        samples: usize,
        /// Tree mode: pick a suggested parent among **existing tree nodes** (not a linear 23-root assignment) - pair with --batch for speed
        #[arg(long)]
        tree: bool,
        /// Requested tree classification batch size
        #[arg(long, default_value_t = 12, hide = true)]
        batch: usize,
        /// Maximum tree depth selected for classification
        #[arg(long, default_value_t = 2, hide = true)]
        tree_depth: usize,
        /// Causal-tree segment rewrite: parent=cause, child=effect; walk by level and let the AI judge order
        /// (prints a resort --spec compatible reparent plan; does not write the store)
        #[arg(long)]
        causal: bool,
        /// Causal mode: only process parents with >= this many children (default 3 - few children have no causal order to sort)
        #[arg(long, default_value_t = 3, hide = true)]
        min_kids: usize,
        /// Causal mode: max segments to process (default 0 = all; more children first)
        #[arg(long, default_value_t = 0, hide = true)]
        segments: usize,
        /// Causal mode: write the reparent plan to a file (resort --spec can consume it)
        #[arg(long)]
        out: Option<String>,
        /// Apply the final organization actions selected by Core
        #[arg(long)]
        auto: bool,
        /// Preview local structural repair actions;
        /// text only, no store writes, no network
        #[arg(long)]
        plan: bool,
        /// Max auto-reparent rounds (default 5)
        #[arg(long, default_value_t = 5, hide = true)]
        rounds: usize,
        /// API base (default TypeSafe; later may switch to a rsrs gateway)
        #[arg(long)]
        api_base: Option<String>,
        /// Model name
        #[arg(long, default_value_t = classify::DEFAULT_MODEL.to_string())]
        model: String,
    },
    /// List recent entries
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Only entries created after this time (RFC3339 or YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Only entries created since the last tidy (maintenance.json resort_at) -
        /// the §3.8 tidy intake: tidy this batch, do not pull the whole store
        #[arg(long)]
        since_resort: bool,
        /// Omit memory bodies from JSON directory rows; use show to read a selected entry.
        #[arg(long)]
        metadata: bool,
    },
    /// Delete (tombstone; propagates with sync)
    Forget { id: String },
    /// Clear current-version ciphertext; pending operations and unresolved conflicts are kept
    Purge { id: String },
    /// Restore a deleted memory by full ID (keeps body and parent; propagates with sync)
    Restore { id: String },
    /// Causal tree: promote - this entry becomes a more-upstream cause (reattach to grandparent/root; effects follow)
    Promote { id: String },
    /// Causal tree: demote - attach this entry under --parent as its effect (extends the chain by one; cycle-safe)
    Demote {
        id: String,
        #[arg(long)]
        parent: String,
    },
    /// Show one entry in full (cause/effect complete, not truncated)
    Show { id: String },
    /// Causal-chain deep dive: fetch the whole chain at once - ancestor bodies (root first) + this entry + descendants to --depth.
    /// After an AI sees a cause-chain name, it can dig without layer-by-layer show.
    Chain {
        id: String,
        /// Descendant depth (default 3; 0 = ancestors + this entry only)
        #[arg(long, default_value_t = 3)]
        depth: usize,
    },
    /// Attach a child under a parent (explicit attach is intent; no judge-then-store; auto-syncs)
    Attach {
        id: String,
        #[arg(long)]
        parent: String,
    },
    /// Causal tree: render a subtree (--from start, default root forest); --outline prints a full-depth id+title directory (AI tidy reads this first)
    Tree {
        #[arg(long, default_value = "")]
        from: String,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        #[arg(long)]
        outline: bool,
        /// Export this node's subtree as Markdown material (same as the client install dialog)
        #[arg(long)]
        material: Option<String>,
    },
    /// Memory tidy: batch reparent (spec={"ops":[{"id":"child","parent":"parent"}]}; id / first 8 chars / root title all work); dry-run by default, --go writes, auto-syncs, and resets the counter
    Resort {
        #[arg(long)]
        go: bool,
        #[arg(long)]
        spec: Option<String>,
        /// Show tidy counter (adds since last tidy / threshold)
        #[arg(long)]
        status: bool,
        /// Manually reset the counter (e.g. after tidying another way)
        #[arg(long)]
        reset: bool,
        /// Set the alert threshold (new-entry count)
        #[arg(long)]
        threshold: Option<u64>,
    },
    /// AI behavior config (agent.json): no args = print the whole file (AI reads it to decide); --set key=value writes a key
    AgentConfig {
        /// Set a key, format key=value (e.g. diary_mode=verbose)
        #[arg(long)]
        set: Option<String>,
    },
    /// Retitle: decrypt -> change title -> reseal (ciphertext payload updates) -> dirty -> auto-sync
    Retitle {
        id: String,
        /// New title (required - omitting it would blank the title; fixed in the 2026-09-20 audit)
        #[arg(long)]
        title: Option<String>,
    },
    /// Batch retitle (JSON array [{"id":...,"title":...}]; one process, one model load)
    RetitleMany { file: String },
    /// Two-way sync: pull cloud delta -> land locally -> push local changes (LWW)
    Sync,
    /// Show conflicts that still need a decision; --all includes resolved and old-generation history
    SyncConflicts {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        all: bool,
        /// Sync latest content and cross-device resolution state first
        #[arg(long)]
        refresh: bool,
    },
    /// Resolve one retained revision; mark it resolved only after the remote confirms
    SyncResolve {
        #[arg(long)]
        epoch: String,
        #[arg(long)]
        rev: i64,
        /// current.rev shown by sync-conflicts, so the head cannot change under you after you compared
        #[arg(long)]
        head_rev: i64,
        #[arg(long,value_parser=["keep-current","take-incoming","merge"])]
        action: String,
        /// For merge, the explicit merged body; metadata stays on the current version
        #[arg(long)]
        content: Option<String>,
    },
    /// Show retained sync revisions; --remote reads server history without local caching
    SyncHistory {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        remote: bool,
    },
    /// Restore a kept revision as a new local edit
    SyncRestore {
        #[arg(long, conflicts_with = "rev")]
        op_id: Option<String>,
        #[arg(long, conflicts_with = "op_id", requires = "epoch")]
        rev: Option<i64>,
        #[arg(long, requires = "rev", conflicts_with = "op_id")]
        epoch: Option<String>,
    },
    /// Rebuild after a server backup restore; keep current recovery snapshots and pending edits
    SyncReset,
    /// Status
    Status,
    /// Read the initialized library's opaque snapshot token without loading a model
    MemoryRevision,
    /// Report supported CLI capabilities without a profile, runtime or model download
    Capabilities,
    /// Recompute all embeddings (for embedder upgrades / dim changes; re-embed by semantics)
    Reembed,
    /// Stock analysis: cluster BGE vectors, flag lone leaves, suggest a tree (read-only)
    Defrag {
        /// User-selected similarity floor for duplicate suggestions
        #[arg(long, default_value_t = 0.60)]
        min: f32,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Split a mixed node (AI decides): no flags prints material JSON for the AI to plan; --go --spec '<json>' applies
    Split {
        id: String,
        /// Apply the split plan
        #[arg(long)]
        go: bool,
        /// AI-authored split-plan JSON (run with no flags first to get material and field docs)
        #[arg(long)]
        spec: Option<String>,
    },
}
