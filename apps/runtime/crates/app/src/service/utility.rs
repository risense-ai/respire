pub fn home_dir() -> Result<PathBuf> {
    if let Ok(home) = crate::env::var("HOME") {
        if !home.trim().is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))
}

/// Auto title: untitled entries take the first body line (strip markers/time prefix), 20 chars — the tree no longer shows a blank title.
pub fn derive_title(content: &str) -> String {
    let mut t = content.trim();
    for p in [
        "\u{3010}\u{65e5}\u{8bb0}\u{3011}",
        "\u{3010}\u{6d41}\u{6c34}\u{3011}",
        "\u{3010}\u{7ecf}\u{9a8c}\u{3011}",
        "\u{3010}\u{5751}\u{3011}",
        "\u{3010}\u{6559}\u{8bad}\u{3011}",
        "\u{3010}\u{6280}\u{80fd}\u{3011}",
    ] {
        if let Some(rest) = t.strip_prefix(p) {
            t = rest.trim_start();
        }
    }
    let first = t.lines().next().unwrap_or("").trim();
    let toks: Vec<&str> = first.splitn(3, ' ').collect();
    let s = if toks.len() == 3
        && toks[0].len() == 10
        && toks[0].as_bytes().get(4) == Some(&b'-')
        && toks[1].contains(':')
    {
        format!("{} {}", toks[1], toks[2])
    } else {
        first.to_owned()
    };
    // First sentence (up to period/semicolon/newline), 40 chars — no more hard 20-char cut that made duplicate titles
    let sent: String = s
        .split(['\u{3002}', '\u{ff1b}', '\n'])
        .next()
        .unwrap_or(&s)
        .trim()
        .chars()
        .take(40)
        .collect();
    if sent.is_empty() {
        "(untitled)".to_owned()
    } else {
        sent
    }
}

pub fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn current_user() -> String {
    crate::env::var("RSRS_USER").unwrap_or_else(|_| "local".to_owned())
}

/// Filter candidates: trim, drop empty, take the first non-empty (an env var set to empty is also dropped —
/// the old impl did not filter, COMPUTERNAME="" became an empty hostname; fixed 2026-09-20)
fn first_non_empty<I: IntoIterator<Item = String>>(vals: I) -> Option<String> {
    vals.into_iter()
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
}

/// Hostname. Four-tier order: /etc/hostname (Linux authoritative static name) → COMPUTERNAME (Windows)
/// → HOSTNAME (env, only on a login shell) → syscall gethostname (fallback).
/// Fourth tier added 2026-09-20: macOS has no /etc/hostname, and a GUI-launched process does not inherit
/// HOSTNAME (launchd does not set it); without the syscall the first tiers all missed and became unknown-host —
/// a user screenshot showed mac as unknown-host/mac when HOSTNAME/COMPUTERNAME were unset.
pub fn host_name() -> Option<String> {
    first_non_empty([
        std::fs::read_to_string("/etc/hostname").unwrap_or_default(),
        crate::env::var("COMPUTERNAME").unwrap_or_default(),
        crate::env::var("HOSTNAME").unwrap_or_default(),
        whoami::hostname().unwrap_or_default(),
    ])
}

/// Device tag (hostname/platform): remember/import fills computer when empty,
/// so recall across devices can tell which machine a path belongs to — the same path is not always valid on another device (frozen 2026-09-12)
pub fn device_tag() -> String {
    let host = host_name().unwrap_or_else(|| "unknown-host".to_owned());
    let os = if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    format!("{host}/{os}")
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

pub fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        "****".to_owned()
    } else {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}\u{2026}{tail}")
    }
}

/// 8-char prefix → full id (exact id first, then exact outline title, then unique prefix; zero/many hits error).
/// Prefix compare is by chars (Chinese/multibyte ids are safe — [0..8] bytes would panic on a multibyte char; hit 2026-09-08).
pub fn resolve_prefix(all: &[crate::memory::model::StoredMemory], prefix: &str) -> Result<String> {
    // list/show print 🆔 with a # prefix (e.g. #dd9b4ade); paste as-is — this entry strips #
    let prefix = prefix.trim_start_matches('#');
    if let Some(m) = all.iter().find(|s| s.id == prefix && !s.deleted) {
        return Ok(m.id.clone());
    }
    // Attach by outline title: unique exact title hit (catalog outline names are attach names; the AI need not look up an id first)
    let by_title: Vec<String> = all
        .iter()
        .filter(|s| !s.deleted && s.local_title == prefix)
        .map(|s| s.id.clone())
        .collect();
    if by_title.len() == 1 {
        return Ok(by_title[0].clone());
    }
    let chars: Vec<char> = prefix.chars().collect();
    let p8: String = chars.iter().take(8).collect();
    let hits: Vec<String> = all
        .iter()
        .filter(|s| !s.deleted && short_id(&s.id) == p8)
        .map(|s| s.id.clone())
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(crate::input_error::InputError(format!("no such entry: {prefix}")).into()),
        n => Err(crate::input_error::InputError(format!("prefix is not unique ({n} hits): {prefix}")).into()),
    }
}

