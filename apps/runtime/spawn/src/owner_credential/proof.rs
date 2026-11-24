//! Stateless v1 local-owner proofs. Callers own OS credential access, live
//! account/instance binding, requested scope equality, and atomic replay checks.
use hmac::{Hmac, Mac};
use sha2::Sha256;

const PREFIX: &str = "rhp1_";
const DOMAIN: &[u8] = b"respire-local-owner-proof-v1\0";
const KEY_LEN: usize = 32;
const EPOCH_LEN: usize = 16;
const TIME_LEN: usize = 8;
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 32;
const NONCE_START: usize = EPOCH_LEN + TIME_LEN;
const SCOPE_OFFSET: usize = NONCE_START + NONCE_LEN;
const CLAIMS_LEN: usize = SCOPE_OFFSET + 1;
const RAW_LEN: usize = CLAIMS_LEN + TAG_LEN;
const VALID_SCOPES: u8 = 0b0011_1111;
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

pub const PROOF_MAX_AGE_MS: u64 = 30_000;

type HmacSha256 = Hmac<Sha256>;

/// Intentionally lacks Debug, Display and serialization.
pub struct OwnerProof(String);

impl OwnerProof {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Only constructed after an authenticated, current-epoch proof is verified.
/// This is not a replay receipt or authorization to access a scope.
pub struct VerifiedProof {
    issued_at_ms: u64,
    nonce: [u8; NONCE_LEN],
    scopes: u8,
}

impl VerifiedProof {
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    pub fn nonce(&self) -> [u8; NONCE_LEN] {
        self.nonce
    }

