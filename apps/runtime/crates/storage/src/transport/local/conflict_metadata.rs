//! Bounded conflict metadata, without selecting or decoding either payload.
use super::LocalStore;
use anyhow::Result;
use rusqlite::{params, Connection};

#[derive(Debug, PartialEq, Eq)]
pub enum ConflictMetadataState {
    HistoricalEpoch,
    Processed,
    Resolving,
    Pending,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConflictMetadata {
    pub epoch: String,
    pub revision: i64,
    pub memory_id: String,
    pub state: ConflictMetadataState,
    pub current_head_revision: Option<i64>,
    pub resolution: Option<String>,
    pub queued_action: Option<String>,
}

impl LocalStore {
    /// Caller must fence access to the active owner and library.
    pub fn list_conflict_metadata(
        &self,
        memory_id: Option<&str>,
        include_processed: bool,
    ) -> Result<Vec<ConflictMetadata>> {
        read_metadata(&self.connection, memory_id, include_processed)
    }
}

const METADATA_SQL: &str = r#"
WITH active(epoch) AS (
    SELECT COALESCE((SELECT value FROM meta WHERE key = 'sync_v2_epoch'), '')
),
retained AS (
    SELECT i.epoch, i.rev, i.id, i.epoch = active.epoch AS current_epoch,
           r.action AS resolution, o.action AS queued_action,
           0 AS reclaimed
    FROM sync_inbox i CROSS JOIN active
    LEFT JOIN sync_resolutions r ON r.epoch = i.epoch AND r.conflict_rev = i.rev
    LEFT JOIN sync_resolution_outbox o ON o.epoch = i.epoch AND o.conflict_rev = i.rev
    WHERE i.status IN ('conflict_saved', 'legacy_rejected')
      AND (?1 IS NULL OR i.id = ?1)
      AND (?2 OR (i.epoch = active.epoch AND r.conflict_rev IS NULL))
    ORDER BY i.rev DESC, i.epoch DESC
    LIMIT 200
),
reclaimed AS (
    SELECT r.epoch, r.conflict_rev AS rev, r.id, r.epoch = active.epoch AS current_epoch,
           r.action AS resolution, NULL AS queued_action, 1 AS reclaimed
    FROM sync_resolutions r CROSS JOIN active
    WHERE ?2 AND (?1 IS NULL OR r.id = ?1)
      AND NOT EXISTS (SELECT 1 FROM sync_inbox i WHERE i.epoch = r.epoch AND i.rev = r.conflict_rev)
    ORDER BY r.conflict_rev DESC, r.epoch DESC
    LIMIT 200
),
page AS (
    SELECT * FROM retained UNION ALL SELECT * FROM reclaimed
    ORDER BY rev DESC, epoch DESC
    LIMIT 200
)
SELECT page.epoch, page.rev, page.id, page.current_epoch, page.resolution, page.queued_action,
       CASE WHEN page.current_epoch AND NOT page.reclaimed THEN
           (SELECT h.rev FROM sync_remote_heads h WHERE h.id = page.id AND h.wire <> '')
       END
FROM page
ORDER BY page.rev DESC, page.epoch DESC
"#;

fn read_metadata(
    db: &Connection,
    memory_id: Option<&str>,
    include_processed: bool,
) -> Result<Vec<ConflictMetadata>> {
    let mut statement = db.prepare(METADATA_SQL)?;
    let rows = statement.query_map(params![memory_id, include_processed], |row| {
        let current_epoch: bool = row.get(3)?;
        let resolution: Option<String> = row.get(4)?;
        let queued_action: Option<String> = row.get(5)?;
        let state = if !current_epoch {
            ConflictMetadataState::HistoricalEpoch
        } else if resolution.is_some() {
            ConflictMetadataState::Processed
        } else if queued_action.is_some() {
            ConflictMetadataState::Resolving
        } else {
            ConflictMetadataState::Pending
        };
        Ok(ConflictMetadata {
            epoch: row.get(0)?,
            revision: row.get(1)?,
            memory_id: row.get(2)?,
            state,
            current_head_revision: row.get(6)?,
            resolution,
            queued_action,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_conflict_metadata_matches_legacy_status_and_receipt_rules() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = LocalStore::open(&dir.path().join("synthetic.db"))?;
        storage.meta_set("sync_v2_epoch", "current")?;
        // Corrupt/non-JSON payloads intentionally prove this path never decodes them.
        storage.connection.execute_batch("
            INSERT INTO sync_inbox(epoch,rev,id,status,wire,decode_error) VALUES
              ('current',300,'queued','conflict_saved','not-json','private diagnostic'),
              ('current',299,'processed','legacy_rejected','not-json','private diagnostic'),
              ('old',298,'queued','conflict_saved','not-json','private diagnostic'),
              ('current',296,'nonconflict','ignored','not-json','private diagnostic');
            INSERT INTO sync_remote_heads(id,rev,wire) VALUES
              ('queued',9223372036854775807,'not-json'),
              ('processed',307,''),
              ('reclaimed',309,'not-json');
            INSERT INTO sync_resolution_outbox(epoch,conflict_rev,action,expected_head_rev)
              VALUES('current',300,'merge',301);
            INSERT INTO sync_resolutions(epoch,conflict_rev,seq,id,action,head_rev,processed_at) VALUES
              ('current',299,1,'processed','keep_current',301,'test'),
              ('current',297,2,'reclaimed','take_incoming',301,'test'),
              ('current',296,3,'nonconflict','keep_current',301,'test');
        ")?;
        for rev in 1..=250 {
            storage.connection.execute(
                "INSERT INTO sync_inbox(epoch,rev,id,status,wire) VALUES ('current',?1,'bounded','conflict_saved','not-json')",
                [rev],
            )?;
        }
        let visible = storage.list_conflict_metadata(None, false)?;
        assert_eq!(visible.len(), 200);
        assert_eq!(
            visible[0],
            ConflictMetadata {
                epoch: "current".into(),
                revision: 300,
                memory_id: "queued".into(),
                state: ConflictMetadataState::Resolving,
                current_head_revision: Some(i64::MAX),
                resolution: None,
                queued_action: Some("merge".into()),
            }
        );
        assert_eq!(visible[1].revision, 250);
        assert!(visible
            .windows(2)
            .all(|rows| rows[0].revision > rows[1].revision));
        let all = storage.list_conflict_metadata(None, true)?;
        assert_eq!(all.len(), 200);
        assert_eq!(
            all.iter()
                .take(4)
                .map(|row| row.revision)
                .collect::<Vec<_>>(),
            [300, 299, 298, 297]
        );
        assert_eq!(all[1].state, ConflictMetadataState::Processed);
        assert_eq!(all[1].current_head_revision, None);
        assert_eq!(all[2].state, ConflictMetadataState::HistoricalEpoch);
        assert_eq!(all[2].current_head_revision, None);
        assert_eq!(all[3].state, ConflictMetadataState::Processed);
        assert_eq!(all[3].current_head_revision, None);
        assert_eq!(all[3].resolution.as_deref(), Some("take_incoming"));
        assert!(storage
            .list_conflict_metadata(Some("nonconflict"), true)?
            .is_empty());
        let filtered = storage.list_conflict_metadata(Some("queued"), true)?;
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[1].state, ConflictMetadataState::HistoricalEpoch);
        assert!(storage
            .list_conflict_metadata(Some("unknown"), false)?
            .is_empty());
        Ok(())
    }
}
