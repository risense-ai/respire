use crate::loopback_http;
use super::*;
use anyhow::Result;

#[test]
fn bounded_download_rejects_headers_stream_overflow_and_short_eof() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    for (headers, body, valid) in [
        ("Content-Length: 4\r\n", "abcd", false),
        (
            "Transfer-Encoding: chunked\r\n",
            "4\r\nabcd\r\n0\r\n\r\n",
            false,
        ),
        ("", "ab", false),
        ("Content-Length: 3\r\n", "abc", true),
    ] {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("model");
        let listener = loopback_http::Server::bind()?;
        let url = format!("http://{}/model", listener.addr());
        let response = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n{body}");
        let worker = std::thread::spawn(move || -> Result<()> {
            let mut socket = listener
                .recv_timeout(std::time::Duration::from_secs(5))?
                .ok_or_else(|| anyhow!("download fixture received no connection"))?
                .into_stream();
            socket.write_all(response.as_bytes())?;
            Ok(())
        });
        let result =
            download_verified_bounded(&url, &target, expected, fast_timeouts(), Some(3));
        worker.join().map_err(|_| anyhow!("server panicked"))??;
        assert_eq!(result.is_ok(), valid, "unexpected result: {result:?}");
        if target.exists() {
            assert!(target.metadata()?.len() <= 3);
        }
    }
    Ok(())
}

fn fast_timeouts() -> DownloadTimeouts {
    use std::time::Duration;
    DownloadTimeouts {
        connect: Duration::from_millis(500),
        read: Duration::from_millis(200),
        write: Duration::from_millis(500),
    }
}

/// Hold the connection open until the test releases it, so advancing to the
/// next source proves the client timeout fired rather than the server's EOF.
fn stalled_source(
    body: Option<(&[u8], usize)>,
) -> Result<(
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<Result<()>>,
)> {
    use std::time::Duration;
    let listener = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", listener.addr());
    let body = body.map(|(prefix, total)| (prefix.to_vec(), total));
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || -> Result<()> {
        let mut socket = listener
            .recv_timeout(std::time::Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("download fixture received no connection"))?
            .into_stream();
        if let Some((prefix, total)) = body {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
            )?;
            socket.write_all(&prefix)?;
            socket.flush()?;
        }
        wait.recv_timeout(Duration::from_secs(10))?;
        Ok(())
    });
    Ok((url, release, server))
}

fn responding_source(
    body: &[u8],
    offset: usize,
) -> Result<(String, std::thread::JoinHandle<Result<()>>)> {
    let server = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", server.addr());
    let body = body.to_vec();
    let worker = std::thread::spawn(move || -> Result<()> {
        let request = server
            .recv_timeout(std::time::Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("fallback source not requested"))?;
        let range = request.header("Range");
        if offset > 0 {
            assert_eq!(
                range,
                Some(format!("bytes={offset}-").as_str())
            );
            request.reply_range(&format!("bytes {offset}-{}/{}", body.len() - 1, body.len()), &body[offset..])?;
        } else {
            assert!(range.is_none());
            request.reply(body)?;
        }
        Ok(())
    });
    Ok((url, worker))
}

#[test]
fn stalled_headers_and_body_fail_over_and_resume_without_waiting_for_eof() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let body = b"pinned artifact from healthy fallback";
    let expected = hex::encode(Sha256::digest(body));
    for offset in [0, 7] {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("model.ort");
        let prefix = if offset == 0 {
            None
        } else {
            Some((&body[..offset], body.len()))
        };
        let (first, release, stalled) = stalled_source(prefix)?;
        let (second, healthy) = responding_source(body, offset)?;
        let started = std::time::Instant::now();
        let result = download_with_fallback(
            &[first, second.clone()],
            &target,
            &expected,
            false,
            fast_timeouts(),
            None,
        );
        let elapsed = started.elapsed();
        release.send(())?;
        stalled
            .join()
            .map_err(|_| anyhow!("stalled source panicked"))??;
        healthy
            .join()
            .map_err(|_| anyhow!("healthy source panicked"))??;
        assert_eq!(result?, second);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "fallback was not bounded: {elapsed:?}"
        );
        assert_eq!(std::fs::read(target)?, body);
    }
    Ok(())
}