    pub fn scopes(&self) -> u8 {
        self.scopes
    }
}

/// Exchanges use an externally supplied Unix timestamp and an OS CSPRNG nonce.
/// The record is exactly a key followed by the Runtime's current epoch.
pub fn issue(record: &[u8; 48], scopes: u8, now_ms: u64) -> Option<OwnerProof> {
    if !valid_scopes(scopes) {
        return None;
    }
    let mut nonce = [0; NONCE_LEN];
    getrandom::fill(&mut nonce).ok()?;
    encode(record, now_ms, nonce, scopes)
}

/// Authenticates the exact canonical claims and fails closed for any other
/// encoding, epoch, time or scope mask. Does not consume the proof.
pub fn verify(record: &[u8; 48], header: &str, now_ms: u64) -> Option<VerifiedProof> {
    let raw = decode(header)?;
    // Mac::verify_slice compares the full 32-byte tag in constant time.
    signer(record, &raw[..CLAIMS_LEN])?
        .verify_slice(&raw[CLAIMS_LEN..])
        .ok()?;

    // The epoch is public in an authenticated proof, not a secret key.
    if raw[..EPOCH_LEN] != record[KEY_LEN..] {
        return None;
    }
    let issued_at_ms = u64::from_be_bytes(raw[EPOCH_LEN..NONCE_START].try_into().ok()?);
    if now_ms.checked_sub(issued_at_ms)? > PROOF_MAX_AGE_MS {
        return None;
    }
    let scopes = raw[SCOPE_OFFSET];
    if !valid_scopes(scopes) {
        return None;
    }
    Some(VerifiedProof {
        issued_at_ms,
        nonce: raw[NONCE_START..SCOPE_OFFSET].try_into().ok()?,
        scopes,
    })
}

fn valid_scopes(scopes: u8) -> bool {
    // Bits 0..=5: grants:read, grants:revoke, stats:read, query-log:read,
    // reviews:read, sync-conflicts:read, in canonical contract enum order.
    scopes != 0 && scopes & !VALID_SCOPES == 0
}

fn signer(record: &[u8; 48], claims: &[u8]) -> Option<HmacSha256> {
    let mut mac = HmacSha256::new_from_slice(&record[..KEY_LEN]).ok()?;
    mac.update(DOMAIN);
    mac.update(claims);
    Some(mac)
}

fn encode(
    record: &[u8; 48],
    issued_at_ms: u64,
    nonce: [u8; NONCE_LEN],
    scopes: u8,
) -> Option<OwnerProof> {
    let mut claims = [0; CLAIMS_LEN];
    claims[..EPOCH_LEN].copy_from_slice(&record[KEY_LEN..]);
    claims[EPOCH_LEN..NONCE_START].copy_from_slice(&issued_at_ms.to_be_bytes());
    claims[NONCE_START..SCOPE_OFFSET].copy_from_slice(&nonce);
    claims[SCOPE_OFFSET] = scopes;
    let tag = signer(record, &claims)?.finalize().into_bytes();

    let mut header = String::with_capacity(PREFIX.len() + 2 * RAW_LEN);
    header.push_str(PREFIX);
    for byte in claims.into_iter().chain(tag) {
        header.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        header.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    Some(OwnerProof(header))
}

fn decode(header: &str) -> Option<[u8; RAW_LEN]> {
    let hex = header.strip_prefix(PREFIX)?.as_bytes();
    if hex.len() != 2 * RAW_LEN {
        return None;
    }
    let mut raw = [0; RAW_LEN];
    for (pair, byte) in hex.chunks_exact(2).zip(&mut raw) {
        *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(raw)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_234_567_890;
    const NONCE: [u8; NONCE_LEN] = [0xa5; NONCE_LEN];
    const MASK: u8 = 0x21;
    const VECTOR: &str = concat!(
        "rhp1_202122232425262728292a2b2c2d2e2f00000000499602d2",
        "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a521",
        "75e3e3b613134f646cbee0cbaa25e4a676977f045a9f24697c24e052c5849497",
    );

    fn fixture_record() -> [u8; 48] {
        std::array::from_fn(|i| i as u8)
    }

    #[test]
    fn exact_wire_vector_and_time_endpoints() -> Result<(), &'static str> {
        let record = fixture_record();
        let proof = encode(&record, NOW, NONCE, MASK).ok_or("fixture encoding failed")?;
        assert_eq!(proof.as_str().len(), 151);
        assert_eq!(proof.as_str(), VECTOR);
        let decoded = verify(&record, VECTOR, NOW).ok_or("fixture verification failed")?;
        assert_eq!(decoded.nonce(), NONCE);
        assert_eq!(decoded.scopes(), MASK);
        assert_eq!(decoded.issued_at_ms(), NOW);
        assert!(verify(&record, VECTOR, NOW + PROOF_MAX_AGE_MS).is_some());
        assert!(verify(&record, VECTOR, NOW + PROOF_MAX_AGE_MS + 1).is_none());
        assert!(verify(&record, VECTOR, NOW - 1).is_none());
        Ok(())
    }

    #[test]
    fn altered_claims_and_tag_wrong_key_epoch_are_invalid() {
        let record = fixture_record();
        for i in (PREFIX.len()..VECTOR.len()).step_by(2) {
            let mut altered = VECTOR.as_bytes().to_vec();
            altered[i] = if altered[i] == b'a' { b'b' } else { b'a' };
            assert!(std::str::from_utf8(&altered)
                .is_ok_and(|header| verify(&record, header, NOW).is_none()));
        }
        let mut other = record;
        other[0] ^= 1;
        assert!(verify(&other, VECTOR, NOW).is_none());
        let mut next_epoch = record;
        next_epoch[KEY_LEN] ^= 1;
        // Same key: tag remains valid, but the previous epoch must be rejected.
        assert!(verify(&next_epoch, VECTOR, NOW).is_none());
    }

    #[test]
    fn canonical_header_and_mask_are_required() -> Result<(), &'static str> {
        let record = fixture_record();
        for malformed in [
            VECTOR.to_uppercase(),
            VECTOR.replacen('a', "A", 1),
            VECTOR.replacen('a', "g", 1),
            format!(" {VECTOR}"),
            format!("{VECTOR}0"),
            VECTOR[..VECTOR.len() - 1].to_owned(),
        ] {
            assert!(verify(&record, &malformed, NOW).is_none());
        }
        for mask in [0, 0x40, 0x80, 0xff] {
            assert!(issue(&record, mask, NOW).is_none());
            let invalid = encode(&record, NOW, NONCE, mask).ok_or("fixture encoding failed")?;
            assert!(verify(&record, invalid.as_str(), NOW).is_none());
        }
        Ok(())
    }

    #[test]
    fn randomly_issued_proofs_use_distinct_nonces() -> Result<(), &'static str> {
        let record = fixture_record();
        let first = issue(&record, MASK, NOW).ok_or("OS proof issuance failed")?;
        let second = issue(&record, MASK, NOW).ok_or("OS proof issuance failed")?;
        assert!(first.as_str() != second.as_str());
        assert_ne!(
            verify(&record, first.as_str(), NOW)
                .ok_or("first verification failed")?
                .nonce(),
            verify(&record, second.as_str(), NOW)
                .ok_or("second verification failed")?
                .nonce()
        );
        Ok(())
    }
}
