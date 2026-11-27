use super::*;

const METADATA_INSERT_TRIGGER: &str = "CREATE TRIGGER memory_metadata_gate_insert BEFORE INSERT ON memories
        WHEN COALESCE((SELECT value FROM meta WHERE key='memory_metadata_v1_enabled'),'0') <> '1'
          AND (
            EXISTS(SELECT 1 FROM memories AS old WHERE old.id=NEW.id
              AND (old.memory_contract IS NOT NEW.memory_contract OR old.summary IS NOT NEW.summary
                OR old.source_refs IS NOT NEW.source_refs OR old.valid_until IS NOT NEW.valid_until
                OR old.review_at IS NOT NEW.review_at OR old.artifact IS NOT NEW.artifact
                OR old.verification IS NOT NEW.verification))
            OR (NOT EXISTS(SELECT 1 FROM memories WHERE id=NEW.id)
              AND (NEW.memory_contract IS NOT 0 OR NEW.summary IS NOT NULL
                OR NEW.source_refs IS NOT '[]' OR NEW.valid_until IS NOT NULL
                OR NEW.review_at IS NOT NULL OR NEW.artifact IS NOT NULL OR NEW.verification IS NOT NULL))
          )
        BEGIN SELECT RAISE(ABORT,'memory_metadata_v1_disabled'); END;";

pub(super) fn metadata_insert_trigger_current(connection: &Connection) -> Result<bool> {
    let sql = connection.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='memory_metadata_gate_insert'",
        [], |row| row.get::<_, String>(0),
    ).optional()?;
    let expected = METADATA_INSERT_TRIGGER.trim_end_matches(';');
    Ok(sql.is_some_and(|value| value.split_ascii_whitespace().eq(expected.split_ascii_whitespace())))
}

fn ensure_metadata_insert_trigger(connection: &Connection) -> Result<()> {
    if !metadata_insert_trigger_current(connection)? {
        connection.execute_batch("DROP TRIGGER IF EXISTS memory_metadata_gate_insert;")?;
        connection.execute_batch(METADATA_INSERT_TRIGGER)?;
    }
    Ok(())
}

/// Validate and upgrade an existing migration staging database before publication.
/// This does not create a database or configure the private Core index root.
pub fn validate_migration_snapshot(path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .context("failed to open existing migration staging database")?;
    super::startup_schema::validate_detached(connection)
}