#[test]
fn stalled_range_probe_fails_over_without_publishing_unverified_bytes() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("model.ort");
    let parts = download_parts_path(&target);
    std::fs::create_dir_all(&parts)?;
    std::fs::write(
        parts.join("0"),
        b"saved segment from an interrupted range source",
    )?;
    let body = b"healthy model";
    let expected = hex::encode(Sha256::digest(body));
    let (first, release, stalled) = stalled_source(None)?;
    let server = loopback_http::Server::bind()?;
    let second = format!("http://{}/model", server.addr());
    let healthy = std::thread::spawn(move || -> Result<()> {
        let probe = server
            .recv_timeout(std::time::Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("probe missing"))?;
        assert!(probe.header("Range") == Some("bytes=0-0"));
        probe.reply(body.to_vec())?;
        let request = server
            .recv_timeout(std::time::Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("full request missing"))?;
        request.reply(body.to_vec())?;
        Ok(())
    });
    let result = download_with_fallback(
        &[first, second.clone()],
        &target,
        &expected,
        true,
        fast_timeouts(),
        None,
    );
    release.send(())?;
    stalled
        .join()
        .map_err(|_| anyhow!("stalled source panicked"))??;
    healthy
        .join()
        .map_err(|_| anyhow!("healthy source panicked"))??;
    assert_eq!(result?, second);
    assert_eq!(std::fs::read(target)?, body);
    assert!(
        !parts.exists(),
        "no-Range fallback left unused CPU segments behind"
    );
    Ok(())
}

#[test]
fn progressing_transfer_outlives_the_per_read_timeout() -> Result<()> {
    use std::time::Duration;
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("model.ort");
    let body = b"abcdefgh";
    let expected = hex::encode(Sha256::digest(body));
    let listener = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", listener.addr());
    let worker = std::thread::spawn(move || -> Result<()> {
        let mut socket = listener
            .recv_timeout(std::time::Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("download fixture received no connection"))?
            .into_stream();
        socket.set_nodelay(true)?;
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        for byte in body {
            socket.write_all(&[*byte])?;
            socket.flush()?;
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    });
    let timeouts = DownloadTimeouts {
        read: Duration::from_millis(500),
        ..fast_timeouts()
    };
    let started = std::time::Instant::now();
    download_with_fallback(&[url], &target, &expected, false, timeouts, None)?;
    assert!(started.elapsed() > timeouts.read);
    worker
        .join()
        .map_err(|_| anyhow!("progressing source panicked"))??;
    assert_eq!(std::fs::read(target)?, body);
    Ok(())
}

#[test]
fn corrupt_source_is_rejected_before_clean_fallback() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("model.ort");
    let expected = hex::encode(Sha256::digest(b"trusted model"));
    let (first, corrupt) = responding_source(b"corrupt model", 0)?;
    let (second, healthy) = responding_source(b"trusted model", 0)?;
    assert_eq!(
        download_with_fallback(
            &[first, second.clone()],
            &target,
            &expected,
            false,
            fast_timeouts(),
            None
        )?,
        second
    );
    corrupt
        .join()
        .map_err(|_| anyhow!("corrupt source panicked"))??;
    healthy
        .join()
        .map_err(|_| anyhow!("healthy source panicked"))??;
    assert_eq!(std::fs::read(target)?, b"trusted model");
    Ok(())
}

#[test]
fn failed_source_releases_operation_slot_and_can_retry() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("model.ort");
    let body = b"verified retry";
    let expected = hex::encode(Sha256::digest(body));
    let (first, release, stalled) = stalled_source(None)?;
    let failed = {
        let _operation = model_progress::Operation::begin_download()?;
        let result =
            download_with_fallback(&[first], &target, &expected, false, fast_timeouts(), None);
        assert_eq!(model_progress::status()?["active"], true);
        result
    };
    release.send(())?;
    stalled
        .join()
        .map_err(|_| anyhow!("stalled source panicked"))??;
    assert!(failed.is_err());
    assert!(!target.exists());
    assert_eq!(model_progress::status()?["active"], false);
    let (second, healthy) = responding_source(body, 0)?;
    {
        let _operation = model_progress::Operation::begin_download()?;
        download_with_fallback(&[second], &target, &expected, false, fast_timeouts(), None)?;
    }
    healthy
        .join()
        .map_err(|_| anyhow!("healthy source panicked"))??;
    assert_eq!(model_progress::status()?["active"], false);
    assert_eq!(std::fs::read(target)?, body);
    Ok(())
}

