//! Bounded FIFO admission for the single shared native inference session.
use anyhow::{anyhow, bail, Result};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(crate) static QUEUE: Queue = Queue::new(32);
pub(crate) static BACKGROUND_QUEUE: Queue = Queue::new(32);
pub(crate) const WAIT_LIMIT: Duration = Duration::from_secs(120);

struct State {
    active: bool,
    next: u64,
    waiting: VecDeque<u64>,
}

pub(crate) struct Queue {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
}

pub(crate) struct Permit<'a>(&'a Queue);

impl Queue {
    pub(crate) fn status(&self) -> Result<(usize, bool)> {
        let state = self.state.lock().map_err(|_| anyhow!("inference queue poisoned"))?;
        Ok((state.waiting.len(), state.active))
    }
    const fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State { active: false, next: 0, waiting: VecDeque::new() }),
            changed: Condvar::new(),
            capacity,
        }
    }

    pub(crate) fn acquire(&self, timeout: Duration) -> Result<Permit<'_>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().map_err(|_| anyhow!("inference queue poisoned"))?;
        if state.waiting.len() >= self.capacity {
            bail!("inference queue is full; retry after the current model task finishes");
        }
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        loop {
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                return Ok(Permit(self));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting.retain(|value| *value != ticket);
                self.changed.notify_all();
                bail!("inference queue wait timed out; request cancelled before inference");
            }
            state = self.changed.wait_timeout(state, remaining)
                .map_err(|_| anyhow!("inference queue poisoned"))?.0;
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.active = false;
            self.0.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn foreground_and_background_admission_are_independent() -> Result<()> {
        let _background = BACKGROUND_QUEUE.acquire(Duration::from_secs(1))?;
        let _foreground = QUEUE.acquire(Duration::from_millis(20))?;
        assert!(BACKGROUND_QUEUE.status()?.1);
        assert!(QUEUE.status()?.1);
        Ok(())
    }

    #[test]
    fn expired_request_is_removed_and_does_not_block_the_next() -> Result<()> {
        let queue = Queue::new(2);
        let active = queue.acquire(Duration::from_secs(1))?;
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| queue.acquire(Duration::from_millis(20)).map(drop));
            assert!(waiting.join().map_err(|_| anyhow!("queue test panicked"))?.is_err());
            Ok::<_, anyhow::Error>(())
        })?;
        drop(active);
        let _next = queue.acquire(Duration::from_secs(1))?;
        Ok(())
    }

    #[test]
    fn queue_capacity_is_bounded() -> Result<()> {
        let queue = Queue::new(1);
        let active = queue.acquire(Duration::from_secs(1))?;
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                let _permit = queue.acquire(Duration::from_secs(2))?;
                tx.send(1)?;
                Ok::<_, anyhow::Error>(())
            });
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if !queue.state.lock().map_err(|_| anyhow!("poisoned"))?.waiting.is_empty() { break; }
                anyhow::ensure!(Instant::now() < deadline, "waiting request did not enter queue");
                std::thread::yield_now();
            }
            assert!(queue.acquire(Duration::from_millis(20)).is_err());
            drop(active);
            assert_eq!(rx.recv_timeout(Duration::from_secs(1))?, 1);
            waiting.join().map_err(|_| anyhow!("queue test panicked"))??;
            Ok::<_, anyhow::Error>(())
        })
    }

    #[test]
    fn admitted_requests_execute_in_fifo_order() -> Result<()> {
        let queue = Queue::new(3);
        let active = queue.acquire(Duration::from_secs(1))?;
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for number in 0..3 {
                let queue_ref = &queue;
                let reply = tx.clone();
                handles.push(scope.spawn(move || {
                    let _permit = queue_ref.acquire(Duration::from_secs(3))?;
                    reply.send(number)?;
                    Ok::<_, anyhow::Error>(())
                }));
                let deadline = Instant::now() + Duration::from_secs(1);
                while queue.status()?.0 != number + 1 {
                    anyhow::ensure!(Instant::now() < deadline, "request did not enter queue");
                    std::thread::yield_now();
                }
            }
            drop(active);
            for expected in 0..3 {
                assert_eq!(rx.recv_timeout(Duration::from_secs(1))?, expected);
            }
            for handle in handles { handle.join().map_err(|_| anyhow!("queue test panicked"))??; }
            Ok::<_, anyhow::Error>(())
        })
    }
}
