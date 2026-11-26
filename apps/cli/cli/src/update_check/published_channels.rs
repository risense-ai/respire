use std::io::Read;
use std::time::Duration;

pub fn component_version_is_dev(version: &str) -> Option<bool> {
    let (core, suffix) = version
        .split_once('-')
        .map_or((version, None), |(core, suffix)| (core, Some(suffix)));
    let canonical_number = |text: &str| {
        text.parse::<u64>()
            .is_ok_and(|number| number <= 9_007_199_254_740_991 && number.to_string() == text)
    };
    if core.split('.').count() != 3 || !core.split('.').all(canonical_number) {
        return None;
    }
    match suffix {
        None => Some(false),
        Some(suffix) => suffix
            .strip_prefix("dev.")
            .filter(|number| canonical_number(number))
            .map(|_| true),
    }
}

const MAX_LIST_BYTES: usize = 512 * 1024;

fn parse(reader: impl Read) -> Option<[String; 2]> {
    let mut bytes = Vec::new();
    reader.take(MAX_LIST_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_LIST_BYTES { return None; }
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
        draft: bool,
        prerelease: bool,
        published_at: Option<String>,
    }
    let releases: Vec<Release> = serde_json::from_slice(&bytes).ok()?;
    let mut channels = [String::new(), String::new()];
    for release in releases {
        if release.draft || release.published_at.as_deref().unwrap_or("").is_empty() { continue; }
        let Some(version) = release.tag_name.strip_prefix("cli-v") else { continue };
        if component_version_is_dev(version) != Some(release.prerelease) { continue; }
        let best = &mut channels[usize::from(release.prerelease)];
        if best.is_empty() || super::is_newer(version, best) { *best = version.to_owned(); }
    }
    Some(channels)
}

pub(super) fn lookup(custom_registry: bool) -> [String; 2] {
    if custom_registry { return Default::default(); }
    // Probe recent releases only. Missing channels retain their legacy lookup.
    ureq::get("https://api.github.com/repos/risense-ai/respire/releases?per_page=20")
        .set("User-Agent", "rsrs-cli-update")
        .timeout(Duration::from_secs(1))
        .call().ok().and_then(|r| parse(r.into_reader())).unwrap_or_default()
}

/// Registry aliases remain the install target unless a newer exact upstream
/// version is also present in this package's trusted registry metadata.
#[derive(Debug)]
pub struct Choice {
    pub version: String,
    pub(super) exact: bool,
}

impl Choice {
    pub fn install_spec(&self, dev: bool) -> String {
        let target: &str = if self.exact && component_version_is_dev(&self.version) == Some(dev) {
            &self.version
        } else if dev { "dev" } else { "latest" };
        format!("rsrs-cli@{target}")
    }
}

pub(super) fn choose(primary: &str, npm: String, dev: bool, package: impl FnOnce() -> Option<serde_json::Value>) -> Choice {
    if component_version_is_dev(primary) == Some(dev) && (npm.is_empty() || super::is_newer(primary, &npm)) {
        if let Some(entry) = package() {
            if entry["name"] == "rsrs-cli" && entry["version"] == primary {
                return Choice { version: primary.to_owned(), exact: true };
            }
        }
    }
    Choice { version: npm, exact: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn published_cli_versions_use_numeric_channel_order() {
        let row = |tag, dev, draft| json!({"tag_name":tag,"prerelease":dev,"draft":draft,"published_at":"date"});
        let bytes = serde_json::to_vec(&[
            row("cli-v2.0.0",false,false), row("runtime-v9.0.0",false,false),
            row("cli-v3.0.0-dev.10",true,false), row("cli-v3.0.0-dev.2",true,false),
            row("cli-v9.0.0",false,true), row("cli-v8.0.0-dev.01",true,false),
        ]).unwrap();
        assert_eq!(parse(bytes.as_slice()).unwrap(), ["2.0.0", "3.0.0-dev.10"]);
    }

    #[test]
    fn malformed_or_large_listings_and_mirrors_bypass_upstream() {
        assert!(parse(b"broken".as_slice()).is_none());
        assert!(parse(vec![b' '; MAX_LIST_BYTES+1].as_slice()).is_none());
        assert_eq!(lookup(true), [String::new(), String::new()]);
    }

    #[test]
    fn newer_installable_primary_wins_and_unavailable_or_stale_primary_falls_back() {
        let verified = || Some(json!({"name":"rsrs-cli","version":"2.0.0"}));
        let win = choose("2.0.0", "1.0.0".into(), false, verified);
        assert_eq!(win.version, "2.0.0");
        assert_eq!(win.install_spec(false), "rsrs-cli@2.0.0");
        for (upstream, alias) in [("1.0.0","3.0.0"), ("","3.0.0"), ("2.0.0;id","3.0.0")] {
            let legacy = choose(upstream, alias.into(), false, || panic!("unexpected lookup"));
            assert_eq!(legacy.version, alias);
            assert_eq!(legacy.install_spec(false), "rsrs-cli@latest");
        }
        let absent = choose("3.0.0-dev.10","3.0.0-dev.2".into(),true,|| None);
        assert_eq!(absent.version,"3.0.0-dev.2");
        assert_eq!(absent.install_spec(true),"rsrs-cli@dev");
    }
}
