//! Bounded Core-owned staging. No pointer to writable backing crosses the ABI.
use super::{Failure, ENGINE_FAILURE};
use respire_core::bge::ModelUpload;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(super) const MAX_MODEL_BYTES: usize = 1 << 30;
pub(super) const MAX_CHUNK_BYTES: usize = 4 << 20;
const STAGING_BUDGET: usize = 1 << 30;
static STAGED: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(super) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

pub(super) struct Reservation { bytes: usize }
impl Drop for Reservation {
    fn drop(&mut self) { STAGED.fetch_sub(self.bytes, Ordering::AcqRel); }
}

pub(super) struct Pending {
    ticket: u64,
    expected: usize,
    upload: ModelUpload,
    reservation: Reservation,
}

impl Pending {
    pub(super) fn begin(expected: usize) -> Result<Self, Failure> {
        Self::begin_with(expected, |size| ModelUpload::new(size).map_err(|error| error.to_string()))
    }

    fn begin_with(expected: usize, reserve: impl FnOnce(usize) -> Result<ModelUpload, String>) -> Result<Self, Failure> {
        if expected == 0 || expected > MAX_MODEL_BYTES || expected > isize::MAX as usize {
            return Err(Failure::invalid("model input length outside supported range"));
        }
        STAGED.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| used.checked_add(expected).filter(|total| *total <= STAGING_BUDGET))
            .map_err(|_| Failure::invalid("model input staging budget exhausted"))?;
        let reservation = Reservation { bytes: expected };
        let ticket = NEXT_TICKET.fetch_update(Ordering::AcqRel, Ordering::Acquire, |next| next.checked_add(1))
            .map_err(|_| Failure::invalid("model input ticket space exhausted"))?;
        let upload = reserve(expected).map_err(|message| Failure { code: ENGINE_FAILURE, message })?;
        Ok(Self { ticket, expected, upload, reservation })
    }

    pub(super) fn ticket(&self) -> u64 { self.ticket }
    pub(super) fn append(&mut self, offset: usize, chunk: &[u8]) -> Result<usize, Failure> {
        if offset != self.upload.bytes().len() || chunk.len() > MAX_CHUNK_BYTES || chunk.len() > self.expected - self.upload.bytes().len() {
            return Err(Failure::invalid("model input offset or chunk length is invalid"));
        }
        // Capacity was fallibly reserved at begin. This cannot reallocate.
        if !self.upload.append(chunk) {
            return Err(Failure { code: ENGINE_FAILURE, message: "model input capacity was not reserved".into() });
        }
        Ok(self.upload.bytes().len())
    }
    pub(super) fn seal(self) -> Result<(ModelUpload, Reservation), Failure> {
        if self.upload.bytes().len() != self.expected { return Err(Failure::invalid("model input is incomplete")); }
        Ok((self.upload, self.reservation))
    }
}

pub(super) fn take(pending: &mut Option<Pending>, ticket: u64) -> Result<Pending, Failure> {
    if pending.as_ref().is_none_or(|input| input.ticket != ticket) {
        return Err(Failure::invalid("unknown model input ticket"));
    }
    pending.take().ok_or_else(|| Failure::invalid("unknown model input ticket"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // One test keeps global accounting assertions independent of test scheduling.
    #[test]
    fn staging_lifecycle_and_failures() -> Result<(), String> {
        let _serial = TEST_LOCK.lock().map_err(|error| error.to_string())?;
        let cvt = |error: Failure| error.message;
        let before = STAGED.load(Ordering::Acquire);
        assert!(Pending::begin(0).is_err());
        assert!(Pending::begin(MAX_MODEL_BYTES + 1).is_err());
        assert!(Pending::begin(usize::MAX).is_err());
        assert!(Pending::begin_with(16, |_| Err("injected allocation failure".into())).is_err());
        assert_eq!(STAGED.load(Ordering::Acquire), before);
        let mut a = Some(Pending::begin(8).map_err(cvt)?);
        let ticket = a.as_ref().ok_or("missing pending")?.ticket();
        let pointer = a.as_ref().ok_or("missing pending")?.upload.bytes().as_ptr();
        assert!(take(&mut a, ticket + 1).is_err());
        let mut other = Some(Pending::begin(8).map_err(cvt)?);
        assert!(take(&mut other, ticket).is_err());
        let input = a.as_mut().ok_or("missing pending")?;
        assert!(input.append(1, &[1]).is_err());
        assert!(input.append(0, &[0; 9]).is_err());
        assert_eq!(input.append(0, &[]).map_err(cvt)?, 0);
        assert_eq!(input.append(0, &[1,2,3,4]).map_err(cvt)?, 4);
        assert!(input.append(0, &[1]).is_err());
        assert!(input.append(usize::MAX, &[1]).is_err());
        assert_eq!(input.append(4, &[5,6,7,8]).map_err(cvt)?, 8);
        let (bytes, reservation) = take(&mut a, ticket).map_err(cvt)?.seal().map_err(cvt)?;
        assert_eq!(pointer, bytes.bytes().as_ptr());
        assert_eq!(bytes.bytes(), &[1,2,3,4,5,6,7,8]);
        assert!(take(&mut a, ticket).is_err());
        a = Some(Pending::begin(4).map_err(cvt)?);
        assert!(take(&mut a, ticket).is_err());
        assert!(a.is_some());
        let newer = a.as_ref().ok_or("missing pending")?.ticket();
        assert!(take(&mut a, newer).map_err(cvt)?.seal().is_err());
        drop(bytes); drop(reservation); drop(other);
        assert_eq!(STAGED.load(Ordering::Acquire), before);
        let full = Pending::begin_with(STAGING_BUDGET, |_| ModelUpload::new(0).map_err(|error| error.to_string())).map_err(cvt)?;
        assert!(Pending::begin(1).is_err());
        drop(full);
        assert_eq!(STAGED.load(Ordering::Acquire), before);
        // Both staging tests hold TEST_LOCK; restore the counter before any assertion.
        let next = NEXT_TICKET.swap(u64::MAX, Ordering::AcqRel);
        let exhausted = Pending::begin(1);
        NEXT_TICKET.store(next, Ordering::Release);
        assert!(exhausted.is_err());
        assert_eq!(STAGED.load(Ordering::Acquire), before);
        let mut chunk_limited = Pending::begin(MAX_CHUNK_BYTES + 1).map_err(cvt)?;
        assert!(chunk_limited.append(0, &vec![0; MAX_CHUNK_BYTES + 1]).is_err());
        assert_eq!(chunk_limited.upload.bytes().len(), 0);
        Ok(())
    }
}
