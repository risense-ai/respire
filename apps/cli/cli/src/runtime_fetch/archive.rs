fn regular_install_file(root: &Path, name: &str) -> Result<PathBuf> {
    anyhow::ensure!(safe_metadata_path(name), "unsafe install path: {name}");
    anyhow::ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "installation root is not a directory"
    );
    let mut path = root.to_path_buf();
    let mut parts = name.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            if parts.peek().is_some() {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
            "install path contains a link or unsupported file: {}",
            path.display()
        );
    }
    Ok(path)
}

fn safe_entry_name(raw: &str) -> Option<String> {
    let name = raw.strip_prefix("./").unwrap_or(raw).trim_end_matches('/');
    safe_metadata_path(name).then(|| name.to_owned())
}

struct TarHeader {
    name: String,
    size: u64,
    typeflag: u8,
    mode: u64,
}

fn octal_field(field: &[u8]) -> Result<u64> {
    anyhow::ensure!(
        field.first().is_none_or(|byte| byte & 0x80 == 0),
        "archive uses unsupported base-256 header fields"
    );
    let text = String::from_utf8_lossy(field);
    let trimmed = text.trim_matches(|c| c == ' ' || c == '\0');
    anyhow::ensure!(!trimmed.is_empty(), "archive header field is empty");
    u64::from_str_radix(trimmed, 8).context("archive header field is not valid octal")
}

fn c_string_field(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn verify_header_checksum(header: &[u8; 512]) -> Result<()> {
    let stored = octal_field(&header[148..156])?;
    let mut sum: u64 = 0;
    for (index, byte) in header.iter().enumerate() {
        sum += if (148..156).contains(&index) {
            u64::from(b' ')
        } else {
            u64::from(*byte)
        };
    }
    anyhow::ensure!(sum == stored, "archive header checksum mismatch");
    Ok(())
}

/// Parse plain ustar headers. Extensions, links and special files are rejected.
fn parse_header(header: &[u8; 512]) -> Result<Option<TarHeader>> {
    if header.iter().all(|byte| *byte == 0) {
        return Ok(None);
    }
    anyhow::ensure!(
        &header[257..262] == b"ustar",
        "archive is not a ustar tarball"
    );
    verify_header_checksum(header)?;
    let typeflag = header[156];
    anyhow::ensure!(
        matches!(typeflag, b'0' | 0 | b'5'),
        "archive contains unsupported links or special files"
    );
    let mut name = c_string_field(&header[0..100]);
    let prefix = c_string_field(&header[345..500]);
    if !prefix.is_empty() {
        name = format!("{prefix}/{name}");
    }
    Ok(Some(TarHeader {
        name,
        size: octal_field(&header[124..136])?,
        mode: octal_field(&header[100..108])?,
        typeflag,
    }))
}

/// Extract a gzipped tar archive into `dest` with the client2 whitelist:
/// directories and regular files only, per-entry 256 MiB and total 512 MiB
/// caps, directory mode 0700 and file mode 0600 (0700 when any execute bit is
/// set in the archive).
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        fs::File::open(archive).with_context(|| format!("failed to read {}", archive.display()))?;
    let mut reader = flate2::read::MultiGzDecoder::new(file);
    create_private_dir(dest)?;
    let mut paths = BTreeSet::new();
    let mut entries = 0usize;
    let mut total: u64 = 0;
    let mut payload = [0u8; 64 * 1024];
    loop {
        let mut header = [0u8; 512];
        read_exact_bounded(&mut reader, &mut header, archive)?;
        let entry = match parse_header(&header)? {
            Some(entry) => entry,
            None => {
                let mut trailer = Vec::new();
                reader.take((1 << 20) + 1).read_to_end(&mut trailer)?;
                anyhow::ensure!(
                    trailer.len() >= 512
                        && trailer.len() <= 1 << 20
                        && trailer.iter().all(|byte| *byte == 0),
                    "invalid archive trailer"
                );
                break;
            }
        };
        // client2 skips plain "./" directory markers and rejects every other
        // name that fails the whitelist.
        entries += 1;
        anyhow::ensure!(entries <= 4096, "runtime archive has too many entries");
        let bare = entry
            .name
            .strip_prefix("./")
            .unwrap_or(&entry.name)
            .trim_end_matches('/');
        if bare.is_empty() || bare == "." {
            anyhow::ensure!(
                entry.typeflag == b'5' && entry.size == 0,
                "unsafe archive path: {}",
                entry.name
            );
            continue;
        }
        let Some(name) = safe_entry_name(&entry.name) else {
            bail!("unsafe archive path: {}", entry.name);
        };
        anyhow::ensure!(
            entry.size <= MAX_ENTRY_BYTES && total + entry.size <= MAX_TOTAL_BYTES,
            "runtime archive exceeds extraction limit"
        );
        anyhow::ensure!(
            paths.insert(name.to_ascii_lowercase()),
            "duplicate archive path: {name}"
        );
        let mut parent = dest.to_path_buf();
        for component in name
            .split('/')
            .take(name.split('/').count().saturating_sub(1))
        {
            parent.push(component);
            match fs::symlink_metadata(&parent) {
                Ok(metadata) => anyhow::ensure!(
                    metadata.is_dir(),
                    "archive parent is a link or non-directory"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let target = dest.join(&name);
        if entry.typeflag == b'5' {
            anyhow::ensure!(entry.size == 0, "archive directory contains data");
            create_private_dir(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            create_private_dir(parent)?;
        }
        let mut out = {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(if entry.mode & 0o111 != 0 {
                    0o700
                } else {
                    0o600
                });
            }
            options.open(&target)
        }
        .with_context(|| format!("failed to create {}", target.display()))?;
        let mut remaining = entry.size;
        while remaining > 0 {
            let chunk = remaining.min(payload.len() as u64) as usize;
            read_exact_bounded(&mut reader, &mut payload[..chunk], archive)?;
            out.write_all(&payload[..chunk])
                .with_context(|| format!("failed to write {}", target.display()))?;
            total += chunk as u64;
            remaining -= chunk as u64;
        }
        out.flush()
            .with_context(|| format!("failed to write {}", target.display()))?;
        out.sync_all()
            .with_context(|| format!("failed to flush {}", target.display()))?;
        drop(out);
        // Entries are padded to whole 512-byte blocks.
        let padding = (512 - (entry.size % 512)) % 512;
        if padding > 0 {
            let mut skip = [0u8; 512];
            read_exact_bounded(&mut reader, &mut skip[..padding as usize], archive)?;
        }
    }
    Ok(())
}

fn create_private_dir(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir(),
            "private directory is a link or non-directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set mode on {}", dir.display()))?;
    }
    #[cfg(windows)]
    let _ = dir;
    Ok(())
}