#[test]
fn local_disk_failure_is_not_retried_as_a_mirror_outage() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let parent = dir.path().join("not-a-directory");
    std::fs::write(&parent, b"keep")?;
    let server = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", server.addr());
    let error = download_with_fallback(
        &[url.clone(), url],
        &parent.join("model.ort"),
        "unused",
        false,
        fast_timeouts(),
        None,
    )
    .unwrap_err();
    assert!(
        error.downcast_ref::<std::io::Error>().is_some(),
        "disk failure identity lost: {error:#}"
    );
    assert!(server
        .recv_timeout(std::time::Duration::from_millis(10))?
        .is_none());
    assert_eq!(std::fs::read(parent)?, b"keep");
    Ok(())
}

#[test]
fn partial_download_resumes_across_sources_and_ignoring_range_restarts_safely() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let body = b"pinned model contents";
    let expected = hex::encode(Sha256::digest(body));
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("partial.bin");
    for supports_range in [true, false] {
        std::fs::write(&target, &body[..7])?;
        let server =
            loopback_http::Server::bind()?;
        let url = format!("http://{}/model", server.addr());
        let worker = std::thread::spawn(move || -> Result<()> {
            let request = server
                .recv_timeout(std::time::Duration::from_secs(10))?
                .ok_or_else(|| anyhow!("download not requested"))?;
            assert!(request.header("Range") == Some("bytes=7-"));
            if supports_range {
                request.reply_range(&format!("bytes 7-{}/{}", body.len() - 1, body.len()), &body[7..])?;
            } else {
                request.reply(body)?;
            }
            Ok(())
        });
        download_verified(&url, &target, &expected, None)?;
        worker.join().map_err(|_| anyhow!("server panicked"))??;
        assert_eq!(std::fs::read(&target)?, body);
    }
    assert!(parse_content_range("bytes 8-9/9").is_err());
    assert!(parse_content_range("bytes 8-7/10").is_err());
    Ok(())
}

#[test]
fn four_parallel_ranges_resume_saved_segments_and_verify_the_merged_file() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let body: Vec<u8> = (0..128 * 1024).map(|index| (index % 251) as u8).collect();
    let expected = hex::encode(Sha256::digest(&body));
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("model_quantized.onnx");
    let parts = target.with_extension("onnx.parts");
    std::fs::create_dir(&parts)?;
    for index in 0..4 {
        let begin = body.len() * index / 4;
        std::fs::write(parts.join(index.to_string()), &body[begin..begin + 3])?;
    }
    let server = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", server.addr());
    let source = body.clone();
    let worker = std::thread::spawn(move || -> Result<()> {

        let probe = server
            .recv_timeout(std::time::Duration::from_secs(10))?
            .ok_or_else(|| anyhow!("probe missing"))?;
        probe.reply_range(&format!("bytes 0-0/{}", source.len()), &source[..1])?;
        // Hold all four responses until all requests arrive. A sequential
        // implementation times out here instead of passing this test.
        let mut requests = Vec::new();
        for _ in 0..4 {
            requests.push(
                server
                    .recv_timeout(std::time::Duration::from_secs(10))?
                    .ok_or_else(|| anyhow!("four concurrent requests did not arrive"))?,
            );
        }
        for request in requests {
            let range = request.header("Range").ok_or_else(|| anyhow!("segment Range missing"))?;
            let (start, end) = range
                .strip_prefix("bytes=")
                .and_then(|value| value.split_once('-'))
                .ok_or_else(|| anyhow!("bad range"))?;
            let start = start.parse::<usize>()?;
            let end = end.parse::<usize>()?;
            assert_eq!(start % (source.len() / 4), 3);
            request.reply_range(&format!("bytes {start}-{end}/{}", source.len()), &source[start..=end])?;
        }
        Ok(())
    });
    assert!(download_parallel(&url, &target, &expected, 0, None)?);
    worker.join().map_err(|_| anyhow!("source panicked"))??;
    assert_eq!(std::fs::read(target)?, body);
    assert!(!parts.exists());
    Ok(())
}

