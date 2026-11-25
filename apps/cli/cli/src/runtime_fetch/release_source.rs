/// The chosen repository and version travel together through metadata,
/// archive and GitHub asset-API recovery. Only discovery can choose legacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeReleaseSource {
    Monorepo,
    Legacy,
}

impl RuntimeReleaseSource {
    fn repository(self) -> &'static str {
        match self {
            Self::Monorepo => "risense-ai/respire",
            Self::Legacy => RUNTIME_REPO,
        }
    }

    fn tag_prefix(self) -> &'static str {
        match self {
            Self::Monorepo => "runtime-v",
            Self::Legacy => "v",
        }
    }

    fn tag(self, version: &str) -> String {
        format!("{}{version}", self.tag_prefix())
    }

    fn api(self) -> String {
        format!(
            "https://api.github.com/repos/{}/releases",
            self.repository()
        )
    }

    fn browser_url(self, origin: &str, version: &str, name: &str) -> String {
        format!(
            "{origin}/{}/releases/download/{}/{name}",
            self.repository(),
            self.tag(version)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeReleaseId {
    source: RuntimeReleaseSource,
    version: String,
}

#[derive(Deserialize)]
struct RuntimeReleaseListing {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}

#[derive(Deserialize)]
struct ExactRuntimeRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}

impl ExactRuntimeRelease {
    fn matches(&self, source: RuntimeReleaseSource, version: &str) -> bool {
        !self.draft
            && self.tag_name == source.tag(version)
            && self
                .published_at
                .as_deref()
                .is_some_and(|date| !date.is_empty())
            && (source == RuntimeReleaseSource::Legacy
                || component_runtime_is_dev(version) == Some(self.prerelease))
    }
}

const RELEASE_PAGE_LIMIT: u32 = 40;
const RELEASE_PAGE_SIZE: usize = 100;

fn component_runtime_is_dev(version: &str) -> Option<bool> {
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
fn runtime_listing_choice(
    source: RuntimeReleaseSource,
    channel: RuntimeChannel,
    mut list: impl FnMut(RuntimeReleaseSource, u32) -> Result<Vec<RuntimeReleaseListing>>,
) -> Result<Option<String>> {
    let mut latest = None::<String>;
    let mut seen = BTreeSet::new();
    for page in 1..=RELEASE_PAGE_LIMIT {
        let entries = list(source, page)?;
        anyhow::ensure!(
            entries.len() <= RELEASE_PAGE_SIZE,
            "invalid Runtime release page size"
        );
        let last = entries.len() < RELEASE_PAGE_SIZE;
        for entry in entries {
            let Some(version) = entry.tag_name.strip_prefix(source.tag_prefix()) else {
                continue;
            };
            if entry.draft {
                continue;
            }
            anyhow::ensure!(
                valid_version(version),
                "invalid published Runtime release tag"
            );
            if source == RuntimeReleaseSource::Monorepo {
                anyhow::ensure!(
                    component_runtime_is_dev(version) == Some(entry.prerelease),
                    "invalid monorepo Runtime component tag or channel"
                );
            }
            let matches_channel = match channel {
                RuntimeChannel::Stable => !entry.prerelease && !version.contains('-'),
                RuntimeChannel::Dev => {
                    entry.prerelease
                        && version.split_once("-dev.").is_some_and(|(base, number)| {
                            !base.contains(['-', '+'])
                                && number
                                    .parse::<u64>()
                                    .is_ok_and(|number_u64| number_u64.to_string() == number)
                        })
                }
            };
            if !matches_channel {
                continue;
            }
            anyhow::ensure!(
                entry
                    .published_at
                    .as_deref()
                    .is_some_and(|date| !date.is_empty())
                    && seen.insert(entry.tag_name.clone()),
                "invalid or duplicate published Runtime release entry"
            );
            if latest
                .as_deref()
                .is_none_or(|old| semver_cmp(version, old).is_gt())
            {
                latest = Some(version.to_owned());
            }
        }
        if last {
            return Ok(latest);
        }
    }
    anyhow::bail!("Runtime release listing exceeded its page limit")
}

fn resolve_runtime_release_with(
    pin: Option<&str>,
    channel: RuntimeChannel,
    mut list: impl FnMut(RuntimeReleaseSource, u32) -> Result<Vec<RuntimeReleaseListing>>,
    mut exact: impl FnMut(RuntimeReleaseSource, &str) -> Result<Option<ExactRuntimeRelease>>,
) -> Result<RuntimeReleaseId> {
    for source in [RuntimeReleaseSource::Monorepo, RuntimeReleaseSource::Legacy] {
        if let Some(version) = pin {
            anyhow::ensure!(valid_version(version), "invalid explicit Runtime version");
            if let Some(release) = exact(source, version)? {
                anyhow::ensure!(
                    source != RuntimeReleaseSource::Monorepo
                        || component_runtime_is_dev(version).is_some(),
                    "invalid exact monorepo Runtime component tag"
                );
                anyhow::ensure!(
                    release.matches(source, version),
                    "invalid exact Runtime release identity"
                );
                return Ok(RuntimeReleaseId {
                    source,
                    version: version.to_owned(),
                });
            }
        } else if let Some(version) = runtime_listing_choice(source, channel, &mut list)? {
            // Once discovery saw a suitable publisher, every subsequent problem,
            // including a release disappearing, is an error for that source.
            let release =
                exact(source, &version)?.context("selected Runtime release disappeared")?;
            anyhow::ensure!(
                release.matches(source, &version),
                "invalid selected Runtime release identity"
            );
            return Ok(RuntimeReleaseId { source, version });
        }
    }
    anyhow::bail!("no public Runtime release matches the requested version or channel")
}

/// Only this exact unauthenticated public GitHub API lookup may classify a
/// validated 404 as absence. Do not classify a mirror or a listing error.
fn runtime_release_api(url: &str, exact: bool) -> Result<Option<Vec<u8>>> {
    let response = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(30))
        .build()
        .get(url)
        .set("User-Agent", "respire-cli")
        .set("Accept", "application/vnd.github+json")
        .call();
    let (response, absent) = match response {
        Ok(response) => {
            anyhow::ensure!(
                response.status() == 200,
                "unexpected Runtime release API status"
            );
            (response, false)
        }
        Err(ureq::Error::Status(404, response)) if exact => (response, true),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_METADATA_BYTES,
        "Runtime release API response too large"
    );
    if absent {
        #[derive(Deserialize)]
        struct NotFound {
            message: String,
        }
        let error: NotFound =
            serde_json::from_slice(&bytes).context("invalid official Runtime absence response")?;
        anyhow::ensure!(
            error.message == "Not Found",
            "unrecognized Runtime absence response"
        );
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn resolve_runtime_release(pin: Option<&str>, channel: RuntimeChannel) -> Result<RuntimeReleaseId> {
    resolve_runtime_release_with(
        pin,
        channel,
        |source, page| {
            let url = format!("{}?per_page={RELEASE_PAGE_SIZE}&page={page}", source.api());
            let bytes =
                runtime_release_api(&url, false)?.context("missing Runtime release listing")?;
            serde_json::from_slice(&bytes).context("invalid Runtime release listing")
        },
        |source, version| {
            let url = format!("{}/tags/{}", source.api(), source.tag(version));
            runtime_release_api(&url, true)?
                .map(|bytes| {
                    serde_json::from_slice(&bytes).context("invalid exact Runtime release metadata")
                })
                .transpose()
        },
    )
}

#[cfg(test)]
mod release_source_tests {
    use super::*;
    use RuntimeReleaseSource::{Legacy, Monorepo};

    fn listing(tag: &str, prerelease: bool) -> RuntimeReleaseListing {
        RuntimeReleaseListing {
            tag_name: tag.to_owned(),
            draft: false,
            prerelease,
            published_at: Some("2026-11-25T00:00:00Z".to_owned()),
        }
    }

    fn release(source: RuntimeReleaseSource, version: &str) -> ExactRuntimeRelease {
        ExactRuntimeRelease {
            tag_name: source.tag(version),
            draft: false,
            prerelease: version.contains('-'),
            published_at: Some("2026-11-25T00:00:00Z".to_owned()),
        }
    }

    #[test]
    fn component_latest_ignores_other_components_and_compares_dev_numeric() -> Result<()> {
        let chosen = resolve_runtime_release_with(
            None,
            RuntimeChannel::Dev,
            |source, page| {
                assert_eq!((source, page), (Monorepo, 1));
                Ok(vec![
                    listing("runtime-v2.0.0", false),
                    listing("cli-v9.0.0-dev.9", true),
                    listing("runtime-v2.0.0-dev.9", true),
                    listing("runtime-v2.0.0-dev.10", true),
                ])
            },
            |source, version| {
                assert_eq!((source, version), (Monorepo, "2.0.0-dev.10"));
                Ok(Some(release(source, version)))
            },
        )?;
        assert_eq!(chosen.source, Monorepo);
        assert_eq!(chosen.version, "2.0.0-dev.10");
        Ok(())
    }

    #[test]
    fn only_explicit_absence_or_valid_listing_without_candidate_permits_legacy() -> Result<()> {
        let mut source_calls = Vec::new();
        let chosen = resolve_runtime_release_with(
            Some("1.2.3"),
            RuntimeChannel::Stable,
            |_, _| panic!("pin uses the exact tag API only"),
            |source, version| {
                source_calls.push(source);
                Ok((source == Legacy).then(|| release(source, version)))
            },
        )?;
        assert_eq!(source_calls, [Monorepo, Legacy]);
        assert_eq!(chosen.source, Legacy);
        source_calls.clear();
        let chosen = resolve_runtime_release_with(
            None,
            RuntimeChannel::Stable,
            |source, _| {
                source_calls.push(source);
                Ok(if source == Monorepo {
                    vec![listing("client2-v9.0.0", false)]
                } else {
                    vec![listing("v1.2.4", false)]
                })
            },
            |source, version| {
                assert_eq!(source, Legacy);
                Ok(Some(release(source, version)))
            },
        )?;
        assert_eq!(source_calls, [Monorepo, Legacy]);
        assert_eq!(chosen.version, "1.2.4");
        Ok(())
    }

    #[test]
    fn ambiguous_primary_never_probes_legacy() {
        for kind in ["auth", "network", "schema", "hash"] {
            let selected = resolve_runtime_release_with(
                Some("1.2.3"),
                RuntimeChannel::Stable,
                |_, _| panic!("pin must not list"),
                |source, _| {
                    assert_eq!(source, Monorepo);
                    anyhow::bail!("{kind}");
                },
            );
            assert!(selected.unwrap_err().to_string().contains(kind));
        }
        let disappeared = resolve_runtime_release_with(
            None,
            RuntimeChannel::Stable,
            |source, _| {
                assert_eq!(source, Monorepo);
                Ok(vec![listing("runtime-v1.2.3", false)])
            },
            |source, _| {
                assert_eq!(source, Monorepo);
                Ok(None)
            },
        );
        assert!(disappeared.unwrap_err().to_string().contains("disappeared"));
        let malformed = resolve_runtime_release_with(
            None,
            RuntimeChannel::Stable,
            |source, _| {
                assert_eq!(source, Monorepo);
                Ok(vec![listing("runtime-v01.2.3", false)])
            },
            |_, _| panic!("invalid catalog must not resolve an exact release"),
        );
        assert!(malformed.unwrap_err().to_string().contains("component tag"));
    }

    #[test]
    fn chosen_source_binds_browser_and_asset_rescue_to_same_exact_tag() -> Result<()> {
        let binary = "runtime-build-x86_64-pc-windows-msvc.json";
        for source in [Monorepo, Legacy] {
            for origin in [RELEASE_BASE, "https://gh-proxy.com/https://github.com"] {
                let url = source.browser_url(origin, "2.5.0", binary);
                assert!(url.contains(&format!(
                    "/{}/releases/download/{}/",
                    source.repository(),
                    source.tag("2.5.0")
                )));
                assert_eq!(
                    official_asset_identity(&url),
                    (origin == RELEASE_BASE).then_some((source, "2.5.0", binary))
                );
            }
        }
        let fixture = serde_json::json!({
            "tag_name":"v2.5.0", "draft":false, "assets":[{
                "name":binary, "id":1, "size":32, "state":"uploaded", "digest":format!("sha256:{}", "a".repeat(64))
            }]
        });
        assert!(public_asset(
            serde_json::from_value(fixture)?,
            Monorepo,
            "2.5.0",
            binary,
            32
        )
        .is_err());
        Ok(())
    }
}
