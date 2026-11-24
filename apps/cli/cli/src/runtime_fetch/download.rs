fn release_asset(origin: &str, path: &str) -> String {
    format!("{origin}/{RUNTIME_REPO}/releases/download/{path}")
}

/// Normalize a user-supplied mirror into a prefix origin, using the same rules
/// as the model installer: http/https origin only, bare hosts default to
/// https, and credentials, paths, queries or fragments are rejected.
fn mirror_origin(raw: &str) -> Result<String> {
    anyhow::ensure!(!raw.trim().is_empty(), "--mirror must not be empty");
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{}", raw.trim())
    };
    let parsed = url::Url::parse(&with_scheme).context("--mirror is invalid")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "--mirror only supports http/https"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "--mirror must be an origin without credentials"
    );
    anyhow::ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "--mirror must be an origin without query or fragment"
    );
    anyhow::ensure!(
        matches!(parsed.path(), "" | "/"),
        "--mirror must be an origin without a path"
    );
    let host = parsed.host_str().context("--mirror is missing a host")?;
    let origin = format!("{}://{}", parsed.scheme(), host);
    if let Some(port) = parsed.port() {
        return Ok(format!("{origin}:{port}"));
    }
    Ok(origin)
}

/// Download origins: an explicit Runtime mirror is tried before the official
/// source. Without an override the
/// official source is tried first and the gh-proxy mirror last, matching
/// client2 `mirrorURL`.
fn download_origins(mirror: Option<&str>) -> Result<Vec<String>> {
    let official = RELEASE_BASE.to_owned();
    let proxied = format!("{MIRROR_PREFIX}/{official}");
    let setting = mirror.map(str::trim).filter(|value| !value.is_empty());
    match setting {
        Some(value) => {
            let origin = mirror_origin(value)?;
            anyhow::ensure!(
                origin == MIRROR_PREFIX || origin == RELEASE_BASE,
                "runtime mirror is not approved"
            );
            let mirrored = if origin == RELEASE_BASE {
                official.clone()
            } else {
                format!("{origin}/{official}")
            };
            if mirrored == official {
                Ok(vec![official])
            } else {
                Ok(vec![mirrored, official])
            }
        }
        None => Ok(vec![official, proxied]),
    }
}

#[derive(Clone, Copy)]
struct DownloadTimeouts {
    connect: Duration,
    read: Duration,
    write: Duration,
}

impl Default for DownloadTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(30),
            write: Duration::from_secs(10),
        }
    }
}

fn download_agent(timeouts: DownloadTimeouts) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(timeouts.connect)
        .timeout_read(timeouts.read)
        .timeout_write(timeouts.write)
        .build()
}

#[derive(Deserialize)]
struct PublicRelease {
    tag_name: String,
    draft: bool,
    assets: Vec<PublicAsset>,
}

fn default_runtime_channel() -> RuntimeChannel {
    if env!("CARGO_PKG_VERSION").contains("-dev.") {
        RuntimeChannel::Dev
    } else {
        RuntimeChannel::Stable
    }
}

fn runtime_channel_name(channel: RuntimeChannel) -> &'static str {
    match channel {
        RuntimeChannel::Stable => "stable",
        RuntimeChannel::Dev => "dev",
    }
}

pub(crate) fn default_runtime_channel_name() -> &'static str {
    runtime_channel_name(default_runtime_channel())
}

fn latest_runtime_release(channel: RuntimeChannel) -> Result<String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        draft: bool,
        prerelease: bool,
        published_at: Option<String>,
    }

    let mut latest: Option<String> = None;
    for page in 1.. {
        let url = format!("https://api.github.com/repos/{RUNTIME_REPO}/releases?per_page=100&page={page}");
        let releases: Vec<Release> = serde_json::from_slice(&fetch_metadata(&url)?)
            .context("invalid Runtime release listing")?;
        let last_page = releases.len() < 100;
        for release in releases {
            let Some(version) = release.tag_name.strip_prefix('v') else { continue };
            let matches_channel = match channel {
                RuntimeChannel::Stable => !release.prerelease && !version.contains('-'),
                RuntimeChannel::Dev => release.prerelease && version.contains("-dev."),
            };
            if !release.draft && release.published_at.is_some() && matches_channel && valid_version(version)
                && latest.as_deref().is_none_or(|previous| semver_cmp(version, previous).is_gt())
            {
                latest = Some(version.to_owned());
            }
        }
        if last_page {
            break;
        }
    }
    latest.with_context(|| format!("no public Runtime release in the {} channel", runtime_channel_name(channel)))
}

#[derive(Deserialize)]
struct PublicAsset {
    id: u64,
    name: String,
    size: u64,
    state: String,
    digest: String,
}

fn official_asset_identity(url: &str) -> Option<(&str, &str)> {
    let path =
        url.strip_prefix("https://github.com/risense-ai/respire-runtime/releases/download/v")?;
    let (version, name) = path.split_once('/')?;
    (valid_version(version) && safe_metadata_path(name) && !name.contains(['/', '?', '#', '%']))
        .then_some((version, name))
}