#[test]
fn corrupt_or_overlong_parallel_segments_restart_cleanly_on_the_same_source() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let body: Vec<u8> = (0..128 * 1024).map(|index| (index % 251) as u8).collect();
    let expected = hex::encode(Sha256::digest(&body));
    for overlong in [false, true] {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("model.ort");
        let parts = download_parts_path(&target);
        std::fs::create_dir(&parts)?;
        for index in 0..4 {
            let bad_length = if overlong { body.len() / 4 + 1 } else { 3 };
            std::fs::write(parts.join(index.to_string()), vec![255; bad_length])?;
        }
        let server =
            loopback_http::Server::bind()?;
        let url = format!("http://{}/model", server.addr());
        let source = body.clone();
        let worker = std::thread::spawn(move || -> Result<()> {

            let next = || {
                server
                    .recv_timeout(std::time::Duration::from_secs(5))?
                    .ok_or_else(|| anyhow!("download request missing"))
            };
            next()?.reply_range(&format!("bytes 0-0/{}", source.len()), &source[..1])?;
            for _ in 0..4 {
                let request = next()?;
                let range = request.header("Range").ok_or_else(|| anyhow!("segment Range missing"))?;
                let (start, end) = range
                    .strip_prefix("bytes=")
                    .and_then(|value| value.split_once('-'))
                    .ok_or_else(|| anyhow!("bad range"))?;
                let start = start.parse::<usize>()?;
                let end = end.parse::<usize>()?;
                assert_eq!(start % (source.len() / 4), if overlong { 0 } else { 3 });
                request.reply_range(&format!("bytes {start}-{end}/{}", source.len()), &source[start..=end])?;
            }
            if !overlong {
                let fresh = next()?;
                assert!(fresh.header("Range").is_none());
                fresh.reply(source)?;
            }
            Ok(())
        });
        assert!(download_parallel_with_timeouts(
            &url,
            &target,
            &expected,
            0,
            fast_timeouts(),
            None
        )?);
        worker
            .join()
            .map_err(|_| anyhow!("range source panicked"))??;
        assert_eq!(std::fs::read(target)?, body);
        assert!(!parts.exists());
    }
    Ok(())
}

#[test]
fn unknown_length_partial_is_retained_and_resumed_after_checksum_failure() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("partial");
    let server = loopback_http::Server::bind()?;
    let url = format!("http://{}/model", server.addr());
    let handler =
        std::thread::spawn(move || -> Result<()> {
            let request = server.recv_timeout(std::time::Duration::from_secs(10))?.ok_or_else(|| anyhow!("download request missing"))?;
            request.reply_until_close(b"abc")?;
            let request = server.recv_timeout(std::time::Duration::from_secs(10))?.ok_or_else(|| anyhow!("download request missing"))?;
            assert!(request.header("Range") == Some("bytes=3-"));
            request.reply_range("bytes 3-5/6", b"def")?;
            let request = server
                .recv_timeout(std::time::Duration::from_secs(10))?
                .ok_or_else(|| anyhow!("corrupt-prefix resume request missing"))?;
            assert!(request.header("Range") == Some("bytes=3-"));
            request.reply_range("bytes 3-5/6", b"def")?;
            let request = server
                .recv_timeout(std::time::Duration::from_secs(10))?
                .ok_or_else(|| anyhow!("fresh request after corrupt prefix missing"))?;
            assert!(request.header("Range").is_none());
            request.reply("abcdef")?;
            Ok(())
        });
    let expected = hex::encode(Sha256::digest(b"abcdef"));
    assert!(download_verified(&url, &target, &expected, None).is_err());
    assert_eq!(std::fs::read(&target)?, b"abc");
    download_verified(&url, &target, &expected, None)?;
    assert_eq!(std::fs::read(&target)?, b"abcdef");
    std::fs::write(&target, b"bad")?;
    download_verified(&url, &target, &expected, None)?;
    handler
        .join()
        .map_err(|_| anyhow!("test server panicked"))??;
    assert_eq!(std::fs::read(target)?, b"abcdef");
    Ok(())
}
