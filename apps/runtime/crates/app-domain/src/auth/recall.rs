use super::RuntimeLibrary;
use crate::{
    review::{AuthorizedRecallSource, ReviewService},
    transport::local::LocalStore,
};

impl RuntimeLibrary<'_> {
    pub fn with_recall_review<T>(
        &self,
        store: &LocalStore,
        owner: &str,
        run: impl FnOnce(&ReviewService<'_>, &AuthorizedRecallSource) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let review = ReviewService::new(self.0, store, owner)?;
        let source = review.recall_source()?;
        if source.entries().next().is_none() {
            return run(&review, &source);
        }
        let lease = crate::resident::recall_authorized_lease(self.0, store, owner, &source)?;
        let _scope = lease.enter();
        run(&review, &source)
    }
}