/// Commit schema upgrades together; any unsupported format or SQL failure leaves
/// the original tables and rows intact.
#[cfg(test)]
pub(super) fn migrate(connection: &Connection) -> Result<()> {
    witness::register_witness_functions(connection)?;
    let transaction = connection.unchecked_transaction()?;
    apply_schema_steps(&transaction)?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn apply_schema_steps(connection: &Connection) -> Result<()> {
    migrate_schema(connection)?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS recall_pairs(a TEXT NOT NULL,b TEXT NOT NULL,n INTEGER NOT NULL,PRIMARY KEY(a,b),CHECK(a<b)); CREATE INDEX IF NOT EXISTS recall_pairs_b ON recall_pairs(b);")?;
    Ok(())
}

pub(super) fn validate_existing_memory_schema(connection: &Connection) -> Result<()> {
    checked_legacy_columns(connection).map(|_| ())
}

fn checked_legacy_columns(connection: &Connection) -> Result<Option<Vec<(String, String, i64)>>> {
    let has_old = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='memories'")?
        .exists([])?;
    if has_old {
        let columns: Vec<(String, String, i64)> = connection
            .prepare("PRAGMA table_info(memories)")?
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?, row.get(5)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        anyhow::ensure!(
            !columns.iter().any(|(name, _, _)| name == "tag_hashes")
                && columns.iter().filter(|(_, _, primary)| *primary > 0).count() == 1
                && columns.iter().any(|(name, kind, primary)| {
                    name == "id" && kind.trim().eq_ignore_ascii_case("TEXT") && *primary == 1
                }),
            "unsupported legacy memory schema: historical INTEGER-ID/tag_hashes demos use a different key derivation and plaintext format; no rows were removed. Keep the original database and export with its original version before importing into rsrs"
        );
        for required in ["user", "ciphertext", "nonce", "created_at", "updated_at"] {
            anyhow::ensure!(
                columns.iter().any(|(name, _, _)| name == required),
                "unsupported memory schema: missing {required}; encrypted fields cannot be reconstructed. No rows were removed"
            );
        }
        Ok(Some(columns))
    } else {
        Ok(None)
    }
}

fn migrate_schema(connection: &Connection) -> Result<()> {
    if let Some(columns) = checked_legacy_columns(connection)? {
        // These are derived indexes or local state, not invented key material.
        // The original ciphertext, nonce, identity and timestamps stay untouched.
        for (name, declaration) in [
            ("embedding_enc", "TEXT NOT NULL DEFAULT ''"),
            ("deleted", "INTEGER NOT NULL DEFAULT 0"),
            ("kind", "TEXT NOT NULL DEFAULT 'context'"),
            ("tags", "TEXT NOT NULL DEFAULT ''"),
            ("title", "TEXT NOT NULL DEFAULT ''"),
            ("project", "TEXT NOT NULL DEFAULT ''"),
            ("computer", "TEXT NOT NULL DEFAULT ''"),
            ("embedding", "BLOB"),
        ] {
            if !columns.iter().any(|(existing, _, _)| existing == name) {
                connection.execute_batch(&format!(
                    "ALTER TABLE memories ADD COLUMN {name} {declaration}"
                ))?;
            }
        }
    }
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            user TEXT NOT NULL DEFAULT '',
            ciphertext TEXT NOT NULL DEFAULT '',
            nonce TEXT NOT NULL DEFAULT '',
            embedding_enc TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '',
            deleted INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL DEFAULT 'context',
            tags TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL DEFAULT '',
            project TEXT NOT NULL DEFAULT '',
            computer TEXT NOT NULL DEFAULT '',
            embedding BLOB,
            created_at TEXT NOT NULL DEFAULT '',
            dirty INTEGER NOT NULL DEFAULT 1,
            parent_id TEXT NOT NULL DEFAULT '',
            content_head TEXT NOT NULL DEFAULT '',
            recall_count INTEGER NOT NULL DEFAULT 0,
            importance TEXT NOT NULL DEFAULT 'normal',
            device TEXT NOT NULL DEFAULT '',
            modified_by TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_memories_updated ON memories(updated_at);
        CREATE TABLE IF NOT EXISTS memory_chunks (
            memory_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            embedding BLOB NOT NULL,
            PRIMARY KEY (memory_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_chunks_memory ON memory_chunks(memory_id);
        CREATE TABLE IF NOT EXISTS retrieval_generations (
            memory_id TEXT NOT NULL, model TEXT NOT NULL, source TEXT NOT NULL,
            embedding BLOB NOT NULL, chunks BLOB NOT NULL,
            PRIMARY KEY(memory_id, model)
        );
        CREATE TRIGGER IF NOT EXISTS clear_deleted_retrieval
        AFTER UPDATE OF deleted ON memories WHEN NEW.deleted = 1
        BEGIN
            DELETE FROM memory_chunks WHERE memory_id = NEW.id;
            DELETE FROM retrieval_generations WHERE memory_id = NEW.id;
        END;
        CREATE TRIGGER IF NOT EXISTS clear_removed_retrieval
        AFTER DELETE ON memories
        BEGIN
            DELETE FROM memory_chunks WHERE memory_id = OLD.id;
            DELETE FROM retrieval_generations WHERE memory_id = OLD.id;
        END;
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;
    // v2 → v3 → v4: add columns (stock defaults, later filled by sync/backfill)
    let cols: Vec<String> = connection
        .prepare("PRAGMA table_info(memories)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !cols.iter().any(|c| c == "dirty") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN dirty INTEGER NOT NULL DEFAULT 1;
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "parent_id") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN parent_id TEXT NOT NULL DEFAULT '';
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "content_head") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN content_head TEXT NOT NULL DEFAULT '';
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "recall_count") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "importance") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN importance TEXT NOT NULL DEFAULT 'normal';
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "device") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN device TEXT NOT NULL DEFAULT '';
            ",
        )?;
    }
    if !cols.iter().any(|c| c == "modified_by") {
        connection.execute_batch(
            "
            ALTER TABLE memories ADD COLUMN modified_by TEXT NOT NULL DEFAULT '';
            ",
        )?;
    }
    // Metadata stays unverified until an explicit, gated review writes its payload.
    for (name, declaration) in [
        ("memory_contract", "INTEGER NOT NULL DEFAULT 0"),
        ("summary", "TEXT"),
        ("source_refs", "TEXT NOT NULL DEFAULT '[]'"),
        ("valid_until", "TEXT"),
        ("review_at", "TEXT"),
        ("artifact", "TEXT"),
        ("verification", "TEXT"),
    ] {
        if !cols.iter().any(|column| column == name) {
            connection.execute_batch(&format!(
                "ALTER TABLE memories ADD COLUMN {name} {declaration}"
            ))?;
        }
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_history (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            entry_id TEXT NOT NULL,
            record_revision TEXT NOT NULL,
            action TEXT NOT NULL,
            actor TEXT NOT NULL,
            happened_at TEXT NOT NULL,
            previous_verification TEXT,
            next_verification TEXT,
            previous_review_at TEXT,
            next_review_at TEXT,
            reason TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_review_history_entry ON review_history(entry_id,seq);
        CREATE TRIGGER IF NOT EXISTS memory_metadata_gate_update BEFORE UPDATE ON memories
        WHEN (OLD.memory_contract IS NOT NEW.memory_contract OR OLD.summary IS NOT NEW.summary
          OR OLD.source_refs IS NOT NEW.source_refs OR OLD.valid_until IS NOT NEW.valid_until
          OR OLD.review_at IS NOT NEW.review_at OR OLD.artifact IS NOT NEW.artifact
          OR OLD.verification IS NOT NEW.verification)
          AND COALESCE((SELECT value FROM meta WHERE key='memory_metadata_v1_enabled'),'0') <> '1'
        BEGIN SELECT RAISE(ABORT,'memory_metadata_v1_disabled'); END;
        CREATE TRIGGER IF NOT EXISTS memory_metadata_revision_update AFTER UPDATE ON memories
        WHEN OLD.memory_contract IS NOT NEW.memory_contract OR OLD.summary IS NOT NEW.summary
          OR OLD.source_refs IS NOT NEW.source_refs OR OLD.valid_until IS NOT NEW.valid_until
          OR OLD.review_at IS NOT NEW.review_at OR OLD.artifact IS NOT NEW.artifact
          OR OLD.verification IS NOT NEW.verification
        BEGIN
            UPDATE meta SET value=lower(hex(randomblob(16))) WHERE key='memory_revision_v1';
        END;",
    )?;
    ensure_metadata_insert_trigger(connection)?;
    // parent index: create after the column exists (new DBs already have it; old DBs add the column then reach here)
    connection.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_memories_parent ON memories(parent_id);
        CREATE TABLE IF NOT EXISTS access_grants (
            id TEXT PRIMARY KEY, label TEXT NOT NULL, root_id TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,
            revoked INTEGER NOT NULL DEFAULT 0
        );
        CREATE TRIGGER IF NOT EXISTS revoke_deleted_root
        AFTER UPDATE OF deleted ON memories WHEN NEW.deleted = 1
        BEGIN UPDATE access_grants SET revoked = 1 WHERE root_id = NEW.id; END;
        CREATE TRIGGER IF NOT EXISTS revoke_removed_root
        AFTER DELETE ON memories
        BEGIN UPDATE access_grants SET revoked = 1 WHERE root_id = OLD.id; END;
        CREATE TABLE IF NOT EXISTS query_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts_unix INTEGER NOT NULL,
            ts TEXT NOT NULL,
            query TEXT NOT NULL,
            project TEXT NOT NULL DEFAULT '',
            scope TEXT NOT NULL DEFAULT '',
            candidates TEXT NOT NULL DEFAULT '[]',
            candidate_scores TEXT NOT NULL DEFAULT '[]',
            adopted TEXT NOT NULL DEFAULT '[]',
            good_ids TEXT NOT NULL DEFAULT '[]',
            bad_ids TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_query_log_ts ON query_log(ts_unix);
        CREATE TABLE IF NOT EXISTS skill_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            skill_id TEXT NOT NULL,
            status TEXT NOT NULL,
            note TEXT NOT NULL DEFAULT '',
            ts_unix INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_skill_runs_skill ON skill_runs(skill_id, ts_unix);
        CREATE TABLE IF NOT EXISTS entry_audit (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            entry_id TEXT NOT NULL,
            action TEXT NOT NULL,
            title_head TEXT NOT NULL DEFAULT '',
            content_head TEXT NOT NULL DEFAULT '',
            actor TEXT NOT NULL DEFAULT '',
            ts TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_entry_audit_id ON entry_audit(entry_id);
        CREATE TRIGGER IF NOT EXISTS audit_mem_insert AFTER INSERT ON memories
        BEGIN
            INSERT INTO entry_audit(entry_id,action,title_head,content_head,actor,ts)
            VALUES (NEW.id,'create',NEW.title,substr(NEW.content_head,1,200),NEW.computer,NEW.updated_at);
        END;
        CREATE TRIGGER IF NOT EXISTS audit_mem_update AFTER UPDATE OF title,content_head,parent_id ON memories
        WHEN OLD.title != NEW.title OR OLD.content_head != NEW.content_head OR OLD.parent_id != NEW.parent_id
        BEGIN
            INSERT INTO entry_audit(entry_id,action,title_head,content_head,actor,ts)
            VALUES (NEW.id,'update',NEW.title,substr(NEW.content_head,1,200),NEW.computer,NEW.updated_at);
        END;
        CREATE TRIGGER IF NOT EXISTS audit_mem_delete AFTER UPDATE OF deleted ON memories
        WHEN OLD.deleted = 0 AND NEW.deleted = 1
        BEGIN
            INSERT INTO entry_audit(entry_id,action,title_head,content_head,actor,ts)
            VALUES (NEW.id,'delete',OLD.title,substr(OLD.content_head,1,200),NEW.computer,NEW.updated_at);
        END;
        CREATE TRIGGER IF NOT EXISTS audit_mem_restore AFTER UPDATE OF deleted ON memories
        WHEN OLD.deleted = 1 AND NEW.deleted = 0
        BEGIN
            INSERT INTO entry_audit(entry_id,action,title_head,content_head,actor,ts)
            VALUES (NEW.id,'restore',NEW.title,substr(NEW.content_head,1,200),NEW.computer,NEW.updated_at);
        END;
        CREATE TRIGGER IF NOT EXISTS audit_mem_purge AFTER DELETE ON memories
        BEGIN
            INSERT INTO entry_audit(entry_id,action,title_head,content_head,actor,ts)
            VALUES (OLD.id,'purge',OLD.title,substr(OLD.content_head,1,200),'',OLD.updated_at);
        END;
        ",
    )?;
    if !connection.prepare("PRAGMA table_info(query_log)")?.query_map([],|row|row.get::<_,String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|name|name=="request_id") {
        connection.execute_batch("ALTER TABLE query_log ADD COLUMN request_id TEXT;")?;
    }
    connection.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_query_request ON query_log(request_id) WHERE request_id IS NOT NULL;
        CREATE TABLE IF NOT EXISTS query_feedback(query_log_id INTEGER NOT NULL REFERENCES query_log(id),memory_id TEXT NOT NULL,good INTEGER NOT NULL CHECK(good IN(0,1)),source TEXT NOT NULL CHECK(source IN('human','model','task_outcome')),marked_at INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(query_log_id,memory_id));")?;

    // Keep historical column names for old readers; populate additive replacements once.
    let qcols: Vec<String> = connection
        .prepare("PRAGMA table_info(query_log)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        qcols.iter().any(|column| column == "hit_ids") == qcols.iter().any(|column| column == "hit_scores"),
        "unsupported legacy query_log schema: hit_ids and hit_scores must be present together"
    );
    for (old, new) in [("hit_ids", "candidates"), ("hit_scores", "candidate_scores")] {
        if qcols.iter().any(|column| column == old) && !qcols.iter().any(|column| column == new) {
            connection.execute_batch(&format!(
                "ALTER TABLE query_log ADD COLUMN {new} TEXT NOT NULL DEFAULT '[]';
                 UPDATE query_log SET {new}={old};"
            ))?;
        }
    }
    if !qcols.is_empty() && !qcols.iter().any(|c| c == "good_ids") {
        connection.execute_batch(
            "
            ALTER TABLE query_log ADD COLUMN good_ids TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE query_log ADD COLUMN bad_ids TEXT NOT NULL DEFAULT '[]';
            ",
        )?;
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS core_artifacts (
            memory_id TEXT NOT NULL, model TEXT NOT NULL, source TEXT NOT NULL,
            artifact BLOB NOT NULL, PRIMARY KEY(memory_id,model)
        );",
    )?;
    connection.execute_batch(include_str!("revision_schema.sql"))?;
    // Upgraded libraries can retain content columns absent from the current schema.
    let mut installed_legacy_revision_trigger = false;
    for column in ["when_to_use", "superseded_by"] {
        if cols.iter().any(|existing| existing == column) {
            let name = format!("memory_revision_{column}_update");
            if connection.prepare(
                "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1 COLLATE NOCASE AND tbl_name='memories' COLLATE NOCASE",
            )?.exists([&name])? {
                continue;
            }
            connection.execute_batch(&format!(
                "CREATE TRIGGER {name}
                 AFTER UPDATE OF {column} ON memories
                 WHEN OLD.{column} IS NOT NEW.{column}
                 BEGIN
                     UPDATE meta SET value=lower(hex(randomblob(16))) WHERE key='memory_revision_v1';
                 END;"
            ))?;
            installed_legacy_revision_trigger = true;
        }
    }
    if installed_legacy_revision_trigger {
        // Earlier revisions may have missed updates to these columns.
        anyhow::ensure!(
            connection.execute(
                "UPDATE meta SET value=lower(hex(randomblob(16))) WHERE key='memory_revision_v1'",
                [],
            )? == 1,
            "memory revision unavailable while installing legacy triggers"
        );
    }
    witness::install(connection)?;
    Ok(())
}