fn read_exact_bounded(reader: &mut impl Read, buf: &mut [u8], archive: &Path) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let read = reader
            .read(&mut buf[filled..])
            .with_context(|| format!("failed to read {}", archive.display()))?;
        if read == 0 {
            bail!("archive is truncated: {}", archive.display());
        }
        filled += read;
    }
    Ok(())
}

/// `core-runtime.json` shipped inside each release archive.
#[derive(Debug, Deserialize)]
struct CoreManifest {
    schema_version: u32,
    target: String,
    files: Vec<CoreManifestFile>,
}

#[derive(Debug, Deserialize)]
struct CoreManifestFile {
    path: String,
    sha256: String,
}

/// Verify the extracted stage against the shipped manifest and the expected
/// binary hash (client2 `verifyRuntime` semantics). `binary_file` is the
/// archive-side binary name inside `stage`.
fn verify_stage(
    stage: &Path,
    binary_file: &str,
    triple: &str,
    binary_hash: &str,
) -> Result<PathBuf> {
    let binary = regular_install_file(stage, binary_file)?;
    let actual = sha256_file(&binary)?;
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(binary_hash),
        "runtime checksum mismatch: expected {binary_hash}, found {actual}"
    );
    let manifest_path = regular_install_file(stage, CORE_MANIFEST_FILE)?;
    anyhow::ensure!(
        manifest_path.is_file(),
        "archive did not contain {}",
        CORE_MANIFEST_FILE
    );
    anyhow::ensure!(
        fs::metadata(&manifest_path)?.len() <= MAX_METADATA_BYTES as u64,
        "runtime manifest is too large"
    );
    let bytes = fs::read(&manifest_path)?;
    let manifest: CoreManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid {} manifest", CORE_MANIFEST_FILE))?;
    anyhow::ensure!(
        manifest.schema_version == 1 && manifest.target == triple && !manifest.files.is_empty(),
        "invalid {} manifest",
        CORE_MANIFEST_FILE
    );
    let mut paths = BTreeSet::new();
    anyhow::ensure!(
        manifest.files.len() <= 4096,
        "runtime manifest has too many files"
    );
    for file in &manifest.files {
        anyhow::ensure!(
            paths.insert(file.path.to_ascii_lowercase()),
            "duplicate runtime manifest path: {}",
            file.path
        );
        anyhow::ensure!(
            safe_metadata_path(&file.path),
            "unsafe runtime manifest path: {}",
            file.path
        );
        anyhow::ensure!(
            valid_sha256(&file.sha256),
            "invalid runtime manifest checksum"
        );
        if file.path.starts_with("core-notices/") && !stage.join(&file.path).try_exists()? {
            continue;
        }
        let target = regular_install_file(stage, &file.path)?;
        anyhow::ensure!(
            target.is_file(),
            "runtime manifest file is missing: {}",
            file.path
        );
        let actual = sha256_file(&target)?;
        anyhow::ensure!(
            actual.eq_ignore_ascii_case(&file.sha256),
            "runtime checksum mismatch: {}",
            file.path
        );
    }
    Ok(binary)
}
