use super::RuntimeLibrary;
use crate::{service, transport::local::LocalStore, MemoryEngine, MemoryTransport};
use anyhow::{ensure, Context, Result};

pub struct PlannedOutline {
    pub root: String,
    pub id: String,
    pub title: String,
    pub members: Vec<String>,
}

impl RuntimeLibrary<'_> {
    /// Apply the exact preview as one local edit, preserving legacy ciphertext and sync writes.
    pub fn apply_deepen(
        &self,
        store: &LocalStore,
        owner: &str,
        revision: &str,
        generation: &str,
        groups: &[PlannedOutline],
        authorize_commit: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        store.write_transaction(|| {
            ensure!(
                store.memory_revision()? == revision
                    && store.active_generation_key()? == generation,
                "outline or index changed after planning"
            );
            let stamp = service::now_stamp();
            for group in groups {
                let root = store
                    .active_memory_exact(&group.root)?
                    .filter(|row| row.user == owner && row.local_importance != "trivial")
                    .context("outline is no longer in scope")?;
                ensure!(
                    store.updated_at_of(&group.id)?.is_none(),
                    "planned outline ID already exists"
                );
                let node = service::deepen_node(
                    &root,
                    group.id.clone(),
                    &group.title,
                    group.members.len(),
                    &stamp,
                );
                crate::metadata_gate::ensure_write(store, &node)?;
                let sealed = MemoryEngine::seal_for_indexing(self.0, &node, owner)?;
                ensure!(store.put(&sealed)?, "planned outline was not written");
                for id in &group.members {
                    let child = store
                        .active_indexed_memory_exact(id, generation)?
                        .filter(|row| {
                            row.user == owner
                                && row.local_parent_id == root.id
                                && row.local_importance != "trivial"
                                && row.id != root.id
                        })
                        .context("planned member is no longer in scope")?;
                    let entry = MemoryEngine::open(self.0, &child)?;
                    ensure!(
                        entry.parent_id == root.id,
                        "planned member has inconsistent parent data"
                    );
                    crate::metadata_gate::ensure_write(store, &entry)?;
                    let stamp = store.edit_stamp(id)?;
                    let sealed = MemoryEngine::reseal_parent(self.0, &child, &group.id, &stamp)?;
                    ensure!(store.put(&sealed)?, "planned member was not moved");
                }
            }
            authorize_commit()
        })
    }
}
