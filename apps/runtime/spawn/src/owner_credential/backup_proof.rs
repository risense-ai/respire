//! Purpose-bound proof for confirming a staged local backup restore.
//! Ordinary owner handshake and HTTP Bearer tokens cannot be used here.
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::proof::PROOF_MAX_AGE_MS;

const PREFIX: &str = "rhb1_";
const DOMAIN: &[u8] = b"respire-private-local-backup-restore-v1\0";
const EPOCH: usize = 16;
const TIME: usize = 8;
const NONCE: usize = 16;
const CLAIMS: usize = EPOCH + TIME + NONCE;
const TAG: usize = 32;
const RAW: usize = CLAIMS + TAG;

#[derive(Clone, Copy)]
pub struct BackupRestoreClaim {
    pub operation_id: [u8; 16],
    pub archive_sha256: [u8; 32],
    pub target_sha256: [u8; 32],
    pub instance: [u8; 16],
    pub generation: u64,
}

/// Intentionally no debug/serialization beyond the explicitly supplied header.
pub struct BackupRestoreProof(String);
impl BackupRestoreProof {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct VerifiedBackupProof {
    pub nonce: [u8; NONCE],
    pub expires_at_ms: u64,
}

pub fn issue(
    record: &[u8; 48],
    claim: BackupRestoreClaim,
    now_ms: u64,
) -> Option<BackupRestoreProof> {
    let mut nonce = [0; NONCE];
    getrandom::fill(&mut nonce).ok()?;
    issue_with_nonce(record, claim, now_ms, nonce)
}

fn issue_with_nonce(
    record: &[u8; 48],
    claim: BackupRestoreClaim,
    now_ms: u64,
    nonce: [u8; NONCE],
) -> Option<BackupRestoreProof> {
    let mut fixed = [0; CLAIMS];
    fixed[..EPOCH].copy_from_slice(&record[32..]);
    fixed[EPOCH..EPOCH + TIME].copy_from_slice(&now_ms.to_be_bytes());
    fixed[EPOCH + TIME..].copy_from_slice(&nonce);
    let tag = signer(record, &fixed, claim)?.finalize().into_bytes();
    let mut wire = String::with_capacity(PREFIX.len() + 2 * RAW);
    wire.push_str(PREFIX);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in fixed.into_iter().chain(tag) {
        wire.push(char::from(HEX[usize::from(b >> 4)]));
        wire.push(char::from(HEX[usize::from(b & 15)]));
    }
    Some(BackupRestoreProof(wire))
}

pub fn verify(
    record: &[u8; 48],
    claim: BackupRestoreClaim,
    header: &str,
    now_ms: u64,
) -> Option<VerifiedBackupProof> {
    let raw = decode(header)?;
    if raw[..EPOCH] != record[32..] {
        return None;
    }
    let issued_at = u64::from_be_bytes(raw[EPOCH..EPOCH + TIME].try_into().ok()?);
    if now_ms.checked_sub(issued_at)? > PROOF_MAX_AGE_MS {
        return None;
    }
    signer(record, &raw[..CLAIMS], claim)?
        .verify_slice(&raw[CLAIMS..])
        .ok()?;
    Some(VerifiedBackupProof {
        nonce: raw[EPOCH + TIME..CLAIMS].try_into().ok()?,
        expires_at_ms: issued_at.checked_add(PROOF_MAX_AGE_MS)?,
    })
}

fn signer(record: &[u8; 48], fixed: &[u8], claim: BackupRestoreClaim) -> Option<Hmac<Sha256>> {
    if claim.operation_id == [0; 16] || claim.instance == [0; 16] || claim.generation == 0 {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&record[..32]).ok()?;
    mac.update(DOMAIN);
    mac.update(fixed);
    mac.update(&claim.instance);
    mac.update(&claim.generation.to_be_bytes());
    mac.update(&claim.operation_id);
    mac.update(&claim.archive_sha256);
    mac.update(&claim.target_sha256);
    Some(mac)
}

fn decode(header: &str) -> Option<[u8; RAW]> {
    let hex = header.strip_prefix(PREFIX)?.as_bytes();
    if hex.len() != 2 * RAW {
        return None;
    }
    let digit = |b| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut raw = [0; RAW];
    for (pair, byte) in hex.chunks_exact(2).zip(&mut raw) {
        *byte = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAIM: BackupRestoreClaim = BackupRestoreClaim {
        operation_id: [1; 16],
        archive_sha256: [2; 32],
        target_sha256: [3; 32],
        instance: [4; 16],
        generation: 77,
    };

    #[test]
    fn verifies_exact_claim_and_rejects_mismatches() {
        let record = [8; 48];
        let proof = issue_with_nonce(&record, CLAIM, 123, [5; 16]).unwrap();
        assert_eq!(proof.as_str().len(), 149);
        assert!(proof.as_str().starts_with("rhb1_"));
        let valid = verify(&record, CLAIM, proof.as_str(), 124).unwrap();
        assert_eq!(valid.nonce, [5; 16]);
        assert_eq!(valid.expires_at_ms, 123 + PROOF_MAX_AGE_MS);
        for bad in [
            BackupRestoreClaim { operation_id: [9; 16], ..CLAIM },
            BackupRestoreClaim { archive_sha256: [9; 32], ..CLAIM },
            BackupRestoreClaim { target_sha256: [9; 32], ..CLAIM },
            BackupRestoreClaim { instance: [9; 16], ..CLAIM },
            BackupRestoreClaim { generation: 78, ..CLAIM },
        ] {
            assert!(verify(&record, bad, proof.as_str(), 124).is_none());
        }
        assert!(verify(&record, CLAIM, proof.as_str(), 123 + PROOF_MAX_AGE_MS + 1).is_none());
        let mut flipped_epoch = record;
        flipped_epoch[32] ^= 1;
        assert!(verify(&flipped_epoch, CLAIM, proof.as_str(), 124).is_none());
    }
}
