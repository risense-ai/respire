use super::RuntimeLibrary;
use crate::{
    service::{self, SplitSpec},
    transport::local::{LocalStore, MemorySourceChanged},
    MemoryEngine, MemoryTransport, StoredMemory,
};
use anyhow::{ensure, Result};

impl RuntimeLibrary<'_> {
    /// Commit the outline and all children together against the planned source.
    pub fn apply_split(
        &self,
        store: &LocalStore,
        owner: &str,
        source: &StoredMemory,
        spec: &SplitSpec,
        authorize_commit: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        ensure!(
            spec.summary.is_some()
                && spec.items.len() >= 2
                && spec.items.iter().all(|item| item.parent.is_empty()),
            "scoped split requires an outline and at least two direct children"
        );
        store.write_transaction(|| {
            let row = store
                .active_memory_exact(&source.id)?
                .filter(|row| {
                    row.user == owner
                        && row.local_importance != "trivial"
                        && row.updated_at == source.updated_at
                        && row.ciphertext == source.ciphertext
                        && row.nonce == source.nonce
                })
                .ok_or(MemorySourceChanged)?;
            let entry = self.open_memory(&row)?;
            ensure!(
                entry.user == owner && entry.importance != "trivial",
                MemorySourceChanged
            );
            service::split_exec_with(store, &entry, spec, |entry, owner| {
                let sealed = MemoryEngine::seal_for_indexing(self.0, entry, owner)?;
                ensure!(store.put(&sealed)?, "split write was not committed");
                Ok(())
            })?;
            authorize_commit()
        })
    }
}
