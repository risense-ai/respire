//! Pull one HTTP body frame at a time. Only clean HTTP end-of-body produces
//! EOF for the archive decoder. A dropped sender is an interrupted upload.
use super::{transport, V1Result};
use axum::body::{Body, Bytes};
use futures_util::{future::{select, Either}, StreamExt};
use sha2::{Digest, Sha256};
use std::{io::{self, Read}, time::Duration};
use tokio::sync::mpsc;

const BLOCK: usize = 64 * 1024;
const IDLE: Duration = Duration::from_secs(15);

pub(super) struct Reader {
    input: mpsc::Receiver<Option<Bytes>>,
    block: Bytes,
    digest: Sha256,
    ended: bool,
}

impl Reader {
    pub(super) fn bridge() -> (mpsc::Sender<Option<Bytes>>, Self) {
        let (sender, input) = mpsc::channel(2);
        (sender, Self { input, block: Bytes::new(), digest: Sha256::new(), ended: false })
    }

    pub(super) fn verified_digest(self) -> V1Result<[u8; 32]> {
        if !self.ended { return Err(transport::invalid()); }
        Ok(self.digest.finalize().into())
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.ended { return Ok(0); }
        if self.block.is_empty() {
            match self.input.blocking_recv() {
                Some(Some(bytes)) => self.block = bytes,
                Some(None) => { self.ended = true; return Ok(0); }
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
        let n = buf.len().min(self.block.len());
        let bytes = self.block.split_to(n);
        buf[..n].copy_from_slice(&bytes);
        self.digest.update(&bytes);
        Ok(n)
    }
}

pub(super) async fn feed(body: Body, sender: mpsc::Sender<Option<Bytes>>) -> V1Result<()> {
    let mut frames = body.into_data_stream();
    loop {
        let next = std::pin::pin!(frames.next());
        let closed = std::pin::pin!(sender.closed());
        let next = match tokio::time::timeout(IDLE, select(next, closed)).await {
            Ok(Either::Left((frame, _))) => frame,
            _ => return Err(transport::invalid()),
        };
        let Some(frame) = next else { break };
        let frame = frame.map_err(|_| transport::invalid())?;
        // The HTTP/1 listener supplies bounded frames. Never retain an oversized
        // allocation from an alternate body producer through a cheap Bytes slice.
        if frame.len() > 1024 * 1024 { return Err(transport::invalid()); }
        for block in frame.chunks(BLOCK) {
            let slot = sender.reserve().await.map_err(|_| transport::invalid())?;
            slot.send(Some(Bytes::copy_from_slice(block)));
        }
    }
    sender.send(None).await.map_err(|_| transport::invalid())
}
