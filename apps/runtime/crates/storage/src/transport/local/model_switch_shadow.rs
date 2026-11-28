use super::catalog::MEMORY_SELECT;
use super::model_switch_source::{
    pending_target, pending_task_id, validate_inline_artifact, INSERT_INLINE_ARTIFACT,
};
use super::retrieval::write_active_selection;
use super::{LocalStore, ModelSwitchArtifact, StoredMemory};
use anyhow::{ensure, Result};
use respire_core_sdk::profiles::Selection;
use rusqlite::{params, Transaction, TransactionBehavior};

impl LocalStore {
    pub fn pending_candidate_counts(&self, generation: &str) -> Result<(usize, usize)> {
        let (total, _, indexed, _) = self.light_status_counts(generation)?;
        Ok((usize::try_from(indexed)?, usize::try_from(total)?))
    }

    pub fn fetch_pending_candidate_batch(
        &self,
        generation: &str,
        limit: usize,
    ) -> Result<Vec<StoredMemory>> {
        let mut statement = self.connection.prepare(&format!(
            "{MEMORY_SELECT} m WHERE m.deleted=0 AND NOT EXISTS (
                SELECT 1 FROM core_artifacts a
                WHERE a.memory_id=m.id AND a.model=?1 AND a.source=m.ciphertext
             ) ORDER BY m.dirty DESC, m.updated_at DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![generation, i64::try_from(limit)?],
            Self::row_to_memory,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn checkpoint_candidate_artifacts(
        &self,
        target: &Selection,
        task_id: &str,
        batch: &[ModelSwitchArtifact<'_>],
    ) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.check_shadow_admission(&tx, target, task_id)?;
        let mut indexed = 0;
        {
            let mut insert = tx.prepare(INSERT_INLINE_ARTIFACT)?;
            for row in batch {
                validate_inline_artifact(row.opaque)?;
                indexed += insert.execute(params![
                    row.id,
                    target.generation,
                    row.ciphertext,
                    row.opaque
                ])?;
            }
        }
        tx.commit()?;
        Ok(indexed)
    }

    pub fn activate_shadow_selection(&self, target: &Selection, task_id: &str) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let wire = self.check_shadow_admission(&tx, target, task_id)?;
        ensure!(
            !self.index_missing_in_generation(&target.generation)?,
            "candidate main index is incomplete"
        );
        write_active_selection(&tx, target, &wire)?;
        tx.commit()?;
        Ok(())
    }

    fn check_shadow_admission(
        &self,
        tx: &Transaction<'_>,
        target: &Selection,
        task_id: &str,
    ) -> Result<String> {
        ensure!(
            pending_task_id(tx)? == task_id,
            "model build admission changed; resume the switch"
        );
        let wire = pending_target(tx)?;
        ensure!(
            wire == serde_json::to_string(target)?,
            "model build target changed"
        );
        ensure!(
            self.model_switch_foreground_generation()? != target.generation,
            "model build cannot modify the active generation"
        );
        Ok(wire)
    }
}

#[cfg(test)]
mod tests;
