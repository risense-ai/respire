//! Adapt one HTTP body to the synchronous archive reader without buffering
//! the archive, conflating a disconnect with EOF or holding the Vault lock.
use super::{transport, Problem, V1Result};
use axum::{
    body::{Body, Bytes, HttpBody},
    http::StatusCode,
};
use futures_util::future::{select, Either};
use std::{future::poll_fn, io::{self, Read}, pin::Pin, time::Duration};
use tokio::sync::mpsc;

const CHUNK: usize = 64 * 1024;
const IDLE: Duration = Duration::from_secs(15);
const DEADLINE: Duration = Duration::from_secs(180);

enum Message {
    Data(Bytes),
    Complete,
}

/// Receiver closure is a broken upload, not a clean zero-byte read.
pub(super) struct BodyReader {
    queue: mpsc::Receiver<Message>,
    remaining: Bytes,
    done: bool,
}

impl Read for BodyReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.done {
            return Ok(0);
        }
        if self.remaining.is_empty() {
            match self.queue.blocking_recv() {
                Some(Message::Data(data)) => self.remaining = data,
                Some(Message::Complete) => {
                    self.done = true;
                    return Ok(0);
                }
                None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "backup upload ended early")),
            }
        }
        let length = output.len().min(self.remaining.len());
        output[..length].copy_from_slice(&self.remaining.split_to(length));
        Ok(length)
    }
}

async fn pump(
    mut body: Body,
    send: mpsc::Sender<Message>,
    limit: u64,
    declared: Option<u64>,
) -> V1Result<()> {
    let mut total = 0u64;
    loop {
        let frame = tokio::time::timeout(IDLE, poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)))
            .await.map_err(|_| timeout())?;
        let Some(frame) = frame else { break; };
        let bytes = frame.map_err(|_| transport::invalid())?
            .into_data().map_err(|_| transport::invalid())?;
        total = total.checked_add(bytes.len() as u64)
            .filter(|count| *count <= limit && declared.is_none_or(|expected| *count <= expected))
            .ok_or_else(too_large)?;
        for chunk in bytes.chunks(CHUNK) {
            send.send(Message::Data(Bytes::copy_from_slice(chunk))).await
                .map_err(|_| transport::invalid())?;
        }
    }
    if total == 0 || declared.is_some_and(|expected| expected != total) {
        return Err(transport::invalid());
    }
    send.send(Message::Complete).await.map_err(|_| transport::invalid())?;
    Ok(())
}

/// Only for a route with separate owner admission and a live-scope gate
/// inside the worker. The worker must read to clean EOF before returning
/// a validated stage; the caller owns any longer-lived admission permit.
pub(super) async fn receive<T: Send + 'static>(
    body: Body,
    limit: u64,
    declared: Option<u64>,
    work: impl FnOnce(&mut BodyReader) -> V1Result<T> + Send + 'static,
) -> V1Result<T> {
    if declared.is_some_and(|size| size == 0 || size > limit) {
        return Err(too_large());
    }
    let (send, queue) = mpsc::channel(2);
    let worker = tokio::task::spawn_blocking(move || {
        let mut reader = BodyReader { queue, remaining: Bytes::new(), done: false };
        let value = work(&mut reader)?;
        if !reader.done {
            return Err(transport::invalid());
        }
        Ok(value)
    });
    let run = async {
        match select(Box::pin(pump(body, send, limit, declared)), worker).await {
            Either::Left((transfer, worker)) => {
                // Pump has released the sender. Reap the worker before
                // replying so unsuccessful request cleanup is observable.
                let result = worker.await.map_err(|_| transport::unavailable())?;
                transfer?;
                result
            }
            Either::Right((result, pump)) => {
                // Worker rejected the header or scoped archive before EOF.
                // Drop the producer even if the peer never sends more.
                drop(pump);
                result.map_err(|_| transport::unavailable())?
            }
        }
    };
    tokio::time::timeout(DEADLINE, run).await.map_err(|_| timeout())?
}

fn too_large() -> Problem {
    Problem::new(StatusCode::PAYLOAD_TOO_LARGE, "backup_upload_size", "Backup upload exceeds its byte budget")
}

fn timeout() -> Problem {
    Problem::new(StatusCode::REQUEST_TIMEOUT, "backup_upload_timeout", "Backup upload timed out")
}
