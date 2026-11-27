//! Coalesce new retrieval-index starts behind a bounded foreground create burst.
use super::IndexWork;
use std::time::{Duration, Instant};

const CREATE_QUIET: Duration = Duration::from_millis(50);
const CREATE_BATCH_MAX: Duration = Duration::from_millis(500);

impl IndexWork {
    pub(super) fn model_stopped(&mut self, error: &anyhow::Error) {
        if matches!(
            error.downcast_ref::<crate::model_progress::OperationStopped>(),
            Some(crate::model_progress::OperationStopped::Preempted)
        ) {
            self.state = "scheduled";
            self.requested = true;
            self.error = None;
        } else {
            self.state = "paused";
            self.requested = false;
            self.error = Some(format!("{error:#}"));
        }
    }

    pub(super) fn schedule(&mut self, now: Instant, batched: bool) {
        if !batched {
            self.create_batch = None;
        } else if let Some((first, _)) = self.create_batch {
            self.create_batch = Some((first, now));
        } else if !self.requested {
            self.create_batch = Some((now, now));
        }
        self.requested = true;
    }

    pub(super) fn create_delay(&self, now: Instant) -> Option<Duration> {
        let (first, last) = self.create_batch?;
        let deadline = (first + CREATE_BATCH_MAX).min(last + CREATE_QUIET);
        deadline
            .checked_duration_since(now)
            .filter(|delay| !delay.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> IndexWork {
        IndexWork {
            requested: false,
            create_batch: None,
            state: "idle",
            error: None,
        }
    }

    #[test]
    fn create_burst_is_coalesced_but_cannot_starve_index() {
        let start = Instant::now();
        let mut work = empty();
        work.schedule(start, true);
        assert_eq!(work.create_delay(start), Some(CREATE_QUIET));
        for t in (10..500).step_by(10) {
            work.schedule(start + Duration::from_millis(t), true);
        }
        assert!(work.requested);
        assert_eq!(
            work.create_delay(start + Duration::from_millis(490)),
            Some(Duration::from_millis(10))
        );
        assert_eq!(work.create_delay(start + CREATE_BATCH_MAX), None);
    }

    #[test]
    fn late_preemption_exit_keeps_the_foreground_resume_request() {
        let mut work = empty();
        work.schedule(Instant::now(), false);
        work.model_stopped(&crate::model_progress::OperationStopped::Preempted.into());
        assert!(work.requested);
        assert_eq!(work.state, "scheduled");
        assert!(work.error.is_none());
        work.model_stopped(&crate::model_progress::OperationStopped::Cancelled.into());
        assert!(!work.requested);
        assert_eq!(work.state, "paused");
    }

    #[test]
    fn ordinary_index_request_is_never_delayed_by_create_burst() {
        let start = Instant::now();
        let mut work = empty();
        work.schedule(start, true);
        work.schedule(start, false);
        work.schedule(start, true);
        assert!(work.requested);
        assert_eq!(work.create_delay(start), None);
    }
}
