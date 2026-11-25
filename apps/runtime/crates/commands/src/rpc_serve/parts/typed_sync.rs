use crate::runtime_http::v1::operations::{OperationFailure, OperationJob, OperationScope};

enum QueuedSync {
    Legacy(Job, usize, i64),
    Typed(Box<TypedSync>),
}

impl QueuedSync {
    fn reject(self, legacy_message: &str) {
        match self {
            Self::Legacy(job, _, _) => { let _ = job.reply.send(Err(legacy_message.into())); }
            Self::Typed(job) => job.operation.fail(OperationFailure::WorkFailed),
        }
    }
}

struct TypedSync {
    operation: OperationJob,
    scope: OperationScope,
}

pub(crate) fn enqueue_v1_sync(
    operation: OperationJob,
    scope: OperationScope,
) -> std::result::Result<(), OperationJob> {
    let mut state = SYNC_KICK.lock().unwrap_or_else(|error| error.into_inner());
    if STOPPING.load(Ordering::Acquire) || state.manual.len() >= 64 {
        return Err(operation);
    }
    state.manual.push_back(QueuedSync::Typed(Box::new(TypedSync { operation, scope })));
    drop(state);
    SYNC_CV.notify_one();
    ensure_sync_worker();
    Ok(())
}

impl TypedSync {
    fn run(self) -> bool {
        let Self { operation, scope } = self;
        if !operation.start() {
            operation.cancelled();
            return false;
        }
        let generation = GENERATION.load(Ordering::Acquire);
        let Ok(_identity) = SCOPE_EXECUTION.read() else {
            operation.fail(OperationFailure::WorkFailed);
            return false;
        };
        if !with_locked_owner_scope(|account, profile, instance, generation, vault_epoch| {
            scope.matches_fenced(account, profile, instance, generation, vault_epoch)
        }).unwrap_or(false) {
            operation.fail(OperationFailure::WorkFailed);
            return false;
        }
        let result = (|| -> anyhow::Result<bool> {
            let (session, local, remote, boundary) = sync_local(generation, || {
                let local = crate::engine::build_local()?;
                let boundary = local.outgoing_boundary()?;
                Ok((crate::engine::build_session()?, local, crate::engine::build_remote()?, boundary))
            })?;
            if !operation.begin_commit() {
                return Ok(false);
            }
            SYNC_CONTEXT.with(|context| context.set(Some((generation, boundary))));
            let stats = crate::sync_flow::sync_with_retry(&session, &local, &remote)?;
            let _ = deliver_post_sync(stats.pulled, stats.pushed);
            Ok(true)
        })();
        match result {
            Ok(true) => operation.complete(),
            Ok(false) => operation.cancelled(),
            Err(_) => operation.fail(OperationFailure::WorkFailed),
        }
        result.is_ok_and(|synchronized| synchronized)
    }
}
