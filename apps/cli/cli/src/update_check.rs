//! update_check - CLI version probe (added 2026-09-20).
//!
//! Why: installed CLIs often lag the latest release (npm wrapper, deb, in-repo binary),
//! and new features (e.g. `chain`) plus inject-source rules travel with the version -
//! if you never learn a release exists, you keep the old behavior.
//! So the memory-maintenance cycle (when write-count hits the TIDY threshold) and `doctor`
//! also query the npm registry once and print an update command when a newer version exists.
//!
//! Three design constraints:
//! 1. **Never block the main flow**: network failure, timeout, or offline all return None.
//!    Version checks must not break remember/doctor.
//! 2. **24h throttle**: results (including failures) land in the CLI installation root's
//!    `update_check.json`;
//!    no second query the same day - avoid hitting the network on every write.
//! 3. **Can be turned off**: `RSRS_UPDATE_CHECK=0` disables;
//!    `RSRS_REGISTRY` swaps the registry (tests / mirrors).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod published_channels;
pub use published_channels::{component_version_is_dev, Choice};

use anyhow::{anyhow, Result};

/// Query timeout in seconds - this is a side action; do not make the user wait.
const TIMEOUT_SECS: u64 = 5;
/// Throttle window in seconds: 24 hours.
const THROTTLE_SECS: i64 = 24 * 3600;
/// Default registry.
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";
/// npm package name for this repo (OIDC publish in the release workflow).
const PACKAGE: &str = "rsrs-cli";

pub fn cache_path() -> Result<PathBuf> {
    Ok(respire_client_host::config::control_installation_root()?.join("update_check.json"))
}

/// Version-check switch: `RSRS_UPDATE_CHECK=0` turns it off.
pub fn enabled() -> bool {
    !matches!(
        respire_client_host::env::var("RSRS_UPDATE_CHECK").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

fn registry_override() -> Option<String> {
    respire_client_host::env::var("RSRS_REGISTRY")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty())
}

/// Semver compare: returns true when `a > b`. Compare numeric segments
/// (`0.2.44` vs `0.2.9` must be correct); a pre-release suffix (`-rc1`) is older
/// than the same numeric version without one.
pub fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> (Vec<u64>, String) {
        let s = s.trim().trim_start_matches('v');
        match s.split_once('-') {
            Some((num, pre)) => (
                num.split('.')
                    .map(|p| p.parse::<u64>().unwrap_or(0))
                    .collect(),
                pre.to_owned(),
            ),
            None => (
                s.split('.')
                    .map(|p| p.parse::<u64>().unwrap_or(0))
                    .collect(),
                String::new(),
            ),
        }
    };
    let (an, apre) = parse(a);
    let (bn, bpre) = parse(b);
    for i in 0..an.len().max(bn.len()) {
        let x = an.get(i).copied().unwrap_or(0);
        let y = bn.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    // Same numeric segments: the one without a pre-release is newer (1.0.0 > 1.0.0-rc1)
    match (apre.is_empty(), bpre.is_empty()) {
        (true, false) => true,
        (false, true) => false,
        _ => match (
            apre.strip_prefix("dev.")
                .and_then(|n| n.parse::<u64>().ok()),
            bpre.strip_prefix("dev.")
                .and_then(|n| n.parse::<u64>().ok()),
        ) {
            (Some(a), Some(b)) => a > b,
            _ => apre > bpre,
        },
    }
}

/// Read cache: (last-checked timestamp, last latest version, last success).
fn read_cache(path: &Path) -> Option<(i64, String, bool, bool)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((
        v["checked_at"].as_i64().unwrap_or(0),
        v["latest"].as_str().unwrap_or("").to_owned(),
        v["ok"].as_bool().unwrap_or(false),
        v["exact"].as_bool().unwrap_or(false),
    ))
}

fn write_cache(path: &Path, latest: &str, ok: bool, exact: bool) {
    let now = chrono::Utc::now().timestamp();
    let v = serde_json::json!({ "checked_at": now, "latest": latest, "ok": ok, "exact": exact });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_vec(&v).unwrap_or_default());
}

/// `latest` and `dev` dist-tags. Failure returns None.
pub fn fetch_dist_tags() -> Option<(String, String)> {
    fetch_channel_choices().map(|(stable, dev)| (stable.version, dev.version))
}

