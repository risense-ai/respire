//! Epoch-local replay protection for the authenticated owner proof.
use std::collections::HashMap;

const MAX_REPLAY_ENTRIES: usize = 256;

pub(super) struct ReplayWindow {
    high_water_ms: u64,
    accepted: HashMap<[u8; 16], u64>,
}

impl ReplayWindow {
    pub(super) fn new() -> Self {
        Self {
            high_water_ms: 0,
            accepted: HashMap::new(),
        }
    }

    // Observe the server clock independently of request data. Reject rollback
    // instead of admitting claims against a time already garbage-collected.
    pub(super) fn observe(&mut self, now_ms: u64) -> bool {
        if now_ms < self.high_water_ms {
            return false;
        }
        self.high_water_ms = now_ms;
        self.accepted.retain(|_, expires| *expires >= now_ms);
        true
    }

    pub(super) fn available(&self, nonce: &[u8; 16]) -> bool {
        self.accepted.len() < MAX_REPLAY_ENTRIES && !self.accepted.contains_key(nonce)
    }

    /// Invoke only after authenticating exact scope/epoch and observing the
    /// current server clock, while holding the same registry lock.
    pub(super) fn consume(&mut self, nonce: [u8; 16], expires_ms: u64) -> bool {
        if expires_ms < self.high_water_ms || !self.available(&nonce) {
            return false;
        }
        self.accepted.insert(nonce, expires_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_never_reopens_garbage_collected_proof() {
        let mut window = ReplayWindow::new();
        assert!(window.observe(1000));
        assert!(window.consume([1; 16], 31_000));
        assert!(window.observe(31_000));
        assert!(!window.consume([1; 16], 31_000));
        assert!(window.observe(31_001));
        assert!(window.accepted.is_empty());
        assert!(!window.observe(1000));
        assert!(!window.consume([1; 16], 31_000));
        assert!(window.observe(31_001));
    }

    #[test]
    fn saturation_never_evicts_an_unexpired_receipt() {
        let mut window = ReplayWindow::new();
        assert!(window.observe(1));
        for index in 0..MAX_REPLAY_ENTRIES {
            let mut nonce = [0; 16];
            nonce[..8].copy_from_slice(&(index as u64).to_be_bytes());
            assert!(window.consume(nonce, 100));
        }
        assert!(!window.consume([255; 16], 100));
        assert!(window.observe(100));
        assert!(!window.consume([0; 16], 100));
        assert!(window.observe(101));
        assert!(window.consume([255; 16], 200));
    }
}
