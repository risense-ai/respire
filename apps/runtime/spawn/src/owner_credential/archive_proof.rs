//! Native proof for admission of one exact staged restore on a serving instance.
use super::proof::PROOF_MAX_AGE_MS;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Digests are of the full archive wire and UTF-8 canonical target database path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ArchiveClaim {
    pub query_only: bool,
    pub inspect_only: bool,
    pub operation: [u8; 16],
    pub archive: [u8; 32],
    pub target: [u8; 32],
    pub target_identity: [u8; 32],
    pub instance: [u8; 16],
    pub generation: u64,
}

fn mac(record: &[u8; 48], claim: ArchiveClaim, time: u64) -> Option<Hmac<Sha256>> {
    if (claim.inspect_only && (claim.query_only || claim.target_identity != [0; 32]))
        || claim.operation[6] >> 4 != 4
        || claim.operation[8] & 0xc0 != 0x80
        || claim.instance == [0; 16]
        || claim.generation == 0
    {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&record[..32]).ok()?;
    mac.update(b"respire/native/archive-restore/claim/v2\0");
    mac.update(&[u8::from(claim.query_only), u8::from(claim.inspect_only)]);
    mac.update(&record[32..]);
    mac.update(&claim.instance);
    mac.update(&claim.generation.to_be_bytes());
    mac.update(&claim.operation);
    mac.update(&claim.archive);
    mac.update(&claim.target);
    mac.update(&claim.target_identity);
    mac.update(&time.to_be_bytes());
    Some(mac)
}

pub fn issue(record: &[u8; 48], claim: ArchiveClaim, time: u64) -> Option<String> {
    use std::fmt::Write;
    let mut out = format!("rha2_{time:016x}");
    for byte in mac(record, claim, time)?.finalize().into_bytes() {
        write!(&mut out, "{byte:02x}").ok()?;
    }
    Some(out)
}

/// The live operation mutex supplies the one-time consumption boundary.
pub fn verify(record: &[u8; 48], claim: ArchiveClaim, proof: &str, now: u64) -> bool {
    let Some(hex) = proof.strip_prefix("rha2_") else {
        return false;
    };
    if hex.len() != 80
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    let Ok(time) = u64::from_str_radix(&hex[..16], 16) else {
        return false;
    };
    if now
        .checked_sub(time)
        .is_none_or(|age| age > PROOF_MAX_AGE_MS)
    {
        return false;
    }
    let mut tag = [0; 32];
    for (index, byte) in tag.iter_mut().enumerate() {
        let start = 16 + index * 2;
        let Ok(value) = u8::from_str_radix(&hex[start..start + 2], 16) else {
            return false;
        };
        *byte = value;
    }
    mac(record, claim, time).is_some_and(|mac| mac.verify_slice(&tag).is_ok())
}
