use super::super::{transport, transport::RestScope, Problem, V1Result};
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

#[path = "backup_restore_receipt.rs"]
pub(super) mod backup_restore_receipt;

const MAX_TRACKED: usize = 128;
const TERMINAL_RETENTION: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Pending,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Serialize)]
pub(super) struct Snapshot {
    operation_id: String,
    kind: &'static str,
    state: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    #[serde(skip)]
    pub(super) revision: u64,
}

impl Snapshot {
    pub(super) fn durable_backup(id: Uuid, state: Phase, revision: u64) -> Self {
        Self {
            operation_id: id.to_string(),
            kind: "backup_import",
            state,
            progress_percent: (state == Phase::Completed).then_some(100),
            error: (state == Phase::Failed).then(|| json!({
                "type": "about:blank", "status": 500, "code": "backup_restore_failed",
                "title": "Backup restoration failed", "detail": "Backup restoration failed",
                "retryable": false,
            })),
            // Kept separate from (and never inserted into) the live SSE registry.
            revision,
        }
    }

    pub(super) fn event(&self) -> &'static str {
        match self.state {
            Phase::Pending | Phase::Running => "progress",
            Phase::Completed => "completed",
            Phase::Cancelled => "cancelled",
            Phase::Failed => "failed",
        }
    }

    pub(super) fn terminal(&self) -> bool {
        self.event() != "progress"
    }
}

struct State {
    snapshot: Snapshot,
    cancel_requested: bool,
    committed: bool,
    finished: Option<Instant>,
}

pub(super) struct Entry {
    scope: RestScope,
    state: Mutex<State>,
    latest: watch::Sender<Snapshot>,
}

impl Entry {
    fn lock(&self) -> V1Result<MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| transport::unavailable())
    }

    fn publish(&self, state: &mut State) {
        state.snapshot.revision += 1;
        self.latest.send_replace(state.snapshot.clone());
    }

    fn finish(&self, state: &mut State, phase: Phase) {
        state.snapshot.state = phase;
        if phase == Phase::Completed {
            state.snapshot.progress_percent = Some(100);
        } else if phase == Phase::Failed && state.snapshot.error.is_none() {
            state.snapshot.error = Some(json!({
                "type": "about:blank", "status": 500, "code": "operation_failed",
                "title": "Operation failed", "detail": "Operation failed", "retryable": false,
            }));
        }
        state.finished = Some(Instant::now());
        self.publish(state);
    }

    pub(super) fn snapshot(&self) -> V1Result<Snapshot> {
        Ok(self.lock()?.snapshot.clone())
    }

    pub(super) fn cancelled(&self) -> V1Result<bool> {
        let state = self.lock()?;
        Ok(state.cancel_requested || state.snapshot.state == Phase::Cancelled)
    }

    pub(super) fn progress(&self, percent: u8) -> V1Result<()> {
        let mut state = self.lock()?;
        if state.snapshot.state == Phase::Running
            && state
                .snapshot
                .progress_percent
                .is_none_or(|old| old < percent)
        {
            state.snapshot.progress_percent = Some(percent);
            self.publish(&mut state);
        }
        Ok(())
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.latest.subscribe()
    }

    pub(super) fn cancel(&self) -> V1Result<Snapshot> {
        let mut state = self.lock()?;
        match state.snapshot.state {
            Phase::Pending => self.finish(&mut state, Phase::Cancelled),
            Phase::Running if state.committed => {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "operation_committed",
                    "Operation has already committed",
                ));
            }
            Phase::Running => state.cancel_requested = true,
            _ => {}
        }
        Ok(state.snapshot.clone())
    }
}

#[derive(Default)]
pub(super) struct Registry(Mutex<HashMap<Uuid, Arc<Entry>>>);

