//! One fixed HTTP/1.1 exchange for large private install bodies.
//! The same verified loopback connection carries admission, bytes and receipt.
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const HEADER_BYTES: usize = 8192;
const RESPONSE_BYTES: usize = 4096;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(200);
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn install(
    port: u16,
    proof: &str,
    model: &str,
    length: u64,
    input: &mut dyn Read,
) -> Result<(u16, Value)> {
    // Both are canonical owner-proof/model strings. Guard the raw HTTP
    // boundary independently so a later caller cannot supply a header break.
    let header_token = |value: &str| {
        !value.is_empty()
            && value.len() <= 160
            && value
                .as_bytes()
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    };
    ensure!(
        header_token(proof)
            && header_token(model)
            && (1..=4 * 1024 * 1024 * 1024).contains(&length),
        "invalid private install envelope"
    );
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let started = Instant::now();
    let mut socket = connect(address, started)?;
    let mut wire = BufReader::new(&mut socket);
    write!(
        wire.get_mut(),
        "POST /api/runtime/private-model/install HTTP/1.1\r\nHost: {address}\r\n\
         Connection: close\r\nExpect: 100-continue\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {length}\r\nx-respire-model-id: {model}\r\nx-respire-model-proof: {proof}\r\n\r\n"
    )?;
    let admission = read_head(&mut wire)?;
    if admission.status != 100 {
        // A successful mutation cannot precede the body's verified upload.
        ensure!(
            (400..600).contains(&admission.status),
            "unexpected private install admission"
        );
        return final_receipt(admission, &mut wire);
    }
    ensure!(
        admission.length.is_none() && admission.media.is_none(),
        "invalid interim install response"
    );
    wire.get_mut().deadline = started + UPLOAD_TIMEOUT;

    // This deliberately never retries, opens another socket, or writes beyond
    // the pinned pair. Missing bytes, resets or failures after admission are
    // conservatively unconfirmed by the caller.
    let sent = io::copy(&mut input.take(length), wire.get_mut())?;
    ensure!(sent == length, "incomplete private install input");
    let final_head = read_head(&mut wire)?;
    final_receipt(final_head, &mut wire)
}

fn connect(address: SocketAddr, started: Instant) -> Result<DeadlineSocket> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    ensure!(
        stream.peer_addr()? == address,
        "unexpected private install endpoint"
    );
    Ok(DeadlineSocket {
        stream,
        deadline: started + ADMISSION_TIMEOUT,
    })
}

struct DeadlineSocket {
    stream: TcpStream,
    deadline: Instant,
}

impl DeadlineSocket {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "private install deadline expired")
            })
    }
}

impl Read for DeadlineSocket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineSocket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[derive(Default)]
struct ReplyHead {
    status: u16,
    length: Option<usize>,
    media: Option<Media>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Media {
    Json,
    Problem,
}

fn read_head(reader: &mut impl BufRead) -> Result<ReplyHead> {
    let mut raw = Vec::new();
    let remaining = reader.take((HEADER_BYTES + 1) as u64);
    let mut limited = remaining;
    loop {
        let start = raw.len();
        ensure!(
            limited.read_until(b'\n', &mut raw)? > 0 && raw.len() <= HEADER_BYTES,
            "incomplete private install response headers"
        );
        let line = &raw[start..];
        ensure!(
            line.ends_with(b"\r\n"),
            "invalid private install response line"
        );
        if line == b"\r\n" {
            break;
        }
    }
    let mut fields = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Response::new(&mut fields);
    ensure!(
        parsed.parse(&raw)? == httparse::Status::Complete(raw.len()) && parsed.version == Some(1),
        "invalid private install response protocol"
    );
    let mut result = ReplyHead {
        status: parsed.code.context("missing private install status")?,
        ..ReplyHead::default()
    };
    for header in parsed.headers.iter() {
        if header.name.eq_ignore_ascii_case("transfer-encoding")
            || header.name.eq_ignore_ascii_case("content-encoding")
        {
            bail!("unexpected private install response encoding");
        }
        if header.name.eq_ignore_ascii_case("content-length") {
            let digits = header.value;
            ensure!(
                result.length.is_none()
                    && !digits.is_empty()
                    && digits.iter().all(u8::is_ascii_digit),
                "invalid private install response length"
            );
            let size: usize = std::str::from_utf8(digits)?.parse()?;
            ensure!(
                size <= RESPONSE_BYTES,
                "private install response exceeds its bound"
            );
            result.length = Some(size);
        }
        if header.name.eq_ignore_ascii_case("content-type") {
            ensure!(
                result.media.is_none(),
                "duplicate private install content type"
            );
            result.media = Some(match header.value {
                b"application/json" => Media::Json,
                b"application/problem+json" => Media::Problem,
                _ => bail!("unexpected private install response type"),
            });
        }
    }
    Ok(result)
}

fn final_receipt(head: ReplyHead, reader: &mut impl Read) -> Result<(u16, Value)> {
    let expected = match head.status {
        201 => Media::Json,
        400..=599 => Media::Problem,
        _ => bail!("unexpected private install final status"),
    };
    ensure!(
        head.media == Some(expected),
        "invalid private install receipt type"
    );
    let size = head
        .length
        .filter(|&size| size > 0)
        .context("missing private install response length")?;
    let mut raw = vec![0u8; size];
    reader.read_exact(&mut raw)?;
    let body: Value = serde_json::from_slice(&raw).context("invalid private install receipt")?;
    ensure!(
        body.is_object() && (expected != Media::Problem || body["status"] == head.status),
        "invalid private install receipt envelope"
    );
    Ok((head.status, body))
}

#[cfg(test)]
#[path = "install_http_tests.rs"]
mod tests;