fn public_asset(
    release: PublicRelease,
    version: &str,
    name: &str,
    limit: u64,
) -> Result<PublicAsset> {
    anyhow::ensure!(
        !release.draft && release.tag_name == format!("v{version}"),
        "invalid official Runtime release identity"
    );
    let mut matches = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .context("official Runtime asset is missing")?;
    anyhow::ensure!(
        matches.next().is_none()
            && asset.id > 0
            && asset.size > 0
            && asset.size <= limit
            && asset.state == "uploaded"
            && asset
                .digest
                .strip_prefix("sha256:")
                .is_some_and(valid_sha256),
        "invalid or ambiguous official Runtime asset identity"
    );
    Ok(asset)
}

// Only the official browser route's explicit transient HTTP failures use the
// public API for the same asset. TLS, transport and mirror failures stay errors.
fn release_response(
    url: &str,
    limit: u64,
    timeout: Option<Duration>,
) -> Result<(ureq::Response, Option<PublicAsset>)> {
    let request = download_agent(DownloadTimeouts::default())
        .get(url)
        .set("User-Agent", "respire-cli")
        .set("Accept-Encoding", "identity");
    let request = match timeout {
        Some(timeout) => request.timeout(timeout),
        None => request,
    };
    match request.call() {
        Ok(response) => Ok((response, None)),
        Err(error) => {
            if !matches!(&error, ureq::Error::Status(429 | 502 | 503 | 504, _)) {
                return Err(error.into());
            }
            let Some((version, name)) = official_asset_identity(url) else {
                return Err(error.into());
            };
            let api = format!("https://api.github.com/repos/{RUNTIME_REPO}/releases");
            let response = download_agent(DownloadTimeouts::default())
                .get(&format!("{api}/tags/v{version}"))
                .set("Accept", "application/vnd.github+json")
                .set("User-Agent", "respire-cli")
                .timeout(Duration::from_secs(30))
                .call()?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take((MAX_METADATA_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() <= MAX_METADATA_BYTES,
                "official Runtime release metadata too large"
            );
            let asset = public_asset(serde_json::from_slice(&bytes)?, version, name, limit)?;
            let request = download_agent(DownloadTimeouts::default())
                .get(&format!("{api}/assets/{}", asset.id))
                .set("Accept", "application/octet-stream")
                .set("Accept-Encoding", "identity")
                .set("User-Agent", "respire-cli");
            let request = match timeout {
                Some(timeout) => request.timeout(timeout),
                None => request,
            };
            Ok((request.call()?, Some(asset)))
        }
    }
}

/// Fetch a small release metadata document, bounded like client2 `fetchJSON`.
fn fetch_metadata(url: &str) -> Result<Vec<u8>> {
    let (response, asset) = release_response(
        url,
        MAX_METADATA_BYTES as u64,
        Some(Duration::from_secs(30)),
    )
    .with_context(|| format!("release metadata request failed ({url})"))?;
    let mut reader = response.into_reader().take((MAX_METADATA_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .context("release metadata read failed")?;
    anyhow::ensure!(
        bytes.len() <= MAX_METADATA_BYTES,
        "release metadata too large"
    );
    if let Some(asset) = asset {
        anyhow::ensure!(
            bytes.len() as u64 == asset.size
                && format!("sha256:{:x}", Sha256::digest(&bytes))
                    .eq_ignore_ascii_case(&asset.digest),
            "official Runtime metadata asset checksum mismatch"
        );
    }
    Ok(bytes)
}

/// Stream the release archive into `dest` while hashing it. Returns the hex
/// sha256 of the received bytes. A failed transfer leaves no retry state; the
/// caller decides whether to try the next origin.
fn download_archive(url: &str, dest: &Path) -> Result<String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let item = dest.file_name().unwrap_or_default().to_string_lossy();
    model_progress::update("connect", &item, 0, None)?;
    let (response, asset) = release_response(url, MAX_ARCHIVE_BYTES, None)
        .with_context(|| format!("download failed ({url})"))?;
    model_progress::check()?;
    let mut reader = response.into_reader();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(dest)
        .with_context(|| format!("failed to create {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("download interrupted ({url})"))?;
        if read == 0 {
            break;
        }
        model_progress::check()?;
        written += read as u64;
        anyhow::ensure!(
            written <= MAX_ARCHIVE_BYTES,
            "release archive exceeds the {}-byte limit",
            MAX_ARCHIVE_BYTES
        );
        hasher.update(&buf[..read]);
        file.write_all(&buf[..read])
            .with_context(|| format!("failed to write {}", dest.display()))?;
        model_progress::update("download", &item, written, None)?;
    }
    file.flush()
        .with_context(|| format!("failed to write {}", dest.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", dest.display()))?;
    drop(file);
    model_progress::update("verify", &item, written, Some(written))?;
    let actual = hex::encode(hasher.finalize());
    if let Some(asset) = asset {
        anyhow::ensure!(
            written == asset.size && format!("sha256:{actual}").eq_ignore_ascii_case(&asset.digest),
            "official Runtime archive asset checksum mismatch"
        );
    }
    Ok(actual)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Whitelist archive extraction (client2 `extractRuntime` semantics)
// ---------------------------------------------------------------------------
