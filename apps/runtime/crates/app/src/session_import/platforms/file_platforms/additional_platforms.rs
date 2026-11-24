// ---------------------------------------------------------------- qwen
// `~/.qwen/projects/*/chats/*.jsonl`; generic JSONL transcript.

fn qwen_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".qwen"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_qwen(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(qwen_roots(), &opts.roots, &[".qwen"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "projects/*/chats/*.jsonl"));
    }
    common::jsonl_sessions("qwen", paths)
}

// ---------------------------------------------------------------- codebuff
// `~/.config/{manicode,manicode-dev,manicode-staging,codebuff}/projects/*/chats/*/chat-messages.json`.

fn codebuff_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = scan_util::home() {
        for name in ["manicode", "manicode-dev", "manicode-staging", "codebuff"] {
            candidates.push(home.join(".config").join(name));
        }
    }
    scan_util::existing(candidates)
}

fn scan_codebuff(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(codebuff_roots(), &opts.roots, &["manicode", "codebuff"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "projects/*/chats/*/chat-messages.json"));
    }
    let mut sessions = Vec::new();
    for path in scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES) {
        sessions.extend(codebuff_sessions(&path));
    }
    sessions
}

fn codebuff_sessions(path: &Path) -> Vec<Session> {
    let data = scan_util::read_json(path).unwrap_or(Value::Null);
    let rows = json_maps(&data);
    if rows.is_empty() {
        return Vec::new();
    }
    let (first_user, last_user) = message_edges(&rows, &["user", "human"]);

    let mut usage = Map::new();
    usage.insert("message_count".to_owned(), json!(rows.len()));

    vec![Session {
        platform: "codebuff".to_owned(),
        id: dir_name(path),
        path: path.display().to_string(),
        cwd: None,
        created_at: scan_util::file_mtime_iso(path),
        updated_at: scan_util::file_mtime_iso(path),
        provider: None,
        model: None,
        first_user_message: first_user,
        last_user_message: last_user,
        usage,
        parent_id: None,
        agent: None,
    }]
}

// ------------------------------------------------- roo-code / kilo-code / cline
// VS Code `globalStorage/<extension>/tasks/*/api_conversation_history.json`.

fn roo_roots() -> Vec<PathBuf> {
    task_roots(ROO_EXTENSION)
}

fn scan_roocode(opts: &ScanOptions) -> Vec<Session> {
    scan_task_dirs("roo-code", ROO_EXTENSION, opts)
}

fn kilo_roots() -> Vec<PathBuf> {
    task_roots(KILO_EXTENSION)
}

fn scan_kilocode(opts: &ScanOptions) -> Vec<Session> {
    scan_task_dirs("kilo-code", KILO_EXTENSION, opts)
}

fn cline_roots() -> Vec<PathBuf> {
    task_roots(CLINE_EXTENSION)
}

fn scan_cline(opts: &ScanOptions) -> Vec<Session> {
    scan_task_dirs("cline", CLINE_EXTENSION, opts)
}

fn task_roots(extension: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = scan_util::home() {
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join(extension),
        );
        candidates.push(
            home.join(".config")
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join(extension),
        );
    }
    if let Some(appdata) = scan_util::env_path("APPDATA") {
        candidates.push(
            appdata
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join(extension),
        );
    }
    scan_util::existing(candidates)
}

fn scan_task_dirs(platform: &str, extension: &str, opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(task_roots(extension), &opts.roots, &[extension]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "tasks/*/api_conversation_history.json"));
    }
    let mut sessions = Vec::new();
    for path in scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES) {
        sessions.extend(task_sessions(platform, &path));
    }
    sessions
}

fn task_sessions(platform: &str, path: &Path) -> Vec<Session> {
    let data = scan_util::read_json(path).unwrap_or(Value::Null);
    let rows = json_maps(&data);
    if rows.is_empty() {
        return Vec::new();
    }
    let (first_user, last_user) = message_edges(&rows, &["user"]);

    let mut usage = Map::new();
    usage.insert("message_count".to_owned(), json!(rows.len()));

    vec![Session {
        platform: platform.to_owned(),
        id: dir_name(path),
        path: path.display().to_string(),
        cwd: None,
        created_at: scan_util::file_mtime_iso(path),
        updated_at: scan_util::file_mtime_iso(path),
        provider: None,
        model: None,
        first_user_message: first_user,
        last_user_message: last_user,
        usage,
        parent_id: None,
        agent: None,
    }]
}

// ---------------------------------------------------------------- aider
// Bounded project roots (`~/local-workspaces`, `~/indent`) containing
// `.aider.chat.history.md`; one session per `# aider chat started at` block.

fn aider_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = scan_util::home() {
        candidates.push(home.join("local-workspaces"));
        candidates.push(home.join("indent"));
    }
    scan_util::existing(candidates)
}

fn scan_aider(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(aider_roots(), &opts.roots, &["local-workspaces"]);
    let mut paths = Vec::new();
    for root in &roots {
        for pattern in [
            ".aider.chat.history.md",
            "*/.aider.chat.history.md",
            "*/*/.aider.chat.history.md",
        ] {
            paths.extend(glob_files(root, pattern));
        }
    }
    let mut sessions = Vec::new();
    for path in scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES) {
        sessions.extend(aider_sessions(&path));
    }
    sessions
}

fn aider_sessions(path: &Path) -> Vec<Session> {
    let text = match scan_util::read_text(path) {
        Some(text) => text,
        None => return Vec::new(),
    };
    let parent = path
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    let directory = dir_name(path);
    let mut sessions = Vec::new();

    for block in text.split("# aider chat started at ") {
        if block.trim().is_empty() {
            continue;
        }
        let (first_line, body) = match block.split_once('\n') {
            Some((line, rest)) => (line, rest),
            None => (block, ""),
        };
        let prompt = first_aider_prompt(body);
        let stamp = first_line.trim();
        let sid = format!(
            "{directory}-{}",
            stamp.replace(':', "-").replace(' ', "-")
        );
        sessions.push(Session {
            platform: "aider".to_owned(),
            id: sid,
            path: path.display().to_string(),
            cwd: Some(parent.clone()),
            created_at: Some(stamp.to_owned()),
            updated_at: scan_util::file_mtime_iso(path),
            provider: None,
            model: None,
            first_user_message: prompt.clone(),
            last_user_message: prompt,
            usage: Map::new(),
            parent_id: None,
            agent: None,
        });
    }
    sessions
}

