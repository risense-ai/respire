//! Verified, resumable model downloads without app or Core dependencies.
//! Source selection and pinned artifact metadata stay with the installer.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::model_progress;

pub(super) fn file_valid(path: &Path, expected: &str, expected_size: Option<u64>) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    if let Some(size) = expected_size {
        if path.metadata()?.len() != size {
            return Ok(false);
        }
    }
    let actual = sha256_file(path)?;
    Ok(actual.eq_ignore_ascii_case(expected))
}

pub(super) fn sha256_file(path: &Path) -> Result<String> {
    let item = path.file_name().unwrap_or_default().to_string_lossy();
    let total = path.metadata()?.len();
    let mut checked = 0;
    model_progress::update("verify", &item, 0, Some(total))?;
    let mut file =
        File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        checked += n as u64;
        model_progress::update("verify", &item, checked, Some(total))?;
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Bound stalled network operations, not the lifetime of a progressing download.
#[derive(Clone, Copy)]
pub struct DownloadTimeouts {
    connect: std::time::Duration,
    read: std::time::Duration,
    write: std::time::Duration,
}

impl Default for DownloadTimeouts {
    fn default() -> Self {
        use std::time::Duration;
        Self {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(30),
            write: Duration::from_secs(10),
        }
    }
}

/// Every source must provide the same pinned artifact. Network/checksum errors
/// advance to the next source; cancellation and local filesystem failures do not.
/// A single explicit source therefore never silently falls back to another host.
pub fn download_with_fallback(
    sources: &[String],
    dest: &Path,
    expected: &str,
    parallel: bool,
    timeouts: DownloadTimeouts,
    expected_size: Option<u64>,
) -> Result<String> {
    let mut failures = Vec::new();
    for url in sources {
        model_progress::check()?;
        let result = if parallel && !dest.exists() {
            download_parallel_with_timeouts(
                url,
                dest,
                expected,
                16 * 1024 * 1024,
                timeouts,
                expected_size,
            )
            .and_then(|used| {
                if used {
                    Ok(())
                } else {
                    download_verified_with_timeouts(url, dest, expected, timeouts, expected_size)
                }
            })
        } else {
            download_verified_with_timeouts(url, dest, expected, timeouts, expected_size)
        };
        match result {
            Ok(()) => {
                // A no-Range fallback may have finished a prior segmented transfer.
                remove_download_parts(dest)?;
                return Ok(url.clone());
            }
            Err(error) => {
                model_progress::check()?;
                if error.downcast_ref::<std::io::Error>().is_some() {
                    return Err(error);
                }
                failures.push(format!("{url}: {error:#}"));
            }
        }
    }
    anyhow::bail!("BGE-M3 download failed: {}", failures.join("; "))
}

#[cfg(test)]
pub(super) fn download_verified(
    url: &str,
    dest: &Path,
    expected: &str,
    expected_size: Option<u64>,
) -> Result<()> {
    download_verified_with_timeouts(
        url,
        dest,
        expected,
        DownloadTimeouts::default(),
        expected_size,
    )
}

fn download_verified_with_timeouts(
    url: &str,
    dest: &Path,
    expected: &str,
    timeouts: DownloadTimeouts,
    expected_size: Option<u64>,
) -> Result<()> {
    download_verified_bounded(url, dest, expected, timeouts, expected_size)
}

fn download_verified_bounded(
    url: &str,
    dest: &Path,
    expected: &str,
    timeouts: DownloadTimeouts,
    expected_size: Option<u64>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if dest.is_file()
        && expected_size
            .is_some_and(|size| dest.metadata().map(|m| m.len() > size).unwrap_or(false))
    {
        std::fs::remove_file(dest)?;
    }
    let offset = if dest.is_file() {
        dest.metadata()?.len()
    } else {
        0
    };
    let resp = match download_response(url, dest, offset, timeouts) {
        Ok(response) => response,
        Err(error)
            if offset > 0
                && error
                    .downcast_ref::<ureq::Error>()
                    .is_some_and(|error| matches!(error, ureq::Error::Status(416, _))) =>
        {
            std::fs::remove_file(dest)?;
            download_response(url, dest, 0, timeouts)?
        }
        Err(error) => return Err(error),
    };
    let resumed = resp.status() == 206;
    let total = if resumed {
        let range = resp
            .header("Content-Range")
            .ok_or_else(|| anyhow!("partial response has no Content-Range"))?;
        let (start, total) = parse_content_range(range)?;
        anyhow::ensure!(
            start == offset,
            "partial response starts at {start}, expected {offset}"
        );
        Some(total)
    } else {
        anyhow::ensure!(
            resp.status() == 200,
            "unexpected download status {}",
            resp.status()
        );
        resp.header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
    };
    if let (Some(pinned), Some(reported)) = (expected_size, total) {
        anyhow::ensure!(
            pinned == reported,
            "model size mismatch: response {reported}, expected {pinned}"
        );
    }
    let item = dest.file_name().unwrap_or_default().to_string_lossy();
    let mut reader = resp.into_reader();
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(dest)
        .with_context(|| format!("failed to write {}", dest.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = if resumed { offset } else { 0 };
    loop {
        model_progress::update("download", &item, written, total)?;
        let n = read_download(&mut reader, &mut buf, url)?;
        if n == 0 {
            break;
        }
        if let Some(limit) = expected_size {
            anyhow::ensure!(
                written <= limit && n as u64 <= limit - written,
                "model response exceeds pinned size {limit}"
            );
        }
        file.write_all(&buf[..n])
            .with_context(|| format!("failed to write {}", dest.display()))?;
        written += n as u64;
    }
    file.flush()
        .with_context(|| format!("failed to write {}", dest.display()))?;
    drop(file);
    if let Some(total) = total {
        anyhow::ensure!(
            written == total,
            "download incomplete: {written}/{total} bytes; partial file retained"
        );
    }
    if let Some(expected) = expected_size {
        anyhow::ensure!(
            written == expected,
            "model size mismatch: {written}/{expected}"
        );
    }
    model_progress::update("verify", &item, written, Some(written))?;
    let actual = sha256_file(dest)?;
    if !actual.eq_ignore_ascii_case(expected) {
        // A saved prefix can be corrupt. Retry this source once from byte zero;
        // the fresh request cannot enter this branch with a nonzero offset.
        if resumed && offset > 0 {
            std::fs::remove_file(dest)?;
            return download_verified_bounded(url, dest, expected, timeouts, expected_size);
        }
        // Unknown-length EOF may be a truncated response. Preserve resumable bytes.
        if total.is_some() {
            std::fs::remove_file(dest)?;
        }
        anyhow::bail!(
            "checksum failed: {} SHA256={actual} (expected {expected})",
            dest.display()
        );
    }
    Ok(())
}

fn parse_content_range(value: &str) -> Result<(u64, u64)> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| anyhow!("invalid Content-Range"))?;
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid Content-Range"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| anyhow!("invalid Content-Range"))?;
    let start = start.parse::<u64>()?;
    let end = end.parse::<u64>()?;
    let total = total.parse::<u64>()?;
    anyhow::ensure!(start <= end && end < total, "invalid Content-Range bounds");
    Ok((start, total))
}

fn download_parts_path(dest: &Path) -> PathBuf {
    // Preserve the existing partial-directory name across installer upgrades.
    dest.with_extension("onnx.parts")
}

pub(super) fn remove_download_parts(dest: &Path) -> Result<()> {
    match std::fs::remove_dir_all(download_parts_path(dest)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove completed model segments"),
    }
}

/// Four persistent ranges. Only the coordinator updates task state; workers
/// receive cancellation through a shared flag and retain their partial files.
#[cfg(test)]
fn download_parallel(
    url: &str,
    dest: &Path,
    expected: &str,
    minimum: u64,
    expected_size: Option<u64>,
) -> Result<bool> {
    download_parallel_with_timeouts(
        url,
        dest,
        expected,
        minimum,
        DownloadTimeouts::default(),
        expected_size,
    )
}

fn download_parallel_with_timeouts(
    url: &str,
    dest: &Path,
    expected: &str,
    minimum: u64,
    timeouts: DownloadTimeouts,
    expected_size: Option<u64>,
) -> Result<bool> {
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let item = dest.file_name().unwrap_or_default().to_string_lossy();
    model_progress::update("connect", &item, 0, None)?;
    let response = download_agent(timeouts)
        .get(url)
        .set("Accept-Encoding", "identity")
        .set("Range", "bytes=0-0")
        .call()
        .with_context(|| format!("download probe failed ({url})"))?;
    model_progress::check()?;
    if response.status() == 200 {
        return Ok(false);
    }
    anyhow::ensure!(response.status() == 206, "unexpected range probe status");
    let range = response
        .header("Content-Range")
        .ok_or_else(|| anyhow!("range probe has no Content-Range"))?;
    let (start, total) = parse_content_range(range)?;
    anyhow::ensure!(start == 0, "range probe starts at {start}");
    if let Some(pinned) = expected_size {
        anyhow::ensure!(
            total == pinned,
            "model size mismatch: response {total}, expected {pinned}"
        );
    }
    drop(response);
    if total < minimum || total < 4 {
        return Ok(false);
    }
    let parts = download_parts_path(dest);
    std::fs::create_dir_all(&parts)?;
    for index in 0..4 {
        let path = parts.join(index.to_string());
        let length = total * (index + 1) / 4 - total * index / 4;
        if path.is_file() && path.metadata()?.len() > length {
            // Stale or corrupt segment lengths cannot be safely resumed.
            std::fs::remove_dir_all(&parts)?;
            std::fs::create_dir_all(&parts)?;
            break;
        }
    }
    let counts: [AtomicU64; 4] = std::array::from_fn(|_| AtomicU64::new(0));
    let stop = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel();
    let result = std::thread::scope(|scope| -> Result<()> {
        for (index, count) in counts.iter().enumerate() {
            let begin = total * index as u64 / 4;
            let end = total * (index + 1) as u64 / 4;
            let path = parts.join(index.to_string());
            let sender = sender.clone();
            let stop = &stop;
            scope.spawn(move || {
                let result = download_part(url, &path, begin, end, total, count, stop, timeouts);
                let failed = result.is_err();
                let _ = sender.send(result);
                if failed {
                    stop.store(true, Ordering::Relaxed);
                }
            });
        }
        drop(sender);
        let mut finished = 0;
        let mut failure = None;
        while finished < 4 {
            let done = counts
                .iter()
                .map(|count| count.load(Ordering::Relaxed))
                .sum();
            if let Err(error) = model_progress::update("download", &item, done, Some(total)) {
                stop.store(true, Ordering::Relaxed);
                if failure.is_none() {
                    failure = Some(error);
                }
            }
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => {
                    finished += 1;
                    if let Err(error) = result {
                        // A concurrent network failure must not hide a disk failure
                        // reported by another worker and turn it into a failover.
                        if failure.is_none() || error.downcast_ref::<std::io::Error>().is_some() {
                            failure = Some(error);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("download workers disconnected")
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    });
    result?;
    let mut merged = File::create(dest)?;
    for index in 0..4 {
        model_progress::check()?;
        std::io::copy(&mut File::open(parts.join(index.to_string()))?, &mut merged)?;
    }
    merged.flush()?;
    drop(merged);
    if !file_valid(dest, expected, expected_size)? {
        std::fs::remove_file(dest)?;
        std::fs::remove_dir_all(&parts)?;
        // A corrupt saved prefix must not exhaust the final healthy source.
        // Discard every segment and retry that source once from byte zero.
        download_verified_with_timeouts(url, dest, expected, timeouts, expected_size)?;
        return Ok(true);
    }
    std::fs::remove_dir_all(parts)?;
    Ok(true)
}

fn download_part(
    url: &str,
    path: &Path,
    begin: u64,
    end: u64,
    total: u64,
    count: &std::sync::atomic::AtomicU64,
    stop: &std::sync::atomic::AtomicBool,
    timeouts: DownloadTimeouts,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let length = end - begin;
    let mut offset = if path.is_file() {
        path.metadata()?.len()
    } else {
        0
    };
    anyhow::ensure!(offset <= length, "saved download segment is too long");
    count.store(offset, Ordering::Relaxed);
    if offset == length {
        return Ok(());
    }
    anyhow::ensure!(!stop.load(Ordering::Relaxed), "download segment stopped");
    let start = begin + offset;
    let response = download_agent(timeouts)
        .get(url)
        .set("Accept-Encoding", "identity")
        .set("Range", &format!("bytes={start}-{}", end - 1))
        .call()
        .with_context(|| format!("segment download failed ({url})"))?;
    anyhow::ensure!(
        response.status() == 206,
        "source did not honor the segment range"
    );
    let expected_range = format!("bytes {start}-{}/{total}", end - 1);
    anyhow::ensure!(
        response.header("Content-Range") == Some(expected_range.as_str()),
        "source returned an incorrect segment range"
    );
    let mut reader = response.into_reader();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    while offset < length {
        anyhow::ensure!(!stop.load(Ordering::Relaxed), "download segment stopped");
        let limit = buffer.len().min((length - offset) as usize);
        let received = reader
            .read(&mut buffer[..limit])
            .map_err(|error| anyhow!("segment interrupted ({url}): {error}"))?;
        anyhow::ensure!(
            received > 0,
            "segment download incomplete; partial bytes retained"
        );
        file.write_all(&buffer[..received])?;
        offset += received as u64;
        count.store(offset, Ordering::Relaxed);
    }
    file.flush()?;
    Ok(())
}

fn download_agent(timeouts: DownloadTimeouts) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(timeouts.connect)
        .timeout_read(timeouts.read)
        .timeout_write(timeouts.write)
        .build()
}

fn download_response(
    url: &str,
    dest: &Path,
    offset: u64,
    timeouts: DownloadTimeouts,
) -> Result<ureq::Response> {
    let item = dest.file_name().unwrap_or_default().to_string_lossy();
    model_progress::update("connect", &item, offset, None)?;
    // Headers and body reads share the same bounded idle wait. No total deadline
    // cuts off a large model while bytes are still arriving.
    let request = download_agent(timeouts)
        .get(url)
        .set("Accept-Encoding", "identity");
    let request = if offset > 0 {
        request.set("Range", &format!("bytes={offset}-"))
    } else {
        request
    };
    let response = request.call();
    model_progress::check()?;
    response.with_context(|| format!("download failed ({url})"))
}

fn read_download(reader: &mut impl Read, buf: &mut [u8], url: &str) -> Result<usize> {
    let result = reader.read(buf);
    model_progress::check()?;
    result.map_err(|error| anyhow!("download interrupted ({url}): {error}"))
}

#[cfg(test)]
mod tests;

/// Download sources all serve the same pinned BGE-M3 files.
pub const MIRRORS: &[&str] = &[
    "auto",
    "https://hf-mirror.com",
    "https://hf-mirror.net",
    "https://huggingface.co",
];

pub fn validate_mirror(value: &str) -> Result<()> {
    if value == "auto" {
        return Ok(());
    }
    origin_from_mirror(value).map(|_| ())
}

pub fn origin_from_mirror(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("--mirror must not be empty");
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let Some((scheme, rest)) = with_scheme.split_once("://") else {
        anyhow::bail!("--mirror is invalid: {raw}");
    };
    if scheme != "http" && scheme != "https" {
        anyhow::bail!("--mirror only supports http/https");
    }
    let hostport = match rest.split('/').next() {
        Some(h) if !h.is_empty() => h.trim(),
        _ => anyhow::bail!("--mirror is missing a host"),
    };
    anyhow::ensure!(
        !hostport.contains(['@', '?', '#']) && !hostport.chars().any(char::is_whitespace),
        "mirror must be a host without credentials, query or fragment"
    );
    Ok(format!("{scheme}://{hostport}"))
}
