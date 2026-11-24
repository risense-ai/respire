//! Purpose-bound proof for a private local model operation. The ordinary
//! owner handshake and its lease tokens cannot be used on this channel.
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::proof::PROOF_MAX_AGE_MS;

const PREFIX: &str = "rhm1_";
const DOMAIN: &[u8] = b"respire-private-local-model-operation-v1\0";
const EPOCH: usize = 16;
const TIME: usize = 8;
const NONCE: usize = 16;
const CLAIMS: usize = EPOCH + TIME + NONCE;
const TAG: usize = 32;
const RAW: usize = CLAIMS + TAG;

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum ModelAction {
    Publish = 1,
    Remove = 2,
}

#[derive(Clone, Copy)]
pub struct ModelClaim<'a> {
    pub action: ModelAction,
    pub model_id: &'a str,
    pub instance: [u8; 16],
    pub generation: u64,
}

/// Intentionally no debug/serialization beyond the explicitly supplied header.
pub struct ModelProof(String);
impl ModelProof {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Valid MAC and epoch only; callers must still atomically consume the nonce
/// under the current owner state before accepting a job.
pub struct VerifiedModelProof {
    pub nonce: [u8; NONCE],
    pub expires_at_ms: u64,
}

pub fn issue(record: &[u8; 48], claim: ModelClaim<'_>, now_ms: u64) -> Option<ModelProof> {
    let mut nonce = [0; NONCE];
    getrandom::fill(&mut nonce).ok()?;
    issue_with_nonce(record, claim, now_ms, nonce)
}

fn issue_with_nonce(
    record: &[u8; 48],
    claim: ModelClaim<'_>,
    now_ms: u64,
    nonce: [u8; NONCE],
) -> Option<ModelProof> {
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
    Some(ModelProof(wire))
}

pub fn verify(
    record: &[u8; 48],
    claim: ModelClaim<'_>,
    header: &str,
    now_ms: u64,
) -> Option<VerifiedModelProof> {
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
    Some(VerifiedModelProof {
        nonce: raw[EPOCH + TIME..CLAIMS].try_into().ok()?,
        expires_at_ms: issued_at.checked_add(PROOF_MAX_AGE_MS)?,
    })
}

fn signer(record: &[u8; 48], fixed: &[u8], claim: ModelClaim<'_>) -> Option<Hmac<Sha256>> {
    let id = claim.model_id.as_bytes();
    if !valid_model_id(claim.model_id) || claim.instance == [0; 16] || claim.generation == 0 {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&record[..32]).ok()?;
    mac.update(DOMAIN);
    mac.update(fixed);
    mac.update(&claim.instance);
    mac.update(&claim.generation.to_be_bytes());
    mac.update(&[claim.action as u8, u8::try_from(id.len()).ok()?]);
    mac.update(id);
    Some(mac)
}

/// Canonical private wire ID, also used for managed datastore projection.
pub fn valid_model_id(model: &str) -> bool {
    let id = model.as_bytes();
    !id.is_empty()
        && id.len() <= 128
        && id[0].is_ascii_alphanumeric()
        && id[id.len() - 1].is_ascii_alphanumeric()
        && id.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'.'
        })
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

    const CLAIM: ModelClaim<'_> = ModelClaim {
        action: ModelAction::Remove,
        model_id: "bge-small-zh-v1.5",
        instance: [3; 16],
        generation: 77,
    };

    #[test]
    fn rejects_other_operation_model_and_live_scope() {
        let record = [8; 48];
        let proof = issue_with_nonce(&record, CLAIM, 123, [4; 16]).unwrap();
        let valid = verify(&record, CLAIM, proof.as_str(), 124).unwrap();
        assert_eq!(valid.nonce, [4; 16]);
        assert_eq!(valid.expires_at_ms, 123 + PROOF_MAX_AGE_MS);
        for bad in [
            ModelClaim {
                action: ModelAction::Publish,
                ..CLAIM
            },
            ModelClaim {
                model_id: "m3",
                ..CLAIM
            },
            ModelClaim {
                instance: [4; 16],
                ..CLAIM
            },
            ModelClaim {
                generation: 78,
                ..CLAIM
            },
        ] {
            assert!(verify(&record, bad, proof.as_str(), 124).is_none());
        }
        let mut rotated = record;
        rotated[32] ^= 1;
        assert!(verify(&rotated, CLAIM, proof.as_str(), 124).is_none());
        assert!(verify(&record, CLAIM, proof.as_str(), 122).is_none());
        assert!(verify(&record, CLAIM, proof.as_str(), 124 + PROOF_MAX_AGE_MS).is_none());
    }

    #[test]
    fn ordinary_proofs_and_noncanonical_ids_do_not_cross_domains() {
        let record = [9; 48];
        let ordinary = super::super::proof::issue(&record, 1, 123).unwrap();
        let model = issue(&record, CLAIM, 123).unwrap();
        assert!(verify(&record, CLAIM, ordinary.as_str(), 123).is_none());
        assert!(super::super::proof::verify(&record, model.as_str(), 123).is_none());
        for id in ["", "../m3", "model/path", ".model", "m3.", "m3:extra", "M3"] {
            assert!(issue(
                &record,
                ModelClaim {
                    model_id: id,
                    ..CLAIM
                },
                123
            )
            .is_none());
        }
    }
}