pub fn fetch_channel_choices() -> Option<(Choice, Choice)> {
    let deadline = Instant::now() + Duration::from_secs(TIMEOUT_SECS);
    let registry = registry_override();
    let primary = published_channels::lookup(registry.is_some());
    let url = format!("{}/{}", registry.as_deref().unwrap_or(DEFAULT_REGISTRY), PACKAGE.replace('/', "%2f"));
    let resp = ureq::get(&url)
        .timeout(deadline.checked_duration_since(Instant::now())?)
        .call()
        .ok()?;
    let value: serde_json::Value = resp.into_json().ok()?;
    let tags = value.get("dist-tags")?;
    let latest = tags
        .get("latest")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let dev = tags
        .get("dev")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let select = |index: usize, alias| {
        let version = &primary[index];
        published_channels::choose(version, alias, index == 1, || value["versions"].get(version.as_str()).cloned())
    };
    let latest = select(0, latest);
    let dev = select(1, dev);
    if latest.version.is_empty() && dev.version.is_empty() { return None; }
    Some((latest, dev))
}

/// Fetch the latest version from the npm registry. Failure (offline/timeout/404/non-JSON) -> None.
/// Proxy: ureq reads `HTTPS_PROXY`/`HTTP_PROXY` by default.
pub fn fetch_latest() -> Option<String> {
    stable_choice().map(|choice| choice.version)
}

fn stable_choice() -> Option<Choice> {
    let deadline = Instant::now() + Duration::from_secs(TIMEOUT_SECS);
    let registry = registry_override();
    let primary = published_channels::lookup(registry.is_some());
    let url = format!("{}/{}/latest", registry.as_deref().unwrap_or(DEFAULT_REGISTRY), PACKAGE);
    let resp = ureq::get(&url)
        .timeout(deadline.checked_duration_since(Instant::now())?)
        .call()
        .ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
    let ver = v["version"].as_str()?.trim().to_owned();
    let version = &primary[0];
    Some(published_channels::choose(version, (!ver.is_empty()).then_some(ver)?, false, || {
        let url = format!("{DEFAULT_REGISTRY}/{PACKAGE}/{version}");
        ureq::get(&url).timeout(deadline.checked_duration_since(Instant::now())?)
            .call().ok()?.into_json().ok()
    }))
}

/// Check result.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    /// Local current version (baked in at compile time).
    pub current: String,
    /// Latest version on the registry.
    pub latest: String,
    /// Whether an update is needed.
    pub outdated: bool,
    /// Whether this came from cache (no network).
    pub cached: bool,
    exact: bool,
}

impl UpdateStatus {
    /// Only a real upgrade uses an arrow; an ahead build is not a downgrade offer.
    pub fn comparison(&self) -> String {
        if self.outdated {
            format!("{} -> {}", self.current, self.latest)
        } else if is_newer(&self.current, &self.latest) {
            format!(
                "{} (ahead of published latest {})",
                self.current, self.latest
            )
        } else {
            format!("{} (up to date)", self.current)
        }
    }

    pub fn install_command(&self) -> String {
        let choice = Choice { version: self.latest.clone(), exact: self.exact };
        format!("npm i -g {}", choice.install_spec(false))
    }

    /// User-facing hint line (only worth showing when outdated).
    pub fn message(&self) -> String {
        format!(
            "PROMOTE rsrs CLI has a new version: {} -> {} (local {}) - update: {}",
            self.current, self.latest, self.current, self.install_command()
        )
    }
}

/// Main entry: version check with throttle and cache. Any error returns None -
/// callers treat that as "no hint".
///
/// `force` = skip throttle (`doctor --check-update` and other explicit requests).
pub fn check(force: bool) -> Option<UpdateStatus> {
    if !enabled() {
        return None;
    }
    // Optional update hints still require a valid installation root.
    let path = cache_path().ok()?;
    let current = crate::VERSION.to_owned();
    let now = chrono::Utc::now().timestamp();
    let cached = read_cache(&path);
    if !force {
        if let Some((ts, latest, ok, exact)) = &cached {
            if now - ts < THROTTLE_SECS && (!*exact || registry_override().is_none()) {
                // Inside the window: reuse the cached conclusion (failures are cached too,
                // so we do not hit the network again the same day).
                return ok.then(|| UpdateStatus {
                    outdated: is_newer(latest, &current),
                    current,
                    latest: latest.clone(),
                    cached: true,
                    exact: *exact,
                });
            }
        }
    }
    match stable_choice() {
        Some(Choice { version: latest, exact }) => {
            write_cache(&path, &latest, true, exact);
            Some(UpdateStatus {
                outdated: is_newer(&latest, &current),
                current,
                latest,
                cached: false,
                exact,
            })
        }
        None => {
            write_cache(&path, "", false, false);
            None
        }
    }
}