pub(super) fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    fn live_entries(&self) -> V1Result<MutexGuard<'_, HashMap<Uuid, Arc<Entry>>>> {
        let mut entries = self.0.lock().map_err(|_| transport::unavailable())?;
        entries.retain(|_, entry| {
            entry
                .lock()
                .ok()
                .and_then(|state| state.finished)
                .is_none_or(|finished| finished.elapsed() < TERMINAL_RETENTION)
        });
        Ok(entries)
    }

    // The caller already holds its live transport scope.
    pub(super) fn begin(&self, scope: RestScope, kind: &'static str) -> V1Result<Job> {
        let mut entries = self.live_entries()?;
        if entries.len() >= MAX_TRACKED {
            let oldest = entries
                .iter()
                .filter_map(|(id, entry)| {
                    entry.lock().ok()?.finished.map(|finished| (*id, finished))
                })
                .min_by_key(|(_, finished)| *finished);
            let (id, _) = oldest.ok_or_else(transport::unavailable)?;
            entries.remove(&id);
        }
        let operation_id = Uuid::new_v4();
        let snapshot = Snapshot {
            operation_id: operation_id.to_string(),
            kind,
            state: Phase::Pending,
            progress_percent: None,
            error: None,
            revision: 1,
        };
        let entry = Arc::new(Entry {
            scope,
            latest: watch::channel(snapshot.clone()).0,
            state: Mutex::new(State {
                snapshot,
                cancel_requested: false,
                committed: false,
                finished: None,
            }),
        });
        match entries.entry(operation_id) {
            std::collections::hash_map::Entry::Vacant(slot) => slot.insert(Arc::clone(&entry)),
            std::collections::hash_map::Entry::Occupied(_) => return Err(transport::unavailable()),
        };
        Ok(Job(entry))
    }

    pub(super) fn lookup(&self, scope: &RestScope, id: Uuid) -> V1Result<Arc<Entry>> {
        self.live_entries()?
            .get(&id)
            .filter(|entry| &entry.scope == scope)
            .cloned()
            .ok_or_else(not_found)
    }
}

pub(super) fn not_found() -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        "operation_not_found",
        "Operation not found",
    )
}

/// The queue owns one ticket. Dropping unfinished work records failure.
pub(super) struct Job(Arc<Entry>);

impl Job {
    pub(super) fn id(&self) -> V1Result<String> {
        Ok(self.0.lock()?.snapshot.operation_id.clone())
    }

    pub(super) fn model_commit(
        &self,
        final_commit: bool,
        action: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.commit(final_commit, action)
    }

    pub(super) fn commit(
        &self,
        final_commit: bool,
        action: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let mut state = self.0.lock().map_err(|_| anyhow::anyhow!("operation state unavailable"))?;
        anyhow::ensure!(
            state.snapshot.state == Phase::Running && !state.cancel_requested && !state.committed,
            crate::model_progress::OperationStopped::Cancelled
        );
        action()?;
        state.committed = final_commit;
        Ok(())
    }

    pub(super) fn accepted(&self) -> V1Result<Value> {
        Ok(json!({"operation_id": self.0.snapshot()?.operation_id, "status": "pending"}))
    }

    pub(super) fn scope(&self) -> &RestScope {
        &self.0.scope
    }

    pub(super) fn start(&self) -> V1Result<bool> {
        let mut state = self.0.lock()?;
        if state.snapshot.state != Phase::Pending {
            return Ok(false);
        }
        state.snapshot.state = Phase::Running;
        self.0.publish(&mut state);
        Ok(true)
    }

    pub(super) fn entry(&self) -> Arc<Entry> {
        Arc::clone(&self.0)
    }

    pub(super) fn progress(&self, percent: u8) -> V1Result<()> {
        self.0.progress(percent)
    }
    pub(super) fn finish(&self, phase: Phase) {
        if let Ok(mut state) = self.0.lock() {
            if !state.snapshot.terminal() {
                self.0.finish(&mut state, phase);
            }
        }
    }
}

impl Job {
    pub(super) fn fail_problem(&self, problem: &Problem) {
        if let Ok(mut state) = self.0.lock() {
            if !state.snapshot.terminal() {
                state.snapshot.error = Some(problem.body());
                self.0.finish(&mut state, Phase::Failed);
            }
        }
    }

