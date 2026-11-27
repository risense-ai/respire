//! Typed memory and diary operations under the verified Runtime Vault.
use super::RuntimeLibrary;
use anyhow::Result;

pub struct RuntimeMemoryPage {
    pub entries: Vec<crate::MemoryEntry>,
    pub next: Option<(String, i64)>,
}

impl RuntimeLibrary<'_> {
    pub fn sync_access<'a>(&'a self, store: &'a crate::transport::local::LocalStore)
        -> crate::sync::SyncLocal<'a>
    {
        crate::sync::SyncLocal::new(store, self.0)
    }

    pub fn memory_exact(&self, store: &crate::transport::local::LocalStore, id: &str)
        -> Result<Option<crate::MemoryEntry>>
    {
        store.active_memory_exact(id)?.map(|row| crate::MemoryEngine::open(self.0, &row)).transpose()
    }

    pub fn create_memory(&self, store: &crate::transport::local::LocalStore, entry: &crate::MemoryEntry)
        -> Result<()>
    {
        self.create_memory_with_admission(store, entry, || true)
    }

    pub fn update_memory_exact(
        &self,
        store: &crate::transport::local::LocalStore,
        id: &str,
        edit: impl FnOnce(&mut crate::MemoryEntry),
    ) -> Result<Option<crate::MemoryEntry>> {
        self.update_memory_with_admission(store, id, edit, || true)
    }

    pub fn update_memory_with_admission(
        &self, store: &crate::transport::local::LocalStore, id: &str,
        edit: impl FnOnce(&mut crate::MemoryEntry), admit: impl FnOnce() -> bool,
    ) -> Result<Option<crate::MemoryEntry>> {
        let Some(current) = store.active_memory_exact(id)? else { return Ok(None); };
        let mut entry = crate::MemoryEngine::open(self.0, &current)?;
        edit(&mut entry);
        entry.updated_at = store.edit_stamp(id)?;
        self.commit_memory_if(store, &entry, &current.user, Some(&current.ciphertext), admit)?;
        Ok(Some(entry))
    }

    pub fn create_memory_with_admission(
        &self, store: &crate::transport::local::LocalStore, entry: &crate::MemoryEntry,
        admit: impl FnOnce() -> bool,
    ) -> Result<()> {
        self.commit_memory_if(store, entry, &entry.user, None, admit)
    }

    fn commit_memory_if(&self, store: &crate::transport::local::LocalStore,
        entry: &crate::MemoryEntry, storage_user: &str, previous: Option<&str>,
        admit: impl FnOnce() -> bool,
    ) -> Result<()> {
        crate::metadata_gate::ensure_write(store, entry)?;
        crate::metadata_gate::ensure_sources(self.0, store, entry)?;
        let sealed = crate::MemoryEngine::seal_for_indexing(self.0, entry, storage_user)?;
        anyhow::ensure!(admit(), "memory commit was not admitted");
        anyhow::ensure!(store.put_checked(&sealed, previous)?, "memory write was not committed");
        Ok(())
    }

    pub fn memory_page(&self, store: &crate::transport::local::LocalStore,
        limit: usize, after: Option<(&str, i64)>) -> Result<RuntimeMemoryPage>
    {
        anyhow::ensure!((1..=200).contains(&limit), "invalid memory page size");
        let rows = store.active_memory_page(limit + 1, after)?;
        let next = (rows.len() > limit)
            .then(|| (rows[limit - 1].1.updated_at.clone(), rows[limit - 1].0));
        let entries = rows.into_iter().take(limit)
            .map(|(_, row)| crate::MemoryEngine::open(self.0, &row))
            .collect::<Result<Vec<_>>>()?;
        Ok(RuntimeMemoryPage { entries, next })
    }

    pub fn diary_entries(&self, store: &crate::transport::local::LocalStore,
        date: Option<&str>, day_of: impl Fn(&str) -> String) -> Result<Vec<crate::MemoryEntry>>
    {
        store.active_diary_entries(date, day_of)?.iter()
            .map(|row| crate::MemoryEngine::open(self.0, row)).collect()
    }

    pub fn open_memory(&self, row: &crate::StoredMemory) -> Result<crate::MemoryEntry> {
        crate::MemoryEngine::open(self.0, row)
    }

    pub fn append_diary(&self, store: &crate::transport::local::LocalStore, entry: &crate::MemoryEntry,
        previous: &crate::StoredMemory, duplicates: &[&crate::StoredMemory]) -> Result<()>
    {
        self.append_diary_with_admission(store, entry, previous, duplicates, || true)
    }

    pub fn append_diary_with_admission(&self, store: &crate::transport::local::LocalStore,
        entry: &crate::MemoryEntry, previous: &crate::StoredMemory,
        duplicates: &[&crate::StoredMemory], admit: impl FnOnce() -> bool) -> Result<()>
    {
        crate::metadata_gate::ensure_write(store, entry)?;
        crate::metadata_gate::ensure_sources(self.0, store, entry)?;
        let sealed = crate::MemoryEngine::seal_for_indexing(self.0, entry, &previous.user)?;
        anyhow::ensure!(admit(), "diary commit was not admitted");
        anyhow::ensure!(store.put_diary_checked(&sealed, &previous.ciphertext, duplicates)?,
            "diary write was not committed");
        Ok(())
    }
}