/// Convenience: one hint line when needed (Some only if outdated).
/// `RSRS_UPDATE_HINT=0` silences it.
pub fn hint(force: bool) -> Option<String> {
    if matches!(
        respire_client_host::env::var("RSRS_UPDATE_HINT").as_deref(),
        Ok("0")
    ) {
        return None;
    }
    check(force).filter(|s| s.outdated).map(|s| s.message())
}

/// For tests and troubleshooting: clear the cache.
pub fn clear_cache() -> Result<()> {
    let p = cache_path()?;
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_compares_numerically_not_lexically() {
        // Numeric compare: 0.2.44 > 0.2.9 (lexical compare would reverse this)
        assert!(is_newer("0.2.44", "0.2.9"));
        assert!(!is_newer("0.2.9", "0.2.44"));
        assert!(is_newer("0.3.0", "0.2.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
        // equal
        assert!(!is_newer("0.2.44", "0.2.44"));
        // unequal segment count: 0.2 and 0.2.0 are equivalent
        assert!(!is_newer("0.2", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2"));
        // v prefix and pre-release
        assert!(is_newer("v0.2.44", "0.2.43"));
        assert!(!is_newer("0.2.44-rc1", "0.2.44"));
        assert!(is_newer("0.2.44", "0.2.44-rc1"));
    }

    #[test]
    fn comparisons_do_not_offer_downgrades() {
        for (current, latest, expected) in [
            ("1.0.9", "1.0.10", "1.0.9 -> 1.0.10"),
            ("1.0.10", "1.0.10", "1.0.10 (up to date)"),
            (
                "1.0.10",
                "1.0.9",
                "1.0.10 (ahead of published latest 1.0.9)",
            ),
            (
                "1.0.11-dev.6",
                "1.0.11-dev.5",
                "1.0.11-dev.6 (ahead of published latest 1.0.11-dev.5)",
            ),
        ] {
            let status = UpdateStatus {
                current: current.into(),
                latest: latest.into(),
                outdated: is_newer(latest, current),
                cached: false,
                exact: false,
            };
            assert_eq!(status.comparison(), expected);
        }
    }

    #[test]
    fn message_mentions_update_command() {
        let s = UpdateStatus {
            current: "0.2.42".into(),
            latest: "0.2.44".into(),
            outdated: true,
            cached: false,
            exact: false,
        };
        let m = s.message();
        assert!(m.contains("0.2.42") && m.contains("0.2.44"), "{m}");
        assert!(m.contains("npm i -g rsrs-cli@latest"), "{m}");
    }

    #[test]
    fn cache_roundtrip_and_throttle_shape() -> Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow!("clock before unix epoch: {e}"))?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("om-uc-{nonce}"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("update_check.json");
        assert!(read_cache(&path).is_none(), "no cache when file is missing");
        write_cache(&path, "0.2.99", true, false);
        let (ts, latest, ok, _) =
            read_cache(&path).ok_or_else(|| anyhow!("expected cache after write"))?;
        assert!(ok && latest == "0.2.99");
        assert!(
            chrono::Utc::now().timestamp() - ts < 60,
            "timestamp should be fresh"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn enabled_respects_env_switch() {
        let _g = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("RSRS_UPDATE_CHECK").ok();
        std::env::remove_var("RSRS_UPDATE_CHECK");
        assert!(enabled(), "enabled by default");
        std::env::set_var("RSRS_UPDATE_CHECK", "0");
        assert!(!enabled(), "=0 disables");
        std::env::set_var("RSRS_UPDATE_CHECK", "off");
        assert!(!enabled(), "=off disables");
        match saved {
            Some(v) => std::env::set_var("RSRS_UPDATE_CHECK", v),
            None => std::env::remove_var("RSRS_UPDATE_CHECK"),
        }
    }
}