    pub(super) fn fail(&self, code: &'static str) {
        if let Ok(mut state) = self.0.lock() {
            if !state.snapshot.terminal() {
                state.snapshot.error = Some(json!({
                    "type": "about:blank", "status": 500, "code": code,
                    "title": "Model download failed", "detail": "Approved model sources exhausted",
                    "retryable": false,
                }));
                self.0.finish(&mut state, Phase::Failed);
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.finish(Phase::Failed);
    }
}

#[cfg(test)]
pub(in crate::runtime_http::v1) fn check_receipts() -> anyhow::Result<()> {
    fn checked<T>(result: V1Result<T>) -> anyhow::Result<T> {
        result.map_err(|failure| anyhow::anyhow!("{failure:?}"))
    }

    let registry = Registry::default();
    let scope = RestScope::receipt_fixture();
    let job = checked(registry.begin(scope.clone(), "sync"))?;
    let receipt = checked(job.accepted())?;
    let id = Uuid::parse_str(
        receipt["operation_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("operation receipt omitted its id"))?,
    )?;
    assert_eq!(receipt["status"], "pending");
    let entry = checked(registry.lookup(&scope, id))?;
    let mut events = entry.subscribe();
    assert_eq!(events.borrow_and_update().revision, 1);

    let mut changed = scope.clone();
    changed.vault_epoch = Uuid::new_v4();
    assert!(registry.lookup(&changed, id).is_err());
    changed = scope.clone();
    changed.generation += 1;
    assert!(registry.lookup(&changed, id).is_err());
    changed = scope.clone();
    changed.canonical_profile.push("other");
    assert!(registry.lookup(&changed, id).is_err());

    assert!(checked(job.start())?);
    checked(job.progress(40))?;
    checked(job.progress(20))?;
    assert_eq!(events.borrow_and_update().progress_percent, Some(40));
    assert_eq!(checked(entry.cancel())?.event(), "progress");
    assert!(checked(entry.cancelled())?);
    job.finish(Phase::Cancelled);
    assert_eq!(events.borrow_and_update().event(), "cancelled");
    drop(job);
    assert_eq!(checked(entry.snapshot())?.event(), "cancelled");

    let pending = checked(registry.begin(scope.clone(), "sync"))?;
    assert_eq!(checked(pending.entry().cancel())?.event(), "cancelled");
    assert!(!checked(pending.start())?);

    let completed = checked(registry.begin(scope.clone(), "sync"))?;
    assert!(checked(completed.start())?);
    completed.finish(Phase::Completed);
    let completed = checked(completed.entry().snapshot())?;
    assert_eq!(completed.event(), "completed");
    assert_eq!(completed.progress_percent, Some(100));
    let frame = super::routes::snapshot_frame(&completed);
    let response = axum::response::sse::Sse::new(futures_util::stream::iter([Ok::<
        _,
        std::convert::Infallible,
    >(frame)]));
    let response = axum::response::IntoResponse::into_response(response);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "text/event-stream"
    );
    let body = tokio::runtime::Builder::new_current_thread()
        .build()?
        .block_on(axum::body::to_bytes(response.into_body(), 2048))?;
    let wire = std::str::from_utf8(&body)?;
    assert!(wire.lines().any(|line| line == "id: 3"));
    assert!(wire.lines().any(|line| line == "event: completed"));
    let data = wire
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .ok_or_else(|| anyhow::anyhow!("missing SSE data"))?;
    let data: Value = serde_json::from_str(data)?;
    assert_eq!(data["event"], "completed");
    assert_eq!(data["data"]["state"], "completed");
    assert_eq!(data["data"]["progress_percent"], 100);

    let cancelled = checked(registry.begin(scope.clone(), "model_switch"))?;
    assert!(checked(cancelled.start())?);
    checked(cancelled.entry().cancel())?;
    assert!(cancelled.model_commit(true, || panic!("cancelled switch published")).is_err());

    let committed = checked(registry.begin(scope.clone(), "model_switch"))?;
    assert!(checked(committed.start())?);
    committed.model_commit(true, || Ok(()))?;
    assert!(committed.entry().cancel().is_err());
    committed.finish(Phase::Completed);
    assert_eq!(checked(committed.entry().snapshot())?.event(), "completed");

    let abandoned = checked(registry.begin(scope, "sync"))?;
    let mut events = abandoned.entry().subscribe();
    drop(abandoned);
    let failed = events.borrow_and_update();
    assert_eq!(failed.event(), "failed");
    assert_eq!(
        failed
            .error
            .as_ref()
            .and_then(|error| error["code"].as_str()),
        Some("operation_failed")
    );
    Ok(())
}
