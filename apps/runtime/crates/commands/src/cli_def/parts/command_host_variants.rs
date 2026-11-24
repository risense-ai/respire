macro_rules! define_command_with_host_variants {
    ($($first:tt)*) => {
#[derive(Subcommand)]
pub enum Command {
$($first)*
    /// Deepen the tree: cluster flat children of a large root into sub-roots (--root to inspect/apply; --auto deepens every flat large root)
    TreeDeepen {
        /// Target root id (8-char prefix ok); default = every flat large root
        #[arg(long)]
        root: Option<String>,
        /// Apply the plan (pair with --titles)
        #[arg(long)]
        go: bool,
        /// Sub-root title array JSON (draft order; default = longest member title in the cluster)
        #[arg(long)]
        titles: Option<String>,
        /// One-shot: auto-deepen every flat large root (sub-root title = longest member title; retitle later)
        #[arg(long)]
        auto: bool,
        /// User-selected similarity floor for Core grouping
        #[arg(long, default_value_t = 0.55)]
        min: f32,
    },
    /// Tree hygiene: root-size bill + lone-leaf attach suggestions (read-only; --id --parent actually attaches)
    TreeCure {
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Apply: attach --id under --parent (skip suggestions, just do it)
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        /// Auto-attach: apply every suggestion, skip failures, then summarize
        #[arg(long)]
        auto: bool,
        /// User-selected similarity floor for Core attachment suggestions
        #[arg(long, default_value_t = 0.50)]
        min: f32,
    },
    /// Heat float: an entry with more hits than its parent becomes the grandparent's child (effects follow; dry-run report, --go applies)
    TreeFloat {
        /// Apply: float each reported entry (dirty; propagates with sync)
        #[arg(long)]
        go: bool,
        /// Hit-count floor to float (default 3; relative heat - must be strictly above the parent's hits)
        #[arg(long, default_value_t = 3)]
        min: i64,
    },
    /// Inspect the local model, store and lock, with an optional remote health check
    Doctor {
        /// Also probe remote server reachability (GET /health, ~1–3s)
        #[arg(long)]
        remote: bool,
    },
    /// Whole-store health audit: orphans / illegal importance / duplicate titles / truncated titles / deep chains; structured report for an AI audit
    Audit {
        /// JSON output (client/AI consumption)
        #[arg(long)]
        json: bool,
    },
    /// Verify the local ciphertext-digest witness chain (not an external trust anchor)
    Witness {
        #[arg(long)]
        verify: bool,
    },
    /// Inspect or configure classification without running a model
    ClassifyConfig {
        #[arg(long, value_parser = ["jev", "ds"])]
        backend: Option<String>,
        #[arg(long)]
        api_base: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Persist the selected backend and optional endpoint/model
        #[arg(long)]
        set: bool,
        /// Read one API key from redirected stdin and store it in the OS keyring
        #[arg(long)]
        key_stdin: bool,
    },
    /// Manage embedding profiles, model resources and inference backends.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// View/set local config (client.json: data dir / server address / auto-sync / auto-cure)
    Config {
        /// Set the data directory (absolute path; empty string clears back to default ~/.rsrs)
        #[arg(long)]
        data_dir: Option<String>,
        /// Set the server address
        #[arg(long)]
        addr: Option<String>,
        /// Set post-write auto-sync
        #[arg(long)]
        autosync: Option<bool>,
        /// Set periodic auto-cure
        #[arg(long)]
        cure_auto: Option<bool>,
        /// Max concurrent ordinary runtime jobs. 0 follows available CPU parallelism; explicit values cannot exceed it. Applies after runtime restart.
        #[arg(long)]
        rpc_parallelism: Option<u32>,
    },
    /// Judge-then-store candidate bill (read-only): high similarity suggests merge, medium suggests attach, similar candidates suggest merging them
    Candidates { content: String },
    /// Memory passport: ASCII-art card (user / memory count / connected agents / devices) for sharing
    Passport,
    /// Entry change history (audit stream: create/update/delete/restore/purge; SQLite triggers record it)
    History {
        /// Entry id (8-char prefix ok); omit = recent changes for the whole store
        id: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Backfill: reseal ciphertext so payload.parent_id matches the plaintext column (fixes dual-source drift; idempotent)
    Repack,
    /// Diary = whole-store time chain (the trivial zone is merged in; not filtered by importance): recent (--limit), a day (--date, today/yesterday), a range (--from/--to inclusive), keyword (--contains)
    Diary {
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Flip a day (YYYY-MM-DD or today/yesterday, local timezone)
        #[arg(long)]
        date: Option<String>,
        /// Range start (YYYY-MM-DD, inclusive; mutually exclusive with --date)
        #[arg(long)]
        from: Option<String>,
        /// Range end (YYYY-MM-DD, inclusive; mutually exclusive with --date; --to without --from means "up to that day")
        #[arg(long)]
        to: Option<String>,
        /// Full-text keyword filter (title and decrypted full body); returns a bounded matching excerpt
        #[arg(long)]
        contains: Option<String>,
    },
    /// Explicit cross-date diary fragment retrieval; never changes normal recall.
    DiarySearch {
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Budget for fragment result JSON, in Unicode characters (not tokens)
        #[arg(long, default_value_t = 4000)]
        max_chars: usize,
        #[arg(long, default_value = "hybrid", value_parser = ["lexical", "hybrid"])]
        mode: String,
    },
    /// Read one immutable diary fragment locator and optional immediate neighbors.
    DiaryFragment {
        source_id: String,
        fragment_id: String,
        #[arg(long, default_value_t = 0)]
        neighbors: usize,
        #[arg(long, default_value_t = 4000)]
        max_chars: usize,
    },
    /// Built-in top-level catalog (20 human-domain + 3 AI-domain = 23, embedded at compile): --list prints it; --ensure <root,...> creates roots; no args shows current state
    Taxonomy {
        /// List every built-in category (domain / title / surface-word count / gist)
        #[arg(long)]
        list: bool,
        /// Create/fill roots (idempotent): titles must match the catalog, comma-separated; default = fill every missing root
        #[arg(long)]
        ensure: Option<String>,
    },
    /// Create a root outside the catalog (needs the user to say yes): AI proposes -> user allows -> run with --yes; the AI must not create one on its own
    RootCreate {
        title: String,
        /// One-line gist (default = the title)
        #[arg(long)]
        content: Option<String>,
        /// User already allowed this (turn their "yes" into this flag; without it, refuse to write)
        #[arg(long)]
        yes: bool,
    },
    /// Update a memory; omitted fields stay unchanged and --clear-* explicitly clears metadata.
    Update {
        id: String,
        #[command(flatten)]
        metadata: metadata::Flags,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        #[arg(long, value_parser = metadata::kind)]
        kind: Option<String>,
        /// Importance revision: important | normal | trivial (reseals ciphertext payload and the index column)
        #[arg(long)]
        importance: Option<String>,
    },
    /// Export the whole store as plaintext JSON (local backup)
    Export { file: String },
    /// Back up the local store file (whole SQLite copy, ciphertext and index)
    Backup { file: String },
    /// Candidate metadata gate; defaults off and does not establish verification.
    Metadata {
        #[command(subcommand)]
        command: metadata::Command,
    },
    /// Create or inspect verified local snapshots; never rolls back cloud state
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// Show account keys: Account Secret / five-keys (manual cross-device join; leak = loss of the store)
    Secret {
        /// Show the full Secret and five-keys (default is masked)
        #[arg(long)]
        reveal: bool,
    },
    /// Export decrypt keys (super password + Secret Key + vault material) - how a new machine logs in; leak = loss of the store
    KeysExport {
        /// Write a file (mode 600) instead of printing; the file holds every decrypt secret - keep it safe
        #[arg(long)]
        out: Option<String>,
    },
    /// Reset the super password: unlock URK with the current code, issue a new one, upload. Data is unchanged
    SuperReset {
        /// Current super password (omit if already in the local keyring)
        #[arg(long = "super")]
        super_pass: Option<String>,
    },
    /// Five-keys join (another machine): verify and write a local session; a non-empty --addr also logs in for a token
    Fivekeys {
        #[arg(long, default_value = "")]
        addr: String,
        #[arg(long, default_value = "")]
        user: String,
        #[arg(long)]
        pass: String,
        #[arg(long, default_value = "")]
        secret: String,
        #[arg(long)]
        kdf_salt: String,
        #[arg(long)]
        wrapped_urk: String,
        #[arg(long)]
        urk_nonce: String,
        /// Super password (new wrap). If set, login password + Account Secret are not used to unwrap
        #[arg(long = "super", default_value = "")]
        super_pass: String,
    },
    /// Register an account (first device): login password for auth + super password wrapping keys.
    /// Address defaults to https://api.rsrs.rs; use --addr to override it.
    /// Missing user/password drops into an interactive prompt (TTY only).
    Register {
        #[arg(long)]
        addr: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        pass: Option<String>,
        /// Super password (for decrypt; separate from the login password; forgotten = memories unrecoverable)
        #[arg(long = "super")]
        super_pass: Option<String>,
    },
    /// Multiple accounts on one machine: each account has its own data dir (local store/keys/session switch as a unit).
    /// Logging into a new account creates a profile; `account list` lists them; `account <name>` switches (main = primary); `remove <name> --yes` deletes.
    Account {
        /// Action: list | remove | or a profile name (same as use; main is the primary profile)
        action: String,
        /// Profile name (required for remove)
        name: Option<String>,
        /// Confirm a real delete on remove (store and keys are unrecoverable)
        #[arg(long)]
        yes: bool,
    },
    /// Explicitly copy a selected old library into a new account; no startup migration.
    Migrate {
        #[arg(long, requires = "account", conflicts_with = "vault")]
        source: Option<String>,
        #[arg(long, requires = "source")]
        account: Option<String>,
        /// Upgrade only the explicitly selected legacy account vault, preserving its data key
        #[arg(long)]
        vault: bool,
        #[arg(long, requires = "vault")]
        addr: Option<String>,
        #[arg(long, requires = "vault")]
        user: Option<String>,
        #[arg(long, requires = "vault")]
        pass: Option<String>,
        #[arg(long = "super", requires = "vault")]
        super_pass: Option<String>,
        #[arg(long, requires = "vault")]
        secret_key: Option<String>,
        /// Explicit v4 recovery code for the unchanged data key (required on a headless host without keyring)
        #[arg(long, requires = "vault")]
        new_super: Option<String>,
    },
    /// Spaces (virtual accounts): an owner can create several virtual accounts; each is a space with its own super key and data dir.
    /// Join = a member builds that space's profile on their machine from an invite; they can switch freely; leaving is `kick`.
    Space {
        /// Action: list | create <name> | use <name> | invite | join <code> | members | kick | remove
        action: String,
        /// Space name (needed for create/use/remove)
        name: Option<String>,
        /// Note on invite (written to the member session's device_name, so you can tell who is who later)
        #[arg(long)]
        note: Option<String>,
        /// invite --readonly: sign a read-only invite (that member can recall, not write; enforced server-side)
        #[arg(long)]
        readonly: bool,
        /// Invite code for join (the name positional also accepts it)
        #[arg(long)]
        code: Option<String>,
        /// Member session id to kick
        #[arg(long)]
        session: Option<String>,
        /// kick --all: revoke every member session in this space
        #[arg(long)]
        all: bool,
        /// Confirm a real delete on remove (store and keys are unrecoverable)
        #[arg(long)]
        yes: bool,
    },
    /// Log out: clear addr/token and drop the cloud (identity and keys stay; login brings them back); --full forgets this machine's identity
    Logout {
        /// Delete the whole session.json (including key material - rejoin needs register or five-keys)
        #[arg(long)]
        full: bool,
    },
    /// Log in through browser authorization, or use --interactive for password and TOTP prompts.
    /// Address defaults to the last server this machine used (kept in session.json by register/logout).
    /// Missing user/password drops into an interactive prompt (TTY only).
    #[command(
        after_long_help = "Vault recovery: cloud authorization does not decrypt memories. Use the existing recovery code or a trusted device's keys-export, then retry login. Missing or incorrect recovery material must not replace the selected account. super-reset rotates an already unlocked vault; it cannot recover a lost data key. keygen --force switches to unrelated offline keys and is not cloud recovery. If all recovery material is lost, existing encrypted data cannot be recovered by resetting a password. See docs/vault-recovery.md."
    )]
    Login {
        #[arg(long)]
        addr: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long, requires = "interactive", conflicts_with = "oauth")]
        pass: Option<String>,
        #[arg(long = "super")]
        super_pass: Option<String>,
        /// Choose OAuth or password/TOTP in this terminal (a supplied --pass selects password login)
        #[arg(long)]
        interactive: bool,
        /// Explicitly select browser authorization (also the default)
        #[arg(long)]
        oauth: bool,
        /// Dashboard origin for a custom server
        #[arg(long)]
        dashboard: Option<String>,
        /// Display the authorization URL without opening a browser
        #[arg(long)]
        no_open: bool,
        /// Copy selected local memories into this server account without plaintext export files
        #[arg(long)]
        migrate_local: bool,
    },
    /// Book material: a root subtree -> volume/chapter full text (for an AI to draft)
    BookMaterial { root: String },
    /// Portrait material: whole-store portrait aggregate (for an AI to draft a portrait)
    PortraitMaterial {
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Share a subtree: emit a prompt another AI can paste (payload is plaintext; the other AI imports from the prompt)
    Share {
        /// Subtree root (8-char prefix ok); default is this machine's scope root, else error
        #[arg(long)]
        root: Option<String>,
        /// Print only the prompt body (pipes/clipboard), no hint lines
        #[arg(long)]
        raw: bool,
        /// Write a file instead of printing (prefer <file>.txt with the full prompt)
        #[arg(long)]
        out: Option<String>,
    },
    /// Import a shared subtree: without --go, print attach-candidate bill (for the AI to judge); --go writes the store
    ShareImport {
        /// Share text file (full prompt or bare payload)
        file: String,
        /// Attach point: hang this subtree under that existing memory (8-char prefix ok); default is a new root
        #[arg(long, default_value = "")]
        parent: String,
        /// Write switch: omit to only print the candidate bill
        #[arg(long)]
        go: bool,
        /// Override the root title (default = the sharer's original title)
        #[arg(long)]
        title: Option<String>,
        /// Bypass Core-reported conflicts and explicitly create a sibling entry
        #[arg(long)]
        force: bool,
    },
    /// Extract local coding-agent sessions (Codex, OpenCode, Claude Code, …) into rsrs memories
    Extract {
        /// List agent sources and whether a local session store exists
        #[arg(long)]
        targets: bool,
        /// Force the interactive TUI picker (needs a TTY)
        #[arg(long, conflicts_with_all = ["targets", "id", "all", "dry_run"])]
        tui: bool,
        /// Extract from every detected source (default when --id is omitted)
        #[arg(long, conflicts_with = "id")]
        all: bool,
        /// Extract from one source (codex/opencode/claude/…); see --targets
        #[arg(long)]
        id: Option<String>,
        /// Scan and summarize without writing any memory
        #[arg(long)]
        dry_run: bool,
        /// Only sessions updated since this bound (today|yesterday|7d|YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Maximum sessions to import per source (newest first)
        #[arg(long)]
        limit: Option<usize>,
        /// Filter by working-directory substring
        #[arg(long)]
        cwd: Option<String>,
        /// Filter by model/provider substring
        #[arg(long)]
        model: Option<String>,
        /// Filter by session id substring
        #[arg(long)]
        session: Option<String>,
        /// Memory kind for imported sessions
        #[arg(long, default_value = "context")]
        kind: String,
        /// Importance: important (default) or trivial (diary)
        #[arg(long, default_value = "important")]
        importance: String,
        /// Optional pre-attach parent (id / tree / anchor title); omit to ingest as roots for a later tidy
        #[arg(long, default_value = "")]
        parent: String,
        /// Include subagent/child sessions (hidden by default)
        #[arg(long)]
        include_subagents: bool,
        /// Re-import sessions already recorded in the cursor
        #[arg(long)]
        force: bool,
    },
}
    };
}
