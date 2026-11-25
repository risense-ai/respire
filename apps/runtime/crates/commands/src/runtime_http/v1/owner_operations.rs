//! Typed execution only against the serving dispatcher's already-open library.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use respire_app::transport::local::LocalStore;

use super::{
    compatibility::service,
    owner_scope::OwnerAction,
    problem::{Problem, V1Result},
};

pub(super) enum OwnerCall {
    ListGrants,
    RevokeGrant {
        grant_id: String,
    },
    Stats,
    QueryLog {
        limit: Option<usize>,
    },
    Reviews {
        memory_id: String,
        limit: Option<usize>,
    },
    Conflicts {
        memory_id: Option<String>,
        include_processed: bool,
    },
}

impl OwnerCall {
    pub(super) fn action(&self) -> OwnerAction {
        match self {
            Self::ListGrants => OwnerAction::ReadGrants,
            Self::RevokeGrant { .. } => OwnerAction::RevokeGrant,
            Self::Stats => OwnerAction::ReadStats,
            Self::QueryLog { .. } => OwnerAction::ReadQueryLog,
            Self::Reviews { .. } => OwnerAction::ReadReviews,
            Self::Conflicts { .. } => OwnerAction::ReadSyncConflicts,
        }
    }

    fn requires_writable(&self, service_paused: bool) -> bool {
        matches!(self, Self::ListGrants | Self::RevokeGrant { .. })
            || (service_paused && matches!(self, Self::Conflicts { .. }))
    }

    fn check_policy(
        &self,
        off: bool,
        paused: bool,
        is_writable: impl FnOnce() -> bool,
    ) -> V1Result<()> {
        if off {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "memory_off",
                "Memory service disabled",
            ));
        }
        if self.requires_writable(paused) && !is_writable() {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "write_disabled",
                "Workspace is read-only",
            ));
        }
        Ok(())
    }

    /// Caller owns the scope fence, checked lease ticket and storage gate
    /// for the duration of this call. This function never reopens the store.
    pub(super) fn run(self, store: &LocalStore, device_local: bool) -> V1Result<Response> {
        self.check_policy(
            respire_app::service::off_mode(),
            respire_app::service::service_paused(),
            || respire_app::service::ensure_writable().is_ok(),
        )?;
        let reply = match self {
            Self::ListGrants => service::list_grants(store),
            Self::RevokeGrant { grant_id } => {
                service::revoke_grant(store, &grant_id).map_err(service_error)?;
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
            Self::Stats => service::stats(store, !device_local && respire_app::sync::remote_configured()),
            Self::QueryLog { limit } => service::query_log(store, limit),
            Self::Reviews { memory_id, limit } => service::reviews(store, &memory_id, limit),
            Self::Conflicts {
                memory_id,
                include_processed,
            } => service::conflicts(store, memory_id.as_deref(), include_processed),
        }
        .map_err(service_error)?;
        Ok((
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            reply.to_string(),
        )
            .into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_and_memory_off_keep_legacy_policy() {
        let revoke = OwnerCall::RevokeGrant {
            grant_id: String::new(),
        };
        let conflict = OwnerCall::Conflicts {
            memory_id: None,
            include_processed: false,
        };
        for action in [&revoke, &OwnerCall::ListGrants] {
            let denied = action
                .check_policy(false, false, || false)
                .err()
                .expect("read-only grant operation denied");
            assert_eq!(denied.into_response().status(), StatusCode::FORBIDDEN);
        }
        assert!(conflict.check_policy(false, true, || false).is_err());
        assert!(conflict.check_policy(false, false, || false).is_ok());
        assert!(OwnerCall::Stats
            .check_policy(false, false, || false)
            .is_ok());
        for action in [&revoke, &OwnerCall::Stats, &conflict] {
            assert!(action.check_policy(true, false, || true).is_err());
        }
    }
}

fn service_error(failure: service::ServiceError) -> Problem {
    use service::ServiceError::*;
    match failure {
        InvalidRequest => Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        ),
        StorageUnavailable | InvalidStoredMetadata => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "Runtime unavailable",
        ),
    }
}
