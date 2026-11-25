use std::{
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use tokio::{sync::oneshot, time};

use super::{transport, Problem, V1Result};

const PREPARING: u8 = 0;
const ADMITTED: u8 = 1;
const ABANDONED: u8 = 2;
pub(super) const COMMIT_WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(in crate::runtime_http::v1) struct Admission(AtomicU8);

impl Admission {
    pub(in crate::runtime_http::v1) fn admit(&self) -> bool {
        self.transition(ADMITTED)
    }

    pub(super) fn abandon(&self) -> bool {
        self.transition(ABANDONED)
    }

    fn transition(&self, next: u8) -> bool {
        self.0.compare_exchange(PREPARING, next, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    fn admitted(&self) -> bool {
        self.0.load(Ordering::Acquire) == ADMITTED
    }
}

fn outcome_unknown() -> Problem {
    Problem::new(
        axum::http::StatusCode::CONFLICT,
        "space_create_outcome_unknown",
        "Local space creation outcome is unknown; do not retry automatically",
    )
}

fn complete<T>(
    admission: &Admission,
    result: Result<V1Result<T>, oneshot::error::RecvError>,
) -> V1Result<T> {
    match result {
        Ok(Ok(value)) if admission.admitted() => Ok(value),
        Ok(Ok(_)) => Err(transport::unavailable()),
        _ if admission.admitted() => Err(outcome_unknown()),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(transport::unavailable()),
    }
}

pub(super) async fn await_publication<T>(
    admission: &Admission,
    mut receiver: oneshot::Receiver<V1Result<T>>,
    queue_deadline: std::time::Instant,
    commit_wait: Duration,
) -> V1Result<T> {
    match time::timeout_at(time::Instant::from_std(queue_deadline), &mut receiver).await {
        Ok(result) => complete(admission, result),
        Err(_) if admission.abandon() || !admission.admitted() => {
            Err(transport::unavailable())
        }
        Err(_) => match time::timeout(commit_wait, receiver).await {
            Ok(result) => complete(admission, result),
            Err(_) => Err(outcome_unknown()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};

    #[test]
    fn deadline_abandon_prevents_later_admission() {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
            .block_on(async {
                let admission = Admission::default();
                let (_pending, receipt) = oneshot::channel::<V1Result<()>>();
                let elapsed = std::time::Instant::now() - Duration::from_secs(1);
                let rejected = await_publication(&admission, receipt, elapsed, Duration::ZERO)
                    .await.expect_err("pending preparation exceeded its deadline");
                assert_eq!(rejected.into_response().status(), StatusCode::SERVICE_UNAVAILABLE);
                assert!(!admission.admit());
            });
    }

    #[test]
    fn committed_or_uncertain_publication_is_not_a_definitive_failure() {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
            .block_on(async {
                let admission = Admission::default();
                assert!(admission.admit());
                assert!(!admission.abandon());
                let elapsed = std::time::Instant::now() - Duration::from_secs(1);
                let (_pending, receipt) = oneshot::channel::<V1Result<()>>();
                let unknown = await_publication(&admission, receipt, elapsed, Duration::ZERO)
                    .await.expect_err("no outcome receipt after publication admission");
                assert_eq!(unknown.into_response().status(), StatusCode::CONFLICT);

                let (writer, receipt) = oneshot::channel::<V1Result<()>>();
                writer.send(Err(transport::unavailable())).expect("receiver alive");
                let unknown = await_publication(&admission, receipt, elapsed, Duration::ZERO)
                    .await.expect_err("admitted I/O may already have published");
                assert_eq!(unknown.into_response().status(), StatusCode::CONFLICT);

                let (writer, receipt) = oneshot::channel::<V1Result<()>>();
                writer.send(Ok(())).expect("receiver alive");
                await_publication(&admission, receipt, elapsed, Duration::ZERO)
                    .await.expect("actual receipt is returned");
            });
    }
}
