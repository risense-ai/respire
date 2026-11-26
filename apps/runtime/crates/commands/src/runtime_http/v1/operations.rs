//! Operation receipts owned by the background worker and scoped to a live owner.
mod deepen;
mod model_download;
pub(super) mod model_switch;
pub(super) mod organize_split;
mod routes;
mod state;
mod sync;
#[cfg(test)]
mod tests;

pub(super) use routes::routes;
#[cfg(target_os = "linux")]
pub(super) use state::archive_claim::retire_pending_archives;
pub(crate) use sync::PendingSync;

#[cfg(test)]
pub(super) use state::check_receipts;

#[cfg(target_os = "linux")]
pub(super) mod backup_import;

#[cfg(all(test, target_os = "linux"))]
pub(super) use state::archive_claim::routes as archive_claim_fixture;
